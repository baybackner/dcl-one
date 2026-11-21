//! Streaming notification recipients and messages shared by cleanup workflows.
use crate::gatekeeper::removed_places::PlaceContext;
use crate::gatekeeper::{CommsState, errors::ApiError};
use crate::interfaces::{StreamingNotification, StreamingNotificationKind};
use crate::{
    Address,
    world_identity::{Parcel, ParcelSet, WorldName},
};
use chrono::NaiveDateTime;
use serde_json::json;
use std::collections::BTreeSet;

pub(in crate::gatekeeper) async fn owner(
    state: &CommsState,
    context: &PlaceContext,
) -> Result<Address, ApiError> {
    if let Some(address) = context
        .owner_address
        .as_deref()
        .and_then(|a| Address::parse(a).ok())
        .filter(|_| context.world)
    {
        return Ok(address);
    }
    if context.world {
        let name = WorldName::parse(
            context
                .world_name
                .as_deref()
                .ok_or_else(|| ApiError::http(503, "place owner unavailable"))?,
        )
        .map_err(|_| ApiError::http(503, "place owner unavailable"))?;
        return state
            .policy
            .world_owner(&name)
            .await
            .map_err(|_| ApiError::http(503, "place owner unavailable"))?
            .ok_or_else(|| ApiError::http(503, "place owner unavailable"));
    }
    let parcel = Parcel::parse(&context.base_position)
        .map_err(|_| ApiError::http(503, "place owner unavailable"))?;
    let document = state
        .policy
        .land_document(parcel)
        .await
        .map_err(|_| ApiError::http(503, "place owner unavailable"))?;
    document
        .as_deref()
        .and_then(|d| d.as_ref())
        .and_then(|d| Address::parse(&d.owner).ok())
        .ok_or_else(|| ApiError::http(503, "place owner unavailable"))
}

pub(in crate::gatekeeper) fn notification(
    context: &PlaceContext,
    recipient: Address,
    key: &str,
    kind: StreamingNotificationKind,
    at: NaiveDateTime,
) -> StreamingNotification {
    let location = context
        .world_name
        .as_deref()
        .filter(|_| context.world)
        .unwrap_or(&context.base_position);
    let (title, description) = match kind {
        StreamingNotificationKind::KeyReset => (
            "Stream Key Reset",
            format!("Check the Admin Tools panel at {location} to get the new key."),
        ),
        StreamingNotificationKind::KeyRevoke => (
            "Stream Key Deactivated",
            format!("Check the Admin Tools panel at {location} if you need a new key."),
        ),
        StreamingNotificationKind::KeyExpired => (
            "Stream Key Expired",
            format!("Go to the Admin Tools panel at {location} if you need a new key."),
        ),
        StreamingNotificationKind::TimeExceeded => (
            "Stream Timed Out",
            format!(
                "Restart your stream in your broadcasting software (e.g. OBS) if you'd like to continue streaming to the scene at {location}"
            ),
        ),
        StreamingNotificationKind::PlaceUpdated => (
            "Stream Location Altered",
            format!(
                "If you'd like to stream to {location} please generate a new stream key from the Admin Tools panel in Decentraland."
            ),
        ),
    };
    let query = if context.world {
        format!("realm={location}")
    } else {
        format!("position={location}")
    };
    StreamingNotification {
        event_key: key.to_string(),
        recipient: recipient.clone(),
        kind,
        occurred_at: at,
        metadata: json!({"title": title, "description": description, "position": context.base_position, "worldName": context.world_name, "isWorld": context.world, "url": format!("https://decentraland.org/jump/?{query}"), "address": recipient.as_str(), "image": "https://assets-cdn.decentraland.org/streaming/streaming-notification.png"}),
    }
}

/// Retained grants cover an unresolved notice even after a later revocation.
/// A grant added after this event is never added to its recipient snapshot.
pub(in crate::gatekeeper) async fn scene_admins<'e, E: sqlx::PgExecutor<'e>>(
    executor: E,
    context: &PlaceContext,
    at: NaiveDateTime,
    exact_keys: &[String],
) -> Result<Vec<Address>, sqlx::Error> {
    let positions = context
        .positions
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let addresses: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT admin_address FROM social.scene_admins \
         WHERE added_at <= $4 AND (active OR removed_at >= $4) AND \
         (($1 AND place_id IN (SELECT identity::text FROM explore.world_scene_place \
             WHERE world_name=lower($2) AND active AND parcels && $3::text[]) OR ($1 AND place_id=ANY($6))) \
          OR (NOT $1 AND place_id=$5)) ORDER BY admin_address",
    )
    .bind(context.world)
    .bind(&context.world_name)
    .bind(&positions)
    .bind(at)
    .bind(&context.base_position)
    .bind(exact_keys)
    .fetch_all(executor)
    .await?;
    Ok(addresses
        .into_iter()
        .filter_map(|value| Address::parse(&value).ok())
        .collect())
}

/// Resolve every authority before marking a recipient snapshot complete.
/// Missing authority remains pending; callers retain the locally captured set.
pub(in crate::gatekeeper) async fn recipients(
    state: &CommsState,
    context: &PlaceContext,
    preserved: &[Address],
) -> Result<(Address, Vec<Address>), ApiError> {
    let mut recipients: BTreeSet<Address> = preserved.iter().cloned().collect();
    let owner = owner(state, context).await?;
    recipients.insert(owner.clone());
    if context.world {
        let world = context
            .world_name
            .as_deref()
            .and_then(|name| WorldName::parse(name).ok())
            .ok_or_else(|| ApiError::http(503, "world notification authority unavailable"))?;
        let positions = context
            .positions
            .as_array()
            .ok_or_else(|| ApiError::http(503, "world notification footprint unavailable"))?;
        let positions: Vec<_> = positions
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| ApiError::http(503, "world notification footprint invalid"))
            })
            .collect::<Result<_, _>>()?;
        let parcels = ParcelSet::parse(&positions)
            .map_err(|_| ApiError::http(503, "world notification footprint invalid"))?;
        if parcels.is_empty() {
            return Err(ApiError::http(
                503,
                "world notification footprint unavailable",
            ));
        }
        recipients.extend(
            state
                .policy
                .world_grantees(&world, &parcels)
                .await
                .map_err(|_| ApiError::http(503, "world notification permissions unavailable"))?,
        );
    } else {
        let parcel = Parcel::parse(&context.base_position)
            .map_err(|_| ApiError::http(503, "LAND notification authority unavailable"))?;
        let document = state
            .policy
            .land_document(parcel)
            .await
            .map_err(|_| ApiError::http(503, "LAND notification authority unavailable"))?;
        let document = document
            .as_deref()
            .and_then(Option::as_ref)
            .ok_or_else(|| ApiError::http(503, "LAND notification authority unavailable"))?;
        for address in document.addresses() {
            recipients.insert(
                Address::parse(&address)
                    .map_err(|_| ApiError::http(503, "LAND notification authority invalid"))?,
            );
        }
    }
    Ok((owner, recipients.into_iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_preserve_location_recipient_and_event_identity() {
        let recipient = Address::parse("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let at = chrono::DateTime::from_timestamp(123, 0)
            .unwrap()
            .naive_utc();
        for world in [false, true] {
            let context = PlaceContext {
                place_id: "place".into(),
                owner_address: None,
                world,
                world_name: Some("scene.dcl.eth".into()),
                base_position: "5,6".into(),
                positions: json!(["5,6"]),
            };
            for (kind, title) in [
                (StreamingNotificationKind::KeyReset, "Stream Key Reset"),
                (
                    StreamingNotificationKind::KeyRevoke,
                    "Stream Key Deactivated",
                ),
                (StreamingNotificationKind::KeyExpired, "Stream Key Expired"),
                (StreamingNotificationKind::TimeExceeded, "Stream Timed Out"),
                (
                    StreamingNotificationKind::PlaceUpdated,
                    "Stream Location Altered",
                ),
            ] {
                let event = notification(&context, recipient.clone(), "event-key", kind, at);
                assert_eq!(event.event_key, "event-key");
                assert_eq!(event.recipient, recipient);
                assert_eq!(event.occurred_at, at);
                assert_eq!(event.metadata["title"], title);
                assert_eq!(event.metadata["address"], recipient.as_str());
                assert_eq!(event.metadata["position"], "5,6");
                assert_eq!(event.metadata["worldName"], "scene.dcl.eth");
                assert_eq!(event.metadata["isWorld"], world);
                assert_eq!(
                    event.metadata["url"],
                    if world {
                        "https://decentraland.org/jump/?realm=scene.dcl.eth"
                    } else {
                        "https://decentraland.org/jump/?position=5,6"
                    }
                );
                assert!(
                    event.metadata["description"]
                        .as_str()
                        .unwrap()
                        .contains(if world { "scene.dcl.eth" } else { "5,6" })
                );
            }
        }
    }
}

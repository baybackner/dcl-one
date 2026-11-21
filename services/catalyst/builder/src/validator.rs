//! The wearable validator's work: the automated review of a standard
//! collection's items, which the validator pulls from the node.
//!
//! A standard collection's first publication and a creator's changes
//! submission record a validation request ([`on_publish`], [`on_changes`])
//! in the transaction that makes them: the item ids, at most one open request
//! per collection (a newer one supersedes it), at most [`MAX_ITEMS`] items
//! (a larger collection gets none and the curators a notice) and at most
//! [`DAILY_REQUESTS`] per collection per UTC day. Nothing is recorded unless
//! `WEARABLE_VALIDATOR_CALLBACK_SECRET` is set and the `builder` flags
//! document, with this node's overrides, has `builder-auto-curation` on
//! ([`enabled`]).
//!
//! The validator claims the oldest claimable request, extends its lease and
//! posts its verdict ([`claims`], over the routes in
//! `handlers::validator`, each call signed with the secret). A verdict is
//! advisory: it settles its request and is logged; it writes no curation.
//! The `builder-validation-requests` job ([`sweep`]) abandons a request
//! after its third lapsed lease, expires one left unsettled for
//! [`UNSETTLED_FOR`], each with a notice to the curators, deletes settled
//! requests after [`KEPT_FOR`] and the seen signatures whose window has
//! passed.

pub mod claims;

use std::collections::HashMap;
use std::time::Duration;

use chrono::{NaiveTime, TimeDelta};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::State;
use crate::chain_state::{self, DecisionView};
use crate::content::JsJson;
use crate::curation::slack::Notice;
use crate::http::ApiError;
use crate::items::hash::{self, Hashed};
use crate::items::schema::{ItemType, sanitize};
use crate::shared::feature_flags;
use crate::shared::limits::RateLimiter;

/// The flags application the builder's flags are read under.
pub const APP: &str = "builder";
/// The feature whose flag (`builder-auto-curation`) turns the automated
/// review on.
pub const FEATURE: &str = "auto-curation";
/// The most items one request names; the validator takes no more.
pub const MAX_ITEMS: usize = 50;
/// The most requests one collection gets in a UTC day.
pub const DAILY_REQUESTS: i64 = 5;
/// How long a claim holds its request.
pub const LEASE: TimeDelta = TimeDelta::minutes(30);
/// How long after its claim a lease may be extended to.
pub const LEASE_LIMIT: TimeDelta = TimeDelta::hours(2);
/// How many claims a request takes before it is abandoned.
pub const MAX_CLAIMS: i16 = 3;
/// How long a request may stay unsettled before it expires.
pub const UNSETTLED_FOR: TimeDelta = TimeDelta::days(7);
/// How long a settled request is kept.
pub const KEPT_FOR: TimeDelta = TimeDelta::days(30);

/// `WEARABLE_VALIDATOR_CALLBACK_SECRET`, never printed.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// The validator's calls per source address: 60 a minute.
pub fn calls_limiter() -> RateLimiter {
    RateLimiter::new(60, Duration::from_secs(60))
}

/// Whether the automated review runs: the secret is set and the `builder`
/// flags document, with this node's overrides, has `builder-auto-curation` on as the
/// boolean `true` (`shared::feature_flags`); read on `conn`, the caller's
/// transaction.
pub async fn enabled(conn: &mut PgConnection, state: &State) -> Result<bool, ApiError> {
    if state.settings.validator_secret.is_none() {
        return Ok(false);
    }
    let document: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT document FROM explore_api.feature_flags WHERE app = $1")
            .bind(APP)
            .fetch_optional(&mut *conn)
            .await?;
    let overrides: Vec<(String, bool)> = sqlx::query_as(
        "SELECT name, value FROM explore_api.feature_flags_local WHERE app = $1 ORDER BY name",
    )
    .bind(APP)
    .fetch_all(&mut *conn)
    .await?;
    let document = feature_flags::apply_local_overrides(
        document.unwrap_or_else(|| serde_json::json!({ "flags": {}, "variants": {} })),
        &overrides,
    );
    Ok(feature_flags::Snapshot::from_document(document).enabled(APP, FEATURE))
}

/// What started a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Publish,
    Changes,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Changes => "changes",
        }
    }
}

/// A minted item of a collection as saved now.
pub(crate) struct Saved {
    pub id: Uuid,
    pub blockchain_item_id: String,
    name: String,
    description: Option<String>,
    rarity: Option<String>,
    item_type: ItemType,
    data: JsJson,
    metrics: JsJson,
    contents: Vec<(String, String)>,
}

type SavedRow = (
    Uuid,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
    String,
);

impl Saved {
    fn hashed(&self) -> Hashed<'_> {
        Hashed {
            name: &self.name,
            description: self.description.as_deref(),
            rarity: Some(self.rarity.as_deref()),
            item_type: self.item_type,
            data: &self.data,
            metrics: &self.metrics,
            contents: &self.contents,
            mappings: None,
        }
    }

    /// The item's entity hash under `contract`.
    pub fn content_hash(&self, contract: &str) -> String {
        hash::standard(&self.hashed(), contract, &self.blockchain_item_id)
    }

    /// The entity metadata the catalyst would receive for the item.
    pub fn metadata(&self, contract: &str) -> JsJson {
        hash::standard_metadata(&self.hashed(), contract, &self.blockchain_item_id)
    }

    /// The files the item deploys, by path.
    pub fn deployed_files(&self) -> JsJson {
        JsJson::Object(
            hash::deployed_files(&self.hashed())
                .map(|(file, hash)| (file.clone(), JsJson::String(hash.clone())))
                .collect(),
        )
    }
}

/// The minted items of `collection` as saved now, cleaned as builder-server
/// cleans them, in creation order; with `only`, those of them.
pub(crate) async fn saved_items(
    conn: &mut PgConnection,
    collection: Uuid,
    only: Option<&[Uuid]>,
) -> Result<Vec<Saved>, ApiError> {
    let rows: Vec<SavedRow> = sqlx::query_as(
        "SELECT i.id, i.blockchain_item_id, i.name, i.description, i.rarity, i.type, i.data::text, i.metrics::text, \
           COALESCE((SELECT json_agg(json_build_array(f.file, f.hash) ORDER BY f.position, f.file) \
             FROM builder.item_contents f WHERE f.item_id = i.id), '[]'::json)::text \
         FROM builder.items i \
         WHERE i.collection_id = $1 AND i.blockchain_item_id IS NOT NULL AND ($2::uuid[] IS NULL OR i.id = ANY($2)) \
         ORDER BY i.created_at, i.id",
    )
    .bind(collection)
    .bind(only)
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(
            |(id, blockchain_item_id, name, description, rarity, kind, data, metrics, contents)| {
                let invalid =
                    |what: &str| ApiError::internal(format!("item {id}: stored {what} unreadable"));
                let mut data: JsJson = serde_json::from_str(&data).map_err(|_| invalid("data"))?;
                let metrics = match metrics {
                    Some(metrics) => {
                        serde_json::from_str(&metrics).map_err(|_| invalid("metrics"))?
                    }
                    None => JsJson::Null,
                };
                let mut contents: Vec<(String, String)> =
                    serde_json::from_str(&contents).map_err(|_| invalid("contents"))?;
                sanitize(&mut contents, &mut data);
                Ok(Saved {
                    id,
                    blockchain_item_id,
                    name,
                    description,
                    rarity,
                    item_type: if kind == "emote" {
                        ItemType::Emote
                    } else {
                        ItemType::Wearable
                    },
                    data,
                    metrics,
                    contents,
                })
            },
        )
        .collect()
}

/// Records a request for `items` of `collection` on `conn`, which holds the
/// collection's row lock; answers the curators' notice when it records none
/// because the collection is too large.
async fn record(
    conn: &mut PgConnection,
    state: &State,
    collection: Uuid,
    trigger: Trigger,
    items: &[Uuid],
) -> Result<Option<Notice>, ApiError> {
    if items.is_empty() {
        tracing::info!(%collection, trigger = trigger.as_str(), "builder validation: no item to review");
        return Ok(None);
    }
    if items.len() > MAX_ITEMS {
        tracing::info!(%collection, trigger = trigger.as_str(), items = items.len(), "builder validation: too large to request");
        return Ok(Some(Notice::ValidationNotRequested {
            reason: format!(
                "it is too large for the automated review ({} items)",
                items.len()
            ),
        }));
    }
    let now = (state.now)().naive_utc();
    let day = now.date().and_time(NaiveTime::MIN);
    let today: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM builder.validation_requests \
         WHERE collection_id = $1 AND created_at >= $2 AND created_at < $3",
    )
    .bind(collection)
    .bind(day)
    .bind(day + TimeDelta::days(1))
    .fetch_one(&mut *conn)
    .await?;
    if today >= DAILY_REQUESTS {
        tracing::info!(%collection, trigger = trigger.as_str(), "builder validation: the collection's requests for the day are spent");
        return Ok(None);
    }
    sqlx::query(
        "UPDATE builder.validation_requests SET settled_at = $2, outcome = 'superseded' \
         WHERE collection_id = $1 AND settled_at IS NULL",
    )
    .bind(collection)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO builder.validation_requests (id, collection_id, trigger, item_ids, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(collection)
    .bind(trigger.as_str())
    .bind(items)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    tracing::info!(validation = %id, %collection, trigger = trigger.as_str(), items = items.len(), "builder validation requested");
    Ok(None)
}

/// A standard collection's first publication of `items`, on the
/// publication's transaction.
pub(crate) async fn on_publish(
    conn: &mut PgConnection,
    state: &State,
    collection: Uuid,
    items: &[Uuid],
) -> Result<Option<Notice>, ApiError> {
    if !enabled(conn, state).await? {
        return Ok(None);
    }
    record(conn, state, collection, Trigger::Publish, items).await
}

/// A creator's changes submission on a published standard collection at
/// `contract`, on the submission's transaction: the minted items whose
/// content differs from what the chain copy holds.
pub(crate) async fn on_changes(
    conn: &mut PgConnection,
    state: &State,
    view: &DecisionView,
    collection: Uuid,
    contract: &str,
) -> Result<Option<Notice>, ApiError> {
    if !enabled(conn, state).await? {
        return Ok(None);
    }
    let on_chain: HashMap<String, Option<String>> = match chain_state::collection(conn, view, contract).await {
        Ok(Some(_)) => sqlx::query_as(
            "SELECT item_id::text, content_hash FROM shop.items WHERE network = 'matic' AND contract_address = lower($1)",
        )
        .bind(contract)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect(),
        Ok(None) => HashMap::new(),
        Err(error) => {
            tracing::warn!(%collection, %error, "builder validation: the chain copy can't tell what changed");
            return Ok(Some(Notice::ValidationNotRequested {
                reason: "the automated review could not be requested".into(),
            }));
        }
    };
    let changed: Vec<Uuid> = saved_items(conn, collection, None)
        .await?
        .iter()
        .filter(|item| {
            on_chain.get(&item.blockchain_item_id).cloned().flatten()
                != Some(item.content_hash(contract))
        })
        .map(|item| item.id)
        .collect();
    record(conn, state, collection, Trigger::Changes, &changed).await
}

/// One pass of the `builder-validation-requests` job: abandons the requests
/// whose third lease lapsed, expires those unsettled for [`UNSETTLED_FOR`]
/// (each with a notice to the curators, after commit), deletes settled ones
/// after [`KEPT_FOR`] and the seen signatures whose window has passed.
/// Nothing while creator writes are off.
pub async fn sweep(state: &State) -> anyhow::Result<()> {
    if !state.settings.writes_enabled {
        return Ok(());
    }
    let now = (state.now)().naive_utc();
    let pool = state.items.pool();
    let abandoned: Vec<(Uuid, String)> = sqlx::query_as(
        "UPDATE builder.validation_requests r SET settled_at = $1, outcome = 'abandoned' \
         FROM builder.collections c \
         WHERE c.id = r.collection_id AND r.settled_at IS NULL AND r.claims >= $2 AND r.lease_until < $1 \
         RETURNING r.collection_id, c.name",
    )
    .bind(now)
    .bind(MAX_CLAIMS)
    .fetch_all(pool)
    .await?;
    let expired: Vec<(Uuid, String)> = sqlx::query_as(
        "UPDATE builder.validation_requests r SET settled_at = $1, outcome = 'expired' \
         FROM builder.collections c \
         WHERE c.id = r.collection_id AND r.settled_at IS NULL AND r.created_at <= $2 \
           AND (r.lease_until IS NULL OR r.lease_until < $1) \
         RETURNING r.collection_id, c.name",
    )
    .bind(now)
    .bind(now - UNSETTLED_FOR)
    .fetch_all(pool)
    .await?;
    let deleted = sqlx::query("DELETE FROM builder.validation_requests WHERE settled_at <= $1")
        .bind(now - KEPT_FOR)
        .execute(pool)
        .await?
        .rows_affected();
    sqlx::query("DELETE FROM builder.validator_seen_signatures WHERE expires_at < $1")
        .bind(now)
        .execute(pool)
        .await?;
    for (reason, settled) in [
        (
            "the automated review got no verdict after 3 attempts",
            &abandoned,
        ),
        ("the automated review did not run", &expired),
    ] {
        for (collection, name) in settled {
            tracing::info!(%collection, reason, "builder validation settled without a verdict");
            state.notices.send(
                &state.guarded,
                *collection,
                name,
                "",
                Notice::ValidationNotRequested {
                    reason: reason.into(),
                },
            );
        }
    }
    if deleted > 0 {
        tracing::info!(deleted, "builder validation: settled requests deleted");
    }
    Ok(())
}

use axum::Json;
use axum::extract::{Path, State};
use bytes::Bytes;
use serde_json::{Value, json};

use crate::SharedState;
use crate::access;
use crate::auth::Signed;
use crate::collections::lifecycle::{self, Published};
use crate::http::{ApiData, ApiError, iso};
use crate::terms;

/// `POST /v1/collections/:id/lock` (`lockCollection`): the lock's date.
pub async fn lock(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
) -> Result<Json<ApiData<String>>, ApiError> {
    let lock = access::lock_collection(&state, &signer, &id).await?;
    let at = lifecycle::lock(&state, &lock).await?;
    Ok(Json(ApiData::ok(iso(&at))))
}

/// `DELETE /v1/collections/:id` (`deleteCollection`): `data` is `true`.
pub async fn delete(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
) -> Result<Json<ApiData<bool>>, ApiError> {
    let manage = access::manage_collection(&state, &signer, &id).await?;
    lifecycle::delete(&state, &manage).await?;
    Ok(Json(ApiData::ok(true)))
}

/// `POST /v1/collections/:id/owner/sync` (`syncCollectionOwner`): the
/// collection's full body, whose owner the node reads from the chain; it
/// stores nothing.
pub async fn owner_sync(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
) -> Result<Json<ApiData<Value>>, ApiError> {
    let sync = access::sync_collection(&state, &signer, &id).await?;
    if sync.third_party() {
        return Err(
            ApiError::bad_request("Third party collections have no on-chain owner")
                .with_data(json!({ "id": sync.id() })),
        );
    }
    Ok(Json(ApiData::ok(
        state.items.collection(&sync.full_read()).await?,
    )))
}

/// `POST /v1/collections/:id/tos` (`saveTOS`): who may, then the body,
/// then the acceptance stored and forwarded; answers `{ok: true}` without
/// data.
pub async fn terms(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let manage = access::manage_collection(&state, &signer, &id).await?;
    let body: Value =
        serde_json::from_slice(&body).map_err(|_| ApiError::bad_request("Invalid request body"))?;
    terms::accept(&state, &manage, terms::parse(&body)?).await?;
    Ok(Json(json!({ "ok": true })))
}

/// `POST /v1/collections/:id/publish` (`publishCollection`) for a standard
/// collection: `{collection, items}` once the chain holds it.
pub async fn publish(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
) -> Result<Json<ApiData<Published>>, ApiError> {
    let sync = access::sync_collection(&state, &signer, &id).await?;
    if sync.third_party() {
        return Err(ApiError::bad_request(
            "Third party collections are published through the linked publication",
        )
        .with_data(json!({ "id": sync.id() })));
    }
    Ok(Json(ApiData::ok(lifecycle::publish(&state, &sync).await?)))
}

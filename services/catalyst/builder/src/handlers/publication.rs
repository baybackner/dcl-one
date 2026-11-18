use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use uuid::Uuid;

use crate::SharedState;
use crate::http::{ApiData, ApiError};
use crate::publication_chain::PublicationChain;
use crate::publication_store::PublicationState;

use crate::auth::Signed;

#[derive(Deserialize)]
pub struct BeginPublication {
    revision: String,
}
#[derive(Deserialize)]
pub struct PublicationTransaction {
    tx_hash: String,
}

fn rpc_url(state: &SharedState) -> Result<&str, ApiError> {
    state.settings.polygon_rpc_url.as_deref().ok_or_else(|| {
        ApiError::service_unavailable(
            "Collection publication is unavailable until the Polygon verifier is configured",
        )
    })
}

pub async fn begin(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<BeginPublication>,
) -> Result<Json<ApiData<PublicationState>>, ApiError> {
    rpc_url(&state)?;
    Ok(Json(ApiData::ok(
        state
            .items
            .begin_publication(owner.as_str(), id, &body.revision)
            .await?,
    )))
}

pub async fn status(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<Option<PublicationState>>>, ApiError> {
    Ok(Json(ApiData::ok(
        state.items.publication_state(owner.as_str(), id).await?,
    )))
}

pub async fn transaction(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<PublicationTransaction>,
) -> Result<Json<ApiData<PublicationState>>, ApiError> {
    let chain = PublicationChain {
        http: &state.http,
        url: rpc_url(&state)?,
    };
    Ok(Json(ApiData::ok(
        chain
            .verify(&state.items, owner.as_str(), id, &body.tx_hash)
            .await?,
    )))
}

pub async fn cancel(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<bool>>, ApiError> {
    state.items.cancel_publication(owner.as_str(), id).await?;
    Ok(Json(ApiData::ok(true)))
}

pub async fn claim(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<BeginPublication>,
) -> Result<Json<ApiData<bool>>, ApiError> {
    rpc_url(&state)?;
    state
        .items
        .claim_publication(owner.as_str(), id, &body.revision)
        .await?;
    Ok(Json(ApiData::ok(true)))
}

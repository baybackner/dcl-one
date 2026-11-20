use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use uuid::Uuid;

use crate::SharedState;
use crate::access;
use crate::http::{ApiData, ApiError};
use crate::linked_publication::LinkedPublicationCheque;
use crate::linked_publication_review::{FoundationLinkedReview, FoundationReviewSignatures};
use crate::linked_publication_store::{LinkedPublicationPreparation, LinkedPublicationState};

use crate::auth::Signed;

#[derive(Deserialize)]
pub struct Revision {
    revision: String,
}

pub async fn prepare(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<LinkedPublicationPreparation>>, ApiError> {
    let publisher = access::publisher(&state, &owner, id).await?;
    Ok(Json(ApiData::ok(
        state.items.prepare_linked_publication(&publisher).await?,
    )))
}

pub async fn status(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<Option<LinkedPublicationState>>>, ApiError> {
    let publisher = access::publisher(&state, &owner, id).await?;
    Ok(Json(ApiData::ok(
        state.items.linked_publication_state(&publisher).await?,
    )))
}

pub async fn begin(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<Revision>,
) -> Result<Json<ApiData<LinkedPublicationState>>, ApiError> {
    let publisher = access::publisher(&state, &owner, id).await?;
    Ok(Json(ApiData::ok(
        state
            .items
            .begin_linked_publication(&publisher, &body.revision)
            .await?,
    )))
}

pub async fn claim(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<Revision>,
) -> Result<Json<ApiData<LinkedPublicationState>>, ApiError> {
    let publisher = access::publisher(&state, &owner, id).await?;
    Ok(Json(ApiData::ok(
        state
            .items
            .claim_linked_publication(&publisher, &body.revision)
            .await?,
    )))
}

pub async fn authorize(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<LinkedPublicationCheque>,
) -> Result<Json<ApiData<LinkedPublicationState>>, ApiError> {
    let publisher = access::publisher(&state, &owner, id).await?;
    Ok(Json(ApiData::ok(
        state
            .items
            .authorize_linked_publication(&publisher, &body)
            .await?,
    )))
}

pub async fn cancel(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<bool>>, ApiError> {
    let publisher = access::publisher(&state, &owner, id).await?;
    state.items.cancel_linked_publication(&publisher).await?;
    Ok(Json(ApiData::ok(true)))
}

pub async fn verify(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<FoundationReviewSignatures>,
) -> Result<Json<ApiData<LinkedPublicationState>>, ApiError> {
    let Some(verifier) = FoundationLinkedReview::new(
        state.foundation.clone(),
        state.settings.foundation_url.as_deref(),
        state.settings.forum.as_ref().map(|f| f.url.as_str()),
    ) else {
        return Err(ApiError::service_unavailable(
            "Foundation review is off on this node: BUILDER_FOUNDATION_URL names no Foundation",
        ));
    };
    let publisher = access::publisher(&state, &owner, id).await?;
    Ok(Json(ApiData::ok(
        verifier.verify(&state.items, &publisher, &body).await?,
    )))
}

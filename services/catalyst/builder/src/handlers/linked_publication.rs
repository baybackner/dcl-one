use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use uuid::Uuid;

use crate::SharedState;
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
    Ok(Json(ApiData::ok(
        state
            .items
            .prepare_linked_publication(owner.as_str(), id)
            .await?,
    )))
}

pub async fn status(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<Option<LinkedPublicationState>>>, ApiError> {
    Ok(Json(ApiData::ok(
        state
            .items
            .linked_publication_state(owner.as_str(), id)
            .await?,
    )))
}

pub async fn begin(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<Revision>,
) -> Result<Json<ApiData<LinkedPublicationState>>, ApiError> {
    Ok(Json(ApiData::ok(
        state
            .items
            .begin_linked_publication(owner.as_str(), id, &body.revision)
            .await?,
    )))
}

pub async fn claim(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<Revision>,
) -> Result<Json<ApiData<LinkedPublicationState>>, ApiError> {
    Ok(Json(ApiData::ok(
        state
            .items
            .claim_linked_publication(owner.as_str(), id, &body.revision)
            .await?,
    )))
}

pub async fn authorize(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<LinkedPublicationCheque>,
) -> Result<Json<ApiData<LinkedPublicationState>>, ApiError> {
    Ok(Json(ApiData::ok(
        state
            .items
            .authorize_linked_publication(owner.as_str(), id, &body)
            .await?,
    )))
}

pub async fn cancel(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<bool>>, ApiError> {
    state
        .items
        .cancel_linked_publication(owner.as_str(), id)
        .await?;
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
    ) else {
        return Err(ApiError::service_unavailable(
            "Foundation review is off on this node: BUILDER_FOUNDATION_URL names no Foundation",
        ));
    };
    Ok(Json(ApiData::ok(
        verifier
            .verify(&state.items, owner.as_str(), id, &body)
            .await?,
    )))
}

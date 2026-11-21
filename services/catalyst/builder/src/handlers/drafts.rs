use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::Value;
use uuid::Uuid;

use crate::SharedState;
use crate::access;
use crate::auth::Signed;
use crate::collections::{lifecycle, save};
use crate::drafts::CollectionListingOut;
use crate::http::{ApiData, ApiError};
use crate::items::body::FullItemOut;
use crate::items::{save as item_save, schema, upload, video};
use crate::publication::PublicationPreparation;

/// `GET /v1/drafts/collections`: the signer's draft collections, charged by
/// the row.
pub async fn get_collection_drafts(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    headers: HeaderMap,
) -> Result<Json<ApiData<Vec<CollectionListingOut>>>, ApiError> {
    let drafts = state
        .items
        .collection_drafts(&access::managing(&state, &signer).await?)
        .await?;
    crate::limits::charge_rows(&state, &headers, Some(signer.as_str()), drafts.len());
    Ok(Json(ApiData::ok(drafts)))
}

/// `GET /v1/drafts/items`: the signer's draft items, charged by the row.
pub async fn get_item_drafts(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    headers: HeaderMap,
) -> Result<Json<ApiData<Vec<Value>>>, ApiError> {
    let drafts = state
        .items
        .item_drafts(&access::managing(&state, &signer).await?)
        .await?;
    crate::limits::charge_rows(&state, &headers, Some(signer.as_str()), drafts.len());
    Ok(Json(ApiData::ok(drafts)))
}

/// `PUT /v1/collections/:id` (`upsertCollection`): the body's schema, the
/// ids, a standard collection's `data`, then who may save it
/// (`access::edit_collection`), then the `initialize` calldata, the check
/// that a new address leaves no deployed contract behind
/// (`collections::lifecycle::guard_address_change`) and the save's own
/// rules (`collections::save`).
pub async fn put_collection(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<ApiData<Value>>, ApiError> {
    let save = save::parse(&body)?;
    if save.id != id {
        return Err(
            ApiError::bad_request("The body and URL collection ids do not match")
                .with_data(serde_json::json!({ "urlId": id, "bodyId": save.id })),
        );
    }
    let uuid = Uuid::parse_str(&id).map_err(|_| ApiError::bad_request("Invalid request body"))?;
    let provider = save
        .urn
        .as_deref()
        .and_then(save::third_party_urn)
        .map(|(provider, _)| provider);
    let standard = provider.is_none();
    if standard {
        save::standard_request(&save, signer.as_str())?;
    }
    let edit = access::edit_collection(&state, &signer, uuid, provider).await?;
    let deployment = standard
        .then(|| save::standard_deployment(&save, signer.as_str()))
        .transpose()?;
    if let Some(deployment) = &deployment {
        lifecycle::guard_address_change(&state, &edit, deployment.contract_address()).await?;
    }
    Ok(Json(ApiData::ok(
        state
            .items
            .save_collection(&edit, &save, deployment)
            .await?,
    )))
}

/// `PUT /v1/items/:idOrURN` (`upsertItem`): the signer, the body's schema
/// (`items::schema`), the path against the body (`items::save::prepare`),
/// who may save it (`access::save_item`), then the save.
pub async fn put_item(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(path): Path<String>,
    body: Bytes,
) -> Result<Json<ApiData<FullItemOut>>, ApiError> {
    let input = schema::parse(&body)?;
    state.items.require_documents_release().await?;
    let target = item_save::prepare(&state, &path, &input).await?;
    let grant = access::save_item(&state, &signer, target.id, &target.intent).await?;
    Ok(Json(ApiData::ok(
        state.items.save_item(&grant, input, &target.intent).await?,
    )))
}

/// `DELETE /v1/items/:id` (`deleteItem`): `data` is `true`.
pub async fn delete_item(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
) -> Result<Json<ApiData<bool>>, ApiError> {
    let change = access::change_item(&state, &signer, &id).await?;
    state.items.delete_item(&change).await?;
    Ok(Json(ApiData::ok(true)))
}

/// `GET /v1/items/:id` (`getItem`).
pub async fn get_item(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
) -> Result<Json<ApiData<FullItemOut>>, ApiError> {
    let read = access::read_item(&state, &signer, &id).await?;
    Ok(Json(ApiData::ok(state.items.item(&read).await?)))
}

/// `POST /v1/items/:id/files` (`uploadItemFiles`): decided, a place taken
/// among the file uploads in flight ([`crate::State::file_uploads`], 429
/// with `Retry-After` at once) and the upload's budgets checked
/// ([`crate::limits::within_storage`]) before the body is read
/// (`items::upload`); answers `{ok: true}` without data.
pub async fn post_item_files(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ApiError> {
    use axum::extract::FromRequest;
    use axum::response::IntoResponse;
    let change = access::change_item(&state, &signer, &id).await?;
    state.items.require_documents_release().await?;
    let source = crate::limits::source(request.headers());
    let _admission = match state.file_uploads.admit(signer.as_str(), source.as_deref()) {
        Ok(admission) => admission,
        Err(busy) => return Ok(busy),
    };
    crate::limits::within_storage(
        &state,
        signer.as_str(),
        request.headers(),
        crate::UPLOAD_BODY_LIMIT as u64,
    )
    .await?;
    let (request, received) = crate::limits::counted(request);
    let multipart = match axum::extract::Multipart::from_request(request, &state).await {
        Ok(multipart) => multipart,
        Err(refused) => return Ok(refused.into_response()),
    };
    let files = upload::read(multipart).await;
    crate::limits::charge_upload(&state, source.as_deref(), &received);
    state.items.save_item_files(&change, files?).await?;
    Ok(Json(serde_json::json!({ "ok": true })).into_response())
}

/// `POST /v1/items/:id/videos` (`uploadItemFiles` with the video bound):
/// access, the item's video name, a place among the uploads in flight and
/// the upload's budgets ([`crate::limits::within_storage`]) are settled
/// before the body is read ([`video`]); a refused place answers 429 with
/// `Retry-After` at once. The store and its record run
/// in a task of their own, so a client that leaves can't cut them short.
pub async fn post_item_videos(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let change = access::change_item(&state, &signer, &id).await?;
    state.items.require_documents_release().await?;
    if !state.items.names_video(change.id()).await? {
        return Err(ApiError::bad_request("The item names no video"));
    }
    let source = crate::limits::source(request.headers());
    let admission = match state.videos.admit(signer.as_str(), source.as_deref()) {
        Ok(admission) => admission,
        Err(busy) => return Ok(busy),
    };
    crate::limits::within_storage(
        &state,
        signer.as_str(),
        request.headers(),
        video::BODY_LIMIT as u64,
    )
    .await?;
    let root = video::spool_root(&state.items.blobs);
    state.videos.sweep(&root).await;
    let (request, sent) = crate::limits::counted(request);
    let received = video::receive(&state.videos, &root, request, admission.progress()).await;
    crate::limits::charge_upload(&state, source.as_deref(), &sent);
    let received = received?;
    admission.received();
    let stored = state.clone();
    tokio::spawn(async move {
        let _admission = admission;
        stored.items.save_item_video(&change, received).await
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))??;
    Ok(Json(serde_json::json!({ "ok": true })).into_response())
}

pub async fn get_publication(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<PublicationPreparation>>, ApiError> {
    let publisher = access::publisher(&state, &signer, id).await?;
    Ok(Json(ApiData::ok(
        state.items.prepare_publication(&publisher).await?,
    )))
}

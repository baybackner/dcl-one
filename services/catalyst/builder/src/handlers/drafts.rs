use axum::Json;
use axum::extract::{Path, State};
use bytes::Bytes;
use serde_json::Value;
use uuid::Uuid;

use crate::SharedState;
use crate::access;
use crate::auth::Signed;
use crate::collections::save;
use crate::drafts::CollectionListingOut;
use crate::http::{ApiData, ApiError};
use crate::items::body::FullItemOut;
use crate::items::{save as item_save, schema, upload};
use crate::publication::PublicationPreparation;

/// Local drafts use the verified signer; upstream global lists belong to the committee.
pub async fn get_collection_drafts(
    State(state): State<SharedState>,
    Signed(signer): Signed,
) -> Result<Json<ApiData<Vec<CollectionListingOut>>>, ApiError> {
    Ok(Json(ApiData::ok(
        state
            .items
            .collection_drafts(&access::wallet(&signer))
            .await?,
    )))
}

pub async fn get_item_drafts(
    State(state): State<SharedState>,
    Signed(signer): Signed,
) -> Result<Json<ApiData<Vec<Value>>>, ApiError> {
    Ok(Json(ApiData::ok(
        state.items.item_drafts(&access::wallet(&signer)).await?,
    )))
}

/// `PUT /v1/collections/:id` (`upsertCollection`): the body's schema, the
/// ids, a standard collection's `data`, then who may save it
/// (`access::edit_collection`), then the `initialize` calldata and the
/// save's own rules (`collections::save`).
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
    let standard = save
        .urn
        .as_deref()
        .and_then(save::third_party_urn)
        .is_none();
    if standard {
        save::standard_request(&save, signer.as_str())?;
    }
    let edit = access::edit_collection(&state, &signer, uuid).await?;
    let deployment = standard
        .then(|| save::standard_deployment(&save, signer.as_str()))
        .transpose()?;
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
    let target = item_save::prepare(&state, &signer, &path, &input).await?;
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

/// `POST /v1/items/:id/files` (`uploadItemFiles`): decided before the body
/// is read (`items::upload`); answers `{ok: true}` without data.
pub async fn post_item_files(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
    multipart: axum::extract::Multipart,
) -> Result<Json<Value>, ApiError> {
    let change = access::change_item(&state, &signer, &id).await?;
    state.items.require_documents_release().await?;
    let files = upload::read(multipart).await?;
    state.items.save_item_files(&change, files).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
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

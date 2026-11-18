use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::SharedState;
use crate::auth::Signed;
use crate::drafts::{CollectionDraft, CollectionListingOut, ItemDraft};
use crate::http::{ApiData, ApiError};
use crate::items::CollectionMetaOut;
use crate::publication::PublicationPreparation;

#[derive(Deserialize)]
pub struct CollectionBody {
    collection: CollectionDraft,
}
#[derive(Deserialize)]
pub struct ItemBody {
    item: ItemDraft,
}

/// Local drafts use the verified signer; upstream global lists belong to the committee.
pub async fn get_collection_drafts(
    State(state): State<SharedState>,
    Signed(owner): Signed,
) -> Result<Json<ApiData<Vec<CollectionListingOut>>>, ApiError> {
    Ok(Json(ApiData::ok(
        state.items.collection_drafts(owner.as_str()).await?,
    )))
}

pub async fn get_item_drafts(
    State(state): State<SharedState>,
    Signed(owner): Signed,
) -> Result<Json<ApiData<Vec<Value>>>, ApiError> {
    Ok(Json(ApiData::ok(
        state.items.item_drafts(owner.as_str()).await?,
    )))
}

pub async fn put_collection(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<CollectionBody>,
) -> Result<Json<ApiData<CollectionMetaOut>>, ApiError> {
    body.collection.validate(id, owner.as_str())?;
    Ok(Json(ApiData::ok(
        state
            .items
            .save_collection_draft(owner.as_str(), &body.collection)
            .await?,
    )))
}

pub async fn put_item(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<ItemBody>,
) -> Result<Json<ApiData<Value>>, ApiError> {
    body.item.validate(id, owner.as_str())?;
    Ok(Json(ApiData::ok(
        state
            .items
            .save_item_draft(owner.as_str(), &body.item)
            .await?,
    )))
}

pub async fn delete_item(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<Value>>, ApiError> {
    state.items.delete_item_draft(owner.as_str(), id).await?;
    Ok(Json(ApiData::ok(serde_json::json!({ "id": id }))))
}

pub async fn get_item(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<Value>>, ApiError> {
    Ok(Json(ApiData::ok(
        state.items.item_draft(owner.as_str(), id).await?,
    )))
}

pub async fn post_item_files(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<ApiData<BTreeMap<String, String>>>, ApiError> {
    state.items.item_draft(owner.as_str(), id).await?;
    let mut files = Vec::new();
    let mut size = 0;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::bad_request("Invalid multipart upload"))?
    {
        let name = field
            .file_name()
            .ok_or_else(|| ApiError::bad_request("Filename is required"))?
            .to_owned();
        if !crate::drafts::valid_filename(&name) || files.len() >= 100 {
            return Err(ApiError::bad_request("Invalid filename or too many files"));
        }
        let bytes = field
            .bytes()
            .await
            .map_err(|_| ApiError::bad_request("File upload exceeds 20 MB or is incomplete"))?;
        size += bytes.len();
        if size > 20 * 1024 * 1024 {
            return Err(ApiError::bad_request("Upload exceeds 20 MB"));
        }
        files.push((name, bytes.to_vec()));
    }
    Ok(Json(ApiData::ok(
        state
            .items
            .save_item_files(owner.as_str(), id, files)
            .await?,
    )))
}

pub async fn get_publication(
    State(state): State<SharedState>,
    Signed(owner): Signed,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiData<PublicationPreparation>>, ApiError> {
    Ok(Json(ApiData::ok(
        state.items.prepare_publication(owner.as_str(), id).await?,
    )))
}

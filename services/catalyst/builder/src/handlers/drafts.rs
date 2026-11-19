use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::SharedState;
use crate::access;
use crate::auth::Signed;
use crate::collections::save;
use crate::drafts::{CollectionListingOut, ItemDraft};
use crate::http::{ApiData, ApiError};
use crate::publication::PublicationPreparation;

#[derive(Deserialize)]
pub struct ItemBody {
    item: ItemDraft,
}

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

pub async fn put_item(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<Uuid>,
    Json(body): Json<ItemBody>,
) -> Result<Json<ApiData<Value>>, ApiError> {
    body.item.validate(id)?;
    let save = access::save_item(&state, &signer, id, body.item.collection_id).await?;
    Ok(Json(ApiData::ok(
        state.items.save_item_draft(&save, &body.item).await?,
    )))
}

pub async fn delete_item(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
) -> Result<Json<ApiData<Value>>, ApiError> {
    let change = access::change_item(&state, &signer, &id, "deleted").await?;
    state.items.delete_item_draft(&change).await?;
    Ok(Json(ApiData::ok(serde_json::json!({ "id": change.id() }))))
}

pub async fn get_item(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
) -> Result<Json<ApiData<Value>>, ApiError> {
    let read = access::read_item(&state, &signer, &id).await?;
    Ok(Json(ApiData::ok(state.items.item_draft(&read).await?)))
}

pub async fn post_item_files(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<ApiData<BTreeMap<String, String>>>, ApiError> {
    let change = access::change_item(&state, &signer, &id, "inserted or updated").await?;
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
        state.items.save_item_files(&change, files).await?,
    )))
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

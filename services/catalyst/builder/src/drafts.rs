//! Collection and item drafts: the wire shapes the Creator Hub saves, their
//! validation (the owner in the body must be the signer, no draft write can
//! claim publication or approval), the owner-scoped listings and the guarded
//! upserts. A draft write is refused once its collection is publishing or
//! published (`publication_pending`, or published as [`crate::chain_state`]
//! derives it, read after the parent rows are taken `FOR UPDATE` so a
//! waiting writer re-reads the committed publication), and an item's owner
//! always equals its collection's. Uploaded files go to the
//! ONE blob store under their CIDv1 and `builder.uploaded_contents` keeps the
//! acceptance record; a draft can only name a hash this node accepted (the
//! `item_contents.hash` FK, the old "upload before saving" rule).

use std::collections::BTreeMap;

use bytes::Bytes;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::access::{self, CollectionEdit, ItemChange, ItemRead, ItemSave, OnCommittee, Wallet};
use crate::chain_state::{self, ReadView};
use crate::storage::OpenedContent;
use dcl_auth::{hash_bytes_v1, is_canonical_cid, is_eth_address};

use super::http::{ApiError, iso};
use super::items::{
    CollectionMetaOut, CollectionMetaRow, ItemsComponent, collection_meta, collection_meta_columns,
};

#[derive(Debug, sqlx::FromRow)]
struct CollectionListingRow {
    #[sqlx(flatten)]
    collection: CollectionMetaRow,
    item_count: i64,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "builder/"))]
pub struct CollectionListingOut {
    #[serde(flatten)]
    pub collection: CollectionMetaOut,
    pub item_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct CollectionDraft {
    pub id: Uuid,
    pub name: String,
    pub eth_address: Option<String>,
    pub salt: Option<String>,
    pub contract_address: Option<String>,
    pub urn: Option<String>,
    #[serde(default)]
    pub is_published: bool,
    #[serde(default)]
    pub is_approved: bool,
}

#[derive(Debug, Deserialize)]
pub struct ItemDraft {
    pub id: Uuid,
    pub name: String,
    pub collection_id: Option<Uuid>,
    pub eth_address: Option<String>,
    #[serde(default)]
    pub description: String,
    pub thumbnail: Option<String>,
    #[serde(rename = "type")]
    pub item_type: String,
    pub rarity: Option<String>,
    pub price: Option<String>,
    pub beneficiary: Option<String>,
    #[serde(default)]
    pub data: Value,
    pub metrics: Option<Value>,
    #[serde(default)]
    pub contents: BTreeMap<String, String>,
    #[serde(default)]
    pub is_published: bool,
    #[serde(default)]
    pub is_approved: bool,
}

/// A publication snapshot's row belongs to the publishing wallet.
pub(crate) fn owned_by(owner: Option<&str>, signer: &str) -> Result<(), ApiError> {
    if owner.is_some_and(|owner| !owner.eq_ignore_ascii_case(signer)) {
        return Err(ApiError::forbidden(
            "The draft owner must match the signing wallet",
        ));
    }
    Ok(())
}

fn validate_name(name: &str, max: usize) -> Result<(), ApiError> {
    if name.trim().is_empty() || name.encode_utf16().count() > max {
        return Err(ApiError::bad_request(format!(
            "Name must contain 1 to {max} characters"
        )));
    }
    Ok(())
}

impl CollectionDraft {
    pub fn validate(&self, id: Uuid) -> Result<(), ApiError> {
        if self.id != id {
            return Err(ApiError::bad_request(
                "The body and URL collection ids do not match",
            ));
        }
        validate_name(&self.name, 42)?;
        if let Some(urn) = &self.urn {
            let parts: Vec<_> = urn.split(':').collect();
            if urn.len() > 512
                || parts.len() != 6
                || parts[..4] != ["urn", "decentraland", "matic", "collections-thirdparty"]
                || parts[4..].iter().any(|part| {
                    part.is_empty() || part.chars().any(|c| c.is_whitespace() || c == '|')
                })
            {
                return Err(ApiError::bad_request("Invalid linked collection URN"));
            }
        }
        if self.is_published || self.is_approved || self.contract_address.is_some() {
            return Err(ApiError::bad_request(
                "Publication requires a verified blockchain receipt",
            ));
        }
        if self.salt.as_ref().is_some_and(|salt| salt.len() > 128) {
            return Err(ApiError::bad_request("Invalid collection salt"));
        }
        Ok(())
    }
}

impl ItemDraft {
    pub fn validate(&self, id: Uuid) -> Result<(), ApiError> {
        if self.id != id {
            return Err(ApiError::bad_request(
                "The body and URL item ids do not match",
            ));
        }
        validate_name(&self.name, 64)?;
        if !matches!(self.item_type.as_str(), "wearable" | "emote") || !self.data.is_object() {
            return Err(ApiError::bad_request("Invalid item type or data"));
        }
        if self.is_published || self.is_approved {
            return Err(ApiError::bad_request(
                "Draft writes cannot change publication or approval",
            ));
        }
        if self.metrics.as_ref().is_some_and(|metrics| {
            !metrics.is_object()
                || metrics.to_string().len() > 1024
                || metrics.as_object().is_some_and(|fields| {
                    fields
                        .values()
                        .any(|v| v.as_f64().is_none_or(|n| n < 0.0 || !n.is_finite()))
                })
        }) {
            return Err(ApiError::bad_request("Invalid item metrics"));
        }
        if self.description.len() > 4096
            || self.data.to_string().len() > 128 * 1024
            || self.contents.len() > 100
        {
            return Err(ApiError::bad_request("Item metadata is too large"));
        }
        if self.price.as_ref().is_some_and(|price| {
            price.is_empty() || price.len() > 78 || !price.bytes().all(|b| b.is_ascii_digit())
        }) {
            return Err(ApiError::bad_request(
                "Price must be an integer amount in MANA wei",
            ));
        }
        if self
            .beneficiary
            .as_ref()
            .is_some_and(|address| !is_eth_address(address))
        {
            return Err(ApiError::bad_request("Invalid beneficiary address"));
        }
        if self.rarity.as_deref().is_some_and(|rarity| {
            !matches!(
                rarity,
                "unique"
                    | "mythic"
                    | "exotic"
                    | "legendary"
                    | "epic"
                    | "rare"
                    | "uncommon"
                    | "common"
            )
        }) {
            return Err(ApiError::bad_request("Invalid rarity"));
        }
        for (file, hash) in &self.contents {
            if !valid_filename(file) || !is_canonical_cid(hash) {
                return Err(ApiError::bad_request("Invalid content filename or CID"));
            }
        }
        Ok(())
    }
}

pub fn valid_filename(file: &str) -> bool {
    !file.is_empty()
        && file.len() <= 256
        && !file.contains(['\\', '\0'])
        && file
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The item fields any signer may read, named one by one (builder-server's
/// public item); a column not named here is never answered.
const PUBLIC_ITEM: &str = "jsonb_build_object('id', i.id, 'urn', i.urn_suffix, 'name', i.name, \
    'description', i.description, 'thumbnail', i.thumbnail, 'video', i.video, 'eth_address', i.eth_address, \
    'collection_id', i.collection_id, 'blockchain_item_id', i.blockchain_item_id, 'price', i.price, \
    'beneficiary', i.beneficiary, 'rarity', i.rarity, 'type', i.type, 'data', i.data, 'metrics', i.metrics, \
    'utility', i.utility, 'mappings', i.mappings, 'is_published', i.is_published, 'is_approved', i.is_approved, \
    'in_catalyst', i.in_catalyst, 'total_supply', i.total_supply, 'content_hash', i.content_hash, \
    'catalyst_content_hash', i.catalyst_content_hash, 'contents', \
    COALESCE((SELECT jsonb_object_agg(file, hash) FROM builder.item_contents WHERE item_id = i.id), '{}'::jsonb))";
/// [`PUBLIC_ITEM`] and what only the item's owner side reads.
const FULL_ITEM: &str = "jsonb_build_object('local_content_hash', i.local_content_hash)";

fn with_timestamps((mut row, created, updated): (Value, NaiveDateTime, NaiveDateTime)) -> Value {
    row["created_at"] = iso(&created.and_utc()).into();
    row["updated_at"] = iso(&updated.and_utc()).into();
    row
}

const FK_VIOLATION: &str = "23503";

/// The bound of the committee-wide and the public listings: the per-owner
/// ones are a wallet's own handful, these span every wallet.
pub const LISTING_LIMIT: i64 = 1000;

fn collection_listing(view: &ReadView) -> String {
    format!(
        "SELECT {}, (SELECT count(*) FROM builder.items i WHERE i.collection_id=c.id) AS item_count \
         FROM {}",
        collection_meta_columns(view),
        view.collections()
    )
}

fn draft_predicate(view: &ReadView) -> String {
    format!("(NOT {} OR c.third_party_id IS NOT NULL)", view.published())
}

const ITEM_DRAFT_LISTING: &str = "SELECT jsonb_build_object('id', id, 'name', name, 'type', type, 'collection_id', collection_id) FROM builder.items";

fn item_draft_predicate(view: &ReadView) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM builder.collections c WHERE c.id=items.collection_id \
         AND (c.publication_pending OR {}))",
        view.published_subquery()
    )
}

const PARENT_NOT_PUBLISHING: &str = "NOT EXISTS (SELECT 1 FROM builder.collections c \
    WHERE c.id=items.collection_id AND c.publication_pending)";

fn listing_out(rows: &[CollectionListingRow]) -> Vec<CollectionListingOut> {
    rows.iter()
        .map(|row| CollectionListingOut {
            collection: CollectionMetaOut::from(&row.collection),
            item_count: row.item_count,
        })
        .collect()
}

impl ItemsComponent {
    pub async fn save_collection_draft(
        &self,
        edit: &CollectionEdit,
        draft: &CollectionDraft,
    ) -> Result<CollectionMetaOut, ApiError> {
        draft.validate(edit.id())?;
        let signer = edit.signer();
        let mut tx = self.pool.begin().await?;
        let view = chain_state::for_decision(&mut tx).await?;
        let stored: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT third_party_id, urn_suffix FROM builder.collections WHERE id=$1 FOR UPDATE",
        )
        .bind(draft.id)
        .fetch_optional(&mut *tx)
        .await?;
        access::confirm_collection_edit(&mut tx, &view, edit).await?;
        let provider = draft
            .urn
            .as_deref()
            .and_then(|urn| urn.rsplit_once(':').map(|(provider, _)| provider));
        if let Some((stored, urn)) = stored {
            kind_kept(draft, signer, stored.as_deref(), provider, urn.as_deref())?;
        }
        let changed = sqlx::query(
            "INSERT INTO builder.collections AS c (id, name, eth_address, salt, urn_suffix, third_party_id) VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, salt = EXCLUDED.salt, updated_at = timezone('UTC', now()) \
             WHERE c.eth_address = $3 AND NOT c.publication_pending \
             AND c.urn_suffix IS NOT DISTINCT FROM $5 AND c.third_party_id IS NOT DISTINCT FROM $6 \
             RETURNING id",
        ).bind(draft.id).bind(draft.name.trim()).bind(signer).bind(&draft.salt)
            .bind(&draft.urn).bind(provider)
            .fetch_optional(&mut *tx).await?;
        if changed.is_none() {
            access::confirm_collection_edit(&mut tx, &view, edit).await?;
            return Err(ApiError::internal(format!(
                "collection {} changed under its row lock",
                draft.id
            )));
        }
        let row = collection_meta(&mut tx, &view, edit)
            .await?
            .ok_or_else(|| ApiError::not_found("Collection not found"))?;
        tx.commit().await?;
        Ok(CollectionMetaOut::from(&row))
    }

    /// "My collections": the owner's unpublished collections and published
    /// linked ones (`collections_owner_idx`).
    pub async fn collection_drafts(
        &self,
        wallet: &Wallet,
    ) -> Result<Vec<CollectionListingOut>, ApiError> {
        let mut conn = self.pool.acquire().await?;
        let view = chain_state::for_read(&mut conn).await?;
        let rows = sqlx::query_as::<_, CollectionListingRow>(sqlx::AssertSqlSafe(format!(
            "{} WHERE c.eth_address = $1 AND {} ORDER BY c.updated_at DESC",
            collection_listing(&view),
            draft_predicate(&view)
        )))
        .bind(wallet.address())
        .fetch_all(&mut *conn)
        .await?;
        Ok(listing_out(&rows))
    }

    /// Every wallet's drafts, newest first: the committee's `GET
    /// /v1/collections` (builder-server's `getCollections` without the
    /// published search), at most [`LISTING_LIMIT`] rows.
    pub async fn collection_drafts_of_everyone(
        &self,
        _committee: &OnCommittee,
    ) -> Result<Vec<CollectionListingOut>, ApiError> {
        let mut conn = self.pool.acquire().await?;
        let view = chain_state::for_read(&mut conn).await?;
        let rows = sqlx::query_as::<_, CollectionListingRow>(sqlx::AssertSqlSafe(format!(
            "{} WHERE {} ORDER BY c.updated_at DESC LIMIT $1",
            collection_listing(&view),
            draft_predicate(&view)
        )))
        .bind(LISTING_LIMIT)
        .fetch_all(&mut *conn)
        .await?;
        Ok(listing_out(&rows))
    }

    /// The published collections whose name contains `q` (case-insensitive,
    /// the pattern characters of `q` literal), newest first: the public
    /// search `GET /v1/collections?q=...&is_published=true`, at most
    /// [`LISTING_LIMIT`] rows.
    pub async fn published_collections_named(
        &self,
        q: &str,
    ) -> Result<Vec<CollectionListingOut>, ApiError> {
        let pattern = q
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let mut conn = self.pool.acquire().await?;
        let view = chain_state::for_read(&mut conn).await?;
        let rows = sqlx::query_as::<_, CollectionListingRow>(sqlx::AssertSqlSafe(format!(
            "{} WHERE {} AND c.name ILIKE '%' || $1 || '%' ESCAPE '\\' \
             ORDER BY c.updated_at DESC LIMIT $2",
            collection_listing(&view),
            view.published()
        )))
        .bind(pattern)
        .bind(LISTING_LIMIT)
        .fetch_all(&mut *conn)
        .await?;
        Ok(listing_out(&rows))
    }

    /// "My item drafts" (`items_owner_updated_idx`).
    pub async fn item_drafts(&self, wallet: &Wallet) -> Result<Vec<Value>, ApiError> {
        let mut conn = self.pool.acquire().await?;
        let view = chain_state::for_read(&mut conn).await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "{ITEM_DRAFT_LISTING} WHERE eth_address = $1 AND NOT is_published AND {} ORDER BY updated_at DESC",
            item_draft_predicate(&view)
        )))
        .bind(wallet.address())
        .fetch_all(&mut *conn)
        .await?)
    }

    /// Every wallet's item drafts, newest first: the committee's `GET
    /// /v1/items` (builder-server's `getItems`), at most [`LISTING_LIMIT`].
    pub async fn item_drafts_of_everyone(
        &self,
        _committee: &OnCommittee,
    ) -> Result<Vec<Value>, ApiError> {
        let mut conn = self.pool.acquire().await?;
        let view = chain_state::for_read(&mut conn).await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "{ITEM_DRAFT_LISTING} WHERE NOT is_published AND {} ORDER BY updated_at DESC LIMIT $1",
            item_draft_predicate(&view)
        )))
        .bind(LISTING_LIMIT)
        .fetch_all(&mut *conn)
        .await?)
    }

    pub async fn item_draft(&self, read: &ItemRead) -> Result<Value, ApiError> {
        item_by_id(&self.pool, read.id(), read.full()).await
    }

    /// One unpublished item by primary key. Use publication's parent-then-item
    /// lock order; cascading content links go away, shared uploaded bytes remain.
    pub async fn delete_item_draft(&self, change: &ItemChange) -> Result<(), ApiError> {
        let id = change.id();
        let mut tx = self.pool.begin().await?;
        let view = chain_state::for_decision(&mut tx).await?;
        lock_item(&mut tx, id, None).await?;
        access::confirm_item_change(&mut tx, &view, change).await?;
        let deleted = sqlx::query("DELETE FROM builder.items WHERE id=$1 AND NOT is_published")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() == 0 {
            return Err(ApiError::not_found("Item not found"));
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn save_item_draft(
        &self,
        save: &ItemSave,
        draft: &ItemDraft,
    ) -> Result<Value, ApiError> {
        draft.validate(save.id())?;
        if draft.collection_id != save.collection() {
            return Err(ApiError::bad_request(
                "The item's collection is not the one its save was decided for",
            ));
        }
        let signer = save.signer();
        let mut tx = self.pool.begin().await?;
        let view = chain_state::for_decision(&mut tx).await?;
        lock_item(&mut tx, draft.id, draft.collection_id).await?;
        access::confirm_item_save(&mut tx, &view, save).await?;
        let changed = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO builder.items (id, name, eth_address, collection_id, description, thumbnail, type, rarity, price, beneficiary, data, metrics) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) \
             ON CONFLICT (id) DO UPDATE SET name=EXCLUDED.name, collection_id=EXCLUDED.collection_id, \
             description=EXCLUDED.description, thumbnail=EXCLUDED.thumbnail, type=EXCLUDED.type, \
             rarity=EXCLUDED.rarity, price=EXCLUDED.price, beneficiary=EXCLUDED.beneficiary, data=EXCLUDED.data, metrics=COALESCE(EXCLUDED.metrics,items.metrics), updated_at=timezone('UTC', now()) \
             WHERE items.eth_address=$3 AND NOT items.is_published AND {PARENT_NOT_PUBLISHING} RETURNING id"
        ))).bind(draft.id).bind(draft.name.trim()).bind(signer).bind(draft.collection_id)
            .bind(&draft.description).bind(&draft.thumbnail).bind(&draft.item_type).bind(&draft.rarity)
            .bind(&draft.price).bind(&draft.beneficiary).bind(&draft.data).bind(&draft.metrics).fetch_optional(&mut *tx).await?;
        if changed.is_none() {
            return Err(ApiError::unauthorized(
                "The user is unauthorized to upsert the collection.",
            )
            .with_data(serde_json::json!({ "id": draft.id, "eth_address": signer })));
        }
        sqlx::query("DELETE FROM builder.item_contents WHERE item_id = $1")
            .bind(draft.id)
            .execute(&mut *tx)
            .await?;
        if !draft.contents.is_empty() {
            let files: Vec<&str> = draft.contents.keys().map(String::as_str).collect();
            let hashes: Vec<&str> = draft.contents.values().map(String::as_str).collect();
            let inserted = sqlx::query(
                "INSERT INTO builder.item_contents (item_id, file, hash) \
                 SELECT $1, f, h FROM unnest($2::text[], $3::text[]) AS t(f, h)",
            )
            .bind(draft.id)
            .bind(&files)
            .bind(&hashes)
            .execute(&mut *tx)
            .await;
            match inserted {
                Ok(_) => {}
                Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some(FK_VIOLATION) => {
                    return Err(ApiError::bad_request(
                        "Upload item content before saving its metadata",
                    ));
                }
                Err(e) => return Err(e.into()),
            }
        }
        tx.commit().await?;
        item_by_id(&self.pool, draft.id, true).await
    }
}

impl ItemsComponent {
    /// The multipart upload: 1..100 files, 20 MB together. Each file's bytes
    /// are stored under their CIDv1 in the ONE blob store before the row
    /// that records the acceptance (the store is immutable and idempotent, so
    /// a rolled-back transaction leaves nothing wrong behind).
    pub async fn save_item_files(
        &self,
        change: &ItemChange,
        files: Vec<(String, Vec<u8>)>,
    ) -> Result<BTreeMap<String, String>, ApiError> {
        let (id, signer) = (change.id(), change.signer());
        if files.is_empty()
            || files.len() > 100
            || files.iter().map(|(_, bytes)| bytes.len()).sum::<usize>() > 20 * 1024 * 1024
        {
            return Err(ApiError::bad_request(
                "Upload 1 to 100 files totaling at most 20 MB",
            ));
        }
        let mut tx = self.pool.begin().await?;
        let view = chain_state::for_decision(&mut tx).await?;
        lock_item(&mut tx, id, None).await?;
        access::confirm_item_change(&mut tx, &view, change).await?;
        let mut contents = BTreeMap::new();
        for (file, bytes) in files {
            if !valid_filename(&file) {
                return Err(ApiError::bad_request("Invalid filename"));
            }
            let hash = hash_bytes_v1(&bytes);
            let size = bytes.len() as i64;
            if !self
                .blobs
                .exist(&hash)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?
            {
                self.blobs
                    .store(&hash, Bytes::from(bytes))
                    .await
                    .map_err(|e| ApiError::internal(format!("storing {hash}: {e}")))?;
            }
            sqlx::query("INSERT INTO builder.uploaded_contents (hash, size, uploaded_by) VALUES ($1,$2,$3) ON CONFLICT (hash) DO NOTHING")
                .bind(&hash).bind(size).bind(signer).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO builder.item_contents (item_id,file,hash) VALUES ($1,$2,$3) ON CONFLICT (item_id,file) DO UPDATE SET hash=EXCLUDED.hash")
                .bind(id).bind(&file).bind(&hash).execute(&mut *tx).await?;
            contents.insert(file, hash);
        }
        sqlx::query("UPDATE builder.items SET updated_at=timezone('UTC', now()) WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(contents)
    }

    pub async fn uploaded_content_exists(&self, hash: &str) -> Result<bool, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM builder.uploaded_contents WHERE hash=$1)",
        )
        .bind(hash)
        .fetch_one(&self.pool)
        .await?)
    }

    /// The accepted upload's bytes, whole: for tests and small files; the
    /// serve route streams through [`Self::open_uploaded_content`].
    pub async fn uploaded_content(&self, hash: &str) -> Result<Option<Bytes>, ApiError> {
        if !self.uploaded_content_exists(hash).await? {
            return Ok(None);
        }
        self.blobs
            .retrieve(hash)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))
    }

    /// `None` unless `builder.uploaded_contents` names the hash (a blob the
    /// store holds for content or worlds is not a builder upload).
    pub async fn open_uploaded_content(
        &self,
        hash: &str,
    ) -> Result<Option<OpenedContent>, ApiError> {
        if !self.uploaded_content_exists(hash).await? {
            return Ok(None);
        }
        self.blobs
            .open(hash)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))
    }
}

async fn item_by_id<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: Uuid,
    full: bool,
) -> Result<Value, ApiError> {
    let fields = if full {
        format!("{PUBLIC_ITEM} || {FULL_ITEM}")
    } else {
        PUBLIC_ITEM.to_string()
    };
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {fields}, i.created_at, i.updated_at FROM builder.items i WHERE i.id = $1"
    )))
    .bind(id)
    .fetch_optional(executor)
    .await?
    .map(with_timestamps)
    .ok_or_else(|| ApiError::not_found("Item not found"))
}

/// The item's collections (the one it is in and the one it moves to), then
/// the item, `FOR UPDATE`: publication's parent-then-item order. 409 when
/// another request moved the item in between.
async fn lock_item(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    next_collection: Option<Uuid>,
) -> Result<(), ApiError> {
    let previous: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT collection_id FROM builder.items WHERE id=$1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    let previous_collection = previous.flatten();
    let mut parents: Vec<_> = previous_collection
        .into_iter()
        .chain(next_collection)
        .collect();
    parents.sort();
    parents.dedup();
    for parent in parents {
        sqlx::query("SELECT id FROM builder.collections WHERE id=$1 FOR UPDATE")
            .bind(parent)
            .fetch_optional(&mut **tx)
            .await?;
    }
    let current: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT collection_id FROM builder.items WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    if current.is_some_and(|parent| parent != previous_collection) {
        return Err(ApiError::http(
            409,
            "Item moved to another collection. Reload it before saving.",
        ));
    }
    Ok(())
}

/// A save keeps the collection's kind and provider: builder-server's 409s,
/// and its 401 for a third-party collection saved without its URN.
fn kind_kept(
    draft: &CollectionDraft,
    signer: &str,
    stored: Option<&str>,
    provider: Option<&str>,
    urn: Option<&str>,
) -> Result<(), ApiError> {
    let id = draft.id;
    match (stored, provider) {
        (None, Some(_)) => Err(ApiError::conflict(
            "The collection can't be converted into a third party collection.",
        )
        .with_data(serde_json::json!({ "id": id }))),
        (Some(_), None) => Err(ApiError::unauthorized("Unauthorized to upsert collection")
            .with_data(serde_json::json!({ "id": id, "eth_address": signer }))),
        (Some(stored), Some(provider)) if stored != provider => Err(ApiError::conflict(
            "The third party id cannot be changed",
        )
        .with_data(
            serde_json::json!({ "id": id, "eth_address": signer, "third_party_id": provider }),
        )),
        (Some(_), Some(_)) if urn != draft.urn.as_deref() => {
            Err(ApiError::conflict("The collection's URN can't be changed.")
                .with_data(serde_json::json!({ "id": id })))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_draft_write_cannot_claim_publication_and_its_owner_field_is_ignored() {
        let id = Uuid::nil();
        let mut draft: CollectionDraft =
            serde_json::from_value(json!({"id": id, "name": "My collection"})).unwrap();
        assert!(draft.validate(id).is_ok());
        draft.eth_address = Some("0xother".into());
        assert!(draft.validate(id).is_ok());
        draft.is_published = true;
        assert!(draft.validate(id).is_err());
    }

    #[test]
    fn content_paths_allow_subdirectories_without_escape() {
        assert!(valid_filename("textures/skin.png"));
        for bad in [
            "",
            "../skin.png",
            "/skin.png",
            "a/../b",
            "a\\b",
            "a//b",
            "a\0b",
        ] {
            assert!(!valid_filename(bad), "{bad}");
        }
    }
}

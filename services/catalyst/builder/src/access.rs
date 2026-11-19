//! Who may do what to a builder collection or item: the one place that
//! decides, deny by default. A decision reads the verified signer, the
//! resource ids the request acts on, the builder's rows, the chain copy
//! through a [`DecisionView`] and the current committee
//! ([`crate::committee`]). It grants as soon as one role answers yes; it
//! refuses with builder-server's refusal only when every source it needed
//! answered no; when a source it needed can't tell, it answers 503 and grants
//! nothing.
//!
//! The roles, as builder-server defines them:
//!
//! - **Owner** of a standard collection: the chain copy's `creator` once the
//!   copy holds the collection, the stored `eth_address` before. A
//!   third-party collection has no owner.
//! - **On-chain manager**: a `managers` entry of the copy's row, so only of
//!   a collection on chain.
//! - **Committee member**: [`crate::committee`].
//! - **Third-party manager** of a collection's provider: until the node reads
//!   the third-party registry, the one wallet stored as the third-party
//!   collection's `eth_address` (the wallet that created it).
//! - **Item owner**: the item row's `eth_address`, which decides for an item
//!   with no collection and for item reads.
//!
//! A grant is a value only this module builds; the store functions that read
//! or change a collection or item take it. A write's value is confirmed again
//! inside the write's transaction, after its rows are locked
//! ([`confirm_collection_edit`], [`confirm_item_save`], [`confirm_item_change`]).
//!
//! ```no_run
//! async fn save(
//!     state: &catalyst_builder::SharedState,
//!     signer: &catalyst_shared::signed_fetch::Signer,
//!     save: &catalyst_builder::collections::save::CollectionSave,
//!     id: uuid::Uuid,
//! ) {
//!     let edit = catalyst_builder::access::edit_collection(state, signer, id).await.unwrap();
//!     let _ = state.items.save_collection(&edit, save, None).await;
//! }
//! ```
//!
//! A grant can't be built outside this module:
//!
//! ```compile_fail,E0451
//! let forged = catalyst_builder::access::CollectionEdit {
//!     id: uuid::Uuid::nil(),
//!     signer: String::new(),
//! };
//! ```
//!
//! and a store call can't skip the decision:
//!
//! ```compile_fail,E0308
//! async fn skip(
//!     state: &catalyst_builder::SharedState,
//!     save: &catalyst_builder::collections::save::CollectionSave,
//! ) {
//!     let _ = state.items.save_collection("0xabc", save, None).await;
//! }
//! ```

use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::SharedState;
use crate::chain_state::{self, DecisionView};
use crate::http::{ApiError, Subject};
use crate::shared::signed_fetch::Signer;

/// A signer's own wallet: what the per-wallet listings filter by.
#[derive(Debug)]
pub struct Wallet {
    address: String,
}

/// The signer is a current committee member.
#[derive(Debug)]
pub struct OnCommittee {
    member: String,
}

/// May read the collection; `full` also reads its unpublished items.
#[derive(Debug)]
pub struct CollectionRead {
    id: Uuid,
    full: bool,
}

/// May create or save the collection.
#[derive(Debug)]
pub struct CollectionEdit {
    id: Uuid,
    signer: String,
}

/// May read the item; `full` also reads what only its owner's side sees.
#[derive(Debug)]
pub struct ItemRead {
    id: Uuid,
    full: bool,
}

/// May save the item into `collection` (none: no collection).
#[derive(Debug)]
pub struct ItemSave {
    id: Uuid,
    signer: String,
    collection: Option<Uuid>,
}

/// May delete the item or upload its files.
#[derive(Debug)]
pub struct ItemChange {
    id: Uuid,
    signer: String,
    action: &'static str,
}

/// May run the node's publication of the collection.
#[derive(Debug)]
pub struct Publisher {
    id: Uuid,
    signer: String,
}

impl Wallet {
    pub fn address(&self) -> &str {
        &self.address
    }
}

impl OnCommittee {
    pub fn member(&self) -> &str {
        &self.member
    }
}

impl CollectionRead {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn full(&self) -> bool {
        self.full
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::CollectionRead {}
    impl Sealed for super::CollectionEdit {}
}

/// A grant over one collection, for the reads that answer its row; only
/// this module's grants implement it.
pub(crate) trait CollectionGrant: sealed::Sealed {
    fn collection(&self) -> Uuid;
}

impl CollectionGrant for CollectionRead {
    fn collection(&self) -> Uuid {
        self.id
    }
}

impl CollectionGrant for CollectionEdit {
    fn collection(&self) -> Uuid {
        self.id
    }
}

impl CollectionEdit {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn signer(&self) -> &str {
        &self.signer
    }
}

impl ItemRead {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn full(&self) -> bool {
        self.full
    }
}

impl ItemSave {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn signer(&self) -> &str {
        &self.signer
    }
    pub fn collection(&self) -> Option<Uuid> {
        self.collection
    }
}

impl ItemChange {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn signer(&self) -> &str {
        &self.signer
    }
}

impl Publisher {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn signer(&self) -> &str {
        &self.signer
    }
}

const COLLECTIONS: &str = "collections";
const ITEMS: &str = "items";
const UPSERT: &str = "inserted or updated";

/// `withModelExists`' 400 for an id that is not a UUID.
fn invalid_uuid(raw: &str) -> ApiError {
    ApiError::bad_request(format!("Invalid uuid {raw}")).with_data(json!({ "id": raw }))
}

/// `withModelExists`' 404.
fn not_found(id: Uuid, table: &str) -> ApiError {
    ApiError::not_found(format!("Couldn't find \"{id}\" on {table}"))
        .with_data(json!({ "id": id, "tableName": table }))
}

/// `withModelAuthorization`'s 401.
fn unauthorized_user(signer: &str, table: &str, id: Uuid) -> ApiError {
    ApiError::unauthorized(format!("Unauthorized user {signer} for {table} {id}"))
        .with_data(json!({ "ethAddress": signer, "tableName": table }))
}

fn parse(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| invalid_uuid(raw))
}

struct Collection {
    stored_owner: String,
    third_party: bool,
    pending: bool,
    published: bool,
    creator: Option<String>,
    managers: Vec<String>,
    minters: Vec<String>,
}

impl Collection {
    fn owner(&self, signer: &str) -> bool {
        !self.third_party && self.creator.as_deref().unwrap_or(&self.stored_owner) == signer
    }

    fn manager(&self, signer: &str) -> bool {
        self.managers.iter().any(|manager| manager == signer)
    }

    fn provider_manager(&self, signer: &str) -> bool {
        self.third_party && self.stored_owner == signer
    }

    fn role(&self, signer: &str) -> bool {
        self.owner(signer) || self.manager(signer) || self.provider_manager(signer)
    }

    fn published_item(&self, id: Uuid, action: &str) -> ApiError {
        if self.third_party {
            ApiError::conflict(format!(
                "The third party item is already published. It can't be {action}."
            ))
            .with_data(json!({ "id": id }))
        } else {
            ApiError::published(Subject::Item, &id.to_string(), action)
        }
    }
}

async fn collection(
    conn: &mut PgConnection,
    view: &DecisionView,
    id: Uuid,
) -> Result<Option<Collection>, ApiError> {
    let row: Option<(String, bool, Option<String>, bool)> = sqlx::query_as(
        "SELECT eth_address, third_party_id IS NOT NULL, contract_address, publication_pending \
         FROM builder.collections WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((stored_owner, third_party, contract, pending)) = row else {
        return Ok(None);
    };
    let published = chain_state::published(conn, view, id).await?;
    let facts = match contract {
        Some(contract) if published && !third_party => {
            chain_state::collection(conn, view, &contract).await?
        }
        _ => None,
    };
    Ok(Some(Collection {
        stored_owner,
        third_party,
        pending,
        published,
        creator: facts.as_ref().map(|f| f.creator.clone()),
        minters: facts
            .as_ref()
            .map(|f| f.minters.clone())
            .unwrap_or_default(),
        managers: facts.map(|f| f.managers).unwrap_or_default(),
    }))
}

struct Item {
    owner: String,
    collection: Option<Uuid>,
    minted: bool,
    published: bool,
}

async fn item(conn: &mut PgConnection, id: Uuid) -> Result<Option<Item>, ApiError> {
    let row: Option<(String, Option<Uuid>, bool, bool)> = sqlx::query_as(
        "SELECT eth_address, collection_id, blockchain_item_id IS NOT NULL, is_published \
         FROM builder.items WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(owner, collection, minted, published)| Item {
        owner,
        collection,
        minted,
        published,
    }))
}

async fn parent(
    conn: &mut PgConnection,
    view: &DecisionView,
    id: Uuid,
) -> Result<Collection, ApiError> {
    collection(conn, view, id)
        .await?
        .ok_or_else(|| ApiError::internal(format!("item of a missing collection {id}")))
}

async fn decision(
    state: &SharedState,
) -> Result<(sqlx::pool::PoolConnection<sqlx::Postgres>, DecisionView), ApiError> {
    let mut conn = state.items.pool().acquire().await?;
    let view = chain_state::for_decision(&mut conn).await?;
    Ok((conn, view))
}

/// Whether a good committee read at hand shows `signer` on it: for a view
/// only the committee widens, never waiting on the source.
fn known_committee(state: &SharedState, signer: &str) -> bool {
    state.committee.known_member(state.market.as_ref(), signer)
}

async fn on_committee(state: &SharedState, signer: &str) -> Result<bool, ApiError> {
    state
        .committee
        .is_member(state.market.as_ref(), signer)
        .await
}

/// The signer's own wallet.
pub fn wallet(signer: &Signer) -> Wallet {
    Wallet {
        address: signer.as_str().to_string(),
    }
}

/// `/v1/{address}/collections` and `/items`: the address must be the
/// signer's; anyone else is refused with the requested address.
pub fn own_address(signer: &Signer, address: &str) -> Result<Wallet, ApiError> {
    let address = address.to_ascii_lowercase();
    if address != signer.as_str() {
        return Err(
            ApiError::unauthorized("Unauthorized").with_data(json!({ "eth_address": address }))
        );
    }
    Ok(wallet(signer))
}

/// The signer must be a current committee member.
pub async fn committee(state: &SharedState, signer: &Signer) -> Result<OnCommittee, ApiError> {
    if !on_committee(state, signer.as_str()).await? {
        return Err(ApiError::unauthorized("Unauthorized")
            .with_data(json!({ "eth_address": signer.as_str() })));
    }
    Ok(OnCommittee {
        member: signer.as_str().to_string(),
    })
}

/// While the copy has a complete pass but a stale one: whether `id` is the
/// signer's own draft, which the copy can't change: its stored owner is the
/// signer and it is third-party, or standard and its contract isn't on the
/// copy. The owner's own listing answers the same rows from the same pass.
async fn own_draft_while_stale(
    conn: &mut PgConnection,
    signer: &Signer,
    id: Uuid,
) -> Result<bool, ApiError> {
    let view = chain_state::for_read(conn).await?;
    let row: Option<(String, bool, Option<String>)> = sqlx::query_as(
        "SELECT eth_address, third_party_id IS NOT NULL, contract_address FROM builder.collections WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((owner, third_party, contract)) = row else {
        return Ok(false);
    };
    if owner != signer.as_str() {
        return Ok(false);
    }
    Ok(match contract {
        Some(contract) if !third_party => !chain_state::holds(conn, &view, &contract).await?,
        _ => true,
    })
}

/// The signer is on the committee by the read at hand, without waiting on
/// the committee source: for a view only the committee widens.
pub fn known_committee_member(state: &SharedState, signer: &Signer) -> Option<OnCommittee> {
    known_committee(state, signer.as_str()).then(|| OnCommittee {
        member: signer.as_str().to_string(),
    })
}

/// `GET /v1/collections/:id`: any signer reads a published collection's
/// public detail, and its minters and the committee (from the read at hand,
/// without waiting) its full body; the owner, an on-chain manager, the
/// provider's manager and the committee read any collection in full.
pub async fn read_collection(
    state: &SharedState,
    signer: &Signer,
    raw: &str,
) -> Result<CollectionRead, ApiError> {
    let id = parse(raw)?;
    let mut conn = state.items.pool().acquire().await?;
    let view = match chain_state::for_decision(&mut conn).await {
        Ok(view) => view,
        Err(stale) => {
            return match own_draft_while_stale(&mut conn, signer, id).await? {
                true => Ok(CollectionRead { id, full: true }),
                false => Err(stale),
            };
        }
    };
    let c = collection(&mut conn, &view, id)
        .await?
        .ok_or_else(|| not_found(id, COLLECTIONS))?;
    drop(conn);
    let s = signer.as_str();
    if c.role(s) {
        return Ok(CollectionRead { id, full: true });
    }
    if c.published {
        let full = c.minters.iter().any(|minter| minter == s) || known_committee(state, s);
        return Ok(CollectionRead { id, full });
    }
    if on_committee(state, s).await? {
        return Ok(CollectionRead { id, full: true });
    }
    Err(ApiError::unauthorized("Unauthorized").with_data(json!({ "id": raw, "eth_address": s })))
}

/// `GET /v1/collections/:idOrContractAddress/items`: any signer lists a
/// published collection's published items; a role holder and the committee
/// list every item. For a published collection the committee only widens the
/// view, from the read at hand ([`crate::committee::Committee::known_member`]):
/// the answer never waits on the committee source, and a member gets what any
/// signer gets while no good read is at hand.
pub async fn collection_items(
    state: &SharedState,
    signer: &Signer,
    raw: &str,
) -> Result<CollectionRead, ApiError> {
    let (mut conn, view) = decision(state).await?;
    let id = match Uuid::parse_str(raw) {
        Ok(id) => id,
        Err(_) if dcl_auth::is_eth_address(raw) => {
            let contract = raw.to_ascii_lowercase();
            sqlx::query_scalar("SELECT id FROM builder.collections WHERE contract_address = $1")
                .bind(&contract)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or_else(|| {
                    ApiError::not_found(format!(
                        "Collection not found for the provided contract address: {contract}"
                    ))
                    .with_data(json!({ "contractAddress": contract }))
                })?
        }
        Err(_) => {
            return Err(ApiError::bad_request(format!(
                "Invalid collection ID or contract address format: {raw}"
            ))
            .with_data(json!({ "idOrContractAddress": raw })));
        }
    };
    let c = collection(&mut conn, &view, id)
        .await?
        .ok_or_else(|| not_found(id, COLLECTIONS))?;
    drop(conn);
    let s = signer.as_str();
    if c.role(s) {
        return Ok(CollectionRead { id, full: true });
    }
    if c.published {
        let full = known_committee(state, s);
        return Ok(CollectionRead { id, full });
    }
    if on_committee(state, s).await? {
        return Ok(CollectionRead { id, full: true });
    }
    Err(ApiError::unauthorized("Unauthorized").with_data(json!({ "eth_address": s })))
}

/// `GET /v1/items/:id`: any signer reads a minted item of a published
/// standard collection; the item's owner, the provider's manager and the
/// committee read any. `full` is false for a caller who reads only what any
/// signer may; for a minted item the committee only widens the view, without
/// waiting, as in [`collection_items`].
pub async fn read_item(
    state: &SharedState,
    signer: &Signer,
    raw: &str,
) -> Result<ItemRead, ApiError> {
    let id = parse(raw)?;
    let (mut conn, view) = decision(state).await?;
    let i = item(&mut conn, id)
        .await?
        .ok_or_else(|| not_found(id, ITEMS))?;
    let parent = match i.collection {
        Some(parent_id) => Some(parent(&mut conn, &view, parent_id).await?),
        None => None,
    };
    drop(conn);
    let s = signer.as_str();
    if i.owner == s || parent.as_ref().is_some_and(|c| c.provider_manager(s)) {
        return Ok(ItemRead { id, full: true });
    }
    if let Some(c) = parent.filter(|c| !c.third_party && c.published && i.minted) {
        let full = c.owner(s) || c.manager(s) || known_committee(state, s);
        return Ok(ItemRead { id, full });
    }
    if on_committee(state, s).await? {
        return Ok(ItemRead { id, full: true });
    }
    Err(ApiError::unauthorized("Unauthorized").with_data(json!({ "id": raw, "eth_address": s })))
}

/// `PUT /v1/collections/:id`: anyone creates a collection under a new id;
/// only the owner, or the provider's manager, saves an existing one.
pub async fn edit_collection(
    state: &SharedState,
    signer: &Signer,
    id: Uuid,
) -> Result<CollectionEdit, ApiError> {
    let edit = CollectionEdit {
        id,
        signer: signer.as_str().to_string(),
    };
    let (mut conn, view) = decision(state).await?;
    confirm_collection_edit(&mut conn, &view, &edit).await?;
    Ok(edit)
}

/// The state of a collection a save was confirmed for: whether it is
/// published and whether the node's own publication of it is in progress.
#[derive(Debug, Default)]
pub struct EditState {
    pub published: bool,
    pub pending: bool,
}

/// [`edit_collection`]'s decision again, inside the write's transaction,
/// with the state the save's own rules read.
pub(crate) async fn confirm_collection_edit(
    conn: &mut PgConnection,
    view: &DecisionView,
    edit: &CollectionEdit,
) -> Result<EditState, ApiError> {
    let Some(c) = collection(conn, view, edit.id).await? else {
        return Ok(EditState::default());
    };
    let s = edit.signer.as_str();
    if !c.owner(s) && !c.provider_manager(s) {
        return Err(ApiError::unauthorized("Unauthorized to upsert collection")
            .with_data(json!({ "id": edit.id.to_string(), "eth_address": s })));
    }
    Ok(EditState {
        published: c.published,
        pending: c.pending,
    })
}

/// `PUT /v1/items/:id`: an item with no collection is saved by its owner
/// (or anyone, when new); an item in a collection, or moved into one, by
/// that collection's owner, its on-chain manager or its provider's manager,
/// and not once the collection is published or while it is being published.
/// An existing item never moves into, out of or between third-party
/// collections. `collection` is the collection the request moves the item
/// into.
pub async fn save_item(
    state: &SharedState,
    signer: &Signer,
    id: Uuid,
    collection: Option<Uuid>,
) -> Result<ItemSave, ApiError> {
    let save = ItemSave {
        id,
        signer: signer.as_str().to_string(),
        collection,
    };
    let (mut conn, view) = decision(state).await?;
    confirm_item_save(&mut conn, &view, &save).await?;
    Ok(save)
}

/// [`save_item`]'s decision again, inside the write's transaction.
pub(crate) async fn confirm_item_save(
    conn: &mut PgConnection,
    view: &DecisionView,
    save: &ItemSave,
) -> Result<(), ApiError> {
    let (id, s) = (save.id, save.signer.as_str());
    let stored = item(conn, id).await?;
    let current = stored.as_ref().and_then(|i| i.collection);
    let mut parents = Vec::new();
    for parent_id in current.into_iter().chain(save.collection) {
        if parents.iter().any(|(seen, _)| *seen == parent_id) {
            continue;
        }
        let c = collection(conn, view, parent_id).await?.ok_or_else(|| {
            ApiError::not_found("The collection doesn't exist.")
                .with_data(json!({ "collectionId": parent_id }))
        })?;
        parents.push((parent_id, c));
    }
    let moves = stored.is_some() && current != save.collection;
    if moves && parents.iter().any(|(_, c)| c.third_party) {
        return Err(
            ApiError::unauthorized("Item can't change between collections.")
                .with_data(json!({ "id": id })),
        );
    }
    if let Some(i) = &stored
        && i.collection.is_none()
        && i.owner != s
    {
        return Err(
            ApiError::unauthorized("The user is unauthorized to upsert the collection.")
                .with_data(json!({ "id": id, "eth_address": s })),
        );
    }
    for (parent_id, c) in &parents {
        if c.third_party && !c.provider_manager(s) {
            return Err(ApiError::unauthorized(
                "The user is unauthorized to upsert the collection.",
            )
            .with_data(json!({ "id": id, "eth_address": s })));
        }
        if !c.third_party && !c.owner(s) && !(c.published && c.manager(s)) {
            return Err(ApiError::unauthorized(
                "You're not authorized to to change this collection.",
            )
            .with_data(json!({ "id": id, "eth_address": s, "collection_id": parent_id })));
        }
    }
    for (_, c) in &parents {
        if c.published || stored.as_ref().is_some_and(|i| i.published) {
            return Err(c.published_item(id, UPSERT));
        }
    }
    if parents.iter().any(|(_, c)| c.pending) {
        return Err(ApiError::locked(Subject::Item, &id.to_string(), UPSERT));
    }
    Ok(())
}

/// `DELETE /v1/items/:id` (`deleted`) and `POST /v1/items/:id/files`
/// (`inserted or updated`): the owner of an item with no collection; for an
/// item in a collection, that collection's owner, on-chain manager or
/// provider's manager, and not once the collection is published or while it
/// is being published.
pub async fn change_item(
    state: &SharedState,
    signer: &Signer,
    raw: &str,
    action: &'static str,
) -> Result<ItemChange, ApiError> {
    let id = parse(raw)?;
    let change = ItemChange {
        id,
        signer: signer.as_str().to_string(),
        action,
    };
    let (mut conn, view) = decision(state).await?;
    confirm_item_change(&mut conn, &view, &change).await?;
    Ok(change)
}

/// [`change_item`]'s decision again, inside the write's transaction.
pub(crate) async fn confirm_item_change(
    conn: &mut PgConnection,
    view: &DecisionView,
    change: &ItemChange,
) -> Result<(), ApiError> {
    let (id, s) = (change.id, change.signer.as_str());
    let i = item(conn, id).await?.ok_or_else(|| not_found(id, ITEMS))?;
    let Some(parent_id) = i.collection else {
        if i.owner != s {
            return Err(unauthorized_user(s, ITEMS, id));
        }
        if i.published {
            return Err(ApiError::published(
                Subject::Item,
                &id.to_string(),
                change.action,
            ));
        }
        return Ok(());
    };
    let c = parent(conn, view, parent_id).await?;
    if !c.role(s) {
        return Err(unauthorized_user(s, ITEMS, id));
    }
    if c.published || i.published {
        return Err(c.published_item(id, change.action));
    }
    if c.pending {
        return Err(ApiError::locked(
            Subject::Item,
            &id.to_string(),
            change.action,
        ));
    }
    Ok(())
}

/// The node's own publication routes: the collection's stored owner, who is
/// also the provider's manager of a third-party one.
pub async fn publisher(
    state: &SharedState,
    signer: &Signer,
    id: Uuid,
) -> Result<Publisher, ApiError> {
    let owner: Option<String> =
        sqlx::query_scalar("SELECT eth_address FROM builder.collections WHERE id = $1")
            .bind(id)
            .fetch_optional(state.items.pool())
            .await?;
    let s = signer.as_str();
    match owner {
        None => Err(not_found(id, COLLECTIONS)),
        Some(owner) if owner == s => Ok(Publisher {
            id,
            signer: s.to_string(),
        }),
        Some(_) => Err(unauthorized_user(s, COLLECTIONS, id)),
    }
}

/// Grants for store tests that exercise a store function by itself; the
/// writes still confirm their decision inside their transaction.
#[cfg(test)]
pub(crate) mod granted {
    use super::*;

    pub fn wallet(address: &str) -> Wallet {
        Wallet {
            address: address.to_string(),
        }
    }

    pub fn read(id: Uuid) -> CollectionRead {
        CollectionRead { id, full: true }
    }

    pub fn edit(signer: &str, id: Uuid) -> CollectionEdit {
        CollectionEdit {
            id,
            signer: signer.to_string(),
        }
    }

    pub fn item(id: Uuid) -> ItemRead {
        ItemRead { id, full: true }
    }

    pub fn save(signer: &str, id: Uuid, collection: Option<Uuid>) -> ItemSave {
        ItemSave {
            id,
            signer: signer.to_string(),
            collection,
        }
    }

    pub fn change(signer: &str, id: Uuid) -> ItemChange {
        ItemChange {
            id,
            signer: signer.to_string(),
            action: "deleted",
        }
    }

    pub fn publisher(signer: &str, id: Uuid) -> Publisher {
        Publisher {
            id,
            signer: signer.to_string(),
        }
    }
}

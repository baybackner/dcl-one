//! The authorization matrix: every route the route ledger marks `Routed`,
//! under both mounts, called anonymously and as the owner, a signed
//! stranger, an on-chain manager, a committee member and the chain creator
//! of a transferred collection, against each resource state that changes the
//! answer. Each cell asserts the status and the envelope's `error`, and a
//! refused write leaves its rows as they were. The last tests are the "can't
//! tell" column: a stale copy, a copy with no complete pass and a committee
//! source that is down answer 503 and write nothing; the small public reads
//! answer alike in each, and only the rarities answer 503 while their
//! source is down.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use super::support::{FakeCommittee, FakeOracle, committee_index, temp_storage, wallet};
use super::upstream_routes::{Route, UPSTREAM};
use crate::shared::signed_fetch::test_support::signed_headers_by;

const OWNER_KEY: &str = super::support::OWNER_KEY;
const STRANGER_KEY: &str = "0x2222222222222222222222222222222222222222222222222222222222222222";
const COMMITTEE_KEY: &str = "0x3333333333333333333333333333333333333333333333333333333333333333";
const MANAGER_KEY: &str = "0x4444444444444444444444444444444444444444444444444444444444444444";
const CREATOR_KEY: &str = "0x5555555555555555555555555555555555555555555555555555555555555555";
const SIBLING_KEY: &str = "0x6666666666666666666666666666666666666666666666666666666666666666";
const PROVIDER: &str = "urn:decentraland:matic:collections-thirdparty:matrix";
const MOUNTS: [&str; 2] = ["", "/builder-api"];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Caller {
    Anonymous,
    Owner,
    Stranger,
    Manager,
    Committee,
    Creator,
    Sibling,
}
use Caller::*;

const CALLERS: [Caller; 7] = [
    Anonymous, Owner, Stranger, Manager, Committee, Creator, Sibling,
];

impl Caller {
    fn key(self) -> Option<&'static str> {
        match self {
            Anonymous => None,
            Owner => Some(OWNER_KEY),
            Stranger => Some(STRANGER_KEY),
            Manager => Some(MANAGER_KEY),
            Committee => Some(COMMITTEE_KEY),
            Creator => Some(CREATOR_KEY),
            Sibling => Some(SIBLING_KEY),
        }
    }

    fn address(self) -> String {
        self.key()
            .map(|key| format!("{:#x}", wallet(key).address()))
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Missing,
    Malformed,
    Draft,
    Published,
    Approved,
    Unminted,
    Locked,
    ThirdParty,
    ThirdPartySubmitted,
    Transferred,
    Orphan,
}
use Kind::*;

const KINDS: [Kind; 11] = [
    Missing,
    Malformed,
    Draft,
    Published,
    Approved,
    Unminted,
    Locked,
    ThirdParty,
    ThirdPartySubmitted,
    Transferred,
    Orphan,
];

impl Kind {
    fn on_chain(self) -> bool {
        matches!(self, Published | Approved | Unminted | Transferred)
    }

    fn published(self) -> bool {
        self.on_chain() || self == ThirdPartySubmitted
    }

    fn third_party(self) -> bool {
        matches!(self, ThirdParty | ThirdPartySubmitted)
    }

    fn owner(self, caller: Caller) -> bool {
        match self {
            Missing | Malformed => false,
            Transferred => caller == Creator,
            _ => caller == Owner,
        }
    }

    fn manager(self, caller: Caller) -> bool {
        self.on_chain() && caller == Manager
    }

    fn item_owner(self, caller: Caller) -> bool {
        !matches!(self, Missing | Malformed) && caller == Owner
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Expect(u16, String);

fn ok() -> Option<Expect> {
    Some(Expect(200, String::new()))
}
/// The keys of the first body in upstream's capture `name`
/// (`tests/fixtures/conformance/builder/source`).
fn upstream_keys(name: &str) -> std::collections::BTreeSet<String> {
    let path = format!(
        "{}/services/catalyst/tests/fixtures/conformance/builder/source/{name}.json",
        env!("DCL_ONE_ROOT")
    );
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let data = &raw["response"]["body_json"]["data"];
    let body = data.as_array().map_or(data, |items| &items[0]);
    body.as_object().unwrap().keys().cloned().collect()
}

/// [`upstream_keys`] of a collection body, plus `third_party_id`: the
/// provider prefix of the `urn` the same caller receives.
/// The keys upstream answers a collection read: the full body or the public
/// detail, of a standard or a third-party collection.
fn collection_keys(full: bool, third_party: bool) -> std::collections::BTreeSet<String> {
    let mut keys = upstream_keys(match (full, third_party) {
        (_, true) => "80-collection-third-party-manager",
        (true, false) => "02-collection-owner",
        (false, false) => "23-collection-published-stranger",
    });
    if third_party && !full {
        for private in ["salt", "forum_link", "forum_id", "lock"] {
            keys.remove(private);
        }
    }
    keys
}

/// The role is admitted: whatever the route answers next, it is not a 401,
/// 403 or 404.
fn admitted() -> Option<Expect> {
    Some(Expect(0, String::new()))
}
fn refused(status: u16, error: impl Into<String>) -> Option<Expect> {
    Some(Expect(status, error.into()))
}
fn unauthenticated() -> Option<Expect> {
    refused(401, "Unauthenticated")
}
fn unauthorized() -> Option<Expect> {
    refused(401, "Unauthorized")
}
fn not_found(id: &str, table: &str) -> Option<Expect> {
    refused(404, format!("Couldn't find \"{id}\" on {table}"))
}
fn invalid_uuid(id: &str) -> Option<Expect> {
    refused(400, format!("Invalid uuid {id}"))
}
fn unauthorized_user(signer: &str, table: &str, id: &str) -> Option<Expect> {
    refused(401, format!("Unauthorized user {signer} for {table} {id}"))
}
fn collection_published(action: &str) -> Option<Expect> {
    refused(
        409,
        format!("The collection is published. It can't be {action}."),
    )
}
fn collection_locked(action: &str) -> Option<Expect> {
    refused(
        423,
        format!("The collection is locked. It can't be {action}."),
    )
}
fn item_published(action: &str) -> Option<Expect> {
    refused(
        409,
        format!(
            "The collection that contains this item has been already published. The item can't be {action}."
        ),
    )
}
fn third_party_item_published(action: &str) -> Option<Expect> {
    refused(
        409,
        format!("The third party item is already published. It can't be {action}."),
    )
}
fn item_locked(action: &str) -> Option<Expect> {
    refused(
        423,
        format!("The collection for the item is locked. The item can't be {action}."),
    )
}

#[derive(Debug, Clone, Default)]
struct Fixture {
    collection: Option<Uuid>,
    item: Option<Uuid>,
    minted: Option<Uuid>,
    urn: Option<String>,
    contract: Option<String>,
}

impl Fixture {
    fn collection_id(&self, kind: Kind) -> String {
        match kind {
            Malformed => "not-a-uuid".into(),
            _ => self.collection.unwrap_or_else(Uuid::new_v4).to_string(),
        }
    }

    fn item_id(&self, kind: Kind) -> String {
        match kind {
            Malformed => "not-a-uuid".into(),
            _ => self.item.unwrap_or_else(Uuid::new_v4).to_string(),
        }
    }

    /// The contract and on-chain id the item is read by.
    fn by_contract(&self, kind: Kind) -> (String, &'static str) {
        match kind {
            Malformed => ("not-an-address".into(), "0"),
            Unminted => (self.contract.clone().unwrap(), "1"),
            _ => (self.contract.clone().unwrap_or_else(contract), "0"),
        }
    }
}

fn contract() -> String {
    format!("0x{}", Uuid::new_v4().simple().to_string()[..20].repeat(2))
}

async fn insert_collection(
    pool: &PgPool,
    id: Uuid,
    third_party: bool,
    contract: Option<&str>,
    pending: bool,
) -> Option<String> {
    let urn = third_party.then(|| format!("{PROVIDER}:{id}"));
    sqlx::query(
        "INSERT INTO builder.collections (id, name, eth_address, contract_address, third_party_id, urn_suffix, publication_pending) \
         VALUES ($1, 'Matrix', $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(Owner.address())
    .bind(contract)
    .bind(third_party.then_some(PROVIDER))
    .bind(&urn)
    .bind(pending)
    .execute(pool)
    .await
    .unwrap();
    urn
}

async fn copy_row(pool: &PgPool, contract: &str, creator: Caller, approved: bool) {
    sqlx::query(
        "INSERT INTO shop.collections (network, contract_address, chain_id, name, creator, owner, urn, \
           is_completed, is_approved, is_editable, managers, minters, created_at, updated_at, reviewed_at, mirrored_at) \
         VALUES ('matic', $1, 137, 'Matrix', $2, $2, 'urn:matrix', true, $3, true, $4, '{}', 1, 1, 1, timezone('UTC', now()))",
    )
    .bind(contract)
    .bind(creator.address())
    .bind(approved)
    .bind(vec![Manager.address().to_uppercase().replace("0X", "0x")])
    .execute(pool)
    .await
    .unwrap();
}

/// The bytes of the one file every matrix item names, as `model.glb`.
const MATRIX_FILE: &[u8] = b"matrix model";

/// A client-shaped `PUT /v1/items/:id` body for the matrix item `id`.
fn item_json(id: &str, collection: Option<Uuid>, urn: Option<String>) -> Value {
    let mut item = json!({
        "id": id, "name": "Matrix renamed", "description": "", "type": "wearable",
        "eth_address": Owner.address(), "rarity": "common", "thumbnail": "thumbnail.png",
        "collection_id": collection,
        "contents": { "model.glb": dcl_auth::hash_bytes_v1(MATRIX_FILE) },
        "metrics": { "meshes": 1, "bodies": 1, "materials": 1, "textures": 1, "triangles": 1, "entities": 1 },
        "data": {
            "category": "hat", "replaces": [], "hides": [], "tags": [],
            "representations": [{
                "bodyShapes": ["urn:decentraland:off-chain:base-avatars:BaseMale"],
                "mainFile": "model.glb", "contents": ["model.glb"],
                "overrideHides": [], "overrideReplaces": []
            }]
        }
    });
    if let Some(urn) = urn {
        item["urn"] = urn.into();
    }
    json!({ "item": item })
}

/// The URN of the matrix item of the third-party collection `urn`.
fn item_urn(collection_urn: &str) -> String {
    format!("{collection_urn}:matrix-item")
}

async fn insert_item(
    pool: &PgPool,
    collection: Option<Uuid>,
    minted: bool,
    third_party: Option<(&str, bool)>,
) -> Uuid {
    let id = Uuid::new_v4();
    let urn = third_party.map(|(urn, _)| item_urn(urn));
    let submitted = third_party.is_some_and(|(_, submitted)| submitted);
    sqlx::query(
        "INSERT INTO builder.items (id, collection_id, eth_address, name, type, blockchain_item_id, local_content_hash, \
           urn_suffix, is_published) \
         VALUES ($1, $2, $3, 'Matrix item', 'wearable', $4, 'matrix-local-hash', $5, $6)",
    )
    .bind(id)
    .bind(collection)
    .bind(Owner.address())
    .bind(if submitted { Some("matrix-item") } else { minted.then_some("0") })
    .bind(&urn)
    .bind(submitted)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO builder.item_contents (item_id, file, hash, named_by) VALUES ($1, 'model.glb', $2, $3)",
    )
    .bind(id)
    .bind(dcl_auth::hash_bytes_v1(MATRIX_FILE))
    .bind(Owner.address())
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn fixture(pool: &PgPool, kind: Kind) -> Fixture {
    let id = Uuid::new_v4();
    match kind {
        Malformed => Fixture::default(),
        Missing => Fixture {
            collection: Some(id),
            item: Some(Uuid::new_v4()),
            ..Fixture::default()
        },
        Orphan => Fixture {
            item: Some(insert_item(pool, None, false, None).await),
            ..Fixture::default()
        },
        Draft | Locked | ThirdParty | ThirdPartySubmitted => {
            let urn = insert_collection(pool, id, kind.third_party(), None, kind == Locked).await;
            if kind == ThirdPartySubmitted {
                sqlx::query(
                    "INSERT INTO builder.linked_collection_publications (collection_id, preparation, snapshot, salt, status, cheque) \
                     VALUES ($1, '{\"revision\":\"r\"}', '{}', $2, 'submitted', '{}')",
                )
                .bind(id)
                .bind(format!("0x{}{}", id.simple(), id.simple()))
                .execute(pool)
                .await
                .unwrap();
            }
            let third_party = urn.as_deref().map(|urn| (urn, kind == ThirdPartySubmitted));
            Fixture {
                collection: Some(id),
                item: Some(insert_item(pool, Some(id), false, third_party).await),
                minted: None,
                urn,
                contract: None,
            }
        }
        Published | Approved | Unminted | Transferred => {
            let contract = contract();
            insert_collection(pool, id, false, Some(&contract), false).await;
            let creator = if kind == Transferred { Creator } else { Owner };
            copy_row(pool, &contract, creator, kind == Approved).await;
            let minted = insert_item(pool, Some(id), true, None).await;
            let unminted = insert_item(pool, Some(id), false, None).await;
            Fixture {
                collection: Some(id),
                item: Some(if kind == Unminted { unminted } else { minted }),
                minted: Some(minted),
                urn: None,
                contract: Some(contract),
            }
        }
    }
}

async fn snapshot(pool: &PgPool) -> Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object( \
           'collections', (SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c.id), '[]') FROM builder.collections c), \
           'items', (SELECT COALESCE(jsonb_agg(to_jsonb(i) ORDER BY i.id), '[]') FROM builder.items i), \
           'contents', (SELECT count(*) FROM builder.item_contents), \
           'uploads', (SELECT count(*) FROM builder.uploaded_contents), \
           'publications', (SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p.collection_id), '[]') FROM builder.collection_publications p), \
           'linked', (SELECT COALESCE(jsonb_agg(to_jsonb(l) ORDER BY l.collection_id), '[]') FROM builder.linked_collection_publications l))",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

enum Payload {
    None,
    Json(Value),
    Upload,
}

const BOUNDARY: &str = "matrix-boundary";

async fn call(
    app: &Router,
    caller: Caller,
    method: &str,
    path: &str,
    payload: Payload,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(key) = caller.key() {
        let now = Utc::now().timestamp_millis().to_string();
        let signed = path.split('?').next().unwrap();
        for (name, value) in signed_headers_by(
            &wallet(key),
            &method.to_lowercase(),
            signed,
            "{}",
            true,
            &now,
        )
        .await
        .iter()
        {
            request = request.header(name, value);
        }
    }
    let body = match payload {
        Payload::None => Body::empty(),
        Payload::Json(value) => {
            let value = match (method, value.get("collection")) {
                ("PUT", Some(collection)) if path.contains("/v1/collections/") => {
                    let signer = caller.key().map_or(Owner.address(), |key| {
                        format!("{:#x}", wallet(key).address())
                    });
                    let mut collection = collection.clone();
                    if let (Some(id), Some(name)) =
                        (collection["id"].as_str(), collection["name"].as_str())
                    {
                        collection["name"] = format!("{name} {}", &id[..8]).into();
                    }
                    super::support::collection_body(collection, &signer)
                }
                _ => value,
            };
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        Payload::Upload => {
            request = request.header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            );
            Body::from(format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"blob\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n{}\r\n--{BOUNDARY}--\r\n",
                dcl_auth::hash_bytes_v1(MATRIX_FILE),
                String::from_utf8_lossy(MATRIX_FILE)
            ))
        }
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    ReadCollection,
    SaveCollection,
    CollectionItems,
    ReadItem,
    SaveItem,
    DeleteItem,
    UploadFiles,
    ListCollections,
    ListItems,
    AddressCollections,
    AddressItems,
    Committee,
    StorageGet,
    StorageHead,
    StorageExists,
    Newsletter,
    DraftCollections,
    DraftItems,
    PublicationPrepare,
    PublicationBegin,
    PublicationTransaction,
    PublicationCancel,
    PublicationClaim,
    PublicationStatus,
    LinkedPrepare,
    LinkedBegin,
    LinkedClaim,
    LinkedAuthorize,
    LinkedCancel,
    LinkedStatus,
    LinkedVerify,
    CurationQueue,
    ItemStatus,
    ItemsStatus,
    Ping,
    Info,
    Rarities,
    Rarity,
    Utility,
    ItemContents,
    Addresses,
}

const RARITIES_UNAVAILABLE: &str = "The rarities are not available. Try again later.";

const NO_FOUNDATION: &str =
    "Foundation review is off on this node: BUILDER_FOUNDATION_URL names no Foundation";
const NOT_ADMIN: &str =
    "Not authorized: curation requires the admin token or a committee-address signed request";

/// Every method and path the builder's router serves, read from where
/// `lib.rs` declares them, so a route added there without a matrix row
/// fails [`ops`].
fn router_routes() -> Vec<(String, String)> {
    const METHODS: [&str; 6] = ["get", "post", "put", "patch", "delete", "head"];
    let source = include_str!("../lib.rs");
    for other in [
        "route_service(",
        "nest_service(",
        ".nest(",
        ".merge(",
        ".fallback(",
        "routing::any",
        "routing::on",
        "MethodFilter",
    ] {
        assert!(
            !source.contains(other),
            "lib.rs declares a route with {other}, which the matrix can't read"
        );
    }
    let imported = source
        .split("use axum::routing::{")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("lib.rs imports its routing methods from axum::routing");
    for name in imported.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        assert!(
            METHODS.contains(&name),
            "lib.rs routes with axum::routing::{name}, which the matrix can't read"
        );
    }
    let mut routes = Vec::new();
    for chunk in source.split(".route(").skip(1) {
        let chunk = chunk.split("\n}\n").next().unwrap();
        let path = chunk.split('"').nth(1).unwrap().to_string();
        let before = routes.len();
        for method in METHODS {
            for (at, _) in chunk.match_indices(&format!("{method}(")) {
                if chunk[..at]
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
                {
                    continue;
                }
                routes.push((method.to_uppercase(), path.clone()));
            }
        }
        assert!(
            routes.len() > before,
            "route {path} declares no method the matrix can read"
        );
    }
    routes
}

impl Op {
    fn of(method: &str, path: &str) -> Option<Self> {
        const PUBLICATION: &str = "/v1/collections/{id}/publication";
        const LINKED: &str = "/v1/collections/{id}/linked-publication";
        Some(match (method, path) {
            ("GET", "/v1/collections/{id}") => Op::ReadCollection,
            ("PUT", "/v1/collections/{id}") => Op::SaveCollection,
            ("GET", "/v1/collections/{id}/items") => Op::CollectionItems,
            ("GET", "/v1/items/{id}") => Op::ReadItem,
            ("PUT", "/v1/items/{id}") => Op::SaveItem,
            ("DELETE", "/v1/items/{id}") => Op::DeleteItem,
            ("POST", "/v1/items/{id}/files") => Op::UploadFiles,
            ("GET", "/v1/collections") => Op::ListCollections,
            ("GET", "/v1/items") => Op::ListItems,
            ("GET", "/v1/{address}/collections") => Op::AddressCollections,
            ("GET", "/v1/{address}/items") => Op::AddressItems,
            ("GET", "/v1/committee") => Op::Committee,
            ("GET", "/v1/storage/contents/{hash}") => Op::StorageGet,
            ("HEAD", "/v1/storage/contents/{hash}") => Op::StorageHead,
            ("GET", "/v1/storage/contents/{hash}/exists") => Op::StorageExists,
            ("POST", "/v1/newsletter") => Op::Newsletter,
            ("GET", "/v1/drafts/collections") => Op::DraftCollections,
            ("GET", "/v1/drafts/items") => Op::DraftItems,
            ("GET", PUBLICATION) => Op::PublicationPrepare,
            ("POST", PUBLICATION) => Op::PublicationBegin,
            ("PUT", PUBLICATION) => Op::PublicationTransaction,
            ("DELETE", PUBLICATION) => Op::PublicationCancel,
            ("PATCH", PUBLICATION) => Op::PublicationClaim,
            ("GET", "/v1/collections/{id}/publication/status") => Op::PublicationStatus,
            ("GET", LINKED) => Op::LinkedPrepare,
            ("POST", LINKED) => Op::LinkedBegin,
            ("PATCH", LINKED) => Op::LinkedClaim,
            ("PUT", LINKED) => Op::LinkedAuthorize,
            ("DELETE", LINKED) => Op::LinkedCancel,
            ("GET", "/v1/collections/{id}/linked-publication/status") => Op::LinkedStatus,
            ("POST", "/v1/collections/{id}/linked-publication/verify") => Op::LinkedVerify,
            ("GET", "/v1/collections/curation") => Op::CurationQueue,
            ("PATCH", "/v1/collections/{id}/items/{item}/status") => Op::ItemStatus,
            ("PATCH", "/v1/collections/{id}/items/status") => Op::ItemsStatus,
            ("GET", "/ping") => Op::Ping,
            ("GET", "/v1/info") => Op::Info,
            ("GET", "/v1/rarities") => Op::Rarities,
            ("GET", "/v1/rarities/{name}") => Op::Rarity,
            ("GET", "/v1/published-collections/{address}/items/{id}/utility") => Op::Utility,
            ("GET", "/v1/items/{id}/{item}/contents") => Op::ItemContents,
            ("GET", "/v1/addresses") => Op::Addresses,
            _ => return None,
        })
    }

    fn publication(self) -> bool {
        matches!(
            self,
            Op::PublicationPrepare
                | Op::PublicationBegin
                | Op::PublicationTransaction
                | Op::PublicationCancel
                | Op::PublicationClaim
                | Op::PublicationStatus
        )
    }

    fn linked(self) -> bool {
        matches!(
            self,
            Op::LinkedPrepare
                | Op::LinkedBegin
                | Op::LinkedClaim
                | Op::LinkedAuthorize
                | Op::LinkedCancel
                | Op::LinkedStatus
                | Op::LinkedVerify
        )
    }

    fn small_read(self) -> bool {
        matches!(
            self,
            Op::Info | Op::Rarities | Op::Rarity | Op::Utility | Op::ItemContents | Op::Addresses
        )
    }

    fn by_contract(self) -> bool {
        matches!(self, Op::Utility | Op::ItemContents)
    }

    fn kinds(self) -> &'static [Kind] {
        if self.by_contract() {
            &[
                Missing,
                Malformed,
                Draft,
                Published,
                Approved,
                Unminted,
                Transferred,
                ThirdParty,
            ]
        } else if self.by_id() {
            &KINDS
        } else if self.publication() {
            &[Missing, Draft]
        } else if self.linked() {
            &[Missing, ThirdParty]
        } else {
            &[Missing]
        }
    }

    fn by_id(self) -> bool {
        matches!(
            self,
            Op::ReadCollection
                | Op::SaveCollection
                | Op::CollectionItems
                | Op::ReadItem
                | Op::SaveItem
                | Op::DeleteItem
                | Op::UploadFiles
        )
    }

    fn writes(self) -> bool {
        matches!(
            self,
            Op::SaveCollection
                | Op::SaveItem
                | Op::DeleteItem
                | Op::UploadFiles
                | Op::PublicationBegin
                | Op::PublicationTransaction
                | Op::PublicationCancel
                | Op::PublicationClaim
                | Op::LinkedBegin
                | Op::LinkedClaim
                | Op::LinkedAuthorize
                | Op::LinkedCancel
                | Op::LinkedVerify
                | Op::ItemStatus
                | Op::ItemsStatus
        )
    }

    fn on_items(self) -> bool {
        matches!(
            self,
            Op::ReadItem | Op::SaveItem | Op::DeleteItem | Op::UploadFiles
        )
    }

    fn request(self, kind: Kind, f: &Fixture) -> (&'static str, String, Payload) {
        let owner = Owner.address();
        let cid = "bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy";
        match self {
            Op::ReadCollection => (
                "GET",
                format!("/v1/collections/{}", f.collection_id(kind)),
                Payload::None,
            ),
            Op::SaveCollection => {
                let id = f.collection_id(kind);
                let mut collection = json!({ "id": id, "name": "Matrix renamed" });
                if let Some(urn) = &f.urn {
                    collection["urn"] = urn.clone().into();
                }
                (
                    "PUT",
                    format!("/v1/collections/{id}"),
                    Payload::Json(json!({ "collection": collection })),
                )
            }
            Op::CollectionItems => (
                "GET",
                format!("/v1/collections/{}/items", f.collection_id(kind)),
                Payload::None,
            ),
            Op::ReadItem => (
                "GET",
                format!("/v1/items/{}", f.item_id(kind)),
                Payload::None,
            ),
            Op::SaveItem => {
                let id = f.item_id(kind);
                let collection = if kind == Missing { None } else { f.collection };
                let urn = f.urn.as_deref().map(item_urn);
                (
                    "PUT",
                    format!("/v1/items/{id}"),
                    Payload::Json(item_json(&id, collection, urn)),
                )
            }
            Op::DeleteItem => (
                "DELETE",
                format!("/v1/items/{}", f.item_id(kind)),
                Payload::None,
            ),
            Op::UploadFiles => (
                "POST",
                format!("/v1/items/{}/files", f.item_id(kind)),
                Payload::Upload,
            ),
            Op::ListCollections => ("GET", "/v1/collections".into(), Payload::None),
            Op::ListItems => ("GET", "/v1/items".into(), Payload::None),
            Op::AddressCollections => ("GET", format!("/v1/{owner}/collections"), Payload::None),
            Op::AddressItems => ("GET", format!("/v1/{owner}/items"), Payload::None),
            Op::Committee => ("GET", "/v1/committee".into(), Payload::None),
            Op::StorageGet => ("GET", format!("/v1/storage/contents/{cid}"), Payload::None),
            Op::StorageHead => ("HEAD", format!("/v1/storage/contents/{cid}"), Payload::None),
            Op::StorageExists => (
                "GET",
                format!("/v1/storage/contents/{cid}/exists"),
                Payload::None,
            ),
            Op::Newsletter => ("POST", "/v1/newsletter".into(), Payload::Json(json!({}))),
            Op::DraftCollections => ("GET", "/v1/drafts/collections".into(), Payload::None),
            Op::DraftItems => ("GET", "/v1/drafts/items".into(), Payload::None),
            Op::CurationQueue => ("GET", "/v1/collections/curation".into(), Payload::None),
            Op::Ping => ("GET", "/ping".into(), Payload::None),
            Op::Info => ("GET", "/v1/info".into(), Payload::None),
            Op::Rarities => ("GET", "/v1/rarities".into(), Payload::None),
            Op::Rarity => ("GET", "/v1/rarities/common".into(), Payload::None),
            Op::Addresses => ("GET", "/v1/addresses?tag=matrix".into(), Payload::None),
            Op::Utility => {
                let (contract, id) = f.by_contract(kind);
                (
                    "GET",
                    format!("/v1/published-collections/{contract}/items/{id}/utility"),
                    Payload::None,
                )
            }
            Op::ItemContents => {
                let (contract, id) = f.by_contract(kind);
                (
                    "GET",
                    format!("/v1/items/{contract}/{id}/contents"),
                    Payload::None,
                )
            }
            _ => {
                let c = f.collection_id(kind);
                let revision = Payload::Json(json!({ "revision": "r" }));
                let (method, path, payload) = match self {
                    Op::PublicationPrepare => ("GET", "publication", Payload::None),
                    Op::PublicationBegin => ("POST", "publication", revision),
                    Op::PublicationTransaction => (
                        "PUT",
                        "publication",
                        Payload::Json(json!({ "tx_hash": format!("0x{}", "1".repeat(64)) })),
                    ),
                    Op::PublicationCancel => ("DELETE", "publication", Payload::None),
                    Op::PublicationClaim => ("PATCH", "publication", revision),
                    Op::PublicationStatus => ("GET", "publication/status", Payload::None),
                    Op::LinkedPrepare => ("GET", "linked-publication", Payload::None),
                    Op::LinkedBegin => ("POST", "linked-publication", revision),
                    Op::LinkedClaim => ("PATCH", "linked-publication", revision),
                    Op::LinkedAuthorize => (
                        "PUT",
                        "linked-publication",
                        Payload::Json(json!({ "qty": 1, "salt": "0x01", "signature": "0x01" })),
                    ),
                    Op::LinkedCancel => ("DELETE", "linked-publication", Payload::None),
                    Op::LinkedStatus => ("GET", "linked-publication/status", Payload::None),
                    Op::LinkedVerify => (
                        "POST",
                        "linked-publication/verify",
                        Payload::Json(json!({ "collection": {}, "items": {}, "curations": {} })),
                    ),
                    Op::ItemStatus => (
                        "PATCH",
                        "items/{item}/status",
                        Payload::Json(json!({ "status": "approved" })),
                    ),
                    _ => (
                        "PATCH",
                        "items/status",
                        Payload::Json(
                            json!({ "status": "approved", "itemIds": [f.item_id(kind)] }),
                        ),
                    ),
                };
                let path = path.replace("{item}", &f.item_id(kind));
                (method, format!("/v1/collections/{c}/{path}"), payload)
            }
        }
    }

    /// The decision table; `None` where the cell doesn't apply.
    fn expect(self, kind: Kind, caller: Caller, f: &Fixture) -> Option<Expect> {
        let signed = caller != Anonymous;
        if self == Op::Ping {
            return ok();
        }
        if self.small_read() {
            let minted = matches!(kind, Published | Approved | Transferred);
            return match self {
                Op::Utility if !minted => refused(404, "The item doesn't exist."),
                Op::ItemContents if kind == Malformed => {
                    refused(400, "Invalid address not-an-address")
                }
                _ => ok(),
            };
        }
        if !signed
            && !matches!(
                self,
                Op::ListCollections
                    | Op::Committee
                    | Op::StorageGet
                    | Op::StorageHead
                    | Op::StorageExists
                    | Op::Newsletter
            )
        {
            return unauthenticated();
        }
        if self.publication() || self.linked() {
            let collection = f.collection_id(kind);
            return match kind {
                _ if self == Op::LinkedVerify => refused(503, NO_FOUNDATION),
                Missing => not_found(&collection, "collections"),
                _ if caller == Owner && self == Op::PublicationTransaction => refused(
                    404,
                    "Prepare this collection before submitting its transaction",
                ),
                _ if caller == Owner => admitted(),
                _ => unauthorized_user(&caller.address(), "collections", &collection),
            };
        }
        match self {
            Op::DraftCollections | Op::DraftItems => return ok(),
            Op::CurationQueue | Op::ItemStatus | Op::ItemsStatus => return refused(403, NOT_ADMIN),
            _ => {}
        }
        if !self.by_id() {
            return match self {
                Op::ListCollections if caller == Committee => ok(),
                Op::ListCollections => unauthorized(),
                Op::ListItems | Op::AddressCollections | Op::AddressItems if !signed => {
                    unauthenticated()
                }
                Op::ListItems if caller == Committee => ok(),
                Op::AddressCollections | Op::AddressItems if caller == Owner => ok(),
                Op::ListItems | Op::AddressCollections | Op::AddressItems => unauthorized(),
                Op::Committee => ok(),
                Op::StorageGet => refused(404, "Not Found"),
                Op::StorageHead | Op::StorageExists => refused(404, ""),
                _ => refused(400, "email is required"),
            };
        }
        if !self.on_items() && matches!(kind, Orphan | Unminted) {
            return None;
        }
        if !signed {
            return unauthenticated();
        }
        let collection = f.collection_id(kind);
        let item = f.item_id(kind);
        let committee = caller == Committee;
        let owner = kind.owner(caller);
        let manager = kind.manager(caller);
        match self {
            Op::ReadCollection => match kind {
                Missing => not_found(&collection, "collections"),
                Malformed => invalid_uuid(&collection),
                _ if kind.published() || owner || committee => ok(),
                _ => unauthorized(),
            },
            Op::SaveCollection => match kind {
                Missing => ok(),
                Malformed => None,
                _ if !owner => refused(401, "Unauthorized to upsert collection"),
                ThirdPartySubmitted => ok(),
                _ if kind.published() => collection_published("updated"),
                Locked => collection_locked("updated"),
                _ => ok(),
            },
            Op::CollectionItems => match kind {
                Missing => not_found(&collection, "collections"),
                Malformed => refused(
                    400,
                    format!("Invalid collection ID or contract address format: {collection}"),
                ),
                _ if kind.published() || owner || manager || committee => ok(),
                _ => unauthorized(),
            },
            Op::ReadItem => match kind {
                Missing => not_found(&item, "items"),
                Malformed => invalid_uuid(&item),
                Published | Approved | Transferred => ok(),
                _ if kind.item_owner(caller) || committee => ok(),
                _ => unauthorized(),
            },
            Op::SaveItem => match kind {
                Missing => ok(),
                Malformed => None,
                Orphan if caller == Owner => ok(),
                Orphan => refused(401, "The user is unauthorized to upsert the collection."),
                _ if kind.third_party() && !owner => {
                    refused(401, "The user is unauthorized to upsert the collection.")
                }
                _ if !owner && !manager => {
                    refused(401, "You're not authorized to to change this collection.")
                }
                Locked => item_locked("inserted or updated"),
                _ => ok(),
            },
            Op::DeleteItem => match kind {
                Missing => not_found(&item, "items"),
                Malformed => invalid_uuid(&item),
                Orphan if caller == Owner => ok(),
                _ if !owner && !manager => unauthorized_user(&caller.address(), "items", &item),
                ThirdPartySubmitted => third_party_item_published("deleted"),
                _ if kind.published() => item_published("deleted"),
                Locked => item_locked("deleted"),
                _ => ok(),
            },
            Op::UploadFiles => match kind {
                Missing => not_found(&item, "items"),
                Malformed => invalid_uuid(&item),
                Orphan if caller == Owner => ok(),
                _ if !owner && !manager => unauthorized_user(&caller.address(), "items", &item),
                _ => ok(),
            },
            _ => None,
        }
    }
}

struct Matrix {
    pool: PgPool,
    app: Router,
    state: crate::SharedState,
}

async fn fresh_pass(pool: &PgPool, minutes_ago: i64) {
    sqlx::query(
        "INSERT INTO shop.mirror_state (mirror, network, source_url, last_complete_pass_at, pass_started_at) \
         VALUES ('collections', 'matic', 'https://subgraph.example', $1, $1) \
         ON CONFLICT (mirror, network) DO UPDATE SET last_complete_pass_at = EXCLUDED.last_complete_pass_at, \
           pass_started_at = EXCLUDED.pass_started_at, cursor_id = NULL",
    )
    .bind(Utc::now().naive_utc() - TimeDelta::minutes(minutes_ago))
    .execute(pool)
    .await
    .unwrap();
}

async fn matrix(committee: &FakeCommittee) -> Option<Matrix> {
    matrix_with(committee, 8, true).await
}

async fn matrix_with(committee: &FakeCommittee, connections: u32, rpc: bool) -> Option<Matrix> {
    let pool = crate::db::scratch::isolated_pool(connections).await?;
    fresh_pass(&pool, 0).await;
    committee.set_rarities(Some(&[("common", "10000000000000000000", "100000")]));
    let oracle = FakeOracle::with(&[("common", "4000000000000000000")]);
    let state = Arc::new(
        crate::State::new(
            pool.clone(),
            temp_storage().await,
            Arc::default(),
            dcl_http::Outbound::for_tests(),
            Some(committee_index(committee).await),
            None,
            crate::Settings {
                writes_enabled: true,
                polygon_rpc_url: if rpc { Some(oracle.url().await) } else { None },
                ..crate::Settings::default()
            },
        )
        .unwrap(),
    );
    let app = Router::new()
        .merge(crate::root_router(state.clone()))
        .nest("/builder-api", crate::router(state.clone()));
    sibling_collection(&pool).await;
    sqlx::query(
        "UPDATE builder.items SET data = '{\"tags\":[\"matrix\"]}'::json WHERE blockchain_item_id IS NOT NULL",
    )
    .execute(&pool)
    .await
    .unwrap();
    Some(Matrix { pool, app, state })
}

/// A third-party collection under the matrix's provider that the sibling
/// wallet created, with one item: it manages that collection and nothing
/// else of the provider.
async fn sibling_collection(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let urn = format!("{PROVIDER}:sibling-{}", id.simple());
    sqlx::query(
        "INSERT INTO builder.collections (id, name, eth_address, third_party_id, urn_suffix) \
         VALUES ($1, 'Sibling', $2, $3, $4)",
    )
    .bind(id)
    .bind(Sibling.address())
    .bind(PROVIDER)
    .bind(&urn)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO builder.items (id, collection_id, eth_address, name, type, urn_suffix) \
         VALUES ($1, $2, $3, 'Sibling item', 'wearable', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(Sibling.address())
    .bind(item_urn(&urn))
    .execute(pool)
    .await
    .unwrap();
    id
}

fn ops() -> Vec<Op> {
    let routes = router_routes();
    let upstream = UPSTREAM
        .iter()
        .filter(|(_, _, route)| *route == Route::Routed);
    assert_eq!(
        routes
            .iter()
            .filter(|(_, path)| path.starts_with("/v1"))
            .count(),
        routes.len() - 1,
        "every route but /ping is under /v1: {routes:?}"
    );
    assert!(
        upstream.count() <= routes.len(),
        "the router declares fewer routes than the ledger marks routed: {routes:?}"
    );
    routes
        .iter()
        .map(|(method, path)| {
            Op::of(method, path)
                .unwrap_or_else(|| panic!("{method} {path} is routed and has no matrix row"))
        })
        .collect()
}

async fn cell(
    m: &Matrix,
    op: Op,
    kind: Kind,
    caller: Caller,
    mount: &str,
    committee_up: bool,
    expect: impl Fn(&Fixture) -> Option<Expect>,
) -> Option<String> {
    if op == Op::Ping && mount.is_empty() {
        return None;
    }
    let f = fixture(&m.pool, kind).await;
    let expected = expect(&f)?;
    let (method, path, payload) = op.request(kind, &f);
    let before = snapshot(&m.pool).await;
    let (status, body) = call(&m.app, caller, method, &format!("{mount}{path}"), payload).await;
    let error = body["error"].as_str().unwrap_or_default().to_string();
    let got = Expect(
        status.as_u16(),
        if status.is_success() {
            String::new()
        } else {
            error
        },
    );
    let mut problems = Vec::new();
    let refusal = matches!(got.0, 401 | 403 | 404);
    if (expected.0 == 0 && refusal) || (expected.0 != 0 && got != expected) {
        problems.push(format!("answered {got:?}, expected {expected:?}"));
    }
    if op.writes() && !matches!(expected.0, 0 | 200) && snapshot(&m.pool).await != before {
        problems.push("a refused write changed rows".to_string());
    }
    let sees_all = (op == Op::ReadItem && kind.item_owner(caller))
        || kind.owner(caller)
        || kind.manager(caller)
        || (committee_up && caller == Committee);
    let items: Vec<&Value> = match op {
        Op::ReadItem if status.is_success() => vec![&body["data"]],
        Op::CollectionItems if status.is_success() => body["data"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        _ => vec![],
    };
    let allowed = match op {
        Op::ReadItem | Op::CollectionItems if kind.third_party() => {
            let mut keys = upstream_keys("161-item-linked-owner");
            if !sees_all {
                keys.remove("local_content_hash");
            }
            keys
        }
        Op::ReadItem | Op::CollectionItems if sees_all => upstream_keys("37-item-published-owner"),
        Op::ReadItem | Op::CollectionItems => upstream_keys("36-item-published-stranger"),
        _ => collection_keys(sees_all, kind.third_party()),
    };
    let bodies: Vec<&Value> = match op {
        Op::ReadCollection if status.is_success() => vec![&body["data"]],
        _ => items.clone(),
    };
    for answered in bodies {
        let extra: Vec<&String> = answered
            .as_object()
            .map(|fields| fields.keys().filter(|k| !allowed.contains(*k)).collect())
            .unwrap_or_default();
        if !extra.is_empty() {
            problems.push(format!("keys upstream doesn't send this caller: {extra:?}"));
        }
        if op == Op::ReadCollection {
            let missing: Vec<&String> = allowed
                .iter()
                .filter(|k| answered.get(k.as_str()).is_none())
                .collect();
            if !missing.is_empty() {
                problems.push(format!(
                    "keys upstream sends this caller missing: {missing:?}"
                ));
            }
        }
    }
    for item in items {
        if item.get("local_content_hash").is_some() != sees_all {
            problems.push(format!(
                "local_content_hash {} for a caller who {} sees the full item",
                if sees_all { "missing" } else { "sent" },
                if sees_all { "" } else { "never" }
            ));
        }
    }
    if op == Op::CollectionItems && status.is_success() {
        let listed: Vec<String> = body["data"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let full =
            kind.owner(caller) || kind.manager(caller) || (committee_up && caller == Committee);
        let expected: Vec<String> = match (full, f.minted) {
            (false, Some(minted)) => vec![minted.to_string()],
            (false, None) if kind == ThirdPartySubmitted => {
                f.item.iter().map(Uuid::to_string).collect()
            }
            (false, None) => vec![],
            (true, _) => {
                let mut all: Vec<String> = sqlx::query_scalar(
                    "SELECT id::text FROM builder.items WHERE collection_id = $1 ORDER BY created_at, id",
                )
                .bind(f.collection)
                .fetch_all(&m.pool)
                .await
                .unwrap();
                all.sort();
                all
            }
        };
        let mut listed = listed;
        listed.sort();
        if listed != expected {
            problems.push(format!("listed {listed:?}, expected {expected:?}"));
        }
    }
    (!problems.is_empty()).then(|| {
        format!(
            "{mount}{method} {path} {kind:?} as {caller:?}: {}",
            problems.join("; ")
        )
    })
}

#[tokio::test]
async fn every_routed_operation_answers_each_caller_as_upstream_decides() {
    let committee = FakeCommittee::with(&[&Committee.address()]);
    let Some(m) = matrix(&committee).await else {
        eprintln!("skipping: CATALYST_TEST_DATABASE_URL not set");
        return;
    };
    let mut failures = Vec::new();
    let mut cells = 0;
    for op in ops() {
        for &kind in op.kinds() {
            for caller in CALLERS {
                for mount in MOUNTS {
                    if let Some(failure) = cell(&m, op, kind, caller, mount, true, |f| {
                        op.expect(kind, caller, f)
                    })
                    .await
                    {
                        failures.push(failure);
                    }
                    cells += 1;
                }
            }
        }
    }
    assert!(cells > 1000, "{cells} cells");
    assert!(
        failures.is_empty(),
        "{} failing cells:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn unavailable(op: Op, kind: Kind, caller: Caller) -> Option<Expect> {
    match caller {
        Anonymous => unauthenticated(),
        _ if op.by_id() && matches!(kind, Draft | ThirdParty | Published) => {
            Some(Expect(503, String::new()))
        }
        _ => None,
    }
}

/// With the copy stale (`stale`) or without a complete pass, every by-id
/// decision answers 503, but a stored owner's read of their own draft
/// (standard, not on chain, or third-party) while the copy is only stale:
/// no chain fact can change that answer.
async fn assert_cannot_tell(m: &Matrix, stale: bool) {
    let mut failures = Vec::new();
    for op in ops().into_iter().filter(|op| op.by_id()) {
        for caller in CALLERS {
            for mount in MOUNTS {
                for kind in [Draft, ThirdParty, Published] {
                    let f = fixture(&m.pool, kind).await;
                    let Some(Expect(mut expected, _)) = unavailable(op, kind, caller) else {
                        continue;
                    };
                    if stale && op == Op::ReadCollection && caller == Owner && kind != Published {
                        expected = 200;
                    }
                    let (method, path, payload) = op.request(kind, &f);
                    let before = snapshot(&m.pool).await;
                    let (status, body) =
                        call(&m.app, caller, method, &format!("{mount}{path}"), payload).await;
                    if status.as_u16() != expected
                        || (expected == 503
                            && body["error"]
                                != "Chain state is not available yet. Try again later.")
                    {
                        failures.push(format!(
                            "{mount}{method} {path} as {caller:?}: {status} {body}"
                        ));
                    }
                    if snapshot(&m.pool).await != before {
                        failures.push(format!(
                            "{mount}{method} {path} as {caller:?}: wrote while the copy can't tell"
                        ));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn a_stale_copy_grants_nothing_and_writes_nothing() {
    let committee = FakeCommittee::with(&[&Committee.address()]);
    let Some(m) = matrix(&committee).await else {
        return;
    };
    fresh_pass(&m.pool, 31).await;
    assert_cannot_tell(&m, true).await;
}

#[tokio::test]
async fn a_copy_without_a_complete_pass_grants_nothing_and_writes_nothing() {
    let committee = FakeCommittee::with(&[&Committee.address()]);
    let Some(m) = matrix(&committee).await else {
        return;
    };
    sqlx::query("DELETE FROM shop.mirror_state")
        .execute(&m.pool)
        .await
        .unwrap();
    assert_cannot_tell(&m, false).await;
}

/// With the committee source down, a decision the committee could change
/// answers 503; one it can't change (an owner's, any write, and what any
/// signer may see of published content) answers as before, a committee
/// member seeing a published collection as any signer does.
#[tokio::test]
async fn a_committee_source_that_is_down_refuses_only_what_the_committee_decides() {
    let committee = FakeCommittee::down();
    let Some(m) = matrix(&committee).await else {
        return;
    };
    let committee_unavailable = Expect(
        503,
        "The curation committee is not available. Try again later.".to_string(),
    );
    let mut failures = Vec::new();
    for op in ops()
        .into_iter()
        .filter(|op| !matches!(op, Op::AddressCollections | Op::AddressItems))
    {
        for &kind in op.kinds() {
            for caller in CALLERS {
                let decides = match op {
                    Op::ReadCollection => {
                        !matches!(kind, Missing | Malformed)
                            && !kind.published()
                            && !kind.owner(caller)
                    }
                    Op::CollectionItems => {
                        !matches!(kind, Missing | Malformed)
                            && !kind.published()
                            && !kind.owner(caller)
                            && !kind.manager(caller)
                    }
                    Op::ReadItem => {
                        !matches!(
                            kind,
                            Missing | Malformed | Published | Approved | Transferred
                        ) && !kind.item_owner(caller)
                    }
                    Op::ListCollections | Op::ListItems | Op::Committee => true,
                    _ => false,
                };
                let expect = |f: &Fixture| {
                    let normal = op.expect(kind, caller, f)?;
                    if (decides && caller != Anonymous) || op == Op::Committee {
                        Some(committee_unavailable.clone())
                    } else {
                        Some(normal)
                    }
                };
                if let Some(failure) = cell(&m, op, kind, caller, "", false, expect).await {
                    failures.push(failure);
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failing cells:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A save that would change a collection's kind or third party is a
/// conflict only for a signer who may save it; anyone else is refused
/// before the body's URN is compared, and nothing is written.
#[tokio::test]
async fn a_kind_conflict_is_answered_only_to_a_signer_with_access() {
    let committee = FakeCommittee::with(&[&Committee.address()]);
    let Some(m) = matrix(&committee).await else {
        return;
    };
    for (kind, urn, expected) in [
        (
            Draft,
            format!("{PROVIDER}:converted"),
            "The collection can't be converted into a third party collection.",
        ),
        (
            ThirdParty,
            "urn:decentraland:matic:collections-thirdparty:other:moved".to_string(),
            "The third party id cannot be changed",
        ),
    ] {
        let f = fixture(&m.pool, kind).await;
        let id = f.collection.unwrap();
        let body = json!({ "collection": { "id": id, "name": "Matrix renamed", "urn": urn } });
        let before = snapshot(&m.pool).await;
        for (caller, status, error) in [
            (Stranger, 401, "Unauthorized to upsert collection"),
            (Committee, 401, "Unauthorized to upsert collection"),
            (Owner, 409, expected),
        ] {
            let (got, answer) = call(
                &m.app,
                caller,
                "PUT",
                &format!("/v1/collections/{id}"),
                Payload::Json(body.clone()),
            )
            .await;
            assert_eq!(
                (got.as_u16(), answer["error"].as_str()),
                (status, Some(error)),
                "{kind:?} as {caller:?}: {answer}"
            );
            assert!(
                caller != Stranger || answer["data"].get("third_party_id").is_none(),
                "{answer}"
            );
        }
        assert_eq!(snapshot(&m.pool).await, before);
    }
}

/// Every move of an existing item between collections that involves a
/// third-party collection, or into or out of one, is refused with
/// builder-server's 401 before roles; moves among standard collections and
/// out of a standard collection follow the roles.
#[tokio::test]
async fn an_item_moves_into_or_out_of_a_third_party_collection_never() {
    let committee = FakeCommittee::with(&[&Committee.address()]);
    let Some(m) = matrix(&committee).await else {
        return;
    };
    let cant_move = "Item can't change between collections.";
    for (from, to, caller, expected) in [
        (Orphan, Some(ThirdParty), Owner, Some(cant_move)),
        (Orphan, Some(ThirdParty), Stranger, Some(cant_move)),
        (ThirdParty, None, Owner, Some(cant_move)),
        (ThirdParty, Some(ThirdParty), Owner, Some(cant_move)),
        (Draft, Some(ThirdParty), Owner, Some(cant_move)),
        (ThirdParty, Some(Draft), Owner, Some(cant_move)),
        (Orphan, Some(Draft), Owner, None),
        (Draft, None, Owner, None),
        (Draft, Some(Draft), Owner, None),
    ] {
        let source = fixture(&m.pool, from).await;
        let target = match to {
            Some(kind) => fixture(&m.pool, kind).await.collection,
            None => None,
        };
        let id = source.item.unwrap();
        let before = snapshot(&m.pool).await;
        let (status, body) = call(
            &m.app,
            caller,
            "PUT",
            &format!("/v1/items/{id}"),
            Payload::Json(item_json(&id.to_string(), target, None)),
        )
        .await;
        let label = format!("{from:?} to {to:?} as {caller:?}: {status} {body}");
        match expected {
            Some(error) => {
                assert_eq!(
                    (status.as_u16(), &body["error"], &body["data"]),
                    (401, &json!(error), &json!({ "id": id })),
                    "{label}"
                );
                assert_eq!(snapshot(&m.pool).await, before, "{label}");
            }
            None => assert_eq!(status, StatusCode::OK, "{label}"),
        }
    }
}

/// A read the committee decides releases its database connection before it
/// waits on the committee source, so a pool of one still answers the
/// owner's own read while a stranger's read waits on a slow subgraph.
#[tokio::test]
async fn a_committee_read_holds_no_database_connection() {
    for op in [Op::ReadCollection, Op::CollectionItems, Op::ReadItem] {
        let committee = FakeCommittee::with(&[&Committee.address()]);
        committee.slow(std::time::Duration::from_secs(3));
        let Some(m) = matrix_with(&committee, 1, true).await else {
            return;
        };
        let f = fixture(&m.pool, Draft).await;
        let (method, path, _) = op.request(Draft, &f);
        let app = m.app.clone();
        let stranger = {
            let path = path.clone();
            tokio::spawn(async move { call(&app, Stranger, method, &path, Payload::None).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let started = std::time::Instant::now();
        let (status, body) = call(&m.app, Owner, method, &path, Payload::None).await;
        let waited = started.elapsed();
        assert_eq!(status, StatusCode::OK, "{op:?}: {body}");
        assert!(
            waited < std::time::Duration::from_secs(1),
            "{op:?}: the owner's read waited {waited:?} behind a committee read"
        );
        assert_eq!(
            stranger.await.unwrap().0,
            StatusCode::UNAUTHORIZED,
            "{op:?}"
        );
    }
}

/// Two signers creating one new collection id at once: one creates it, the
/// other is refused as for an id that exists and isn't theirs.
#[tokio::test]
async fn the_loser_of_a_concurrent_creation_is_refused_as_a_stranger() {
    let committee = FakeCommittee::with(&[&Committee.address()]);
    let Some(m) = matrix(&committee).await else {
        return;
    };
    let id = Uuid::new_v4();
    let mut winner = m.pool.begin().await.unwrap();
    let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *winner)
        .await
        .unwrap();
    sqlx::query("INSERT INTO builder.collections (id, name, eth_address) VALUES ($1, 'First', $2)")
        .bind(id)
        .bind(Owner.address())
        .execute(&mut *winner)
        .await
        .unwrap();
    let app = m.app.clone();
    let loser = tokio::spawn(async move {
        call(
            &app,
            Stranger,
            "PUT",
            &format!("/v1/collections/{id}"),
            Payload::Json(json!({ "collection": { "id": id, "name": "Second" } })),
        )
        .await
    });
    let queued = crate::shared::test_wait::until(async || {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))",
        )
        .bind(blocker)
        .fetch_one(&m.pool)
        .await
        .unwrap()
    })
    .await;
    assert!(queued, "the second creation never waited on the first");
    winner.commit().await.unwrap();
    let (status, body) = loser.await.unwrap();
    assert_eq!(
        (status.as_u16(), &body["error"], &body["data"]),
        (
            401,
            &json!("Unauthorized to upsert collection"),
            &json!({ "id": id, "eth_address": Stranger.address() })
        ),
        "{body}"
    );
    let name: String = sqlx::query_scalar("SELECT name FROM builder.collections WHERE id = $1")
        .bind(id)
        .fetch_one(&m.pool)
        .await
        .unwrap();
    assert_eq!(name, "First");
}

/// What any signer sees of published content never waits on the committee
/// source: a stranger's, the owner's and a committee member's reads of a
/// published collection's items and of a minted item answer at once while the
/// source is slow, at cold start, at expiry and while it fails. A committee
/// member gets the wide view only from a good read, kept while its refresh
/// runs.
#[tokio::test]
async fn published_content_never_waits_on_the_committee_source() {
    let committee = FakeCommittee::with(&[&Committee.address()]);
    committee.slow(std::time::Duration::from_secs(2));
    let Some(m) = matrix(&committee).await else {
        return;
    };
    let f = fixture(&m.pool, Published).await;
    let collection = f.collection.unwrap();
    let minted = f.minted.unwrap();
    let check = async |caller: Caller, wide: bool, moment: &str| {
        for path in [
            format!("/v1/collections/{collection}/items"),
            format!("/v1/items/{minted}"),
        ] {
            let started = std::time::Instant::now();
            let (status, body) = call(&m.app, caller, "GET", &path, Payload::None).await;
            let waited = started.elapsed();
            assert_eq!(
                status,
                StatusCode::OK,
                "{moment} {path} as {caller:?}: {body}"
            );
            assert!(
                waited < std::time::Duration::from_millis(500),
                "{moment} {path} as {caller:?} waited {waited:?}"
            );
            let items: Vec<&Value> = match body["data"].as_array() {
                Some(list) => list.iter().collect(),
                None => vec![&body["data"]],
            };
            let full = items.iter().all(|i| i.get("local_content_hash").is_some());
            let listed = body["data"].as_array().map(Vec::len);
            assert_eq!(
                (full, listed.is_none_or(|n| n == if wide { 2 } else { 1 })),
                (wide, true),
                "{moment} {path} as {caller:?}: {body}"
            );
        }
    };
    check(Stranger, false, "cold start").await;
    check(Owner, true, "cold start").await;
    check(Committee, false, "cold start, no good read").await;
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    check(Committee, true, "after a good read").await;
    check(Stranger, false, "after a good read").await;
    m.state
        .committee
        .backdate(crate::committee::MEMBERS_TTL + std::time::Duration::from_secs(1));
    check(Committee, true, "expired, refresh in flight").await;
    check(Stranger, false, "expired, refresh in flight").await;
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    committee.set(None);
    m.state
        .committee
        .backdate(crate::committee::MEMBERS_TTL + std::time::Duration::from_secs(1));
    check(Committee, true, "expired, failing refresh in flight").await;
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    check(Committee, false, "the source failing").await;
    check(Stranger, false, "the source failing").await;
    check(Owner, true, "the source failing").await;
}

/// A committee member removed while the source fails keeps the wide view of
/// published content only during the one refresh that follows the expiry of
/// the builder's last good read, never in a later round.
#[tokio::test]
async fn an_expired_committee_read_never_widens_after_its_refresh_fails() {
    let committee = FakeCommittee::with(&[&Committee.address()]);
    let Some(m) = matrix(&committee).await else {
        return;
    };
    let f = fixture(&m.pool, Published).await;
    let path = format!("/v1/collections/{}/items", f.collection.unwrap());
    let listed = async || {
        let (status, body) = call(&m.app, Committee, "GET", &path, Payload::None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["data"].as_array().unwrap().len()
    };
    assert!(
        m.state
            .committee
            .is_member(m.state.market.as_ref(), &Committee.address())
            .await
            .unwrap()
    );
    assert_eq!(listed().await, 2, "a fresh read widens the view");
    committee.set(None);
    committee.slow(std::time::Duration::from_millis(300));
    m.state
        .committee
        .backdate(std::time::Duration::from_secs(7 * 24 * 60 * 60));
    listed().await;
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    for round in 1..4 {
        assert_eq!(listed().await, 1, "round {round}: after a failed refresh");
        m.state
            .committee
            .backdate(crate::committee::FAILURE_TTL + std::time::Duration::from_secs(1));
        assert_eq!(
            listed().await,
            1,
            "round {round}: while the next refresh runs"
        );
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }
}

#[tokio::test]
async fn a_sibling_collection_of_the_provider_grants_nothing_over_the_others() {
    let committee = FakeCommittee::with(&[&Committee.address()]);
    let Some(m) = matrix(&committee).await else {
        return;
    };
    let f = fixture(&m.pool, ThirdParty).await;
    let (status, body) = call(
        &m.app,
        Sibling,
        "GET",
        &format!("/v1/{}/items", Sibling.address()),
        Payload::None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert_eq!(listed.len(), 1, "only the sibling's own item: {body}");
    assert!(
        !listed.contains(&f.item.unwrap().to_string().as_str()),
        "the sibling lists an item of the owner's collection: {body}"
    );
    for (method, path, payload) in [
        Op::ReadItem.request(ThirdParty, &f),
        Op::CollectionItems.request(ThirdParty, &f),
        Op::SaveItem.request(ThirdParty, &f),
        Op::UploadFiles.request(ThirdParty, &f),
        Op::DeleteItem.request(ThirdParty, &f),
    ] {
        let before = snapshot(&m.pool).await;
        let (status, body) = call(&m.app, Sibling, method, &path, payload).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}: {body}");
        assert_eq!(
            snapshot(&m.pool).await,
            before,
            "{method} {path} changed rows"
        );
    }
}

/// The small public reads read no signature, no committee and, but for
/// `addresses`, no chain copy: they answer as in the matrix with the copy
/// stale and with the committee source down; without a complete pass only
/// `addresses` answers 503, and with the rarities source down only the
/// rarities do.
#[tokio::test]
async fn the_small_reads_cannot_tell_only_without_their_own_source() {
    let small_reads: Vec<Op> = ops().into_iter().filter(|op| op.small_read()).collect();
    assert_eq!(small_reads.len(), 6);
    for condition in ["stale", "no pass", "committee down", "rarities down"] {
        let committee = if condition == "committee down" {
            FakeCommittee::down()
        } else {
            FakeCommittee::with(&[&Committee.address()])
        };
        let Some(m) = matrix(&committee).await else {
            return;
        };
        match condition {
            "stale" => fresh_pass(&m.pool, 31).await,
            "no pass" => {
                sqlx::query("DELETE FROM shop.mirror_state")
                    .execute(&m.pool)
                    .await
                    .unwrap();
            }
            "rarities down" => committee.set_rarities(None),
            _ => {}
        }
        let mut failures = Vec::new();
        for &op in &small_reads {
            for &kind in op.kinds() {
                for caller in CALLERS {
                    let expect = |f: &Fixture| match (condition, op) {
                        ("no pass", Op::Addresses) => Some(Expect(
                            503,
                            "Chain state is not available yet. Try again later.".to_string(),
                        )),
                        ("rarities down", Op::Rarities | Op::Rarity) => {
                            refused(503, RARITIES_UNAVAILABLE)
                        }
                        _ => op.expect(kind, caller, f),
                    };
                    for mount in MOUNTS {
                        if let Some(failure) = cell(
                            &m,
                            op,
                            kind,
                            caller,
                            mount,
                            condition != "committee down",
                            expect,
                        )
                        .await
                        {
                            failures.push(format!("{condition}: {failure}"));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

/// Without a Polygon verifier, the publication routes that need one answer
/// 503 to the collection's owner, and every other caller gets the refusal
/// the matrix gives it, ahead of the 503.
#[tokio::test]
async fn without_a_polygon_verifier_publication_answers_503_after_the_refusals() {
    let committee = FakeCommittee::with(&[&Committee.address()]);
    let Some(m) = matrix_with(&committee, 8, false).await else {
        return;
    };
    let mut failures = Vec::new();
    for op in [
        Op::PublicationBegin,
        Op::PublicationTransaction,
        Op::PublicationClaim,
    ] {
        for kind in [Missing, Draft] {
            for caller in CALLERS {
                let expect = |f: &Fixture| match (kind, caller) {
                    (Draft, Owner) => refused(
                        503,
                        "Collection publication is unavailable until the Polygon verifier is configured",
                    ),
                    _ => op.expect(kind, caller, f),
                };
                for mount in MOUNTS {
                    if let Some(failure) = cell(&m, op, kind, caller, mount, true, expect).await {
                        failures.push(failure);
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

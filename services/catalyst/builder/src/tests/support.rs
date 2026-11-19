// from: tests/support/db.rs (the scratch database), catalyrst/crates/catalyrst-builder/tests/*.rs (each old file built its own pool, store, auth chain and cleanup; shared here)
//! Shared fixtures of the builder's copied contract tests: the process's
//! scratch database (`db::scratch`, skipped without
//! `CATALYST_TEST_DATABASE_URL`) with a throwaway blob store, an
//! explorer-style auth chain minted from a known key, and the row cleanup
//! every DB-backed test runs at its end (rows are uuid-scoped, the scratch
//! database is shared with the other lib tests).

use std::collections::BTreeMap;
use std::sync::Arc;

use alloy::signers::{Signer as _, local::PrivateKeySigner};
use uuid::Uuid;

use crate::items::ItemsComponent;
use crate::shared::signed_fetch::{
    AUTH_CHAIN_HEADER_PREFIX, AUTH_METADATA_HEADER, AUTH_TIMESTAMP_HEADER, build_legacy_payload,
    build_payload_v6,
};
use crate::storage::Storage;

/// `0x19e7e376e7c213b7e7e7e46cc70a5dd086daff2a`, the wallet of the old
/// fixtures' key.
pub const OWNER_KEY: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

pub async fn temp_storage() -> Arc<Storage> {
    let dir = std::env::temp_dir().join(format!("catalyst-builder-test-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    Arc::new(Storage::from_config(&dir).await.unwrap())
}

/// The process's scratch database with a fresh complete pass of the
/// Polygon collections copy, so derived chain state answers; `None` (skip)
/// without `CATALYST_TEST_DATABASE_URL`.
pub async fn scratch_pool(max_connections: u32) -> Option<sqlx::PgPool> {
    let pool = crate::db::scratch::pool(max_connections).await?;
    sqlx::query(
        "INSERT INTO shop.mirror_state (mirror, network, source_url, last_complete_pass_at, pass_started_at) \
         VALUES ('collections', 'matic', 'https://subgraph.example', timezone('UTC', now()), timezone('UTC', now())) \
         ON CONFLICT (mirror, network) DO UPDATE SET last_complete_pass_at = EXCLUDED.last_complete_pass_at, \
           pass_started_at = EXCLUDED.pass_started_at, cursor_id = NULL",
    )
    .execute(&pool)
    .await
    .unwrap();
    Some(pool)
}

/// Puts collection `id` on chain the way the shop's copy shows it: a
/// contract address on the builder row and its Polygon row in
/// `shop.collections`, created by the row's owner. Returns the contract for
/// [`off_chain`].
pub async fn on_chain(pool: &sqlx::PgPool, id: Uuid, approved: bool) -> String {
    let contract = format!("0x{}", Uuid::new_v4().simple().to_string()[..20].repeat(2));
    sqlx::query("UPDATE builder.collections SET contract_address = $2 WHERE id = $1")
        .bind(id)
        .bind(&contract)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO shop.collections (network, contract_address, chain_id, name, creator, owner, urn, \
           is_completed, is_approved, is_editable, created_at, updated_at, reviewed_at, mirrored_at) \
         SELECT 'matic', $1, 137, 'On chain', eth_address, eth_address, 'urn:c', true, $2, true, 1, 1, 1, timezone('UTC', now()) \
         FROM builder.collections WHERE id = $3",
    )
    .bind(&contract)
    .bind(approved)
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
    contract
}

/// A client-shaped `PUT /v1/collections/:id` body for `signer`: the
/// schema's required fields around `collection`, and `initialize` calldata
/// naming `signer` unless `collection` brings its own `data`.
pub fn collection_body(collection: serde_json::Value, signer: &str) -> serde_json::Value {
    let mut fields =
        serde_json::json!({ "eth_address": signer, "salt": null, "contract_address": null });
    let mut collection = collection.as_object().cloned().unwrap_or_default();
    let data = collection
        .remove("data")
        .unwrap_or_else(|| crate::collections::contract::initialize_calldata(signer).into());
    fields.as_object_mut().unwrap().extend(collection);
    serde_json::json!({ "collection": fields, "data": data })
}

/// Saves `collection` (its `id`, `name` and, for a third-party one, `urn`)
/// as `PUT /v1/collections/:id` saves it for `signer`, with `initialize`
/// calldata naming `signer` for a standard one, under a test grant.
pub async fn save_collection(
    store: &ItemsComponent,
    signer: &str,
    collection: serde_json::Value,
) -> Result<serde_json::Value, crate::http::ApiError> {
    use crate::collections::save;
    let request = save::parse(&collection_body(collection, signer))?;
    let deployment = match request.urn.as_deref().and_then(save::third_party_urn) {
        Some(_) => None,
        None => Some(save::standard_deployment(&request, signer)?),
    };
    let id = Uuid::parse_str(&request.id).unwrap();
    store
        .save_collection(
            &crate::access::granted::edit(signer, id),
            &request,
            deployment,
        )
        .await
}

/// Undoes [`on_chain`].
pub async fn off_chain(pool: &sqlx::PgPool, contract: &str) {
    sqlx::query("DELETE FROM shop.collections WHERE network = 'matic' AND contract_address = $1")
        .bind(contract)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE builder.collections SET contract_address = NULL WHERE contract_address = $1",
    )
    .bind(contract)
    .execute(pool)
    .await
    .unwrap();
}

/// `None` (skip) without `CATALYST_TEST_DATABASE_URL`.
pub async fn scratch_component() -> Option<ItemsComponent> {
    Some(ItemsComponent::new(
        scratch_pool(6).await?,
        temp_storage().await,
    ))
}

/// Deletes the test's collections (items, contents and publications cascade)
/// and the uploads it stored.
pub async fn cleanup(store: &ItemsComponent, collections: &[Uuid], hashes: &[String]) {
    sqlx::query("DELETE FROM builder.collections WHERE id = ANY($1)")
        .bind(collections)
        .execute(store.pool())
        .await
        .unwrap();
    if !hashes.is_empty() {
        sqlx::query("DELETE FROM builder.item_contents WHERE hash = ANY($1)")
            .bind(hashes)
            .execute(store.pool())
            .await
            .unwrap();
        sqlx::query("DELETE FROM builder.uploaded_contents WHERE hash = ANY($1)")
            .bind(hashes)
            .execute(store.pool())
            .await
            .unwrap();
    }
}

/// Bytes no other test hashes to: the old fixtures reused `b"model"`, which
/// two tests sharing one database would race on through `uploaded_contents`.
pub fn unique(label: &str) -> Vec<u8> {
    format!("{label}-{}", Uuid::new_v4()).into_bytes()
}

pub fn wallet(key: &str) -> PrivateKeySigner {
    key.parse().unwrap()
}

/// The two-link chain (SIGNER + ECDSA_SIGNED_ENTITY) an explorer signs over
/// `method:path:timestamp:metadata`, in the 6.x or the legacy payload shape,
/// as a header map.
pub async fn signed_map(
    key: &str,
    method: &str,
    path: &str,
    metadata: &str,
    legacy: bool,
) -> BTreeMap<String, String> {
    let timestamp = chrono::Utc::now().timestamp_millis().to_string();
    let payload = if legacy {
        build_legacy_payload(method, path, &timestamp, metadata)
    } else {
        build_payload_v6(method, path, &timestamp, metadata)
    };
    let wallet = wallet(key);
    let signature = wallet
        .sign_message(payload.as_bytes())
        .await
        .unwrap()
        .to_string();
    let mut map = BTreeMap::from([
        (AUTH_TIMESTAMP_HEADER.to_string(), timestamp),
        (AUTH_METADATA_HEADER.to_string(), metadata.to_string()),
    ]);
    map.insert(
        format!("{AUTH_CHAIN_HEADER_PREFIX}0"),
        serde_json::json!({ "type": "SIGNER", "payload": format!("{:#x}", wallet.address()), "signature": "" }).to_string(),
    );
    map.insert(
        format!("{AUTH_CHAIN_HEADER_PREFIX}1"),
        serde_json::json!({ "type": "ECDSA_SIGNED_ENTITY", "payload": payload, "signature": signature }).to_string(),
    );
    map
}

fn lazy_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://nobody:nothing@127.0.0.1:9/none")
        .expect("lazy pool from static url")
}

async fn state_over(
    pool: sqlx::PgPool,
    market: Option<Arc<crate::chain_index::ChainIndex>>,
) -> crate::SharedState {
    state_over_with(pool, market, crate::Settings::default()).await
}

async fn state_over_with(
    pool: sqlx::PgPool,
    market: Option<Arc<crate::chain_index::ChainIndex>>,
    settings: crate::Settings,
) -> crate::SharedState {
    Arc::new(
        crate::State::new(
            pool,
            temp_storage().await,
            Arc::default(),
            dcl_http::Outbound::for_tests(),
            market,
            None,
            settings,
        )
        .unwrap(),
    )
}

/// [`state_without_db`] with `settings`.
pub async fn state_with_settings(settings: crate::Settings) -> crate::SharedState {
    state_over_with(lazy_pool(), None, settings).await
}

/// A builder state over a pool that never connects: for the handler tests
/// that answer before touching the database.
pub async fn state_without_db(
    market: Option<Arc<crate::chain_index::ChainIndex>>,
) -> crate::SharedState {
    state_over(lazy_pool(), market).await
}

/// A loopback Polygon collections subgraph that answers the committee
/// accounts it holds (`None` answers a GraphQL error) and counts its reads.
#[derive(Clone, Default)]
pub struct FakeCommittee {
    members: Arc<std::sync::Mutex<Option<Vec<String>>>>,
    hits: Arc<std::sync::atomic::AtomicUsize>,
    delay: Arc<std::sync::Mutex<std::time::Duration>>,
}

impl FakeCommittee {
    pub fn with(members: &[&str]) -> Self {
        let fake = Self::default();
        fake.set(Some(members));
        fake
    }

    pub fn down() -> Self {
        Self::default()
    }

    pub fn set(&self, members: Option<&[&str]>) {
        *self.members.lock().unwrap() = members.map(|m| m.iter().map(|a| a.to_string()).collect());
    }

    /// Answers each read after `delay`.
    pub fn slow(&self, delay: std::time::Duration) {
        *self.delay.lock().unwrap() = delay;
    }

    pub fn hits(&self) -> usize {
        self.hits.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn answer(
        axum::extract::State(fake): axum::extract::State<Self>,
        axum::Json(body): axum::Json<serde_json::Value>,
    ) -> axum::Json<serde_json::Value> {
        fake.hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let delay = *fake.delay.lock().unwrap();
        tokio::time::sleep(delay).await;
        let Some(mut members) = fake.members.lock().unwrap().clone() else {
            return axum::Json(serde_json::json!({ "errors": [{ "message": "indexing error" }] }));
        };
        members.sort_by_key(|a| a.to_ascii_lowercase());
        let from = body["variables"]["idFrom"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let first = body["variables"]["first"].as_u64().unwrap_or(1000) as usize;
        let accounts: Vec<_> = members
            .iter()
            .filter(|a| a.to_ascii_lowercase() > from)
            .take(first)
            .map(|a| serde_json::json!({ "id": a.to_ascii_lowercase(), "address": a }))
            .collect();
        axum::Json(serde_json::json!({ "data": { "accounts": accounts } }))
    }
}

/// A chain index whose Polygon collections subgraph is `fake`.
pub async fn committee_index(fake: &FakeCommittee) -> Arc<crate::chain_index::ChainIndex> {
    let app = axum::Router::new()
        .route("/subgraph", axum::routing::post(FakeCommittee::answer))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let index = market_without_db(crate::chain_index::IndexSettings {
        matic_collections_subgraph_url: Some(format!("http://{addr}/subgraph")),
        ..crate::chain_index::IndexSettings::for_tests()
    });
    index.allow_loopback_for_tests();
    index
}

/// A chain index with `settings` (no subgraph configured by default).
pub fn market_without_db(
    settings: crate::chain_index::IndexSettings,
) -> Arc<crate::chain_index::ChainIndex> {
    Arc::new(crate::chain_index::ChainIndex::new(
        dcl_http::Outbound::for_tests(),
        settings,
    ))
}

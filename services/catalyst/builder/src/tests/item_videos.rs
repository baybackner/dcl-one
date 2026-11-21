//! `POST /v1/items/:id/videos` as a creator, a stranger and an attacker meet
//! it, through the router `serve` mounts, with real signatures, the scratch
//! database and a blob store in a temporary directory: who may make the node
//! read a body, what is kept, what is refused before or while the body
//! streams, and how a video then reads back.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, header};
use futures::StreamExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use super::support::{
    FakeCommittee, OWNER_KEY, committee_index, on_chain, save_collection, saved_item, signed_map,
    temp_storage, unique, wallet,
};
use crate::items::video::{self, VIDEO_FILE};
use crate::storage::Storage;

const STRANGER_KEY: &str = "0x8101010101010101010101010101010101010101010101010101010101010101";
const COMMITTEE_KEY: &str = "0x8202020202020202020202020202020202020202020202020202020202020202";
const MANAGER_KEY: &str = "0x8303030303030303030303030303030303030303030303030303030303030303";
const OTHER_KEY: &str = "0x8404040404040404040404040404040404040404040404040404040404040404";
const BOUNDARY: &str = "videoboundary";

fn address(key: &str) -> String {
    format!("{:#x}", wallet(key).address())
}

/// A playable MP4 container's top-level boxes around `label`: `ftyp`, a
/// `moov` and an `mdat` carrying the label, so every video hashes apart.
fn mp4(label: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut boxed = |kind: &[u8; 4], payload: &[u8]| {
        bytes.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
    };
    boxed(b"ftyp", b"isom\0\0\x02\0isomiso2mp41");
    boxed(b"moov", b"\0\0\0\x08mvhd");
    boxed(b"mdat", &unique(label));
    bytes
}

/// An MP4 of exactly `len` bytes: [`mp4`]'s boxes with the `mdat` padded.
fn mp4_of(len: usize) -> Vec<u8> {
    let head = mp4("");
    let mut bytes = head[..head.len() - unique("").len() - 8].to_vec();
    let rest = len - bytes.len();
    bytes.extend_from_slice(&(rest as u32).to_be_bytes());
    bytes.extend_from_slice(b"mdat");
    bytes.resize(len, 7);
    bytes
}

fn hash(bytes: &[u8]) -> String {
    dcl_auth::hash_bytes_v1(bytes)
}

/// A multipart form of `parts`, each `(field, filename, bytes)`.
fn form(parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, filename, bytes) in parts {
        body.extend_from_slice(
            format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"").as_bytes(),
        );
        if let Some(filename) = filename {
            body.extend_from_slice(
                format!("; filename=\"{filename}\"\r\nContent-Type: video/mp4").as_bytes(),
            );
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

fn video_form(bytes: &[u8]) -> Vec<u8> {
    form(&[(VIDEO_FILE, Some("video.mp4"), bytes)])
}

/// A body that counts every byte the node pulls from it: `head`, then
/// `filler` bytes of 7 made as they are pulled, then `tail`; `stall` pends
/// forever after `head`.
fn counted(head: Vec<u8>, filler: u64, tail: Vec<u8>, stall: bool) -> (Body, Arc<AtomicU64>) {
    let pulled = Arc::new(AtomicU64::new(0));
    let chunk = 64 * 1024u64;
    let fill = futures::stream::iter(
        (0..filler.div_ceil(chunk))
            .map(move |i| Bytes::from(vec![7u8; chunk.min(filler - i * chunk) as usize])),
    );
    let stream = futures::stream::iter([Bytes::from(head)])
        .chain(fill)
        .chain(futures::stream::iter([Bytes::from(tail)]))
        .chain(futures::stream::pending().take(usize::from(stall)))
        .map({
            let pulled = pulled.clone();
            move |chunk: Bytes| {
                pulled.fetch_add(chunk.len() as u64, Ordering::SeqCst);
                Ok::<_, std::io::Error>(chunk)
            }
        });
    (Body::from_stream(stream), pulled)
}

/// A body that records being read and never yields a byte.
fn untouchable() -> (Body, Arc<AtomicU64>) {
    let polled = Arc::new(AtomicU64::new(0));
    let stream = futures::stream::poll_fn({
        let polled = polled.clone();
        move |_| {
            polled.fetch_add(1, Ordering::SeqCst);
            std::task::Poll::<Option<Result<Bytes, std::io::Error>>>::Pending
        }
    });
    (Body::from_stream(stream), polled)
}

/// Waits until `ready` holds, failing the test after 30 seconds.
async fn until(what: &str, ready: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} never happened"));
}

struct Node {
    pool: PgPool,
    app: Router,
    state: crate::SharedState,
    storage: Arc<Storage>,
}

async fn node_with(configure: impl FnOnce(&mut crate::State), writes: bool) -> Option<Node> {
    node_over(8, configure, writes).await
}

async fn node_over(
    connections: u32,
    configure: impl FnOnce(&mut crate::State),
    writes: bool,
) -> Option<Node> {
    let pool = super::support::scratch_pool(connections).await?;
    let committee = FakeCommittee::with(&[&address(COMMITTEE_KEY)]);
    committee.publish(&pool).await;
    let storage = temp_storage().await;
    let mut state = crate::State::new(
        pool.clone(),
        storage.clone(),
        Arc::default(),
        dcl_http::Outbound::for_tests(),
        Some(committee_index(&committee).await),
        None,
        crate::Settings {
            writes_enabled: writes,
            ..crate::Settings::default()
        },
    )
    .unwrap();
    configure(&mut state);
    let state = Arc::new(state);
    let app = Router::new()
        .merge(crate::root_router(state.clone()))
        .nest("/builder-api", crate::router(state.clone()));
    Some(Node {
        pool,
        app,
        state,
        storage: storage.builder().unwrap().clone(),
    })
}

async fn node() -> Option<Node> {
    node_with(|_| {}, true).await
}

/// `body` as a multipart upload of `path`, signed by `key` unless `None`,
/// from the source address `ip` if any.
async fn post(key: Option<&str>, path: &str, ip: Option<&str>, body: Body) -> Request<Body> {
    let mut request = Request::builder().method("POST").uri(path).header(
        header::CONTENT_TYPE,
        format!("multipart/form-data; boundary={BOUNDARY}"),
    );
    if let Some(key) = key {
        for (name, value) in signed_map(key, "post", path, "{}", false).await {
            request = request.header(name, value);
        }
    }
    if let Some(ip) = ip {
        request = request.header("x-real-ip", ip);
    }
    request.body(body).unwrap()
}

/// The status, JSON body and `Retry-After` of `response`.
async fn answer(response: axum::response::Response) -> (u16, Value, Option<String>) {
    let status = response.status().as_u16();
    let retry = response
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        retry,
    )
}

/// [`post`] through the router, failing the test when the node takes a
/// minute to answer.
async fn send_from(
    app: &Router,
    key: Option<&str>,
    path: &str,
    ip: Option<&str>,
    body: Body,
) -> (u16, Value, Option<String>) {
    let request = post(key, path, ip, body).await;
    let response = tokio::time::timeout(Duration::from_secs(60), app.clone().oneshot(request))
        .await
        .expect("the node answers within a minute")
        .unwrap();
    answer(response).await
}

async fn send(app: &Router, key: Option<&str>, path: &str, body: Body) -> (u16, Value) {
    let (status, body, _) = send_from(app, key, path, None, body).await;
    (status, body)
}

/// [`post`] through the router with no bound of its own, for a test that
/// moves the clock.
async fn send_unbounded(app: &Router, key: &str, path: &str, body: Body) -> (u16, Value) {
    let response = app
        .clone()
        .oneshot(post(Some(key), path, None, body).await)
        .await
        .unwrap();
    let (status, body, _) = answer(response).await;
    (status, body)
}

async fn upload(node: &Node, key: Option<&str>, item: Uuid, body: Vec<u8>) -> (u16, Value) {
    send(
        &node.app,
        key,
        &format!("/v1/items/{item}/videos"),
        Body::from(body),
    )
    .await
}

/// Reads `hash` from the storage route, signed by `key` unless `None`, with
/// `range` if any: status, content type, cache control and body.
async fn read(
    node: &Node,
    key: Option<&str>,
    hash: &str,
    range: Option<&str>,
) -> (u16, String, String, Vec<u8>) {
    let path = format!("/v1/storage/contents/{hash}");
    let mut request = Request::builder().uri(&path);
    if let Some(key) = key {
        for (name, value) in signed_map(key, "get", &path, "{}", false).await {
            request = request.header(name, value);
        }
    }
    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }
    let response = node
        .app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let text = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let (status, kind, cache) = (
        response.status().as_u16(),
        text(header::CONTENT_TYPE),
        text(header::CACHE_CONTROL),
    );
    if response.status().is_success() {
        assert_eq!(
            (
                response.headers()[header::X_CONTENT_TYPE_OPTIONS]
                    .to_str()
                    .unwrap(),
                response.headers()[header::CONTENT_SECURITY_POLICY]
                    .to_str()
                    .unwrap(),
            ),
            ("nosniff", "sandbox"),
            "a served file is never sniffed or run as a page"
        );
    }
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .unwrap();
    (status, kind, cache, bytes.to_vec())
}

/// A standard draft collection of the owner's with a smart wearable naming
/// `video` as its preview, as the `/create` dApp saves one.
async fn smart_wearable(node: &Node, video: &[u8]) -> (Uuid, Uuid) {
    let collection = Uuid::new_v4();
    save_collection(
        &node.state.items,
        &address(OWNER_KEY),
        json!({ "id": collection, "name": format!("Videos {}", &collection.simple().to_string()[..12]) }),
    )
    .await
    .unwrap();
    let item = Uuid::new_v4();
    name_video(node, collection, item, video).await;
    (collection, item)
}

/// [`smart_wearable`] owned by the wallet of `key`.
async fn smart_wearable_of(node: &Node, key: &str, video: &[u8]) -> Uuid {
    let (collection, item) = smart_wearable(node, video).await;
    sqlx::query("UPDATE builder.collections SET eth_address = $2 WHERE id = $1")
        .bind(collection)
        .bind(address(key))
        .execute(&node.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE builder.items SET eth_address = $2 WHERE id = $1")
        .bind(item)
        .bind(address(key))
        .execute(&node.pool)
        .await
        .unwrap();
    collection
}

/// Saves item `item` of `collection` naming `video` at `video.mp4`.
async fn name_video(node: &Node, collection: Uuid, item: Uuid, video: &[u8]) {
    let model = unique("model");
    saved_item(
        &node.state.items,
        &address(OWNER_KEY),
        &json!({
            "id": item, "name": "Smart hat", "collection_id": collection, "rarity": "common",
            "video": hash(video),
            "contents": { "model.glb": hash(&model), VIDEO_FILE: hash(video) },
        }),
    )
    .await;
}

async fn records(pool: &PgPool, hash: &str) -> Vec<(i64, String)> {
    sqlx::query_as("SELECT size, uploaded_by FROM builder.uploaded_contents WHERE hash = $1")
        .bind(hash)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn stored(node: &Node, hash: &str) -> bool {
    node.storage.exist(hash).await.unwrap()
}

fn spool_entries(node: &Node) -> usize {
    std::fs::read_dir(video::spool_root(&node.storage)).map_or(0, |dir| dir.count())
}

async fn referenced(pool: &PgPool, hash: &str) -> bool {
    let mut conn = pool.acquire().await.unwrap();
    crate::storage::gc::referenced_rows_among_on(&mut conn, &[hash.to_string()])
        .await
        .unwrap()
        .contains(hash)
}

async fn cleanup(pool: &PgPool, collections: &[Uuid], hashes: &[String]) {
    sqlx::query("DELETE FROM builder.collections WHERE id = ANY($1)")
        .bind(collections)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM builder.uploaded_contents WHERE hash = ANY($1)")
        .bind(hashes)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_creator_uploads_the_video_their_smart_wearable_names_and_plays_it_back_signed() {
    let Some(node) = node().await else { return };
    let video = mp4("creator");
    let (collection, item) = smart_wearable(&node, &video).await;
    for mount in ["", "/builder-api"] {
        let body = form(&[(
            VIDEO_FILE,
            Some("../../etc/passwd%0D%0AX-Injected: 1<script>.html"),
            &video,
        )]);
        let (status, answer) = send(
            &node.app,
            Some(OWNER_KEY),
            &format!("{mount}/v1/items/{item}/videos"),
            Body::from(body),
        )
        .await;
        assert_eq!((status, answer), (200, json!({ "ok": true })), "{mount}");
    }
    assert_eq!(
        records(&node.pool, &hash(&video)).await,
        vec![(video.len() as i64, address(OWNER_KEY))]
    );
    assert!(stored(&node, &hash(&video)).await);
    assert_eq!(spool_entries(&node), 0, "the spool keeps nothing");

    let (status, kind, cache, bytes) = read(&node, Some(OWNER_KEY), &hash(&video), None).await;
    assert_eq!(
        (status, kind.as_str(), cache.as_str()),
        (200, "video/mp4", "private, no-store")
    );
    assert_eq!(bytes, video);
    let (status, _, _, bytes) =
        read(&node, Some(OWNER_KEY), &hash(&video), Some("bytes=4-7")).await;
    assert_eq!((status, bytes.as_slice()), (206, b"ftyp".as_slice()));
    assert_eq!(read(&node, None, &hash(&video), None).await.0, 404);
    assert_eq!(
        read(&node, Some(STRANGER_KEY), &hash(&video), None).await.0,
        404
    );
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

#[tokio::test]
async fn a_forgotten_video_uploaded_again_while_the_collector_unlinks_it_keeps_its_bytes() {
    let Some(node) = node().await else { return };
    let video = mp4("forgotten then sent again");
    let (collection, item) = smart_wearable(&node, &video).await;
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&video))
            .await
            .0,
        200
    );
    sqlx::query("DELETE FROM builder.uploaded_contents WHERE hash = $1")
        .bind(hash(&video))
        .execute(&node.pool)
        .await
        .unwrap();

    let batch = crate::storage::gc::lock_store_exclusive(&node.pool)
        .await
        .unwrap();
    let again = tokio::spawn({
        let app = node.app.clone();
        let body = video_form(&video);
        async move {
            send(
                &app,
                Some(OWNER_KEY),
                &format!("/v1/items/{item}/videos"),
                Body::from(body),
            )
            .await
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !super::support::waiting_for_the_store(&node.pool).await {
        assert!(
            !again.is_finished(),
            "the upload committed while the collector's unlink batch held the store"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "the upload never asked for the store"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    node.storage.delete_strict(&hash(&video)).await.unwrap();
    batch.commit().await.unwrap();

    assert_eq!(again.await.unwrap().0, 200);
    assert_eq!(
        records(&node.pool, &hash(&video)).await,
        vec![(video.len() as i64, address(OWNER_KEY))]
    );
    assert!(stored(&node, &hash(&video)).await);
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

#[tokio::test]
async fn after_publication_and_approval_the_video_opens_unsigned_as_the_marketplace_reads_it() {
    let Some(node) = node().await else { return };
    let video = mp4("approved");
    let (collection, item) = smart_wearable(&node, &video).await;
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&video))
            .await
            .0,
        200
    );
    let contract = on_chain(&node.pool, collection, true).await;
    let (status, kind, cache, bytes) = read(&node, None, &hash(&video), None).await;
    assert_eq!(
        (status, kind.as_str(), cache.as_str()),
        (200, "video/mp4", "public,max-age=31536000,immutable")
    );
    assert_eq!(bytes, video);
    let (status, _, _, bytes) = read(&node, None, &hash(&video), Some("bytes=0-3")).await;
    assert_eq!((status, bytes.len()), (206, 4));
    super::support::off_chain(&node.pool, &contract).await;
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

#[tokio::test]
async fn nobody_without_a_right_to_the_item_makes_the_node_read_a_byte() {
    let Some(node) = node().await else { return };
    let video = mp4("refused");
    let (collection, item) = smart_wearable(&node, &video).await;
    let contract = on_chain(&node.pool, collection, false).await;
    sqlx::query("UPDATE shop.collections SET managers = $2 WHERE contract_address = $1")
        .bind(&contract)
        .bind(vec![address(OTHER_KEY)])
        .execute(&node.pool)
        .await
        .unwrap();
    let path = format!("/v1/items/{item}/videos");
    let validator = {
        let timestamp = chrono::Utc::now().timestamp_millis().to_string();
        let signature = crate::shared::validator_signature::sign(
            b"example-validator-secret-of-32-bytes",
            &timestamp,
            "POST",
            &path,
            b"",
        );
        vec![
            (
                crate::shared::validator_signature::SIGNATURE_HEADER,
                signature,
            ),
            (
                crate::shared::validator_signature::TIMESTAMP_HEADER,
                timestamp,
            ),
        ]
    };
    for (who, key, extra) in [
        ("another creator", Some(STRANGER_KEY), vec![]),
        ("a former manager", Some(MANAGER_KEY), vec![]),
        ("a committee member", Some(COMMITTEE_KEY), vec![]),
        ("an unsigned caller", None, vec![]),
        ("a validator-signed caller", None, validator),
    ] {
        let (body, polled) = untouchable();
        let mut request = Request::builder().method("POST").uri(&path).header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        );
        if let Some(key) = key {
            for (name, value) in signed_map(key, "post", &path, "{}", false).await {
                request = request.header(name, value);
            }
        }
        for (name, value) in extra {
            request = request.header(name, value);
        }
        let response = node
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 401, "{who}");
        assert_eq!(polled.load(Ordering::SeqCst), 0, "{who}: the body was read");
    }
    let (body, polled) = untouchable();
    assert_eq!(
        send(
            &node.app,
            Some(OWNER_KEY),
            &format!("/v1/items/{}/videos", Uuid::new_v4()),
            body
        )
        .await
        .0,
        404
    );
    assert_eq!(
        polled.load(Ordering::SeqCst),
        0,
        "an unknown item's body was read"
    );
    assert!(records(&node.pool, &hash(&video)).await.is_empty());
    assert!(!stored(&node, &hash(&video)).await);
    super::support::off_chain(&node.pool, &contract).await;
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

#[tokio::test]
async fn a_current_on_chain_manager_uploads_like_the_owner() {
    let Some(node) = node().await else { return };
    let video = mp4("manager");
    let (collection, item) = smart_wearable(&node, &video).await;
    let contract = on_chain(&node.pool, collection, false).await;
    sqlx::query("UPDATE shop.collections SET managers = $2 WHERE contract_address = $1")
        .bind(&contract)
        .bind(vec![address(MANAGER_KEY)])
        .execute(&node.pool)
        .await
        .unwrap();
    assert_eq!(
        upload(&node, Some(MANAGER_KEY), item, video_form(&video))
            .await
            .0,
        200
    );
    assert_eq!(
        records(&node.pool, &hash(&video)).await,
        vec![(video.len() as i64, address(MANAGER_KEY))]
    );
    super::support::off_chain(&node.pool, &contract).await;
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

#[tokio::test]
async fn a_video_at_the_cap_is_kept_and_one_byte_over_stops_there_leaving_nothing() {
    let Some(node) = node_with(|state| state.videos.max_bytes = 4096, true).await else {
        return;
    };
    let at_cap = mp4_of(4096);
    let over = mp4_of(4097);
    let (collection, item) = smart_wearable(&node, &over).await;
    let (status, answer) = upload(&node, Some(OWNER_KEY), item, video_form(&over)).await;
    assert_eq!(
        (status, answer),
        (
            413,
            json!({ "ok": false, "data": { "maxBytes": 4096 }, "error": "The video is too large" })
        )
    );
    assert!(!stored(&node, &hash(&over)).await);
    assert!(records(&node.pool, &hash(&over)).await.is_empty());
    assert_eq!(spool_entries(&node), 0);

    name_video(&node, collection, item, &at_cap).await;
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&at_cap))
            .await
            .0,
        200
    );
    assert_eq!(records(&node.pool, &hash(&at_cap)).await.len(), 1);
    cleanup(&node.pool, &[collection], &[hash(&at_cap), hash(&over)]).await;
}

#[tokio::test]
async fn the_default_cap_is_upstreams_250_mb_and_the_node_stops_reading_past_it() {
    let Some(node) = node().await else { return };
    assert_eq!(node.state.videos.max_bytes, 250_000_000);
    let video = mp4("default cap");
    let (collection, item) = smart_wearable(&node, &video).await;
    let head = form(&[(VIDEO_FILE, Some("video.mp4"), &mp4_of(64))]);
    let head = head[..head.len() - format!("\r\n--{BOUNDARY}--\r\n").len()].to_vec();
    let (body, pulled) = counted(
        head,
        250_000_000,
        b"\r\n--videoboundary--\r\n".to_vec(),
        false,
    );
    let (status, answer) = send(
        &node.app,
        Some(OWNER_KEY),
        &format!("/v1/items/{item}/videos"),
        body,
    )
    .await;
    assert_eq!(
        (status, &answer["data"]),
        (413, &json!({ "maxBytes": 250_000_000 }))
    );
    assert!(
        (250_000_000..250_000_064 + 256 * 1024).contains(&pulled.load(Ordering::SeqCst)),
        "read {} bytes",
        pulled.load(Ordering::SeqCst)
    );
    assert_eq!(spool_entries(&node), 0);
    cleanup(&node.pool, &[collection], &[]).await;
}

#[tokio::test]
async fn a_file_that_is_not_a_playable_mp4_is_refused_and_not_stored() {
    let Some(node) = node().await else { return };
    let page = b"<!DOCTYPE html><script>alert(document.cookie)</script>".to_vec();
    let mut truncated = mp4("truncated");
    truncated.truncate(truncated.len() - 3);
    let mut moovless = Vec::new();
    moovless.extend_from_slice(&mp4("moovless")[..24]);
    moovless.extend_from_slice(b"\0\0\0\x0cmdatdata");
    let (collection, item) = smart_wearable(&node, &page).await;
    for (what, bytes) in [
        ("a page", &page),
        ("a truncated container", &truncated),
        ("no moov", &moovless),
    ] {
        name_video(&node, collection, item, bytes).await;
        let (status, answer) = upload(&node, Some(OWNER_KEY), item, video_form(bytes)).await;
        assert_eq!(
            (status, answer),
            (
                400,
                json!({ "ok": false, "data": {}, "error": "The video is not an MP4 file" })
            ),
            "{what}"
        );
        assert!(!stored(&node, &hash(bytes)).await, "{what}");
        assert!(records(&node.pool, &hash(bytes)).await.is_empty(), "{what}");
    }
    name_video(&node, collection, item, &page).await;
    let (status, answer) = send(
        &node.app,
        Some(OWNER_KEY),
        &format!("/v1/items/{item}/files"),
        Body::from(form(&[(&hash(&page), Some("blob"), &page)])),
    )
    .await;
    assert_eq!(
        (status, answer),
        (
            400,
            json!({ "ok": false, "data": { "hash": hash(&page) }, "error": "The video is uploaded through the videos route" })
        ),
        "the item's video sent as a file"
    );
    assert!(!stored(&node, &hash(&page)).await);
    assert!(records(&node.pool, &hash(&page)).await.is_empty());
    let head = form(&[(VIDEO_FILE, Some("video.mp4"), &page)]);
    let head = head[..head.len() - format!("\r\n--{BOUNDARY}--\r\n").len()].to_vec();
    let (body, pulled) = counted(
        head,
        8 * 1024 * 1024,
        b"\r\n--videoboundary--\r\n".to_vec(),
        false,
    );
    let (status, _) = send(
        &node.app,
        Some(OWNER_KEY),
        &format!("/v1/items/{item}/videos"),
        body,
    )
    .await;
    assert_eq!(status, 400, "a page with a long tail");
    assert!(
        pulled.load(Ordering::SeqCst) < 1024 * 1024,
        "the node read {} bytes of a page",
        pulled.load(Ordering::SeqCst)
    );
    assert_eq!(spool_entries(&node), 0);
    cleanup(&node.pool, &[collection], &[]).await;
}

#[tokio::test]
async fn a_second_video_replaces_the_first_which_the_collector_may_then_take() {
    let Some(node) = node().await else { return };
    let (first, second) = (mp4("first"), mp4("second"));
    let (collection, item) = smart_wearable(&node, &first).await;
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&first))
            .await
            .0,
        200
    );
    assert!(referenced(&node.pool, &hash(&first)).await);
    name_video(&node, collection, item, &second).await;
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&second))
            .await
            .0,
        200
    );
    let named: (String, Option<String>) = sqlx::query_as(
        "SELECT c.hash, i.video FROM builder.item_contents c JOIN builder.items i ON i.id = c.item_id \
         WHERE c.item_id = $1 AND c.file = $2",
    )
    .bind(item)
    .bind(VIDEO_FILE)
    .fetch_one(&node.pool)
    .await
    .unwrap();
    assert_eq!(named, (hash(&second), Some(hash(&second))));
    assert!(referenced(&node.pool, &hash(&second)).await);
    assert!(
        !referenced(&node.pool, &hash(&first)).await,
        "the replaced video stays referenced"
    );
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&first))
            .await
            .0,
        400,
        "the item no longer names it"
    );
    cleanup(&node.pool, &[collection], &[hash(&first), hash(&second)]).await;
}

#[tokio::test]
async fn uploads_follow_the_item_and_the_switch_not_the_lock_or_publication() {
    let Some(node) = node().await else { return };
    let video = mp4("states");
    let (collection, item) = smart_wearable(&node, &video).await;
    sqlx::query("UPDATE builder.collections SET lock = timezone('UTC', now()) WHERE id = $1")
        .bind(collection)
        .execute(&node.pool)
        .await
        .unwrap();
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&video))
            .await
            .0,
        200,
        "locked"
    );
    let contract = on_chain(&node.pool, collection, true).await;
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&video))
            .await
            .0,
        200,
        "published"
    );
    super::support::off_chain(&node.pool, &contract).await;
    sqlx::query("DELETE FROM builder.items WHERE id = $1")
        .bind(item)
        .execute(&node.pool)
        .await
        .unwrap();
    let (body, polled) = untouchable();
    assert_eq!(
        send(
            &node.app,
            Some(OWNER_KEY),
            &format!("/v1/items/{item}/videos"),
            body
        )
        .await
        .0,
        404,
        "deleted"
    );
    assert_eq!(polled.load(Ordering::SeqCst), 0);

    let Some(off) = node_with(|_| {}, false).await else {
        return;
    };
    let (body, polled) = untouchable();
    let (status, answer) = send(
        &off.app,
        Some(OWNER_KEY),
        &format!("/v1/items/{item}/videos"),
        body,
    )
    .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (503, Some(crate::writes::DISABLED_MESSAGE))
    );
    assert_eq!(polled.load(Ordering::SeqCst), 0);
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

#[tokio::test]
async fn malformed_forms_are_refused_and_store_nothing() {
    let Some(node) = node().await else { return };
    let video = mp4("forms");
    let (collection, item) = smart_wearable(&node, &video).await;
    let long_name = "n".repeat(2 * 1024 * 1024);
    let cases: Vec<(&str, Vec<u8>, u16)> = vec![
        (
            "two file parts",
            form(&[
                (VIDEO_FILE, Some("a.mp4"), &video),
                (VIDEO_FILE, Some("b.mp4"), &video),
            ]),
            400,
        ),
        (
            "a part after the file",
            form(&[(VIDEO_FILE, Some("a.mp4"), &video), ("note", None, b"x")]),
            400,
        ),
        (
            "an unknown field",
            form(&[("model.glb", Some("a.mp4"), &video)]),
            400,
        ),
        (
            "the hash as the field",
            form(&[(&hash(&video), Some("a.mp4"), &video)]),
            400,
        ),
        ("no file part", form(&[]), 400),
        (
            "a field, not a file",
            form(&[(VIDEO_FILE, None, &video)]),
            400,
        ),
        (
            "a 2 MiB field name",
            form(&[(&long_name, Some("a.mp4"), &video)]),
            400,
        ),
        ("not multipart", video.clone(), 400),
    ];
    for (what, body, expected) in cases {
        let (status, _) = upload(&node, Some(OWNER_KEY), item, body).await;
        assert_eq!(status, expected, "{what}");
        assert!(!stored(&node, &hash(&video)).await, "{what}");
        assert!(
            records(&node.pool, &hash(&video)).await.is_empty(),
            "{what}"
        );
    }
    let mut preamble = vec![b'x'; 2 * 1024 * 1024];
    preamble.extend_from_slice(b"\r\n");
    let (body, pulled) = counted(preamble, 0, video_form(&video), false);
    let (status, _) = send(
        &node.app,
        Some(OWNER_KEY),
        &format!("/v1/items/{item}/videos"),
        body,
    )
    .await;
    assert_eq!(status, 400, "a 2 MiB preamble");
    assert!(
        pulled.load(Ordering::SeqCst) <= 2 * 1024 * 1024 + 2,
        "the preamble was read whole"
    );
    assert_eq!(spool_entries(&node), 0);
    cleanup(&node.pool, &[collection], &[]).await;
}

/// A body the test feeds: its sender and the bytes the node has pulled.
fn fed() -> (
    Body,
    tokio::sync::mpsc::UnboundedSender<Bytes>,
    Arc<AtomicU64>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let pulled = Arc::new(AtomicU64::new(0));
    let counter = pulled.clone();
    let body = Body::from_stream(futures::stream::unfold(rx, move |mut rx| {
        let counter = counter.clone();
        async move {
            let chunk = rx.recv().await?;
            counter.fetch_add(chunk.len() as u64, Ordering::SeqCst);
            Some((Ok::<_, std::io::Error>(chunk), rx))
        }
    }));
    (body, tx, pulled)
}

/// Lets every ready task run, without a timer.
async fn settle() {
    for _ in 0..1_000 {
        tokio::task::yield_now().await;
    }
}

/// Spins until `pulled` reaches `bytes` or `ended` holds, without a timer,
/// so it holds under a paused clock.
async fn pulled_to(pulled: &AtomicU64, bytes: usize, ended: impl Fn() -> bool) {
    for _ in 0..1_000_000 {
        if pulled.load(Ordering::SeqCst) >= bytes as u64 || ended() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the node never read {bytes} bytes");
}

/// An upload of `item`'s `video` whose body the test feeds in steps of
/// `step` paused seconds, `rounds` times, `per_step` bytes each; then the
/// rest.
async fn fed_upload(
    node: &Node,
    item: Uuid,
    video: &[u8],
    step: u64,
    rounds: usize,
    per_step: usize,
) -> (u16, Value) {
    let (body, tx, pulled) = fed();
    let uploading = tokio::spawn({
        let app = node.app.clone();
        async move { send_unbounded(&app, OWNER_KEY, &format!("/v1/items/{item}/videos"), body).await }
    });
    let whole = video_form(video);
    let mut sent = 120;
    tx.send(Bytes::copy_from_slice(&whole[..sent])).unwrap();
    until("the upload's spool file", || spool_entries(node) == 1).await;
    pulled_to(&pulled, sent, || uploading.is_finished()).await;
    tokio::time::pause();
    for _ in 0..rounds {
        tokio::time::advance(Duration::from_secs(step)).await;
        if tx
            .send(Bytes::copy_from_slice(&whole[sent..sent + per_step]))
            .is_err()
        {
            break;
        }
        sent += per_step;
        pulled_to(&pulled, sent, || uploading.is_finished()).await;
    }
    tokio::time::resume();
    let _ = tx.send(Bytes::copy_from_slice(&whole[sent..]));
    drop(tx);
    tokio::time::timeout(Duration::from_secs(10), uploading)
        .await
        .expect("the upload ended")
        .unwrap()
}

#[tokio::test]
async fn a_sender_that_stalls_is_cut_at_the_idle_bound_and_leaves_nothing() {
    let Some(node) = node().await else { return };
    assert_eq!(
        (node.state.videos.deadline, node.state.videos.idle_timeout),
        (Duration::from_secs(1200), Duration::from_secs(30))
    );
    let video = mp4("stalled");
    let (collection, item) = smart_wearable(&node, &video).await;
    let (body, tx, pulled) = fed();
    let uploading = tokio::spawn({
        let app = node.app.clone();
        async move { send_unbounded(&app, OWNER_KEY, &format!("/v1/items/{item}/videos"), body).await }
    });
    tx.send(Bytes::copy_from_slice(&video_form(&video)[..120]))
        .unwrap();
    until("the upload's spool file", || spool_entries(&node) == 1).await;
    pulled_to(&pulled, 120, || false).await;
    tokio::time::pause();
    settle().await;
    tokio::time::advance(Duration::from_secs(29)).await;
    settle().await;
    assert!(!uploading.is_finished(), "cut before the idle bound");
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    tokio::time::resume();
    let (status, answer) = tokio::time::timeout(Duration::from_secs(10), uploading)
        .await
        .expect("a stalled upload is cut at the idle bound")
        .unwrap();
    assert_eq!(
        (status, answer["error"].as_str()),
        (408, Some("The upload timed out"))
    );
    assert_eq!(spool_entries(&node), 0);
    assert!(records(&node.pool, &hash(&video)).await.is_empty());
    assert!(node.state.videos.idle(), "the place was kept");
    drop(tx);
    cleanup(&node.pool, &[collection], &[]).await;
}

#[tokio::test]
async fn a_slow_steady_sender_inside_the_total_finishes() {
    let Some(node) = node_with(|state| state.videos.min_window_bytes = 1, true).await else {
        return;
    };
    let video = mp4("slow and steady");
    let (collection, item) = smart_wearable(&node, &video).await;
    assert_eq!(
        fed_upload(&node, item, &video, 20, 6, 1).await,
        (200, json!({ "ok": true }))
    );
    assert_eq!(records(&node.pool, &hash(&video)).await.len(), 1);
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

#[tokio::test]
async fn a_steady_sender_past_the_total_is_cut_and_frees_its_place() {
    let Some(node) = node_with(
        |state| {
            state.videos.deadline = Duration::from_secs(90);
            state.videos.min_window_bytes = 1;
        },
        true,
    )
    .await
    else {
        return;
    };
    let video = mp4("too slow");
    let (collection, item) = smart_wearable(&node, &video).await;
    let (status, answer) = fed_upload(&node, item, &video, 20, 6, 1).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (408, Some("The upload timed out"))
    );
    assert_eq!(spool_entries(&node), 0);
    assert!(records(&node.pool, &hash(&video)).await.is_empty());
    assert!(node.state.videos.idle(), "the place was kept");
    cleanup(&node.pool, &[collection], &[]).await;
}

#[tokio::test]
async fn a_sender_that_trickles_below_the_minimum_rate_is_cut_and_frees_its_place() {
    let Some(node) = node().await else { return };
    assert_eq!(node.state.videos.min_window_bytes, 512 * 1024);
    let video = mp4("trickled");
    let (collection, item) = smart_wearable(&node, &video).await;
    let (status, answer) = fed_upload(&node, item, &video, 20, 6, 1).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (408, Some("The upload timed out"))
    );
    assert_eq!(spool_entries(&node), 0);
    assert!(records(&node.pool, &hash(&video)).await.is_empty());
    assert!(node.state.videos.idle(), "the place was kept");
    cleanup(&node.pool, &[collection], &[]).await;
}

#[tokio::test]
async fn a_sender_at_the_minimum_rate_keeps_its_place_until_it_finishes() {
    let Some(node) = node().await else { return };
    let per_window = node.state.videos.min_window_bytes as usize;
    let video = mp4_of(4 * per_window);
    let (collection, item) = smart_wearable(&node, &video).await;
    assert_eq!(
        fed_upload(&node, item, &video, 29, 3, per_window).await,
        (200, json!({ "ok": true }))
    );
    assert_eq!(records(&node.pool, &hash(&video)).await.len(), 1);
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

#[tokio::test]
async fn concurrent_uploads_are_bounded_and_the_item_ends_naming_exactly_one() {
    let Some(node) = node().await else { return };
    let (first, second) = (mp4("racing first"), mp4("racing second"));
    let (collection, item) = smart_wearable(&node, &first).await;
    let path = format!("/v1/items/{item}/videos");
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let paused = Body::from_stream(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|b| (Ok::<_, std::io::Error>(b), rx))
    }));
    let racing = tokio::spawn({
        let app = node.app.clone();
        let path = path.clone();
        async move { send(&app, Some(OWNER_KEY), &path, paused).await }
    });
    let whole = video_form(&first);
    let split = whole.len() / 2;
    tx.send(Bytes::copy_from_slice(&whole[..split])).unwrap();
    until("the first upload's admission", || !node.state.videos.idle()).await;

    let (held, _) = counted(b"--videoboundary\r\n".to_vec(), 0, vec![], true);
    let second_slot = tokio::spawn({
        let app = node.app.clone();
        let path = path.clone();
        async move { send(&app, Some(OWNER_KEY), &path, held).await }
    });
    until("the second upload's admission", || {
        node.state.videos.in_flight() == 2
    })
    .await;
    let (body, polled) = untouchable();
    let (status, answer) = send(&node.app, Some(OWNER_KEY), &path, body).await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (
            429,
            Some("Too many video uploads of this wallet in progress")
        )
    );
    assert_eq!(polled.load(Ordering::SeqCst), 0);
    second_slot.abort();
    let _ = second_slot.await;
    until("the aborted upload's release", || {
        node.state.videos.in_flight() == 1
    })
    .await;

    name_video(&node, collection, item, &second).await;
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&second))
            .await
            .0,
        200
    );
    tx.send(Bytes::copy_from_slice(&whole[split..])).unwrap();
    drop(tx);
    let (status, _) = racing.await.unwrap();
    assert_eq!(status, 400, "the first video is no longer named");
    assert!(!stored(&node, &hash(&first)).await);
    assert!(records(&node.pool, &hash(&first)).await.is_empty());
    assert_eq!(records(&node.pool, &hash(&second)).await.len(), 1);
    assert_eq!(spool_entries(&node), 0);
    assert!(node.state.videos.idle());
    cleanup(&node.pool, &[collection], &[hash(&second)]).await;
}

/// The one item of `collection`.
async fn item_of(node: &Node, collection: Uuid) -> Uuid {
    sqlx::query_scalar("SELECT id FROM builder.items WHERE collection_id = $1")
        .bind(collection)
        .fetch_one(&node.pool)
        .await
        .unwrap()
}

/// Holds a place for `key`'s upload of a new smart wearable from `ip`, its
/// body stalled after the part headers: the task and the item's collection.
async fn hold(node: &Node, key: &str, ip: &str) -> (tokio::task::JoinHandle<()>, Uuid) {
    let collection = smart_wearable_of(node, key, &mp4("held")).await;
    let item = item_of(node, collection).await;
    let before = node.state.videos.in_flight();
    let (body, _) = counted(b"--videoboundary\r\n".to_vec(), 0, vec![], true);
    let app = node.app.clone();
    let (key, ip) = (key.to_string(), ip.to_string());
    let task = tokio::spawn(async move {
        let request = post(
            Some(&key),
            &format!("/v1/items/{item}/videos"),
            Some(&ip),
            body,
        )
        .await;
        let _ = app.oneshot(request).await;
    });
    until("a held place", || {
        node.state.videos.in_flight() == before + 1
    })
    .await;
    (task, collection)
}

async fn release(node: &Node, held: Vec<(tokio::task::JoinHandle<()>, Uuid)>) -> Vec<Uuid> {
    let mut collections = Vec::new();
    for (task, collection) in held {
        task.abort();
        let _ = task.await;
        collections.push(collection);
    }
    until("every release", || node.state.videos.idle()).await;
    collections
}

/// `key`'s upload of a new smart wearable's video from `ip`: status, error,
/// `Retry-After` and the item's collection.
async fn another_upload(
    node: &Node,
    key: &str,
    ip: &str,
) -> (u16, Option<String>, Option<String>, Uuid) {
    let video = mp4("another");
    let collection = smart_wearable_of(node, key, &video).await;
    let item = item_of(node, collection).await;
    let (status, answer, retry) = send_from(
        &node.app,
        Some(key),
        &format!("/v1/items/{item}/videos"),
        Some(ip),
        Body::from(video_form(&video)),
    )
    .await;
    (
        status,
        answer["error"].as_str().map(str::to_string),
        retry,
        collection,
    )
}

const WALLET_KEYS: [&str; 5] = [
    "0x8501010101010101010101010101010101010101010101010101010101010101",
    "0x8502020202020202020202020202020202020202020202020202020202020202",
    "0x8503030303030303030303030303030303030303030303030303030303030303",
    "0x8504040404040404040404040404040404040404040404040404040404040404",
    "0x8505050505050505050505050505050505050505050505050505050505050505",
];

#[tokio::test]
async fn two_wallets_holding_every_place_they_may_leave_another_creator_room() {
    let Some(node) = node().await else { return };
    let mut held = Vec::new();
    for (key, ip) in [
        (WALLET_KEYS[0], "198.51.100.1"),
        (WALLET_KEYS[1], "198.51.100.2"),
    ] {
        for _ in 0..2 {
            held.push(hold(&node, key, ip).await);
        }
    }
    let mut others = Vec::new();
    for (key, ip, error) in [
        (
            WALLET_KEYS[0],
            "198.51.100.9",
            "Too many video uploads of this wallet in progress",
        ),
        (
            WALLET_KEYS[2],
            "198.51.100.1",
            "Too many video uploads from this address in progress",
        ),
    ] {
        let (status, refused, retry, collection) = another_upload(&node, key, ip).await;
        assert_eq!(
            (status, refused.as_deref(), retry.as_deref()),
            (429, Some(error), Some("30")),
            "{key} from {ip}"
        );
        others.push(collection);
    }
    let (status, _, _, collection) = another_upload(&node, WALLET_KEYS[2], "198.51.100.3").await;
    assert_eq!(
        status, 200,
        "a third creator uploads beside the two wallets"
    );
    others.push(collection);
    others.extend(release(&node, held).await);
    cleanup(&node.pool, &others, &[]).await;
}

#[tokio::test]
async fn behind_a_gateway_the_node_does_not_trust_every_creator_shares_no_source_place() {
    let Some(node) = node().await else { return };
    let mut held = Vec::new();
    for key in &WALLET_KEYS[..3] {
        held.push(hold(&node, key, "10.0.0.7").await);
    }
    assert_eq!(node.state.videos.in_flight(), 3);
    let collections = release(&node, held).await;
    cleanup(&node.pool, &collections, &[]).await;
}

async fn hold_files(node: &Node, key: &str, ip: &str) -> (tokio::task::JoinHandle<()>, Uuid) {
    let collection = smart_wearable_of(node, key, &mp4("held files")).await;
    let item = item_of(node, collection).await;
    let before = node.state.file_uploads.in_flight();
    let (body, _) = counted(b"--videoboundary\r\n".to_vec(), 0, vec![], true);
    let app = node.app.clone();
    let (key, ip) = (key.to_string(), ip.to_string());
    let task = tokio::spawn(async move {
        let request = post(
            Some(&key),
            &format!("/v1/items/{item}/files"),
            Some(&ip),
            body,
        )
        .await;
        let _ = app.oneshot(request).await;
    });
    until("a held file upload", || {
        node.state.file_uploads.in_flight() == before + 1
    })
    .await;
    (task, collection)
}

#[tokio::test]
async fn file_uploads_in_flight_are_bounded_per_wallet_and_refused_before_their_body() {
    let Some(node) = node().await else { return };
    let mut held = Vec::new();
    for n in 0..crate::FILE_UPLOADS_PER_SIGNER {
        held.push(hold_files(&node, WALLET_KEYS[0], &format!("198.51.100.{}", n + 1)).await);
    }
    let collection = smart_wearable_of(&node, WALLET_KEYS[0], &mp4("one more")).await;
    let item = item_of(&node, collection).await;
    let (body, polled) = untouchable();
    let (status, answer, retry) = send_from(
        &node.app,
        Some(WALLET_KEYS[0]),
        &format!("/v1/items/{item}/files"),
        Some("198.51.100.9"),
        body,
    )
    .await;
    assert_eq!(
        (status, answer["error"].as_str(), retry.as_deref()),
        (
            429,
            Some("Too many file uploads of this wallet in progress"),
            Some("30")
        )
    );
    assert_eq!(
        polled.load(Ordering::SeqCst),
        0,
        "the refused body was read"
    );
    held.push(hold_files(&node, WALLET_KEYS[1], "198.51.100.9").await);
    let mut collections = vec![collection];
    for (task, held_collection) in held {
        task.abort();
        let _ = task.await;
        collections.push(held_collection);
    }
    until("every file upload's release", || {
        node.state.file_uploads.in_flight() == 0
    })
    .await;
    cleanup(&node.pool, &collections, &[]).await;
}

#[tokio::test]
async fn the_node_reads_at_most_eight_videos_at_once_and_refuses_the_ninth_at_once() {
    let Some(node) = node().await else { return };
    let mut held = Vec::new();
    for (n, key) in WALLET_KEYS[..4].iter().enumerate() {
        for _ in 0..2 {
            held.push(hold(&node, key, &format!("198.51.100.{}", n + 1)).await);
        }
    }
    assert_eq!(node.state.videos.in_flight(), 8);
    let video = mp4("ninth");
    let collection = smart_wearable_of(&node, WALLET_KEYS[4], &video).await;
    let item = item_of(&node, collection).await;
    let (body, polled) = untouchable();
    let (status, answer, retry) = send_from(
        &node.app,
        Some(WALLET_KEYS[4]),
        &format!("/v1/items/{item}/videos"),
        Some("198.51.100.5"),
        body,
    )
    .await;
    assert_eq!(
        (status, answer["error"].as_str(), retry.as_deref()),
        (
            429,
            Some("Too many video uploads in progress, try again later"),
            Some("30")
        )
    );
    assert_eq!(polled.load(Ordering::SeqCst), 0);
    let mut collections = release(&node, held).await;
    assert_eq!(
        send_from(
            &node.app,
            Some(WALLET_KEYS[4]),
            &format!("/v1/items/{item}/videos"),
            Some("198.51.100.5"),
            Body::from(video_form(&video))
        )
        .await
        .0,
        200
    );
    collections.push(collection);
    cleanup(&node.pool, &collections, &[hash(&video)]).await;
}

#[tokio::test]
async fn a_spool_left_by_a_crash_is_removed_by_the_first_upload_after_it_ages() {
    let Some(node) = node().await else { return };
    let root = video::spool_root(&node.storage);
    std::fs::create_dir_all(&root).unwrap();
    let (old, fresh) = (root.join("old-crash"), root.join("live-upload"));
    std::fs::write(&old, b"left over").unwrap();
    std::fs::write(&fresh, b"in flight").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(
            std::time::SystemTime::now() - video::SPOOL_ORPHAN_AGE - Duration::from_secs(1),
        )
        .unwrap();
    let video = mp4("sweep");
    let (collection, item) = smart_wearable(&node, &video).await;
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&video))
            .await
            .0,
        200
    );
    assert!(!old.exists(), "the orphan stays");
    assert!(fresh.exists(), "a live upload's spool was removed");
    assert!(node.state.videos.deadline < video::SPOOL_ORPHAN_AGE);

    std::fs::File::options()
        .write(true)
        .open(&fresh)
        .unwrap()
        .set_modified(
            std::time::SystemTime::now() - video::SPOOL_ORPHAN_AGE - Duration::from_secs(1),
        )
        .unwrap();
    let later = mp4("a later upload");
    name_video(&node, collection, item, &later).await;
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&later))
            .await
            .0,
        200
    );
    assert!(
        !fresh.exists(),
        "a leftover too young at the first sweep stays once it has aged"
    );
    cleanup(&node.pool, &[collection], &[hash(&video), hash(&later)]).await;
}

#[tokio::test]
async fn a_video_larger_than_the_file_uploads_bound_is_kept_with_its_size() {
    let Some(node) = node().await else { return };
    let video = mp4_of(21 * 1024 * 1024);
    let (collection, item) = smart_wearable(&node, &video).await;
    assert_eq!(
        upload(&node, Some(OWNER_KEY), item, video_form(&video)).await,
        (200, json!({ "ok": true }))
    );
    assert_eq!(
        records(&node.pool, &hash(&video)).await,
        vec![(video.len() as i64, address(OWNER_KEY))]
    );
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

/// A body whose bytes the test sends when it chooses.
fn paused() -> (Body, tokio::sync::mpsc::UnboundedSender<Bytes>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let body = Body::from_stream(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|b| (Ok::<_, std::io::Error>(b), rx))
    }));
    (body, tx)
}

#[tokio::test]
async fn an_item_that_names_no_video_is_refused_before_its_body() {
    let Some(node) = node().await else { return };
    let video = mp4("unnamed");
    let (collection, item) = smart_wearable(&node, &video).await;
    sqlx::query("DELETE FROM builder.item_contents WHERE item_id = $1 AND file = $2")
        .bind(item)
        .bind(VIDEO_FILE)
        .execute(&node.pool)
        .await
        .unwrap();
    let (body, polled) = untouchable();
    let (status, answer) = send(
        &node.app,
        Some(OWNER_KEY),
        &format!("/v1/items/{item}/videos"),
        body,
    )
    .await;
    assert_eq!(
        (status, answer["error"].as_str()),
        (400, Some("The item names no video"))
    );
    assert_eq!(polled.load(Ordering::SeqCst), 0);
    cleanup(&node.pool, &[collection], &[]).await;
}

#[tokio::test]
async fn a_manager_removed_while_the_video_streams_stores_nothing() {
    let Some(node) = node().await else { return };
    let video = mp4("revoked");
    let (collection, item) = smart_wearable(&node, &video).await;
    let contract = on_chain(&node.pool, collection, false).await;
    let managers = |list: Vec<String>| {
        sqlx::query("UPDATE shop.collections SET managers = $2 WHERE contract_address = $1")
            .bind(contract.clone())
            .bind(list)
            .execute(&node.pool)
    };
    managers(vec![address(MANAGER_KEY)]).await.unwrap();
    let (body, tx) = paused();
    let uploading = tokio::spawn({
        let app = node.app.clone();
        async move {
            send(
                &app,
                Some(MANAGER_KEY),
                &format!("/v1/items/{item}/videos"),
                body,
            )
            .await
        }
    });
    let whole = video_form(&video);
    tx.send(Bytes::copy_from_slice(&whole[..10])).unwrap();
    until("the manager's admission", || !node.state.videos.idle()).await;
    managers(vec![]).await.unwrap();
    tx.send(Bytes::copy_from_slice(&whole[10..])).unwrap();
    drop(tx);
    assert_eq!(uploading.await.unwrap().0, 401);
    assert!(records(&node.pool, &hash(&video)).await.is_empty());
    assert!(!stored(&node, &hash(&video)).await);
    super::support::off_chain(&node.pool, &contract).await;
    cleanup(&node.pool, &[collection], &[]).await;
}

#[tokio::test]
async fn no_connection_or_row_lock_is_held_while_the_video_streams() {
    let Some(node) = node_over(1, |_| {}, true).await else {
        return;
    };
    let video = mp4("streaming");
    let (collection, item) = smart_wearable(&node, &video).await;
    let (body, tx) = paused();
    let uploading = tokio::spawn({
        let app = node.app.clone();
        async move {
            send(
                &app,
                Some(OWNER_KEY),
                &format!("/v1/items/{item}/videos"),
                body,
            )
            .await
        }
    });
    let whole = video_form(&video);
    tx.send(Bytes::copy_from_slice(&whole[..10])).unwrap();
    until("the admission", || !node.state.videos.idle()).await;
    let mut tx_db = tokio::time::timeout(Duration::from_secs(10), node.pool.begin())
        .await
        .expect("the upload holds the only connection")
        .unwrap();
    let locked: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM builder.items WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(item)
            .fetch_optional(&mut *tx_db)
            .await
            .expect("the upload holds the item's row");
    assert_eq!(locked, Some(item));
    tx_db.rollback().await.unwrap();
    tx.send(Bytes::copy_from_slice(&whole[10..])).unwrap();
    drop(tx);
    assert_eq!(uploading.await.unwrap().0, 200);
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

#[tokio::test]
async fn a_client_that_leaves_once_its_video_arrived_cannot_cut_the_store_short() {
    let Some(node) = node().await else { return };
    let video = mp4("left");
    let (collection, item) = smart_wearable(&node, &video).await;
    let mut holder = node.pool.begin().await.unwrap();
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    sqlx::query("SELECT 1 FROM builder.items WHERE id = $1 FOR UPDATE")
        .bind(item)
        .execute(&mut *holder)
        .await
        .unwrap();
    let uploading = tokio::spawn({
        let app = node.app.clone();
        let body = Body::from(video_form(&video));
        async move {
            send(
                &app,
                Some(OWNER_KEY),
                &format!("/v1/items/{item}/videos"),
                body,
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))",
            )
            .bind(holder_pid)
            .fetch_one(&node.pool)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the store waits on the item's row");
    uploading.abort();
    let _ = uploading.await;
    holder.rollback().await.unwrap();
    until("the store's end", || node.state.videos.idle()).await;
    assert_eq!(
        records(&node.pool, &hash(&video)).await,
        vec![(video.len() as i64, address(OWNER_KEY))]
    );
    assert!(stored(&node, &hash(&video)).await);
    assert_eq!(spool_entries(&node), 0);
    cleanup(&node.pool, &[collection], &[hash(&video)]).await;
}

const BOUNDED_KEY: &str = "0x8505050505050505050505050505050505050505050505050505050505050505";
const KEPT_COUNT_KEY: &str = "0x8606060606060606060606060606060606060606060606060606060606060606";

async fn declared(node: &Node, key: &str, path: &str, length: usize, body: Body) -> (u16, Value) {
    let mut request = post(Some(key), path, None, body).await;
    request
        .headers_mut()
        .insert(header::CONTENT_LENGTH, length.into());
    let response = tokio::time::timeout(Duration::from_secs(60), node.app.clone().oneshot(request))
        .await
        .expect("the node answers within a minute")
        .unwrap();
    let (status, body, _) = answer(response).await;
    (status, body)
}

async fn bounded_wearable(node: &Node, key: &str, video: &[u8]) -> (Uuid, Uuid) {
    let owner = address(key);
    let collection = Uuid::new_v4();
    save_collection(
        &node.state.items,
        &owner,
        json!({ "id": collection, "name": format!("Bounded {}", &collection.simple().to_string()[..12]) }),
    )
    .await
    .unwrap();
    let item = Uuid::new_v4();
    saved_item(
        &node.state.items,
        &owner,
        &json!({
            "id": item, "name": "Smart hat", "collection_id": collection, "rarity": "common",
            "video": hash(video),
            "contents": { "model.glb": hash(&unique("model")), VIDEO_FILE: hash(video) },
        }),
    )
    .await;
    (collection, item)
}

#[tokio::test]
async fn one_address_sends_at_most_its_daily_upload_bytes_whatever_its_wallets() {
    let Some(node) = node_with(
        |state| {
            state.limits.source_upload_mib =
                crate::limits::budget(2, std::time::Duration::from_secs(86_400));
        },
        true,
    )
    .await
    else {
        return;
    };
    let mut collections = Vec::new();
    let mut hashes = Vec::new();
    for (n, (key, ip, expected)) in [
        (WALLET_KEYS[0], "198.51.100.20", 200),
        (WALLET_KEYS[1], "198.51.100.20", 200),
        (WALLET_KEYS[2], "198.51.100.20", 429),
        (WALLET_KEYS[2], "198.51.100.21", 200),
    ]
    .into_iter()
    .enumerate()
    {
        let mut video = mp4_of(900 * 1024);
        video[900 * 1024 - 1] = n as u8;
        let collection = smart_wearable_of(&node, key, &video).await;
        let item = item_of(&node, collection).await;
        let form = video_form(&video);
        let mut request = post(
            Some(key),
            &format!("/v1/items/{item}/videos"),
            Some(ip),
            Body::from(form.clone()),
        )
        .await;
        request
            .headers_mut()
            .insert(header::CONTENT_LENGTH, form.len().into());
        let (status, body, retry) = answer(node.app.clone().oneshot(request).await.unwrap()).await;
        assert_eq!(status, expected, "upload {n} from {ip}: {body}");
        if expected == 429 {
            assert!(retry.is_some(), "a refusal says when to come back");
        }
        collections.push(collection);
        hashes.push(hash(&video));
    }
    cleanup(&node.pool, &collections, &hashes).await;
}

#[tokio::test]
async fn uploads_within_the_minute_a_count_answers_still_count_toward_the_bound() {
    super::validator::set_clock(chrono::Utc::now());
    let Some(node) = node_with(
        |state| {
            state.limits.wallet_bytes = 4_000;
            state.now = super::validator::clock;
        },
        true,
    )
    .await
    else {
        return;
    };
    let videos = |item: Uuid| format!("/v1/items/{item}/videos");
    let mut published = mp4_of(3_000);
    published[2_999] = 9;
    let (published_collection, item) = bounded_wearable(&node, KEPT_COUNT_KEY, &published).await;
    let form = video_form(&published);
    assert_eq!(
        declared(
            &node,
            KEPT_COUNT_KEY,
            &videos(item),
            form.len(),
            Body::from(form)
        )
        .await
        .0,
        200
    );
    let contract = on_chain(&node.pool, published_collection, false).await;
    for (n, expected) in [(1u8, 200), (2, 200), (3, 200), (4, 413)] {
        let mut video = mp4_of(1_000);
        video[999] = n;
        let (_, item) = bounded_wearable(&node, KEPT_COUNT_KEY, &video).await;
        let form = video_form(&video);
        assert_eq!(
            declared(
                &node,
                KEPT_COUNT_KEY,
                &videos(item),
                form.len(),
                Body::from(form)
            )
            .await
            .0,
            expected,
            "draft video {n}"
        );
    }
    super::support::off_chain(&node.pool, &contract).await;
}

#[tokio::test]
async fn a_wallet_at_its_storage_bound_is_refused_the_next_upload_until_it_deletes_or_publishes() {
    super::validator::set_clock(chrono::Utc::now());
    let Some(node) = node_with(
        |state| {
            state.limits.wallet_bytes = 4_000;
            state.now = super::validator::clock;
        },
        true,
    )
    .await
    else {
        return;
    };
    let [first, second, third] = [1u8, 2, 3].map(|n| {
        let mut video = mp4_of(3_000);
        video[2_999] = n;
        video
    });
    let (_, a) = bounded_wearable(&node, BOUNDED_KEY, &first).await;
    let (b_collection, b) = bounded_wearable(&node, BOUNDED_KEY, &second).await;
    let (_, c) = bounded_wearable(&node, BOUNDED_KEY, &third).await;
    let videos = |item: Uuid| format!("/v1/items/{item}/videos");
    let refused = json!({
        "ok": false,
        "data": { "maxBytes": 4_000 },
        "error": "The wallet's unpublished files reached their limit"
    });

    let form = video_form(&first);
    assert_eq!(
        declared(&node, BOUNDED_KEY, &videos(a), form.len(), Body::from(form))
            .await
            .0,
        200
    );

    for path in [videos(b), format!("/builder-api/v1/items/{b}/files")] {
        let (body, polled) = untouchable();
        assert_eq!(
            declared(&node, BOUNDED_KEY, &path, 1_200, body).await,
            (413, refused.clone()),
            "{path}"
        );
        assert_eq!(
            polled.load(Ordering::SeqCst),
            0,
            "{path}: the body was read"
        );
    }

    let mut request = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/items/{a}"));
    for (name, value) in signed_map(
        BOUNDED_KEY,
        "delete",
        &format!("/v1/items/{a}"),
        "{}",
        false,
    )
    .await
    {
        request = request.header(name, value);
    }
    let deleted = answer(
        node.app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(deleted.0, 200, "{:?}", deleted.1);
    let form = video_form(&second);
    assert_eq!(
        declared(&node, BOUNDED_KEY, &videos(b), form.len(), Body::from(form))
            .await
            .0,
        200
    );

    let small = mp4("published elsewhere");
    let (d_collection, d) = bounded_wearable(&node, BOUNDED_KEY, &small).await;
    let form = video_form(&small);
    assert_eq!(
        declared(&node, BOUNDED_KEY, &videos(d), form.len(), Body::from(form))
            .await
            .0,
        200
    );
    let elsewhere = on_chain(&node.pool, d_collection, false).await;

    let form = video_form(&third);
    let (body, _) = untouchable();
    assert_eq!(
        declared(&node, BOUNDED_KEY, &videos(c), form.len(), body)
            .await
            .0,
        413,
        "another item's publication frees nothing of this one's"
    );
    let contract = on_chain(&node.pool, b_collection, false).await;
    let (body, polled) = untouchable();
    assert_eq!(
        declared(&node, BOUNDED_KEY, &videos(c), form.len(), body)
            .await
            .0,
        413,
        "the wallet's count answers for a minute, whatever the rate of its uploads"
    );
    assert_eq!(polled.load(Ordering::SeqCst), 0);
    super::validator::advance(chrono::Duration::seconds(61));
    assert_eq!(
        declared(&node, BOUNDED_KEY, &videos(c), form.len(), Body::from(form))
            .await
            .0,
        200,
        "a published item's files no longer count once the minute is over"
    );
    super::support::off_chain(&node.pool, &contract).await;
    super::support::off_chain(&node.pool, &elsewhere).await;
}

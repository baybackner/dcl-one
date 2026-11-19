//! Items beyond what the conformance captures show: the upload's bounds and
//! hash checks, the blob collector's view of a deleted item's files, how a
//! third-party URN in a path is matched and how an item part is unique
//! across a provider, and listing filters as bound values. The provider's
//! virtual third party lists the owner.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use super::support::{OWNER_KEY, temp_storage, wallet};
use crate::shared::signed_fetch::test_support::signed_headers_by;

const STRANGER_KEY: &str = "0x2222222222222222222222222222222222222222222222222222222222222222";
const PROVIDER: &str = "urn:decentraland:matic:collections-thirdparty:items-test";
const BOUNDARY: &str = "items-test-boundary";

struct App {
    pool: PgPool,
    state: crate::SharedState,
    router: Router,
}

async fn app() -> Option<App> {
    let pool = crate::db::scratch::isolated_pool(8).await?;
    sqlx::query(
        "INSERT INTO shop.mirror_state (mirror, network, source_url, last_complete_pass_at, pass_started_at) \
         VALUES ('collections', 'matic', 'https://subgraph.example', timezone('UTC', now()), timezone('UTC', now()))",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO builder.virtual_third_parties (id, managers, raw_metadata) VALUES ($1, $2, 'tp:1:Items test:')",
    )
    .bind(PROVIDER)
    .bind(vec![address(OWNER_KEY)])
    .execute(&pool)
    .await
    .unwrap();
    Some(over(pool).await)
}

async fn over(pool: PgPool) -> App {
    let state = Arc::new(
        crate::State::new(
            pool.clone(),
            temp_storage().await,
            Arc::default(),
            dcl_http::Outbound::for_tests(),
            Some(super::support::registry_only_index().await),
            None,
            crate::Settings {
                writes_enabled: true,
                ..crate::Settings::default()
            },
        )
        .unwrap(),
    );
    App {
        pool,
        router: crate::router(state.clone()),
        state,
    }
}

fn address(key: &str) -> String {
    format!("{:#x}", wallet(key).address())
}

async fn send(
    app: &App,
    key: &str,
    method: &str,
    path: &str,
    body: Body,
    kind: &str,
) -> (u16, Value) {
    let mut request = Request::builder().method(method).uri(path);
    let now = chrono::Utc::now().timestamp_millis().to_string();
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
    let request = request
        .header(header::CONTENT_TYPE, kind)
        .body(body)
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 64 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn item(
    id: Uuid,
    key: &str,
    collection: Option<Uuid>,
    contents: &[(&str, &str)],
    urn: Option<&str>,
) -> Value {
    let files: serde_json::Map<String, Value> = contents
        .iter()
        .map(|(f, h)| (f.to_string(), json!(h)))
        .collect();
    let main = contents.first().map_or("model.glb", |(f, _)| *f);
    let mut item = json!({
        "id": id, "name": "Items test", "description": null, "type": "wearable",
        "eth_address": address(key), "collection_id": collection, "contents": files,
        "metrics": { "meshes": 1, "bodies": 1, "materials": 1, "textures": 1, "triangles": 1, "entities": 1 },
        "data": {
            "category": "hat", "replaces": [], "hides": [], "tags": [],
            "representations": [{
                "bodyShapes": ["urn:decentraland:off-chain:base-avatars:BaseMale"],
                "mainFile": main, "contents": [main], "overrideHides": [], "overrideReplaces": []
            }]
        }
    });
    if let Some(urn) = urn {
        item["urn"] = urn.into();
    }
    json!({ "item": item })
}

async fn put(app: &App, key: &str, path: &str, body: &Value) -> (u16, Value) {
    send(
        app,
        key,
        "PUT",
        path,
        Body::from(body.to_string()),
        "application/json",
    )
    .await
}

fn multipart(parts: &[(&str, &[u8])]) -> Body {
    let mut body = Vec::new();
    for (name, bytes) in parts {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\ncontent-disposition: form-data; name=\"{name}\"; filename=\"blob\"\r\n\
                 content-type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    Body::from(body)
}

async fn upload(app: &App, key: &str, id: Uuid, parts: &[(&str, &[u8])]) -> (u16, Value) {
    send(
        app,
        key,
        "POST",
        &format!("/v1/items/{id}/files"),
        multipart(parts),
        &format!("multipart/form-data; boundary={BOUNDARY}"),
    )
    .await
}

async fn uploaders(pool: &PgPool, hash: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT uploaded_by FROM builder.uploaded_contents WHERE hash = $1 ORDER BY uploaded_by",
    )
    .bind(hash)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn an_upload_past_a_bound_or_unlike_its_item_stores_nothing() {
    let Some(app) = app().await else {
        return;
    };
    let model = b"items test model".to_vec();
    let hash = dcl_auth::hash_bytes_v1(&model);
    let id = Uuid::new_v4();
    let saved = put(
        &app,
        OWNER_KEY,
        &format!("/v1/items/{id}"),
        &item(id, OWNER_KEY, None, &[("model.glb", &hash)], None),
    )
    .await;
    assert_eq!(saved.0, 200, "{saved:?}");

    let other = b"a file the item doesn't name".to_vec();
    let other_hash = dcl_auth::hash_bytes_v1(&other);
    let big = vec![7u8; crate::items::upload::MAX_BYTES + 1];
    let big_hash = dcl_auth::hash_bytes_v1(&big);
    let many: Vec<(&str, &[u8])> = (0..=crate::items::upload::MAX_FILES)
        .map(|_| (hash.as_str(), model.as_slice()))
        .collect();
    let empty_hash = dcl_auth::hash_bytes_v1(b"");
    let many_empty: Vec<(&str, &[u8])> = (0..=crate::items::upload::MAX_FILES)
        .map(|_| (empty_hash.as_str(), b"".as_slice()))
        .collect();
    for (label, parts, status, error) in [
        ("one file more than the bound", many, 400, "Too many files"),
        (
            "one empty part more than the bound",
            many_empty,
            400,
            "Too many files",
        ),
        (
            "more bytes than the bound",
            vec![(big_hash.as_str(), big.as_slice())],
            413,
            "The upload is too large",
        ),
        (
            "bytes under another hash",
            vec![(hash.as_str(), other.as_slice())],
            400,
            "The file does not match its hash",
        ),
        (
            "a hash the item doesn't name",
            vec![(other_hash.as_str(), other.as_slice())],
            400,
            "The item does not name this file",
        ),
        (
            "a field name that isn't a hash",
            vec![("../../model.glb", model.as_slice())],
            400,
            "Each file is sent under its content hash",
        ),
    ] {
        let (got, body) = upload(&app, OWNER_KEY, id, &parts).await;
        assert_eq!(
            (got, body["error"].as_str()),
            (status, Some(error)),
            "{label}: {body}"
        );
    }
    for refused in [&hash, &other_hash, &big_hash] {
        assert!(
            uploaders(&app.pool, refused).await.is_empty(),
            "{refused} recorded"
        );
        assert!(
            !app.state.items.blobs.exist(refused).await.unwrap(),
            "{refused} stored"
        );
    }

    let (got, body) = upload(&app, STRANGER_KEY, id, &[(&hash, &model)]).await;
    assert_eq!(got, 401, "{body}");
    let (got, body) = upload(&app, OWNER_KEY, id, &[(&hash, &model)]).await;
    assert_eq!((got, &body), (200, &json!({ "ok": true })));
    assert_eq!(uploaders(&app.pool, &hash).await, vec![address(OWNER_KEY)]);
    assert!(app.state.items.blobs.exist(&hash).await.unwrap());
}

#[tokio::test]
async fn naming_a_file_grants_nothing_and_its_records_go_when_no_item_names_it() {
    let Some(app) = app().await else {
        return;
    };
    let model = format!("shared model {}", Uuid::new_v4()).into_bytes();
    let hash = dcl_auth::hash_bytes_v1(&model);
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    let save = async |id: Uuid, key: &str| {
        let saved = put(
            &app,
            key,
            &format!("/v1/items/{id}"),
            &item(id, key, None, &[("model.glb", &hash)], None),
        )
        .await;
        assert_eq!(saved.0, 200, "{saved:?}");
    };
    save(first, OWNER_KEY).await;
    let referenced = async || {
        let mut conn = app.pool.acquire().await.unwrap();
        crate::storage::gc::referenced_rows_among_on(&mut conn, std::slice::from_ref(&hash))
            .await
            .unwrap()
            .contains(&hash)
    };
    assert!(!referenced().await, "a name alone keeps the file");
    assert_eq!(
        upload(&app, OWNER_KEY, first, &[(&hash, &model)]).await.0,
        200
    );
    assert!(referenced().await, "the upload record keeps the file");
    save(second, STRANGER_KEY).await;
    assert!(
        referenced().await,
        "a save naming an uploaded file keeps its record"
    );
    assert_eq!(
        uploaders(&app.pool, &hash).await,
        vec![address(OWNER_KEY)],
        "naming the hash records nothing for the namer"
    );
    assert_eq!(
        upload(&app, STRANGER_KEY, second, &[(&hash, &model)])
            .await
            .0,
        200
    );
    assert_eq!(
        uploaders(&app.pool, &hash).await.len(),
        2,
        "a second wallet uploading the same bytes is recorded too"
    );

    let (got, _) = send(
        &app,
        OWNER_KEY,
        "DELETE",
        &format!("/v1/items/{first}"),
        Body::empty(),
        "application/json",
    )
    .await;
    assert_eq!(got, 200);
    assert_eq!(
        uploaders(&app.pool, &hash).await.len(),
        2,
        "another item still names the file"
    );
    assert!(
        referenced().await,
        "the collector keeps a file another item names"
    );

    let (got, _) = put(
        &app,
        STRANGER_KEY,
        &format!("/v1/items/{second}"),
        &item(
            second,
            STRANGER_KEY,
            None,
            &[("other.glb", &dcl_auth::hash_bytes_v1(b"other"))],
            None,
        ),
    )
    .await;
    assert_eq!(got, 200);
    assert!(
        uploaders(&app.pool, &hash).await.is_empty(),
        "no item names the file any more"
    );
    assert!(!referenced().await, "the collector may take the file");
}

#[tokio::test]
async fn a_third_party_urn_in_the_path_reaches_exactly_one_item() {
    let Some(app) = app().await else {
        return;
    };
    let collection = Uuid::new_v4();
    let collection_urn = format!("{PROVIDER}:{}", collection.simple());
    sqlx::query(
        "INSERT INTO builder.collections (id, name, eth_address, third_party_id, urn_suffix) VALUES ($1, 'Linked', $2, $3, $4)",
    )
    .bind(collection)
    .bind(address(OWNER_KEY))
    .bind(PROVIDER)
    .bind(&collection_urn)
    .execute(&app.pool)
    .await
    .unwrap();
    let hash = dcl_auth::hash_bytes_v1(b"linked model");
    let contents = [("model.glb", hash.as_str())];
    let lower = format!("{collection_urn}:hat");
    let upper = format!("{collection_urn}:HAT");
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    for (id, urn) in [(first, &lower), (second, &upper)] {
        let saved = put(
            &app,
            OWNER_KEY,
            &format!("/v1/items/{id}"),
            &item(id, OWNER_KEY, Some(collection), &contents, Some(urn)),
        )
        .await;
        assert_eq!(
            saved.0, 200,
            "items whose URNs differ in case are two items: {saved:?}"
        );
    }
    let mut by_urn = item(first, OWNER_KEY, Some(collection), &contents, Some(&upper));
    by_urn["item"].as_object_mut().unwrap().remove("id");
    by_urn["item"]["name"] = "Shouted".into();
    let (got, body) = put(&app, OWNER_KEY, &format!("/v1/items/{upper}"), &by_urn).await;
    assert_eq!(
        (got, body["data"]["id"].as_str()),
        (200, Some(second.to_string().as_str()))
    );
    let names: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM builder.items WHERE collection_id = $1 ORDER BY name")
            .bind(collection)
            .fetch_all(&app.pool)
            .await
            .unwrap();
    assert_eq!(
        names,
        vec![
            (first, "Items test".to_string()),
            (second, "Shouted".to_string())
        ]
    );

    let mixed = format!("{collection_urn}:Hat");
    let mut unknown = by_urn.clone();
    unknown["item"]["urn"] = mixed.clone().into();
    let (got, body) = put(&app, OWNER_KEY, &format!("/v1/items/{mixed}"), &unknown).await;
    assert_eq!(
        (got, body["error"].as_str()),
        (404, Some("The third party item can not be created by URN."))
    );

    let sibling = Uuid::new_v4();
    let sibling_urn = format!("{PROVIDER}:{}", sibling.simple());
    sqlx::query(
        "INSERT INTO builder.collections (id, name, eth_address, third_party_id, urn_suffix) VALUES ($1, 'Sibling', $2, $3, $4)",
    )
    .bind(sibling)
    .bind(address(OWNER_KEY))
    .bind(PROVIDER)
    .bind(&sibling_urn)
    .execute(&app.pool)
    .await
    .unwrap();
    let mut in_flight = app.pool.begin().await.unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('builder item urn ' || 'scarf', 0))",
    )
    .execute(&mut *in_flight)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO builder.items (id, collection_id, eth_address, name, type, urn_suffix) \
         VALUES ($1, $2, $3, 'In flight', 'wearable', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(sibling)
    .bind(address(OWNER_KEY))
    .bind(format!("{sibling_urn}:scarf"))
    .execute(&mut *in_flight)
    .await
    .unwrap();
    let late = Uuid::new_v4();
    let body = item(
        late,
        OWNER_KEY,
        Some(collection),
        &contents,
        Some(&format!("{collection_urn}:scarf")),
    );
    let path = format!("/v1/items/{late}");
    let save = put(&app, OWNER_KEY, &path, &body);
    tokio::pin!(save);
    tokio::select! {
        biased;
        answer = &mut save => panic!("the save took the item part another save holds: {answer:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
    }
    in_flight.commit().await.unwrap();
    let (got, body) = save.await;
    assert_eq!(
        (got, body["error"].as_str()),
        (
            409,
            Some("The URN provided already belong to another item. The item can't be inserted.")
        ),
        "the waiting save sees the committed item of the provider's other collection"
    );
}

#[tokio::test]
async fn listing_filters_reach_the_database_as_values() {
    let Some(app) = app().await else {
        return;
    };
    let id = Uuid::new_v4();
    let hash = dcl_auth::hash_bytes_v1(b"filtered");
    assert_eq!(
        put(
            &app,
            OWNER_KEY,
            &format!("/v1/items/{id}"),
            &item(id, OWNER_KEY, None, &[("model.glb", &hash)], None)
        )
        .await
        .0,
        200
    );
    let owner = address(OWNER_KEY);
    for (query, status) in [
        ("collectionId=null&name=x", 200),
        ("collectionId=%27%20OR%20%271%27%3D%271", 400),
        (
            "collectionId=null&page=1&limit=1%3B%20DROP%20TABLE%20builder.items",
            200,
        ),
        ("page=-1&limit=5", 400),
        ("page=1&limit=-5", 400),
    ] {
        let (got, body) = send(
            &app,
            OWNER_KEY,
            "GET",
            &format!("/v1/{owner}/items?{query}"),
            Body::empty(),
            "application/json",
        )
        .await;
        assert_eq!(got, status, "{query}: {body}");
    }
    let collection = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO builder.collections (id, name, eth_address) VALUES ($1, 'Filtered', $2)",
    )
    .bind(collection)
    .bind(&owner)
    .execute(&app.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO builder.items (id, collection_id, eth_address, name, type) \
         VALUES ($1, $2, $3, 'Kept', 'wearable')",
    )
    .bind(Uuid::new_v4())
    .bind(collection)
    .bind(&owner)
    .execute(&app.pool)
    .await
    .unwrap();
    for (query, listed) in [
        ("name=zzz%25%27%20OR%20i.name%20%3C%3E%20%27", 0),
        ("name=%25%27%20OR%201%3D1%20--", 0),
        ("mappingStatus=%27%3B%20DROP", 1),
        ("status=pending&synced=%27", 0),
    ] {
        let (got, body) = send(
            &app,
            OWNER_KEY,
            "GET",
            &format!("/v1/collections/{collection}/items?{query}"),
            Body::empty(),
            "application/json",
        )
        .await;
        assert_eq!(
            (got, body["data"].as_array().map(Vec::len)),
            (200, Some(listed)),
            "{query}: {body}"
        );
    }
    let items: i64 = sqlx::query_scalar("SELECT count(*) FROM builder.items")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(items, 2);
}

async fn linked_collection(app: &App, key: &str, name: &str) -> (Uuid, String) {
    let id = Uuid::new_v4();
    let urn = format!("{PROVIDER}:{name}-{}", id.simple());
    sqlx::query(
        "INSERT INTO builder.collections (id, name, eth_address, third_party_id, urn_suffix) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(name)
    .bind(address(key))
    .bind(PROVIDER)
    .bind(&urn)
    .execute(&app.pool)
    .await
    .unwrap();
    (id, urn)
}

#[tokio::test]
async fn an_item_part_is_taken_across_the_providers_collections_and_only_its_managers_save() {
    let Some(app) = app().await else {
        return;
    };
    let hash = dcl_auth::hash_bytes_v1(b"sibling model");
    let contents = [("model.glb", hash.as_str())];
    let (theirs, their_urn) = linked_collection(&app, STRANGER_KEY, "theirs").await;
    let squatted = Uuid::new_v4();
    let (got, body) = put(
        &app,
        STRANGER_KEY,
        &format!("/v1/items/{squatted}"),
        &item(
            squatted,
            STRANGER_KEY,
            Some(theirs),
            &contents,
            Some(&format!("{their_urn}:scarf")),
        ),
    )
    .await;
    assert_eq!(
        got, 401,
        "creating a collection under the provider makes no manager of it: {body}"
    );

    let (mine, my_urn) = linked_collection(&app, OWNER_KEY, "mine").await;
    let id = Uuid::new_v4();
    let (got, body) = put(
        &app,
        OWNER_KEY,
        &format!("/v1/items/{id}"),
        &item(
            id,
            OWNER_KEY,
            Some(mine),
            &contents,
            Some(&format!("{my_urn}:scarf")),
        ),
    )
    .await;
    assert_eq!(got, 200, "{body}");

    let second = Uuid::new_v4();
    let (got, body) = put(
        &app,
        OWNER_KEY,
        &format!("/v1/items/{second}"),
        &item(
            second,
            OWNER_KEY,
            Some(theirs),
            &contents,
            Some(&format!("{their_urn}:scarf")),
        ),
    )
    .await;
    assert_eq!(
        got, 409,
        "the provider's collections share item parts, whoever created them: {body}"
    );

    let path = format!("{my_urn}:scarf");
    let mut by_urn = item(id, OWNER_KEY, Some(mine), &contents, Some(&path));
    by_urn["item"].as_object_mut().unwrap().remove("id");
    by_urn["item"]["name"] = "Mine by URN".into();
    let (got, body) = put(&app, OWNER_KEY, &format!("/v1/items/{path}"), &by_urn).await;
    assert_eq!(
        (got, body["data"]["id"].as_str()),
        (200, Some(id.to_string().as_str())),
        "a save by URN reaches the provider's item of that URN: {body}"
    );
    let (got, body) = put(&app, STRANGER_KEY, &format!("/v1/items/{path}"), &by_urn).await;
    assert_eq!(
        got, 401,
        "a wallet that manages no provider is refused: {body}"
    );

    let cap = Uuid::new_v4();
    let cap_urn = format!("{their_urn}:cap");
    let (got, body) = put(
        &app,
        OWNER_KEY,
        &format!("/v1/items/{cap}"),
        &item(cap, OWNER_KEY, Some(theirs), &contents, Some(&cap_urn)),
    )
    .await;
    assert_eq!(
        got, 200,
        "a manager saves into a collection another wallet created: {body}"
    );
    let mut by_urn = item(cap, OWNER_KEY, Some(theirs), &contents, Some(&cap_urn));
    by_urn["item"].as_object_mut().unwrap().remove("id");
    let (got, body) = put(&app, OWNER_KEY, &format!("/v1/items/{cap_urn}"), &by_urn).await;
    assert_eq!(
        (got, body["data"]["id"].as_str()),
        (200, Some(cap.to_string().as_str())),
        "a save by URN reaches an item of a collection another wallet created: {body}"
    );
}

#[tokio::test]
async fn an_upload_at_the_bound_is_stored_and_one_byte_more_or_a_larger_body_is_too_large() {
    let Some(app) = app().await else {
        return;
    };
    let at_bound = vec![1u8; crate::items::upload::MAX_BYTES];
    let over = vec![2u8; crate::items::upload::MAX_BYTES + 1];
    let body_over = vec![3u8; 22 * 1024 * 1024];
    let hashes = [&at_bound, &over, &body_over].map(|bytes| dcl_auth::hash_bytes_v1(bytes));
    let id = Uuid::new_v4();
    let contents: Vec<(&str, &str)> = vec![
        ("at.bin", hashes[0].as_str()),
        ("over.bin", hashes[1].as_str()),
        ("body.bin", hashes[2].as_str()),
    ];
    let saved = put(
        &app,
        OWNER_KEY,
        &format!("/v1/items/{id}"),
        &item(id, OWNER_KEY, None, &contents, None),
    )
    .await;
    assert_eq!(saved.0, 200, "{saved:?}");
    for (label, bytes, hash, status) in [
        ("one byte over the bound", &over, &hashes[1], 413),
        ("a body over the request bound", &body_over, &hashes[2], 413),
        ("exactly the bound", &at_bound, &hashes[0], 200),
    ] {
        let (got, body) = upload(&app, OWNER_KEY, id, &[(hash.as_str(), bytes.as_slice())]).await;
        assert_eq!(got, status, "{label}: {body}");
        if status == 413 {
            assert_eq!(body["error"], "The upload is too large", "{label}");
            assert!(
                uploaders(&app.pool, hash).await.is_empty(),
                "{label} recorded"
            );
        }
    }
}

const ITEM_DOCUMENTS_RELEASE: &str = include_str!(concat!(
    env!("DCL_ONE_ROOT"),
    "/schemas/releases/builder-item-documents-and-uploaders.sql"
));

async fn back_to_the_shape_before_the_release(pool: &PgPool) {
    sqlx::raw_sql(
        "ALTER TABLE builder.items DROP CONSTRAINT items_data_object_check; \
         ALTER TABLE builder.items DROP CONSTRAINT items_metrics_object_check; \
         ALTER TABLE builder.items ALTER COLUMN data DROP DEFAULT; \
         ALTER TABLE builder.items ALTER COLUMN data TYPE jsonb USING data::jsonb; \
         ALTER TABLE builder.items ALTER COLUMN data SET DEFAULT '{}'::jsonb; \
         ALTER TABLE builder.items ALTER COLUMN metrics TYPE jsonb USING metrics::jsonb; \
         ALTER TABLE builder.items ADD CONSTRAINT items_data_object_check CHECK (jsonb_typeof(data) = 'object'); \
         ALTER TABLE builder.items ADD CONSTRAINT items_metrics_object_check \
           CHECK (metrics IS NULL OR jsonb_typeof(metrics) = 'object'); \
         ALTER TABLE builder.uploaded_contents DROP CONSTRAINT uploaded_contents_pkey; \
         ALTER TABLE builder.uploaded_contents ADD CONSTRAINT uploaded_contents_pkey PRIMARY KEY (hash);",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn item_saves_and_uploads_wait_for_the_documents_release_and_reads_do_not() {
    let Some(before) = app().await else {
        return;
    };
    let bytes = b"waits for the release".as_slice();
    let hash = dcl_auth::hash_bytes_v1(bytes);
    let id = Uuid::new_v4();
    let body = item(id, OWNER_KEY, None, &[("model.glb", &hash)], None);
    let saved = put(&before, OWNER_KEY, &format!("/v1/items/{id}"), &body).await;
    assert_eq!(saved.0, 200, "{saved:?}");
    back_to_the_shape_before_the_release(&before.pool).await;
    let app = over(before.pool.clone()).await;

    let waits = |label: &str, (status, body): (u16, Value)| {
        assert_eq!(status, 503, "{label}: {body}");
        assert_eq!(body["ok"], false, "{label}");
        assert!(
            body["error"].as_str().is_some_and(
                |e| e.contains("schemas/releases/builder-item-documents-and-uploaders.sql")
            ),
            "{label}: {body}"
        );
    };
    waits(
        "a save",
        put(&app, OWNER_KEY, &format!("/v1/items/{id}"), &body).await,
    );
    let other = Uuid::new_v4();
    waits(
        "a new item",
        put(
            &app,
            OWNER_KEY,
            &format!("/v1/items/{other}"),
            &item(other, OWNER_KEY, None, &[], None),
        )
        .await,
    );
    waits(
        "an upload",
        upload(&app, OWNER_KEY, id, &[(&hash, bytes)]).await,
    );
    assert!(
        uploaders(&app.pool, &hash).await.is_empty(),
        "an upload before the release recorded"
    );

    let read = send(
        &app,
        OWNER_KEY,
        "GET",
        &format!("/v1/items/{id}"),
        Body::empty(),
        "application/json",
    )
    .await;
    assert_eq!(read.0, 200, "{read:?}");
    assert_eq!(read.1["data"]["contents"]["model.glb"], hash.as_str());
    let listed = send(
        &app,
        OWNER_KEY,
        "GET",
        &format!("/v1/{}/items", address(OWNER_KEY)),
        Body::empty(),
        "application/json",
    )
    .await;
    assert_eq!(listed.0, 200, "{listed:?}");
    assert_eq!(listed.1["data"][0]["id"], id.to_string());

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ITEM_DOCUMENTS_RELEASE.replace("\\set ON_ERROR_STOP on\n", ""),
    ))
    .execute(&app.pool)
    .await
    .unwrap();
    assert_eq!(
        put(&app, OWNER_KEY, &format!("/v1/items/{id}"), &body)
            .await
            .0,
        200
    );
    assert_eq!(upload(&app, OWNER_KEY, id, &[(&hash, bytes)]).await.0, 200);
    assert_eq!(uploaders(&app.pool, &hash).await, vec![address(OWNER_KEY)]);
}

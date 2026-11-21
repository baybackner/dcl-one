//! Callers of the builder routes, each with a budget of requests, through
//! the router at both mounts as `serve` mounts them and with the node's
//! clock driven by the test: a source address, a signer, a review caller
//! and a listing reader over their budget get 429 with `Retry-After`, before
//! the body is validated and before the database is asked; another caller is
//! unaffected; the same caller is served again once the window refills.

use std::sync::Arc;
use std::time::Duration as Window;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use chrono::Duration;
use tower::ServiceExt;
use uuid::Uuid;

use super::support::{OWNER_KEY, lazy_pool, signed_map, temp_storage};
use super::validator::{advance, clock, set_clock};

const OTHER_KEY: &str = "0x5555555555555555555555555555555555555555555555555555555555555555";
const SOURCE: &str = "203.0.113.9";
const OTHER_SOURCE: &str = "198.51.100.4";
const NOT_A_CID: &str = "not-a-cid";
const TOO_MANY: &str =
    r#"{"data":{},"error":"Too many requests, try again in a minute","ok":false}"#;

pub(super) async fn app_over(pool: sqlx::PgPool, tune: impl FnOnce(&mut crate::State)) -> Router {
    node_over(pool, tune).await.0
}

pub(super) async fn node_over(
    pool: sqlx::PgPool,
    tune: impl FnOnce(&mut crate::State),
) -> (Router, crate::SharedState) {
    set_clock(
        chrono::DateTime::from_timestamp_millis(chrono::Utc::now().timestamp_millis()).unwrap(),
    );
    let mut state = crate::State::new(
        pool,
        temp_storage().await,
        Arc::default(),
        dcl_http::Outbound::for_tests(),
        None,
        None,
        crate::Settings {
            writes_enabled: true,
            ..crate::Settings::default()
        },
    )
    .unwrap();
    state.now = clock;
    tune(&mut state);
    let state = Arc::new(state);
    let app = Router::new()
        .merge(crate::root_router(state.clone()))
        .nest("/builder-api", crate::router(state.clone()));
    (app, state)
}

pub(super) struct Call<'a> {
    pub(super) method: &'a str,
    pub(super) path: String,
    pub(super) key: Option<&'a str>,
    pub(super) source: Option<&'a str>,
    pub(super) body: Vec<u8>,
}

impl Call<'_> {
    pub(super) fn get(path: impl Into<String>) -> Self {
        Self {
            method: "GET",
            path: path.into(),
            key: None,
            source: None,
            body: Vec::new(),
        }
    }
}

pub(super) async fn send(app: &Router, call: &Call<'_>) -> (u16, String, Option<String>) {
    let mut request = Request::builder().method(call.method).uri(&call.path);
    if let Some(key) = call.key {
        for (name, value) in
            signed_map(key, &call.method.to_lowercase(), &call.path, "{}", false).await
        {
            request = request.header(name, value);
        }
    }
    if let Some(source) = call.source {
        request = request.header("x-real-ip", source);
    }
    if !call.body.is_empty() {
        request = request.header("content-type", "application/json");
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(call.body.clone())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let retry = response
        .headers()
        .get("retry-after")
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned(), retry)
}

fn per_minute(n: u32) -> crate::shared::limits::RateLimiter {
    crate::limits::budget(n, Window::from_secs(60))
}

fn save<'a>(key: &'a str, mount: &str) -> Call<'a> {
    Call {
        method: "PUT",
        path: format!("{mount}/v1/items/{}", Uuid::new_v4()),
        key: Some(key),
        source: None,
        body: b"not json".to_vec(),
    }
}

fn file_read<'a>(key: Option<&'a str>, source: Option<&'a str>, mount: &str) -> Call<'a> {
    Call {
        method: "GET",
        path: format!("{mount}/v1/storage/contents/{NOT_A_CID}"),
        key,
        source,
        body: Vec::new(),
    }
}

#[tokio::test]
async fn a_signer_over_its_write_budget_waits_before_its_body_is_read() {
    let app = app_over(lazy_pool(), |state| {
        state.limits.wallet_writes = per_minute(3)
    })
    .await;
    for mount in ["", "/builder-api", ""] {
        assert_eq!(send(&app, &save(OWNER_KEY, mount)).await.0, 400, "{mount}");
    }
    let (status, body, retry) = send(&app, &save(OWNER_KEY, "/builder-api")).await;
    assert_eq!(
        (status, body.as_str(), retry.as_deref()),
        (429, TOO_MANY, Some("20"))
    );
    assert_eq!(
        send(&app, &save(OTHER_KEY, "")).await.0,
        400,
        "another signer keeps its budget"
    );
    assert_eq!(
        send(&app, &file_read(Some(OWNER_KEY), None, "")).await.0,
        404,
        "reads keep their own budget"
    );
    advance(Duration::seconds(19));
    assert_eq!(send(&app, &save(OWNER_KEY, "")).await.0, 429);
    advance(Duration::seconds(1));
    assert_eq!(send(&app, &save(OWNER_KEY, "")).await.0, 400);
    assert_eq!(send(&app, &save(OWNER_KEY, "")).await.0, 429);
}

#[tokio::test]
async fn a_signer_over_its_read_budget_waits_and_a_failed_signature_spends_no_signers_budget() {
    let app = app_over(lazy_pool(), |state| {
        state.limits.wallet_reads = per_minute(2)
    })
    .await;
    for mount in ["", "/builder-api"] {
        assert_eq!(
            send(&app, &file_read(Some(OWNER_KEY), None, mount)).await.0,
            404
        );
    }
    let (status, body, retry) = send(&app, &file_read(Some(OWNER_KEY), None, "")).await;
    assert_eq!(
        (status, body.as_str(), retry.as_deref()),
        (429, TOO_MANY, Some("30"))
    );
    assert_eq!(
        send(&app, &file_read(None, None, "")).await.0,
        404,
        "unsigned reads spend no signer's budget"
    );
    assert_eq!(
        send(&app, &file_read(Some(OTHER_KEY), None, "")).await.0,
        404
    );

    let mut forged = Request::builder().uri(format!("/v1/storage/contents/{NOT_A_CID}"));
    for (name, value) in signed_map(OTHER_KEY, "get", "/v1/somewhere-else", "{}", false).await {
        forged = forged.header(name, value);
    }
    for _ in 0..3 {
        let response = app.clone().oneshot(forged_request(&forged)).await.unwrap();
        assert_eq!(response.status().as_u16(), 401);
    }
    assert_eq!(
        send(&app, &file_read(Some(OTHER_KEY), None, "")).await.0,
        404,
        "requests that claim a signer without its signature spend nothing of its budget"
    );
}

fn forged_request(template: &axum::http::request::Builder) -> Request<Body> {
    let mut request = Request::builder().uri(template.uri_ref().unwrap().clone());
    for (name, value) in template.headers_ref().unwrap() {
        request = request.header(name, value);
    }
    request.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn a_source_over_its_budget_waits_whoever_signs_and_an_internal_source_has_none() {
    let app = app_over(lazy_pool(), |state| {
        state.limits.source_reads = per_minute(4);
        state.limits.source_writes = per_minute(1);
    })
    .await;
    for (key, mount) in [
        (None, ""),
        (Some(OWNER_KEY), "/builder-api"),
        (Some(OTHER_KEY), ""),
        (None, ""),
    ] {
        assert_eq!(
            send(&app, &file_read(key, Some(SOURCE), mount)).await.0,
            404
        );
    }
    let (status, body, retry) = send(&app, &file_read(Some(OTHER_KEY), Some(SOURCE), "")).await;
    assert_eq!(
        (status, body.as_str(), retry.as_deref()),
        (429, TOO_MANY, Some("15"))
    );
    let mut write = save(OWNER_KEY, "");
    write.source = Some(SOURCE);
    assert_eq!(
        send(&app, &write).await.0,
        400,
        "writes keep their own budget"
    );
    assert_eq!(send(&app, &write).await.0, 429);
    assert_eq!(
        send(&app, &file_read(None, Some(OTHER_SOURCE), "")).await.0,
        404
    );
    let ipv6 = |n: u8| format!("2001:db8:1:2::{n}");
    for n in 0..4 {
        assert_eq!(
            send(&app, &file_read(None, Some(&ipv6(n)), "")).await.0,
            404
        );
    }
    assert_eq!(
        send(&app, &file_read(None, Some(&ipv6(9)), "")).await.0,
        429,
        "one IPv6 /64 is one source"
    );
    for internal in ["10.0.0.7", "127.0.0.1", "fd00::1"] {
        for _ in 0..6 {
            assert_eq!(
                send(&app, &file_read(None, Some(internal), "")).await.0,
                404,
                "{internal}"
            );
        }
    }
    for _ in 0..6 {
        assert_eq!(send(&app, &file_read(None, None, "")).await.0, 404);
    }
    advance(Duration::seconds(15));
    assert_eq!(send(&app, &file_read(None, Some(SOURCE), "")).await.0, 404);
}

#[tokio::test]
async fn the_production_budgets_hold_at_their_bounds() {
    let app = app_over(lazy_pool(), |_| ()).await;
    let signed = save(OWNER_KEY, "");
    let mut request = Request::builder().method("PUT").uri(&signed.path);
    for (name, value) in signed_map(OWNER_KEY, "put", &signed.path, "{}", false).await {
        request = request.header(name, value);
    }
    let template = request;
    let mut statuses = Vec::new();
    for _ in 0..=crate::limits::WALLET_WRITES {
        let mut request = Request::builder()
            .method("PUT")
            .uri(template.uri_ref().unwrap().clone());
        for (name, value) in template.headers_ref().unwrap() {
            request = request.header(name, value);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from("x")).unwrap())
            .await
            .unwrap();
        statuses.push(response.status().as_u16());
    }
    assert!(statuses[..statuses.len() - 1].iter().all(|s| *s == 400));
    assert_eq!(statuses.last(), Some(&429));

    let mut statuses = Vec::new();
    for _ in 0..=crate::limits::SOURCE_READS {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/storage/contents/{NOT_A_CID}"))
                    .header("x-real-ip", OTHER_SOURCE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        statuses.push(response.status().as_u16());
    }
    assert!(statuses[..statuses.len() - 1].iter().all(|s| *s == 404));
    assert_eq!(statuses.last(), Some(&429));
}

#[tokio::test]
async fn an_item_save_larger_than_the_body_cap_is_refused_before_its_keys_are_read() {
    let app = app_over(lazy_pool(), |_| ()).await;
    let large =
        serde_json::json!({ "item": { "id": Uuid::new_v4(), "unknown": "x".repeat(3 << 20) } });
    let mut call = save(OWNER_KEY, "");
    call.body = large.to_string().into_bytes();
    assert_eq!(send(&app, &call).await.0, 413);
}

#[tokio::test]
async fn a_review_caller_over_its_minute_waits_even_with_the_flow_off() {
    let Some(pool) = super::validator::own_pool().await else {
        return;
    };
    let app = app_over(pool, |_| ()).await;
    let id = Uuid::new_v4();
    let write = |key: &'static str, n: usize| Call {
        method: "POST",
        path: format!(
            "/v1/collections/{id}/{}",
            ["validations", "curation/appeal", "comments"][n % 3]
        ),
        key: Some(key),
        source: None,
        body: Vec::new(),
    };
    for n in 0..crate::limits::REVIEW_WRITES as usize {
        assert_eq!(send(&app, &write(OWNER_KEY, n)).await.0, 404, "{n}");
    }
    let (status, body, retry) = send(&app, &write(OWNER_KEY, 0)).await;
    assert_eq!(
        (status, body.as_str(), retry.as_deref()),
        (429, TOO_MANY, Some("2"))
    );
    assert_eq!(send(&app, &write(OTHER_KEY, 0)).await.0, 404);

    let events = |key: &'static str| Call {
        key: Some(key),
        ..Call::get(format!("/builder-api/v1/collections/{id}/events"))
    };
    for n in 0..crate::limits::REVIEW_READS {
        assert_eq!(send(&app, &events(OWNER_KEY)).await.0, 404, "{n}");
    }
    let (status, _, retry) = send(&app, &events(OWNER_KEY)).await;
    assert_eq!((status, retry.as_deref()), (429, Some("1")));
    advance(Duration::seconds(2));
    assert_eq!(send(&app, &write(OWNER_KEY, 1)).await.0, 404);
}

async fn seeded(state: &crate::SharedState, n: usize) -> Uuid {
    let owner = super::validator::address(OWNER_KEY);
    let collection = Uuid::new_v4();
    super::support::save_collection(
        &state.items,
        &owner,
        serde_json::json!({ "id": collection, "name": format!("Listing {}", &collection.simple().to_string()[..8]) }),
    )
    .await
    .unwrap();
    for i in 0..n {
        super::support::save_item(
            &state.items,
            &owner,
            &serde_json::json!({ "id": Uuid::new_v4(), "name": format!("Item {i}"), "collection_id": collection }),
        )
        .await
        .unwrap();
    }
    sqlx::query(
        "WITH made AS (INSERT INTO builder.collections (id, name, eth_address) \
           SELECT gen_random_uuid(), 'Curated ' || g, $1 FROM generate_series(1, $2) g RETURNING id) \
         INSERT INTO builder.collection_curations (id, collection_id, status) \
         SELECT gen_random_uuid(), id, 'pending' FROM made",
    )
    .bind(&owner)
    .bind(n as i32)
    .execute(state.items.pool())
    .await
    .unwrap();
    collection
}

async fn a_listing_charges_its_rows(
    listing: impl Fn(Uuid) -> String,
    committee: bool,
    item_curations: bool,
) {
    for by_source in [false, true] {
        let Some(pool) = super::validator::own_pool().await else {
            return;
        };
        if committee {
            super::support::FakeCommittee::with(&[&super::validator::address(OWNER_KEY)])
                .publish(&pool)
                .await;
        }
        let registry = super::support::registry_only_index().await;
        let (app, state) = node_over(pool, |state| {
            state.market = Some(registry);
            state.limits = crate::limits::Limits::unbounded();
            if by_source {
                state.limits.source_reads = per_minute(5);
            } else {
                state.limits.wallet_reads = per_minute(5);
            }
        })
        .await;
        let collection = seeded(&state, 60).await;
        if item_curations {
            sqlx::query(
                "INSERT INTO builder.item_curations (id, item_id, status, content_hash) \
                 SELECT gen_random_uuid(), id, 'pending', repeat('a', 64) FROM builder.items WHERE collection_id = $1",
            )
            .bind(collection)
            .execute(state.items.pool())
            .await
            .unwrap();
        }
        let source = by_source.then_some(SOURCE);
        let call = |path: String, key| Call {
            key: Some(key),
            source,
            ..Call::get(path)
        };
        let (status, body, _) = send(&app, &call(listing(collection), OWNER_KEY)).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            send(
                &app,
                &call(format!("/v1/storage/contents/{NOT_A_CID}"), OWNER_KEY)
            )
            .await
            .0,
            404
        );
        let next = if by_source { OTHER_KEY } else { OWNER_KEY };
        let (status, _, retry) = send(
            &app,
            &call(format!("/v1/storage/contents/{NOT_A_CID}"), next),
        )
        .await;
        assert_eq!(
            (status, retry.as_deref()),
            (429, Some("12")),
            "{}",
            listing(collection)
        );
    }
}

#[tokio::test]
async fn a_collections_items_cost_their_reader_by_the_row() {
    a_listing_charges_its_rows(|id| format!("/v1/collections/{id}/items"), false, false).await;
}

#[tokio::test]
async fn an_addresss_items_cost_their_reader_by_the_row() {
    let owner = super::validator::address(OWNER_KEY);
    a_listing_charges_its_rows(
        move |_| format!("/builder-api/v1/{owner}/items"),
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn every_item_costs_the_committee_member_by_the_row() {
    a_listing_charges_its_rows(|_| "/v1/items".to_string(), true, false).await;
}

#[tokio::test]
async fn the_curations_cost_their_reader_by_the_row() {
    a_listing_charges_its_rows(|_| "/v1/curations".to_string(), true, false).await;
}

#[tokio::test]
async fn an_addresss_collections_cost_their_reader_by_the_row() {
    let owner = super::validator::address(OWNER_KEY);
    a_listing_charges_its_rows(move |_| format!("/v1/{owner}/collections"), false, false).await;
}

#[tokio::test]
async fn a_collections_item_curations_cost_their_reader_by_the_row() {
    a_listing_charges_its_rows(
        |id| format!("/v1/collections/{id}/itemCurations"),
        true,
        true,
    )
    .await;
}

#[tokio::test]
async fn the_addresses_listing_costs_its_source_as_fifty_reads() {
    let Some(pool) = super::validator::own_pool().await else {
        return;
    };
    let app = app_over(pool, |state| state.limits.source_reads = per_minute(102)).await;
    let addresses = |query: &str| Call {
        source: Some(SOURCE),
        ..Call::get(format!("/v1/addresses{query}"))
    };
    assert_eq!(send(&app, &addresses("")).await.0, 200);
    assert_eq!(send(&app, &addresses("?tag=hat")).await.0, 200);
    let (status, _, retry) = send(&app, &addresses("?tag=other")).await;
    assert_eq!((status, retry.as_deref()), (429, Some("1")));
    assert_eq!(
        send(
            &app,
            &Call {
                source: Some(OTHER_SOURCE),
                ..Call::get("/v1/addresses")
            }
        )
        .await
        .0,
        200
    );
}

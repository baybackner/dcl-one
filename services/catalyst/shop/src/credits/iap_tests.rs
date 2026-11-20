//! The App Store routes over the module as `serve` mounts it, with a chain
//! generated per test (`apple::testkit`) as the configured root. DB-backed;
//! without `CATALYST_TEST_DATABASE_URL` they skip.

use std::collections::HashMap;
use std::sync::Arc;

use alloy::signers::{Signer as _, local::PrivateKeySigner};
use axum::Router;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use serde_json::{Value, json};
use sqlx::PgPool;

use super::apple::testkit::{Chain, ChainSpec, chain};
use super::apple::{Environment, account_token};
use super::iap::{Iap, Limits};
use super::tests::{StateCfg, Wallet, app, call, pool, scratch_wallet, send, test_state};
use crate::auth::{AuthLink, AuthLinkType};
use crate::shared::signed_fetch::{
    AUTH_CHAIN_HEADER_PREFIX, AUTH_METADATA_HEADER, AUTH_TIMESTAMP_HEADER, build_legacy_payload,
};

const PRODUCT: &str = "org.example.credits100";

fn iap(c: &Chain, limits: Limits) -> Arc<Iap> {
    Arc::new(Iap {
        verifier: c.verifier(Environment::Sandbox),
        catalog: HashMap::from([(PRODUCT.to_string(), 100)]),
        limits,
    })
}

fn open_limits() -> Limits {
    Limits {
        wallet_total_cents: 0,
        wallet_daily_cents: 0,
        daily_cents: i64::MAX / 4,
    }
}

fn setup(pool: &PgPool, c: &Chain, limits: Limits) -> Router {
    app(test_state(
        pool.clone(),
        StateCfg {
            iap: Some(iap(c, limits)),
            ..StateCfg::default()
        },
    ))
}

fn tx_id() -> String {
    format!("2000{}", uuid::Uuid::new_v4().as_u128() % 1_000_000_000_000)
}

async fn available(pool: &PgPool, addr: &str) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT COALESCE((SELECT available::text FROM credits.wallets WHERE address = $1), '0')",
    )
    .bind(addr)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn notify(app: &Router, signed: String) -> (StatusCode, Value) {
    let (status, _, body) = send(
        app,
        "POST",
        "/credits/apple/webhook",
        HeaderMap::new(),
        Some(json!({ "signedPayload": signed })),
    )
    .await;
    (status, body)
}

async fn register(app: &Router, w: &Wallet) -> String {
    let (status, out) = call(app, w, "post", "/credits/credits/iap/register", &[], None).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    out["data"]["appAccountToken"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_signed_purchase_credits_once_and_its_refund_charges_it_back() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let token = register(&app, &w).await;
    assert_eq!(token, account_token(&w.address));
    let id = tx_id();
    let signed_tx = c.transaction(&id, PRODUCT, Some(&token));

    // A payload not signed through the configured root credits nothing.
    let other = chain(ChainSpec::default());
    let (status, _) = notify(
        &app,
        other.notification(
            "ONE_TIME_CHARGE",
            Some(other.transaction(&id, PRODUCT, Some(&token))),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send(
        &app,
        "POST",
        "/credits/apple/webhook",
        HeaderMap::new(),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(available(&pool, &w.address).await, "0");

    let (status, out) = notify(
        &app,
        c.notification("ONE_TIME_CHARGE", Some(signed_tx.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["alreadyExisted"], false);
    assert_eq!(available(&pool, &w.address).await, "100");

    // The device path and a redelivery find it credited.
    let (status, out) = call(
        &app,
        &w,
        "post",
        "/credits/credits/iap/verify",
        &[],
        Some(json!({ "jwsRepresentation": signed_tx })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["data"]["alreadyExisted"], true, "{out}");
    let (_, out) = notify(
        &app,
        c.notification("ONE_TIME_CHARGE", Some(signed_tx.clone())),
    )
    .await;
    assert_eq!(out["alreadyExisted"], true);
    assert_eq!(available(&pool, &w.address).await, "100");

    let (status, out) = notify(&app, c.notification("REFUND", Some(signed_tx.clone()))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["action"], "refunded");
    assert_eq!(available(&pool, &w.address).await, "0");
    let (_, out) = notify(&app, c.notification("REFUND", Some(signed_tx.clone()))).await;
    assert_eq!(out["action"], "already_refunded");

    // The original signed transaction is never credited again.
    let (_, out) = call(
        &app,
        &w,
        "post",
        "/credits/credits/iap/verify",
        &[],
        Some(json!({ "jwsRepresentation": signed_tx })),
    )
    .await;
    assert_eq!(out["code"], "refunded_transaction", "{out}");
    assert_eq!(available(&pool, &w.address).await, "0");

    let path = format!("/credits/users/{}/credit-orders", w.address);
    let (status, out) = call(&app, &w, "get", &path, &[], None).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["total"], 1);
    let order = &out["orders"][0];
    assert_eq!(order["provider"], "apple");
    assert_eq!(order["id"], format!("apple:{id}"));
    assert_eq!(order["status"], "reversed");
    assert_eq!(order["credits"], 100);
    assert_eq!(order["usdCents"], 1000);
    assert_eq!(order["packId"], PRODUCT);
}

#[tokio::test]
async fn a_refund_before_the_purchase_keeps_it_from_being_credited() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let token = register(&app, &w).await;
    let signed_tx = c.transaction(&tx_id(), PRODUCT, Some(&token));

    let (_, out) = notify(&app, c.notification("REFUND", Some(signed_tx.clone()))).await;
    assert_eq!(out["action"], "unknown_credit");
    let (status, out) = notify(&app, c.notification("ONE_TIME_CHARGE", Some(signed_tx))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["error"], "refunded_transaction", "{out}");
    assert_eq!(available(&pool, &w.address).await, "0");
}

#[tokio::test]
async fn the_device_path_credits_only_a_transaction_bought_for_the_signer() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let buyer = scratch_wallet();
    let other = scratch_wallet();
    let signed_tx = c.transaction(&tx_id(), PRODUCT, Some(&account_token(&buyer.address)));

    let (status, out) = call(
        &app,
        &other,
        "post",
        "/credits/credits/iap/verify",
        &[],
        Some(json!({ "jwsRepresentation": signed_tx })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["code"], "token_mismatch", "{out}");
    assert_eq!(available(&pool, &other.address).await, "0");

    let mut revoked = super::apple::testkit::transaction_payload(
        &tx_id(),
        PRODUCT,
        Some(&account_token(&buyer.address)),
    );
    revoked["revocationDate"] = json!(1_700_000_000_000i64);
    let (_, out) = call(
        &app,
        &buyer,
        "post",
        "/credits/credits/iap/verify",
        &[],
        Some(json!({ "jwsRepresentation": c.sign(&revoked) })),
    )
    .await;
    assert_eq!(out["code"], "revoked_transaction", "{out}");

    let (_, out) = call(
        &app,
        &buyer,
        "post",
        "/credits/credits/iap/verify",
        &[],
        Some(json!({ "jwsRepresentation": signed_tx })),
    )
    .await;
    assert_eq!(out["data"]["credited"], true, "{out}");
    assert_eq!(available(&pool, &buyer.address).await, "100");
}

#[tokio::test]
async fn a_refund_the_balance_cannot_cover_queues_signed_credits_up_to_the_uncovered_part() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let token = register(&app, &w).await;
    let signed_tx = c.transaction(&tx_id(), PRODUCT, Some(&token));
    let (_, out) = notify(
        &app,
        c.notification("ONE_TIME_CHARGE", Some(signed_tx.clone())),
    )
    .await;
    assert_eq!(out["action"], "minted", "{out}");

    // 80 of the 100 Credits pledged to a signed, unexpired credit.
    let auth_id = format!("auth-{}", uuid::Uuid::new_v4().simple());
    let hash = format!(
        "0x{}",
        hex::encode(alloy_primitives::keccak256(auth_id.as_bytes()))
    );
    sqlx::query(
        "INSERT INTO credits.authorizations (id, address, usd_cents, amount_wei, status, expires_at, chain_id, contract, credit_hash) \
         VALUES ($1, $2, 800, 1, 'authorized', timezone('UTC', now()) + interval '5 minutes', 137, $3, $4)",
    )
    .bind(&auth_id)
    .bind(&w.address)
    .bind("0x00000000000000000000000000000000000000c2")
    .bind(&hash)
    .execute(&pool)
    .await
    .unwrap();

    // A second purchase: 200 available, 110 pledged, so a 100 refund leaves 10 uncovered,
    // which the newest signed credit covers; the older one stays live.
    let (_, out) = notify(
        &app,
        c.notification(
            "ONE_TIME_CHARGE",
            Some(c.transaction(&tx_id(), PRODUCT, Some(&token))),
        ),
    )
    .await;
    assert_eq!(out["action"], "minted", "{out}");
    let older_id = format!("auth-{}", uuid::Uuid::new_v4().simple());
    let older_hash = format!(
        "0x{}",
        hex::encode(alloy_primitives::keccak256(older_id.as_bytes()))
    );
    sqlx::query(
        "INSERT INTO credits.authorizations (id, address, usd_cents, amount_wei, status, expires_at, chain_id, contract, credit_hash, created_at) \
         VALUES ($1, $2, 300, 1, 'authorized', timezone('UTC', now()) + interval '5 minutes', 137, $3, $4, timezone('UTC', now()) - interval '1 minute')",
    )
    .bind(&older_id)
    .bind(&w.address)
    .bind("0x00000000000000000000000000000000000000c2")
    .bind(&older_hash)
    .execute(&pool)
    .await
    .unwrap();

    let (_, out) = notify(&app, c.notification("REFUND", Some(signed_tx))).await;
    assert_eq!(out["action"], "refunded", "{out}");
    assert_eq!(out["revocations"], 1);
    let older_queued: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM credits.revocations WHERE credit_hash = $1)",
    )
    .bind(&older_hash)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!older_queued, "only the uncovered part is revoked");
    assert_eq!(available(&pool, &w.address).await, "100");
    let (reason, status): (String, String) =
        sqlx::query_as("SELECT reason, status FROM credits.revocations WHERE credit_hash = $1")
            .bind(&hash)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(reason.starts_with("reversed:apple-iap:"), "{reason}");
    assert_eq!(status, "pending");
    let blocked: bool = sqlx::query_scalar(
        "SELECT is_blocked_for_claiming FROM credits.wallets WHERE address = $1",
    )
    .bind(&w.address)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(blocked);
}

#[tokio::test]
async fn the_caps_hold_on_quote_and_on_credit() {
    let Some(own) = super::tests::OwnDatabase::create("iap_caps").await else {
        return;
    };
    let pool = own.pool.clone();
    let c = chain(ChainSpec::default());
    let app = setup(
        &pool,
        &c,
        Limits {
            wallet_total_cents: 0,
            wallet_daily_cents: 1500,
            daily_cents: 100_000,
        },
    );
    let w = scratch_wallet();
    let (status, out) = call(
        &app,
        &w,
        "post",
        "/credits/credits/iap/quote",
        &[],
        Some(json!({ "productId": PRODUCT })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["ok"], true);
    assert_eq!(out["data"]["credits"], 100);
    assert_eq!(out["data"]["appAccountToken"], account_token(&w.address));
    let (_, out) = call(
        &app,
        &w,
        "post",
        "/credits/credits/iap/quote",
        &[],
        Some(json!({ "productId": "nope" })),
    )
    .await;
    assert_eq!(out["code"], "unknown_product");

    let token = account_token(&w.address);
    let (_, out) = notify(
        &app,
        c.notification(
            "ONE_TIME_CHARGE",
            Some(c.transaction(&tx_id(), PRODUCT, Some(&token))),
        ),
    )
    .await;
    assert_eq!(out["action"], "minted", "{out}");
    let (_, out) = call(
        &app,
        &w,
        "post",
        "/credits/credits/iap/quote",
        &[],
        Some(json!({ "productId": PRODUCT })),
    )
    .await;
    assert_eq!(
        (out["code"].as_str(), out["reason"].as_str()),
        (Some("cap_exceeded"), Some("daily_limit")),
        "{out}"
    );
    let (_, out) = notify(
        &app,
        c.notification(
            "ONE_TIME_CHARGE",
            Some(c.transaction(&tx_id(), PRODUCT, Some(&token))),
        ),
    )
    .await;
    assert_eq!(out["error"], "wallet_cap_exceeded", "{out}");
    assert_eq!(available(&pool, &w.address).await, "100");

    // A purchase over the node's daily ceiling is left for Apple to retry.
    let node_capped = setup(
        &pool,
        &c,
        Limits {
            wallet_total_cents: 0,
            wallet_daily_cents: 0,
            daily_cents: 1500,
        },
    );
    let w2 = scratch_wallet();
    let (status, out) = notify(
        &node_capped,
        c.notification(
            "ONE_TIME_CHARGE",
            Some(c.transaction(&tx_id(), PRODUCT, Some(&w2.address))),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{out}");
    assert_eq!(available(&pool, &w2.address).await, "0");

    // An unknown product is retried by Apple.
    let (status, _) = notify(
        &app,
        c.notification(
            "ONE_TIME_CHARGE",
            Some(c.transaction(&tx_id(), "nope", Some(&token))),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    drop(app);
    own.drop().await;
}

#[tokio::test]
async fn the_operator_routes_credit_by_product_and_refund_once() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    // A wallet cap below one product: the operator credit reconciles what it refused.
    let app = app(test_state(
        pool.clone(),
        StateCfg {
            iap: Some(iap(
                &c,
                Limits {
                    wallet_total_cents: 500,
                    ..open_limits()
                },
            )),
            admin_token: Some("op-secret-123456789012345678901234"),
            ..StateCfg::default()
        },
    ));
    let w = scratch_wallet();
    let id = tx_id();
    let mut bearer = HeaderMap::new();
    bearer.insert(
        "authorization",
        "Bearer op-secret-123456789012345678901234".parse().unwrap(),
    );
    let (status, _, out) = send(
        &app,
        "POST",
        "/credits/credits/iap",
        HeaderMap::new(),
        Some(json!({ "userAddress": w.address, "appleTransactionId": id, "appleProductId": PRODUCT })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{out}");
    let (status, _, out) = send(
        &app,
        "POST",
        "/credits/credits/iap",
        bearer.clone(),
        Some(json!({ "userAddress": w.address, "appleTransactionId": id, "appleProductId": PRODUCT })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["credits"], 100);
    assert_eq!(available(&pool, &w.address).await, "100");
    let refund = json!({ "appleTransactionId": id, "refundId": "r-1", "reason": "support" });
    let (status, _, out) = send(
        &app,
        "POST",
        "/credits/credits/iap/refund",
        bearer.clone(),
        Some(refund.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(available(&pool, &w.address).await, "0");
    let (status, _, _) = send(
        &app,
        "POST",
        "/credits/credits/iap/refund",
        bearer.clone(),
        Some(refund),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, _) = send(
        &app,
        "POST",
        "/credits/credits/iap/refund",
        bearer,
        Some(json!({ "appleTransactionId": tx_id(), "refundId": "r-2", "reason": "support" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_node_without_app_store_configuration_answers_503() {
    let state = test_state(
        sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://nobody:nowhere@127.0.0.1:1/never")
            .unwrap(),
        StateCfg::default(),
    );
    let (status, _, _) = send(
        &app(state),
        "POST",
        "/credits/apple/webhook",
        HeaderMap::new(),
        Some(json!({ "signedPayload": "a.b.c" })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

/// The iOS app's request: the wallet delegates to an ephemeral key ("Decentraland Login"),
/// which signs the pre-6.0.0 folded payload with camelCase metadata.
async fn login_signed_legacy(
    w: &Wallet,
    method: &str,
    path: &str,
    metadata: &str,
    expiration: &str,
) -> HeaderMap {
    let ephemeral = PrivateKeySigner::random();
    let login = format!(
        "Decentraland Login\nEphemeral address: {:#x}\nExpiration: {expiration}",
        ephemeral.address()
    );
    let login_signature = w
        .signer
        .sign_message(login.as_bytes())
        .await
        .unwrap()
        .to_string();
    let timestamp = chrono::Utc::now().timestamp_millis().to_string();
    let payload = build_legacy_payload(method, path, &timestamp, metadata);
    let signature = ephemeral
        .sign_message(payload.as_bytes())
        .await
        .unwrap()
        .to_string();
    let chain = [
        AuthLink {
            link_type: AuthLinkType::SIGNER,
            payload: w.address.clone(),
            signature: None,
        },
        AuthLink {
            link_type: AuthLinkType::EcdsaEphemeral,
            payload: login,
            signature: Some(login_signature),
        },
        AuthLink {
            link_type: AuthLinkType::EcdsaSignedEntity,
            payload,
            signature: Some(signature),
        },
    ];
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTH_TIMESTAMP_HEADER,
        HeaderValue::from_str(&timestamp).unwrap(),
    );
    headers.insert(
        AUTH_METADATA_HEADER,
        HeaderValue::from_str(metadata).unwrap(),
    );
    for (i, link) in chain.iter().enumerate() {
        headers.insert(
            HeaderName::from_bytes(format!("{AUTH_CHAIN_HEADER_PREFIX}{i}").as_bytes()).unwrap(),
            HeaderValue::from_str(&serde_json::to_string(link).unwrap()).unwrap(),
        );
    }
    headers
}

#[tokio::test]
async fn the_app_login_chain_signing_the_folded_payload_quotes_and_verifies() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let live = "2099-01-01T00:00:00.000Z";

    let path = "/credits/credits/iap/quote";
    let metadata = json!({ "productId": PRODUCT }).to_string();
    let headers = login_signed_legacy(&w, "post", path, &metadata, live).await;
    let (status, _, out) = send(
        &app,
        "POST",
        path,
        headers,
        Some(json!({ "productId": PRODUCT })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["data"]["appAccountToken"], account_token(&w.address));

    let signed_tx = c.transaction(&tx_id(), PRODUCT, Some(&account_token(&w.address)));
    let path = "/credits/credits/iap/verify";
    let metadata = json!({ "jwsRepresentation": signed_tx }).to_string();
    let expired =
        login_signed_legacy(&w, "post", path, &metadata, "2001-01-01T00:00:00.000Z").await;
    let (status, _, out) = send(
        &app,
        "POST",
        path,
        expired,
        Some(json!({ "jwsRepresentation": signed_tx })),
    )
    .await;
    assert!(
        status.is_client_error(),
        "an expired login delegation: {out}"
    );
    assert_eq!(out["error"], "Invalid Auth Chain", "{out}");
    assert_eq!(available(&pool, &w.address).await, "0");

    let headers = login_signed_legacy(&w, "post", path, &metadata, live).await;
    let (status, _, out) = send(
        &app,
        "POST",
        path,
        headers,
        Some(json!({ "jwsRepresentation": signed_tx })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["data"]["credited"], true, "{out}");
    assert_eq!(available(&pool, &w.address).await, "100");
}

#[tokio::test]
async fn app_store_grants_reconcile_with_their_purchases() {
    let Some(own) = super::tests::OwnDatabase::create("iap_reconcile").await else {
        return;
    };
    let pool = own.pool.clone();
    let c = chain(ChainSpec::default());
    let state = test_state(
        pool.clone(),
        StateCfg {
            iap: Some(iap(&c, open_limits())),
            ..StateCfg::default()
        },
    );
    let app = app(state.clone());
    let w = scratch_wallet();
    let token = register(&app, &w).await;
    let signed_tx = c.transaction(&tx_id(), PRODUCT, Some(&token));
    let (_, out) = notify(
        &app,
        c.notification("ONE_TIME_CHARGE", Some(signed_tx.clone())),
    )
    .await;
    assert_eq!(out["action"], "minted", "{out}");
    let report = state.credits.reconcile().await.unwrap();
    assert!(
        report.purchase_grant_mismatches.is_empty(),
        "{:?}",
        report.purchase_grant_mismatches
    );

    let (_, out) = notify(&app, c.notification("REFUND", Some(signed_tx))).await;
    assert_eq!(out["action"], "refunded", "{out}");
    let report = state.credits.reconcile().await.unwrap();
    assert!(
        report.purchase_grant_mismatches.is_empty(),
        "{:?}",
        report.purchase_grant_mismatches
    );
    drop(app);
    drop(state);
    own.drop().await;
}

#[tokio::test]
async fn concurrent_registrations_of_one_wallet_all_return_its_token() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let calls = (0..16).map(|_| call(&app, &w, "post", "/credits/credits/iap/register", &[], None));
    for (status, out) in futures::future::join_all(calls).await {
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["data"]["appAccountToken"], account_token(&w.address));
    }
}

fn bearer() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        "authorization",
        "Bearer op-secret-123456789012345678901234".parse().unwrap(),
    );
    h
}

fn setup_with_admin(pool: &PgPool, c: &Chain, limits: Limits) -> Router {
    app(test_state(
        pool.clone(),
        StateCfg {
            iap: Some(iap(c, limits)),
            admin_token: Some("op-secret-123456789012345678901234"),
            ..StateCfg::default()
        },
    ))
}

async fn row(pool: &PgPool, id: &str) -> Option<(String, Option<String>, Option<String>)> {
    sqlx::query_as(
        "SELECT status, address, held_reason FROM credits.iap_transactions WHERE transaction_id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn held_ids(app: &Router) -> Vec<String> {
    let (status, _, out) = send(
        app,
        "GET",
        "/credits/credits/iap/held?limit=200",
        bearer(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    out["held"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["appleTransactionId"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_purchase_over_a_wallet_cap_is_held_until_an_operator_credits_it() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup_with_admin(
        &pool,
        &c,
        Limits {
            wallet_total_cents: 500,
            ..open_limits()
        },
    );
    let w = scratch_wallet();
    let token = register(&app, &w).await;
    let id = tx_id();
    let (status, out) = notify(
        &app,
        c.notification(
            "ONE_TIME_CHARGE",
            Some(c.transaction(&id, PRODUCT, Some(&token))),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["error"], "wallet_cap_exceeded");
    assert_eq!(out["held"], true);
    assert_eq!(
        row(&pool, &id).await,
        Some((
            "held".into(),
            Some(w.address.clone()),
            Some("total_limit".into())
        ))
    );
    assert!(held_ids(&app).await.contains(&id));
    assert_eq!(available(&pool, &w.address).await, "0");

    // A held purchase is never credited to another wallet.
    let other = scratch_wallet();
    let (status, _, out) = send(
        &app,
        "POST",
        "/credits/credits/iap",
        bearer(),
        Some(json!({ "userAddress": other.address, "appleTransactionId": id, "appleProductId": PRODUCT })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");

    let (status, _, out) = send(
        &app,
        "POST",
        "/credits/credits/iap",
        bearer(),
        Some(json!({ "userAddress": w.address, "appleTransactionId": id, "appleProductId": PRODUCT })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["alreadyExisted"], false);
    assert_eq!(available(&pool, &w.address).await, "100");
    assert_eq!(row(&pool, &id).await.unwrap().0, "paid");
    assert!(!held_ids(&app).await.contains(&id));

    // The same transaction again, for another wallet: already credited elsewhere.
    let (status, _, out) = send(
        &app,
        "POST",
        "/credits/credits/iap",
        bearer(),
        Some(json!({ "userAddress": other.address, "appleTransactionId": id, "appleProductId": PRODUCT })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert_eq!(available(&pool, &other.address).await, "0");
}

#[tokio::test]
async fn a_held_purchase_is_credited_by_a_later_delivery() {
    let Some(own) = super::tests::OwnDatabase::create("iap_held_retry").await else {
        return;
    };
    let pool = own.pool.clone();
    let c = chain(ChainSpec::default());
    let w = scratch_wallet();
    let token = w.address.clone();

    // Over the node's ceiling, then an unknown product: both held and retried by Apple.
    let closed = setup(
        &pool,
        &c,
        Limits {
            daily_cents: 0,
            ..open_limits()
        },
    );
    let capped = tx_id();
    let signed = c.transaction(&capped, PRODUCT, Some(&token));
    let (status, _) = notify(
        &closed,
        c.notification("ONE_TIME_CHARGE", Some(signed.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        row(&pool, &capped).await.unwrap().2.as_deref(),
        Some("service_daily_limit")
    );
    let unknown = tx_id();
    let unknown_signed = c.transaction(&unknown, "org.example.later", Some(&token));
    let (status, _) = notify(
        &closed,
        c.notification("ONE_TIME_CHARGE", Some(unknown_signed.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        row(&pool, &unknown).await.unwrap().2.as_deref(),
        Some("unknown_product")
    );

    // The ceiling resets and the product is configured: the retries credit both.
    let mut reopened = iap(&c, open_limits());
    Arc::get_mut(&mut reopened)
        .unwrap()
        .catalog
        .insert("org.example.later".into(), 50);
    let app = app(test_state(
        pool.clone(),
        StateCfg {
            iap: Some(reopened),
            ..StateCfg::default()
        },
    ));
    let (_, out) = notify(&app, c.notification("ONE_TIME_CHARGE", Some(signed))).await;
    assert_eq!(out["action"], "minted", "{out}");
    let (_, out) = notify(
        &app,
        c.notification("ONE_TIME_CHARGE", Some(unknown_signed)),
    )
    .await;
    assert_eq!(out["action"], "minted", "{out}");
    assert_eq!(available(&pool, &w.address).await, "150");
    assert_eq!(row(&pool, &capped).await.unwrap().0, "paid");
    let report = test_state(pool.clone(), StateCfg::default())
        .credits
        .reconcile()
        .await
        .unwrap();
    assert!(report.purchase_grant_mismatches.is_empty());
    drop(app);
    drop(closed);
    own.drop().await;
}

#[tokio::test]
async fn a_purchase_naming_no_wallet_is_held_and_credited_by_the_device() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup_with_admin(&pool, &c, open_limits());
    let w = scratch_wallet();
    let token = account_token(&w.address);
    let id = tx_id();
    let signed = c.transaction(&id, PRODUCT, Some(&token));
    let (status, out) = notify(
        &app,
        c.notification("ONE_TIME_CHARGE", Some(signed.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["error"], "unresolvable_app_account_token");
    assert_eq!(out["held"], true);
    let token_kept: Option<String> = sqlx::query_scalar(
        "SELECT app_account_token FROM credits.iap_transactions WHERE transaction_id = $1",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(token_kept, Some(token));
    assert!(held_ids(&app).await.contains(&id));

    let (_, out) = call(
        &app,
        &w,
        "post",
        "/credits/credits/iap/verify",
        &[],
        Some(json!({ "jwsRepresentation": signed })),
    )
    .await;
    assert_eq!(out["data"]["credited"], true, "{out}");
    assert_eq!(available(&pool, &w.address).await, "100");
    assert_eq!(row(&pool, &id).await.unwrap().0, "paid");
}

#[tokio::test]
async fn a_quantity_out_of_range_is_held_without_a_retry() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let id = tx_id();
    let mut p = super::apple::testkit::transaction_payload(&id, PRODUCT, Some(&w.address));
    p["quantity"] = json!(11);
    let (status, out) = notify(&app, c.notification("ONE_TIME_CHARGE", Some(c.sign(&p)))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["error"], "quantity_out_of_range");
    assert_eq!(
        row(&pool, &id).await,
        Some((
            "held".into(),
            Some(w.address.clone()),
            Some("quantity_out_of_range".into())
        ))
    );
    assert_eq!(available(&pool, &w.address).await, "0");
}

#[tokio::test]
async fn a_transaction_revoked_when_first_seen_is_never_credited() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let id = tx_id();
    let mut revoked = super::apple::testkit::transaction_payload(&id, PRODUCT, Some(&w.address));
    revoked["revocationDate"] = json!(1_700_000_000_000i64);
    let (_, out) = notify(
        &app,
        c.notification("ONE_TIME_CHARGE", Some(c.sign(&revoked))),
    )
    .await;
    assert_eq!(out["error"], "revoked_transaction", "{out}");
    assert_eq!(row(&pool, &id).await.unwrap().0, "refunded");

    let (_, out) = notify(
        &app,
        c.notification(
            "ONE_TIME_CHARGE",
            Some(c.transaction(&id, PRODUCT, Some(&w.address))),
        ),
    )
    .await;
    assert_eq!(out["error"], "refunded_transaction", "{out}");
    assert_eq!(available(&pool, &w.address).await, "0");
}

#[tokio::test]
async fn a_reversed_refund_grants_the_charge_back_again() {
    let Some(own) = super::tests::OwnDatabase::create("iap_refund_reversed").await else {
        return;
    };
    let pool = own.pool.clone();
    let c = chain(ChainSpec::default());
    let state = test_state(
        pool.clone(),
        StateCfg {
            iap: Some(iap(&c, open_limits())),
            ..StateCfg::default()
        },
    );
    let app = app(state.clone());
    let w = scratch_wallet();
    let signed = c.transaction(&tx_id(), PRODUCT, Some(&w.address));
    let (_, out) = notify(
        &app,
        c.notification("ONE_TIME_CHARGE", Some(signed.clone())),
    )
    .await;
    assert_eq!(out["action"], "minted", "{out}");
    for _ in 0..2 {
        let (_, out) = notify(&app, c.notification("REFUND", Some(signed.clone()))).await;
        assert_eq!(out["action"], "refunded", "{out}");
        assert_eq!(available(&pool, &w.address).await, "0");
        let (_, out) = notify(
            &app,
            c.notification("REFUND_REVERSED", Some(signed.clone())),
        )
        .await;
        assert_eq!(out["action"], "reinstated", "{out}");
        assert_eq!(available(&pool, &w.address).await, "100");
    }
    let (_, out) = notify(&app, c.notification("REFUND_REVERSED", Some(signed))).await;
    assert_eq!(out["action"], "not_refunded", "{out}");
    assert_eq!(available(&pool, &w.address).await, "100");
    let report = state.credits.reconcile().await.unwrap();
    assert!(report.ok, "{report:?}");

    // A refund that arrived before the purchase, then reversed: the purchase credits.
    let early = c.transaction(&tx_id(), PRODUCT, Some(&w.address));
    let (_, out) = notify(&app, c.notification("REFUND", Some(early.clone()))).await;
    assert_eq!(out["action"], "unknown_credit", "{out}");
    let (_, out) = notify(&app, c.notification("REFUND_REVERSED", Some(early))).await;
    assert_eq!(out["action"], "minted", "{out}");
    assert_eq!(available(&pool, &w.address).await, "200");
    drop(app);
    drop(state);
    own.drop().await;
}

#[tokio::test]
async fn a_partial_refund_charges_back_its_share() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let id = tx_id();
    let (_, out) = notify(
        &app,
        c.notification(
            "ONE_TIME_CHARGE",
            Some(c.transaction(&id, PRODUCT, Some(&w.address))),
        ),
    )
    .await;
    assert_eq!(out["action"], "minted", "{out}");
    let mut refunded = super::apple::testkit::transaction_payload(&id, PRODUCT, Some(&w.address));
    refunded["revocationDate"] = json!(1_700_000_000_000i64);
    refunded["revocationPercentage"] = json!(25_000);
    let (_, out) = notify(&app, c.notification("REFUND", Some(c.sign(&refunded)))).await;
    assert_eq!(out["action"], "refunded", "{out}");
    assert_eq!(available(&pool, &w.address).await, "75");
    let orders = test_state(pool.clone(), StateCfg::default())
        .credits
        .credit_orders_history(&w.address, 10, 0)
        .await
        .unwrap();
    assert_eq!(orders.orders[0].status, "partially_reversed");
}

#[tokio::test]
async fn the_device_path_answers_the_cap_codes() {
    let Some(own) = super::tests::OwnDatabase::create("iap_verify_caps").await else {
        return;
    };
    let pool = own.pool.clone();
    let c = chain(ChainSpec::default());
    let w = scratch_wallet();
    let token = account_token(&w.address);
    let wallet_capped = setup(
        &pool,
        &c,
        Limits {
            wallet_daily_cents: 500,
            ..open_limits()
        },
    );
    let verify = |app: Router, id: String| {
        let w = &w;
        let signed = c.transaction(&id, PRODUCT, Some(&token));
        async move {
            call(
                &app,
                w,
                "post",
                "/credits/credits/iap/verify",
                &[],
                Some(json!({ "jwsRepresentation": signed })),
            )
            .await
            .1
        }
    };
    let out = verify(wallet_capped.clone(), tx_id()).await;
    assert_eq!(
        (out["code"].as_str(), out["reason"].as_str()),
        (Some("cap_exceeded"), Some("daily_limit")),
        "{out}"
    );
    let node_capped = setup(
        &pool,
        &c,
        Limits {
            daily_cents: 500,
            ..open_limits()
        },
    );
    let out = verify(node_capped.clone(), tx_id()).await;
    assert_eq!(out["code"], "service_daily_limit", "{out}");
    let mut other = super::apple::testkit::transaction_payload(&tx_id(), "nope", Some(&token));
    other["quantity"] = json!(1);
    let (_, out) = call(
        &node_capped,
        &w,
        "post",
        "/credits/credits/iap/verify",
        &[],
        Some(json!({ "jwsRepresentation": c.sign(&other) })),
    )
    .await;
    assert_eq!(out["code"], "unknown_product", "{out}");
    assert_eq!(available(&pool, &w.address).await, "0");
    drop(wallet_capped);
    drop(node_capped);
    own.drop().await;
}

#[tokio::test]
async fn the_operator_credit_is_held_to_the_node_ceiling() {
    let Some(own) = super::tests::OwnDatabase::create("iap_operator_ceiling").await else {
        return;
    };
    let pool = own.pool.clone();
    let c = chain(ChainSpec::default());
    let app = setup_with_admin(
        &pool,
        &c,
        Limits {
            daily_cents: 500,
            ..open_limits()
        },
    );
    let w = scratch_wallet();
    let (status, _, out) = send(
        &app,
        "POST",
        "/credits/credits/iap",
        bearer(),
        Some(json!({ "userAddress": w.address, "appleTransactionId": tx_id(), "appleProductId": PRODUCT })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{out}");
    assert_eq!(available(&pool, &w.address).await, "0");
    assert!(
        held_ids(&app).await.is_empty(),
        "an operator credit records no held row"
    );
    drop(app);
    own.drop().await;
}

#[tokio::test]
async fn imported_purchases_count_toward_the_caps() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(
        &pool,
        &c,
        Limits {
            wallet_total_cents: 1500,
            ..open_limits()
        },
    );
    let w = scratch_wallet();
    sqlx::query(
        "INSERT INTO credits.iap_transactions (transaction_id, product_id, address, credits, amount_cents, status, created_at) \
         VALUES ($1, $2, $3, 100, 1000, 'imported', timezone('UTC', now()) - interval '30 days')",
    )
    .bind(tx_id())
    .bind(PRODUCT)
    .bind(&w.address)
    .execute(&pool)
    .await
    .unwrap();
    let (_, out) = call(
        &app,
        &w,
        "post",
        "/credits/credits/iap/quote",
        &[],
        Some(json!({ "productId": PRODUCT })),
    )
    .await;
    assert_eq!(
        (out["code"].as_str(), out["reason"].as_str()),
        (Some("cap_exceeded"), Some("total_limit")),
        "{out}"
    );
    // An imported purchase is not this node's credit order.
    let path = format!("/credits/users/{}/credit-orders", w.address);
    let (_, out) = call(&app, &w, "get", &path, &[], None).await;
    assert_eq!(out["total"], 0, "{out}");
}

#[tokio::test]
async fn credit_orders_page_through_card_and_app_store_rows_once_each() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    // Every row at one instant: only the tie-break orders them.
    sqlx::query(
        "INSERT INTO credits.purchases (address, sku, credits, amount_cents, currency, status, created_at) \
         VALUES ($1, 'pack', 10, 100, 'usd', 'paid', '2026-01-01')",
    )
    .bind(&w.address)
    .execute(&pool)
    .await
    .unwrap();
    for _ in 0..6 {
        sqlx::query(
            "INSERT INTO credits.iap_transactions (transaction_id, product_id, address, credits, amount_cents, status, credited_by, created_at) \
             VALUES ($1, $2, $3, 100, 1000, 'paid', 'device', '2026-01-01')",
        )
        .bind(tx_id())
        .bind(PRODUCT)
        .bind(&w.address)
        .execute(&pool)
        .await
        .unwrap();
    }
    let state = test_state(pool.clone(), StateCfg::default());
    let mut seen = std::collections::HashSet::new();
    for offset in 0..7 {
        let page = state
            .credits
            .credit_orders_history(&w.address, 1, offset)
            .await
            .unwrap();
        assert_eq!(page.total, 7);
        assert!(seen.insert(page.orders[0].id.clone()), "offset {offset}");
    }
    assert_eq!(seen.len(), 7);
    drop(app);
}

#[tokio::test]
async fn a_held_purchase_refunded_is_never_credited() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup_with_admin(
        &pool,
        &c,
        Limits {
            wallet_total_cents: 500,
            ..open_limits()
        },
    );
    let w = scratch_wallet();
    let id = tx_id();
    let signed = c.transaction(&id, PRODUCT, Some(&w.address));
    let (_, out) = notify(
        &app,
        c.notification("ONE_TIME_CHARGE", Some(signed.clone())),
    )
    .await;
    assert_eq!(out["held"], true, "{out}");
    let (_, out) = notify(&app, c.notification("REFUND", Some(signed))).await;
    assert_eq!(out["action"], "unknown_credit", "{out}");
    assert_eq!(row(&pool, &id).await.unwrap().0, "refunded");
    assert!(!held_ids(&app).await.contains(&id));
    let (status, _, out) = send(
        &app,
        "POST",
        "/credits/credits/iap",
        bearer(),
        Some(json!({ "userAddress": w.address, "appleTransactionId": id, "appleProductId": PRODUCT })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert_eq!(available(&pool, &w.address).await, "0");
}

#[tokio::test]
async fn an_imported_purchase_is_neither_credited_nor_charged_back_again() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let (usd, mana) = (tx_id(), tx_id());
    sqlx::query(
        "INSERT INTO credits.iap_transactions (transaction_id, product_id, address, credits, amount_cents, status) \
         VALUES ($1, $2, $3, 100, 1000, 'imported')",
    )
    .bind(&usd)
    .bind(PRODUCT)
    .bind(&w.address)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO credits.iap_transactions (transaction_id, product_id, status, refund_ref, refund_reason, refunded_at) \
         VALUES ($1, $2, 'refunded', 'credits-server', 'credited upstream', timezone('UTC', now()))",
    )
    .bind(&mana)
    .bind(PRODUCT)
    .execute(&pool)
    .await
    .unwrap();
    let signed = c.transaction(&usd, PRODUCT, Some(&w.address));
    let (_, out) = notify(
        &app,
        c.notification("ONE_TIME_CHARGE", Some(signed.clone())),
    )
    .await;
    assert_eq!(out["alreadyExisted"], true, "{out}");
    let (_, out) = notify(&app, c.notification("REFUND", Some(signed))).await;
    assert_eq!(out["action"], "unknown_credit", "{out}");
    assert_eq!(row(&pool, &usd).await.unwrap().0, "imported");

    // A reversed refund of an upstream purchase keeps its marker.
    let signed = c.transaction(&mana, PRODUCT, Some(&w.address));
    let (_, out) = notify(&app, c.notification("REFUND_REVERSED", Some(signed))).await;
    assert_eq!(out["action"], "not_refunded", "{out}");
    assert_eq!(row(&pool, &mana).await.unwrap().0, "refunded");
    assert_eq!(available(&pool, &w.address).await, "0");
}

/// A signed credit of `address` pledging `cents`, `age_secs` old, expiring
/// `expires_in_secs` from now; returns its hash.
async fn pledge(
    pool: &PgPool,
    address: &str,
    cents: i64,
    age_secs: i64,
    expires_in_secs: i64,
) -> String {
    let id = format!("auth-{}", uuid::Uuid::new_v4().simple());
    let hash = format!(
        "0x{}",
        hex::encode(alloy_primitives::keccak256(id.as_bytes()))
    );
    sqlx::query(
        "INSERT INTO credits.authorizations (id, address, usd_cents, amount_wei, status, expires_at, chain_id, contract, credit_hash, created_at) \
         VALUES ($1, $2, $3, 1, 'authorized', timezone('UTC', now()) + make_interval(secs => $4), 137, $5, $6, \
                 timezone('UTC', now()) - make_interval(secs => $7))",
    )
    .bind(&id)
    .bind(address)
    .bind(cents)
    .bind(expires_in_secs as f64)
    .bind("0x00000000000000000000000000000000000000c2")
    .bind(&hash)
    .bind(age_secs as f64)
    .execute(pool)
    .await
    .unwrap();
    hash
}

async fn queued(pool: &PgPool, hash: &str) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM credits.revocations WHERE credit_hash = $1)")
        .bind(hash)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn blocked(pool: &PgPool, address: &str) -> bool {
    sqlx::query_scalar("SELECT is_blocked_for_claiming FROM credits.wallets WHERE address = $1")
        .bind(address)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Credits `n` signed purchases of `PRODUCT` to `w`; returns their signed transactions.
async fn buy(app: &Router, c: &Chain, w: &Wallet, n: usize) -> Vec<String> {
    let mut signed = Vec::new();
    for _ in 0..n {
        let t = c.transaction(&tx_id(), PRODUCT, Some(&w.address));
        let (_, out) = notify(app, c.notification("ONE_TIME_CHARGE", Some(t.clone()))).await;
        assert_eq!(out["action"], "minted", "{out}");
        signed.push(t);
    }
    signed
}

#[tokio::test]
async fn an_operator_credits_a_held_purchase_as_apple_signed_it() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup_with_admin(
        &pool,
        &c,
        Limits {
            wallet_total_cents: 500,
            ..open_limits()
        },
    );
    let w = scratch_wallet();
    let id = tx_id();
    let mut p = super::apple::testkit::transaction_payload(&id, PRODUCT, Some(&w.address));
    p["quantity"] = json!(10);
    let (_, out) = notify(&app, c.notification("ONE_TIME_CHARGE", Some(c.sign(&p)))).await;
    assert_eq!(out["held"], true, "{out}");

    for body in [
        json!({ "userAddress": w.address, "appleTransactionId": id, "appleProductId": PRODUCT, "quantity": 1 }),
        json!({ "userAddress": w.address, "appleTransactionId": id, "appleProductId": "org.example.other" }),
    ] {
        let (status, _, out) =
            send(&app, "POST", "/credits/credits/iap", bearer(), Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{out}");
        assert_eq!(row(&pool, &id).await.unwrap().0, "held");
    }

    let (status, _, out) = send(
        &app,
        "POST",
        "/credits/credits/iap",
        bearer(),
        Some(json!({ "userAddress": w.address, "appleTransactionId": id, "appleProductId": PRODUCT })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["credits"], 1000);
    assert_eq!(available(&pool, &w.address).await, "1000");
}

#[tokio::test]
async fn an_operator_refund_of_a_held_purchase_reports_it() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup_with_admin(
        &pool,
        &c,
        Limits {
            wallet_total_cents: 500,
            ..open_limits()
        },
    );
    let w = scratch_wallet();
    let id = tx_id();
    let signed = c.transaction(&id, PRODUCT, Some(&w.address));
    let (_, out) = notify(&app, c.notification("ONE_TIME_CHARGE", Some(signed))).await;
    assert_eq!(out["held"], true, "{out}");
    let (status, _, out) = send(
        &app,
        "POST",
        "/credits/credits/iap/refund",
        bearer(),
        Some(json!({ "appleTransactionId": id, "refundId": "r-1", "reason": "support" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["held"], true);
    assert_eq!(out["removed"], "0");
    assert_eq!(row(&pool, &id).await.unwrap().0, "refunded");
}

#[tokio::test]
async fn a_later_refund_revokes_credits_an_earlier_one_did_not_queue() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 3).await;
    // 300 available; a newer 200 and an older 90 pledged.
    let newer = pledge(&pool, &w.address, 2000, 0, 300).await;
    let older = pledge(&pool, &w.address, 900, 60, 300).await;

    let (_, out) = notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    assert_eq!(out["revocations"], 1, "{out}");
    assert!(queued(&pool, &newer).await);
    assert!(!queued(&pool, &older).await);
    // 100 left covers the older 90.
    let (_, out) = notify(&app, c.notification("REFUND", Some(bought[1].clone()))).await;
    assert_eq!(out["revocations"], 0, "{out}");
    assert!(!queued(&pool, &older).await);
    // Nothing left: the older credit is revoked, though the newer one fills the window.
    let (_, out) = notify(&app, c.notification("REFUND", Some(bought[2].clone()))).await;
    assert_eq!(out["revocations"], 1, "{out}");
    assert!(queued(&pool, &older).await);
}

#[tokio::test]
async fn an_expired_pledge_does_not_widen_the_revocations() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 2).await;
    // 200 available; 150 on a credit past its expiry (not swept yet), 50 and 40 live.
    let expired = pledge(&pool, &w.address, 1500, 0, -60).await;
    let live_newer = pledge(&pool, &w.address, 500, 30, 300).await;
    let live_older = pledge(&pool, &w.address, 400, 60, 300).await;

    let (_, out) = notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    assert_eq!(out["action"], "refunded", "{out}");
    // The 100 left covers the 90 still spendable.
    assert_eq!(out["revocations"], 0, "{out}");
    for hash in [&expired, &live_newer, &live_older] {
        assert!(!queued(&pool, hash).await);
    }
    assert!(
        blocked(&pool, &w.address).await,
        "the block still counts every pledge"
    );
}

#[tokio::test]
async fn a_reversed_refund_withdraws_its_unsent_revocations_and_lifts_its_block() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());

    // The refund blocked the wallet and queued two credits; one was sent since.
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 1).await;
    let unsent = pledge(&pool, &w.address, 400, 0, 300).await;
    let sent = pledge(&pool, &w.address, 400, 60, 300).await;
    let (_, out) = notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    assert_eq!(out["revocations"], 2, "{out}");
    assert!(blocked(&pool, &w.address).await);
    sqlx::query(
        "UPDATE credits.revocations SET status = 'sent', tx_hash = $2 WHERE credit_hash = $1",
    )
    .bind(&sent)
    .bind(format!("0x{}", "ab".repeat(32)))
    .execute(&pool)
    .await
    .unwrap();
    let (_, out) = notify(
        &app,
        c.notification("REFUND_REVERSED", Some(bought[0].clone())),
    )
    .await;
    assert_eq!(out["action"], "reinstated", "{out}");
    assert_eq!(out["revocationsWithdrawn"], 1, "{out}");
    assert!(!queued(&pool, &unsent).await);
    assert!(queued(&pool, &sent).await, "a sent revocation is final");
    assert!(!blocked(&pool, &w.address).await);

    // A wallet already blocked stays blocked.
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 1).await;
    pledge(&pool, &w.address, 800, 0, 300).await;
    sqlx::query("UPDATE credits.wallets SET is_blocked_for_claiming = true WHERE address = $1")
        .bind(&w.address)
        .execute(&pool)
        .await
        .unwrap();
    let (_, out) = notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    assert_eq!(out["action"], "refunded", "{out}");
    let (_, out) = notify(
        &app,
        c.notification("REFUND_REVERSED", Some(bought[0].clone())),
    )
    .await;
    assert_eq!(out["action"], "reinstated", "{out}");
    assert!(blocked(&pool, &w.address).await);
}

#[tokio::test]
async fn a_reversed_refund_keeps_a_revocation_a_later_refund_relies_on() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 2).await;
    // 200 available; a newer 150 and an older 40 pledged.
    let newer = pledge(&pool, &w.address, 1500, 0, 300).await;
    let older = pledge(&pool, &w.address, 400, 60, 300).await;
    let (_, out) = notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    assert_eq!(out["revocations"], 1, "{out}");
    let (_, out) = notify(&app, c.notification("REFUND", Some(bought[1].clone()))).await;
    assert_eq!(out["revocations"], 1, "{out}");
    assert!(queued(&pool, &older).await);

    // 100 back cannot cover the newer 150 once more.
    let (_, out) = notify(
        &app,
        c.notification("REFUND_REVERSED", Some(bought[0].clone())),
    )
    .await;
    assert_eq!(out["action"], "reinstated", "{out}");
    assert_eq!(out["revocationsWithdrawn"], 0, "{out}");
    assert!(queued(&pool, &newer).await);
    assert!(blocked(&pool, &w.address).await);
}

#[tokio::test]
async fn a_reversed_refund_keeps_a_revocation_the_job_claimed() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());

    // The job claimed the revocation before the reversal: it may be on chain.
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 1).await;
    let claimed = pledge(&pool, &w.address, 400, 0, 300).await;
    let (_, out) = notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    assert_eq!(out["revocations"], 1, "{out}");
    assert!(
        super::settlement::claim_revocation(&pool, &claimed, 0)
            .await
            .unwrap()
    );
    let (_, out) = notify(
        &app,
        c.notification("REFUND_REVERSED", Some(bought[0].clone())),
    )
    .await;
    assert_eq!(out["action"], "reinstated", "{out}");
    assert_eq!(out["revocationsWithdrawn"], 0, "{out}");
    assert!(queued(&pool, &claimed).await);

    // The reversal withdrew it first: the job's claim finds nothing to send.
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 1).await;
    let withdrawn = pledge(&pool, &w.address, 400, 0, 300).await;
    notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    let (_, out) = notify(
        &app,
        c.notification("REFUND_REVERSED", Some(bought[0].clone())),
    )
    .await;
    assert_eq!(out["revocationsWithdrawn"], 1, "{out}");
    assert!(
        !super::settlement::claim_revocation(&pool, &withdrawn, 0)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_sent_revocation_keeps_its_failure_budget_for_polling() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 1).await;
    let hash = pledge(&pool, &w.address, 400, 0, 300).await;
    notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    let last = super::settlement::REVOCATION_MAX_ATTEMPTS - 1;
    sqlx::query("UPDATE credits.revocations SET attempts = $2 WHERE credit_hash = $1")
        .bind(&hash)
        .bind(last)
        .execute(&pool)
        .await
        .unwrap();
    // The last allowed broadcast succeeds: the row is still polled for its receipt.
    assert!(
        super::settlement::claim_revocation(&pool, &hash, last)
            .await
            .unwrap()
    );
    let tx = format!("0x{}", "ab".repeat(32));
    super::settlement::mark_revocation(&pool, &hash, "sent", Some(&tx), None, -1)
        .await
        .unwrap();
    let (status, attempts): (String, i32) =
        sqlx::query_as("SELECT status, attempts FROM credits.revocations WHERE credit_hash = $1")
            .bind(&hash)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((status.as_str(), attempts), ("sent", last));
    assert!(attempts < super::settlement::REVOCATION_MAX_ATTEMPTS);
}

async fn unblock_audits(pool: &PgPool, address: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM ops.admin_audit \
         WHERE domain = 'credits' AND action = 'user.unblock' AND actor = 'apple' AND target = $1",
    )
    .bind(address)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_reversed_refund_audits_the_block_it_lifts() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 1).await;
    pledge(&pool, &w.address, 400, 0, 300).await;
    notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    assert!(blocked(&pool, &w.address).await);
    let (_, out) = notify(
        &app,
        c.notification("REFUND_REVERSED", Some(bought[0].clone())),
    )
    .await;
    assert_eq!(out["action"], "reinstated", "{out}");
    assert!(!blocked(&pool, &w.address).await);
    assert_eq!(unblock_audits(&pool, &w.address).await, 1);
}

#[tokio::test]
async fn a_reversed_refund_leaves_a_block_an_operator_set_again() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let state = test_state(
        pool.clone(),
        StateCfg {
            iap: Some(iap(&c, open_limits())),
            ..StateCfg::default()
        },
    );
    let app = app(state.clone());
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 1).await;
    pledge(&pool, &w.address, 400, 0, 300).await;
    notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    assert!(blocked(&pool, &w.address).await);
    // The operator reviewed the refund and kept the block.
    state
        .credits
        .admin_set_blocked(
            &w.address,
            true,
            Some("review"),
            Some("token:op"),
            &json!({}),
        )
        .await
        .unwrap();
    let (_, out) = notify(
        &app,
        c.notification("REFUND_REVERSED", Some(bought[0].clone())),
    )
    .await;
    assert_eq!(out["action"], "reinstated", "{out}");
    assert!(blocked(&pool, &w.address).await);
    assert_eq!(unblock_audits(&pool, &w.address).await, 0);
}

#[tokio::test]
async fn a_reversed_refund_leaves_a_block_a_later_reversal_relies_on() {
    let Some(pool) = pool().await else { return };
    let c = chain(ChainSpec::default());
    let app = setup(&pool, &c, open_limits());
    let w = scratch_wallet();
    let bought = buy(&app, &c, &w, 2).await;
    // 200 available, 150 pledged: each refund leaves the pledge uncovered.
    pledge(&pool, &w.address, 1500, 0, 300).await;
    notify(&app, c.notification("REFUND", Some(bought[0].clone()))).await;
    notify(&app, c.notification("REFUND", Some(bought[1].clone()))).await;
    assert!(blocked(&pool, &w.address).await);
    // Both reversed: the balance covers the pledge again, but the block is
    // no longer the first refund's alone.
    for signed in [&bought[1], &bought[0]] {
        let (_, out) = notify(
            &app,
            c.notification("REFUND_REVERSED", Some(signed.clone())),
        )
        .await;
        assert_eq!(out["action"], "reinstated", "{out}");
    }
    assert_eq!(available(&pool, &w.address).await, "200");
    assert!(blocked(&pool, &w.address).await);
}

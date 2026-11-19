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
async fn a_refund_the_balance_cannot_cover_queues_live_signed_credits_for_revocation() {
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

    let (_, out) = notify(&app, c.notification("REFUND", Some(signed_tx))).await;
    assert_eq!(out["action"], "refunded", "{out}");
    assert_eq!(out["revocations"], 1);
    let (reason, status): (String, String) =
        sqlx::query_as("SELECT reason, status FROM credits.revocations WHERE credit_hash = $1")
            .bind(&hash)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((reason.as_str(), status.as_str()), ("reversed", "pending"));
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

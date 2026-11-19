// from: credits-server@d9c6a261f:src/controllers/handlers/apple-iap-webhook.ts, iap-verify.ts, iap-register.ts, iap-quote.ts, mint-iap-credit.ts, refund-iap-credit.ts
//! The App Store routes (`credits::iap`):
//!
//! - `POST /apple/webhook` (`/v1/apple/webhook`): App Store Server
//!   Notifications V2. The signed payload is the authentication: a payload
//!   that does not verify answers 400; once verified, `ONE_TIME_CHARGE`
//!   credits the wallet registered for the transaction's account token and
//!   `REFUND` reverses it. Every other outcome answers 200 except the ones
//!   Apple should retry: an unknown product, a purchase over the node's daily
//!   ceiling and a node without App Store configuration answer 503.
//! - `POST /credits/iap/verify` (`/v1/iap/verify`, signed fetch): the device
//!   posts StoreKit's `jwsRepresentation`; it credits the signer only when the
//!   transaction's account token is the signer's. Business outcomes answer
//!   200 with `ok: false` and a `code`.
//! - `POST /credits/iap/register` (`/v1/iap/register`, signed fetch): records
//!   the signer's account token.
//! - `POST /credits/iap/quote` (`/v1/iap/quote`, signed fetch): whether the
//!   signer may buy `productId` under the caps; an allowed quote records the
//!   account token.
//! - operator `POST /credits/iap` (`/admin/iap/credits`) and
//!   `POST /credits/iap/refund` (`/admin/iap/refunds`): the same credit and
//!   reversal for support, behind the operator bearer.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, Uri};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};

use super::signer_from;
use crate::credits::SharedState;
use crate::credits::admin::{RequireAdmin, normalize_address};
use crate::credits::errors::ApiError;
use crate::credits::iap::{CapReason, Channel, CreditOutcome, Iap, Purchase, RefundOutcome};
use crate::types::UsdCents;

fn configured(state: &SharedState) -> Result<&Iap, ApiError> {
    state.settings.iap.as_deref().ok_or_else(|| {
        ApiError::service_unavailable("App Store purchases are not configured on this node")
    })
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Debug, Deserialize)]
pub struct WebhookBody {
    #[serde(rename = "signedPayload")]
    signed_payload: Option<String>,
}

pub async fn webhook(
    State(state): State<SharedState>,
    body: Result<Json<WebhookBody>, JsonRejection>,
) -> Result<Json<JsonValue>, ApiError> {
    let iap = configured(&state)?;
    let Json(body) = body.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let signed = body
        .signed_payload
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("missing signedPayload"))?;
    let notification = iap.verifier.notification(&signed, now()).map_err(|e| {
        tracing::warn!(error = %e, "app store notification refused");
        ApiError::bad_request("invalid signedPayload")
    })?;
    let uuid = notification.notification_uuid.as_str();
    let Some(t) = notification.transaction else {
        tracing::info!(notification_uuid = %uuid, kind = %notification.notification_type, "app store notification without a transaction");
        return Ok(Json(json!({ "ok": true, "skipped": true })));
    };
    match notification.notification_type.as_str() {
        "ONE_TIME_CHARGE" => {
            let Some(address) = state
                .credits
                .wallet_for_iap_token(t.app_account_token.as_deref())
                .await?
            else {
                tracing::error!(notification_uuid = %uuid, transaction_id = %t.transaction_id, "app store purchase names no registered wallet: paid but not credited, reconcile by hand");
                return Ok(Json(
                    json!({ "ok": true, "error": "unresolvable_app_account_token" }),
                ));
            };
            let outcome = state
                .credits
                .credit_iap(
                    iap,
                    Purchase {
                        address: &address,
                        transaction_id: &t.transaction_id,
                        original_transaction_id: t.original_transaction_id.as_deref(),
                        product_id: &t.product_id,
                        quantity: t.quantity,
                        revoked: t.revoked,
                        channel: Channel::Notification,
                    },
                )
                .await?;
            match outcome {
                CreditOutcome::Credited { already, .. } => Ok(Json(
                    json!({ "ok": true, "action": "minted", "creditId": t.transaction_id, "alreadyExisted": already }),
                )),
                CreditOutcome::Revoked => {
                    Ok(Json(json!({ "ok": true, "error": "revoked_transaction" })))
                }
                CreditOutcome::Refunded => {
                    Ok(Json(json!({ "ok": true, "error": "refunded_transaction" })))
                }
                CreditOutcome::UnknownProduct => {
                    tracing::error!(notification_uuid = %uuid, transaction_id = %t.transaction_id, product_id = %t.product_id, "app store purchase of a product outside IAP_PRODUCT_CATALOG_USD: paid but not credited");
                    Err(ApiError::service_unavailable("unknown_product"))
                }
                CreditOutcome::Capped(CapReason::ServiceDailyLimit) => {
                    tracing::error!(notification_uuid = %uuid, transaction_id = %t.transaction_id, "app store purchase over IAP_DAILY_LIMIT_CENTS: answered for a later retry");
                    Err(ApiError::service_unavailable("service_daily_limit"))
                }
                CreditOutcome::Capped(reason) => {
                    tracing::error!(notification_uuid = %uuid, transaction_id = %t.transaction_id, address = %address, reason = reason.as_str(), "app store purchase over a cap: paid but not credited, reconcile by hand");
                    Ok(Json(
                        json!({ "ok": true, "error": "wallet_cap_exceeded", "reason": reason.as_str() }),
                    ))
                }
            }
        }
        "REFUND" => {
            let reason = format!(
                "apple_refund:{}",
                notification.subtype.as_deref().unwrap_or("unspecified")
            );
            match state
                .credits
                .refund_iap(&t.transaction_id, Some(&t.product_id), uuid, &reason)
                .await?
            {
                RefundOutcome::Reversed {
                    removed,
                    revocations,
                    ..
                } => Ok(Json(json!({
                    "ok": true, "action": "refunded", "creditId": t.transaction_id,
                    "removed": removed, "revocations": revocations,
                }))),
                RefundOutcome::AlreadyRefunded => {
                    Ok(Json(json!({ "ok": true, "action": "already_refunded" })))
                }
                RefundOutcome::NotCredited => {
                    tracing::warn!(notification_uuid = %uuid, transaction_id = %t.transaction_id, "app store refund of a transaction never credited: recorded so it is never credited");
                    Ok(Json(json!({ "ok": true, "action": "unknown_credit" })))
                }
            }
        }
        "TEST" => Ok(Json(json!({ "ok": true, "test": true }))),
        other => {
            tracing::info!(notification_uuid = %uuid, kind = %other, "app store notification type not acted on");
            Ok(Json(json!({ "ok": true, "ignored": true })))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct VerifyBody {
    #[serde(rename = "jwsRepresentation")]
    jws_representation: Option<String>,
}

fn refused(code: &str) -> Json<JsonValue> {
    Json(json!({ "ok": false, "code": code }))
}

pub async fn verify(
    State(state): State<SharedState>,
    original: OriginalUri,
    uri: Uri,
    headers: HeaderMap,
    body: Result<Json<VerifyBody>, JsonRejection>,
) -> Result<Json<JsonValue>, ApiError> {
    let iap = configured(&state)?;
    let signer = signer_from(&state, &headers, "post", uri.path(), &original).await?;
    let wallet = signer.as_str().to_string();
    let Json(body) = body.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let jws = body
        .jws_representation
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("jwsRepresentation is required"))?;
    let t = match iap.verifier.transaction(&jws, now()) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(wallet = %wallet, error = %e, "app store transaction refused");
            return Ok(refused("invalid_jws"));
        }
    };
    // The transaction must have been bought for the signer's account.
    let token = t
        .app_account_token
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if token != crate::credits::apple::account_token(&wallet) && token != wallet {
        tracing::warn!(wallet = %wallet, transaction_id = %t.transaction_id, "app store transaction bought for another account");
        return Ok(refused("token_mismatch"));
    }
    let outcome = state
        .credits
        .credit_iap(
            iap,
            Purchase {
                address: &wallet,
                transaction_id: &t.transaction_id,
                original_transaction_id: t.original_transaction_id.as_deref(),
                product_id: &t.product_id,
                quantity: t.quantity,
                revoked: t.revoked,
                channel: Channel::Device,
            },
        )
        .await?;
    Ok(match outcome {
        CreditOutcome::Credited { already, .. } => Json(json!({
            "ok": true,
            "data": { "credited": true, "creditId": t.transaction_id, "alreadyExisted": already },
        })),
        CreditOutcome::Revoked => refused("revoked_transaction"),
        CreditOutcome::Refunded => refused("refunded_transaction"),
        CreditOutcome::UnknownProduct => {
            tracing::error!(wallet = %wallet, transaction_id = %t.transaction_id, product_id = %t.product_id, "app store purchase of a product outside IAP_PRODUCT_CATALOG_USD: paid but not credited");
            refused("unknown_product")
        }
        CreditOutcome::Capped(reason) if reason.as_str() == "service_daily_limit" => {
            refused("service_daily_limit")
        }
        CreditOutcome::Capped(reason) => {
            Json(json!({ "ok": false, "code": "cap_exceeded", "reason": reason.as_str() }))
        }
    })
}

pub async fn register(
    State(state): State<SharedState>,
    original: OriginalUri,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Json<JsonValue>, ApiError> {
    configured(&state)?;
    let signer = signer_from(&state, &headers, "post", uri.path(), &original).await?;
    let token = state.credits.link_iap_token(signer.as_str()).await?;
    Ok(Json(
        json!({ "ok": true, "data": { "appAccountToken": token } }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct QuoteBody {
    #[serde(rename = "productId")]
    product_id: Option<String>,
}

pub async fn quote(
    State(state): State<SharedState>,
    original: OriginalUri,
    uri: Uri,
    headers: HeaderMap,
    body: Result<Json<QuoteBody>, JsonRejection>,
) -> Result<Json<JsonValue>, ApiError> {
    let iap = configured(&state)?;
    let signer = signer_from(&state, &headers, "post", uri.path(), &original).await?;
    let wallet = signer.as_str();
    let Json(body) = body.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let product_id = body
        .product_id
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("productId is required"))?;
    let Some(credits) = iap.credits_per_unit(&product_id) else {
        return Ok(Json(
            json!({ "ok": false, "code": "unknown_product", "data": { "productId": product_id } }),
        ));
    };
    let cents = UsdCents::from_credits(credits).map_or(i64::MAX, UsdCents::get);
    let totals = crate::credits::store::Credits::iap_totals(&state.credits.pool, wallet).await?;
    let refusal = totals.decide(&iap.limits, cents);
    let token = match refusal {
        None => Some(state.credits.link_iap_token(wallet).await?),
        Some(_) => None,
    };
    let remaining = |cap: i64, used: i64| (cap > 0).then(|| (cap - used).max(0) as f64 / 100.0);
    let data = json!({
        "productId": product_id,
        "denomination": "USD",
        "amount": credits,
        "credits": credits,
        "appAccountToken": token,
        "balance": totals.total_cents as f64 / 100.0,
        "totalRemaining": remaining(iap.limits.wallet_total_cents, totals.total_cents),
        "dailyRemaining": remaining(iap.limits.wallet_daily_cents, totals.daily_cents),
    });
    Ok(Json(match refusal {
        None => json!({ "ok": true, "data": data }),
        Some(r) if r.as_str() == "service_daily_limit" => {
            json!({ "ok": false, "code": "service_daily_limit", "data": data })
        }
        Some(r) => {
            json!({ "ok": false, "code": "cap_exceeded", "reason": r.as_str(), "data": data })
        }
    }))
}

#[derive(Debug, Deserialize)]
pub struct AdminCreditBody {
    #[serde(rename = "userAddress")]
    user_address: String,
    #[serde(rename = "appleTransactionId")]
    apple_transaction_id: String,
    #[serde(rename = "appleOriginalTransactionId")]
    apple_original_transaction_id: Option<String>,
    #[serde(rename = "appleProductId")]
    apple_product_id: String,
    #[serde(default = "one")]
    quantity: i64,
}

fn one() -> i64 {
    1
}

fn validate_id(name: &str, v: &str) -> Result<(), ApiError> {
    if v.is_empty() || v.len() > 128 {
        return Err(ApiError::bad_request(format!(
            "{name} must be 1 to 128 characters"
        )));
    }
    Ok(())
}

/// Operator credit of one App Store transaction, by its product (never an amount).
pub async fn admin_credit(
    _admin: RequireAdmin,
    State(state): State<SharedState>,
    body: Result<Json<AdminCreditBody>, JsonRejection>,
) -> Result<Json<JsonValue>, ApiError> {
    let iap = configured(&state)?;
    let Json(b) = body.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let address = normalize_address(&b.user_address)?;
    validate_id("appleTransactionId", &b.apple_transaction_id)?;
    validate_id("appleProductId", &b.apple_product_id)?;
    let outcome = state
        .credits
        .credit_iap(
            iap,
            Purchase {
                address: &address,
                transaction_id: &b.apple_transaction_id,
                original_transaction_id: b.apple_original_transaction_id.as_deref(),
                product_id: &b.apple_product_id,
                quantity: b.quantity,
                revoked: false,
                channel: Channel::Operator,
            },
        )
        .await?;
    match outcome {
        CreditOutcome::Credited {
            address: credited,
            credits,
            already,
        } => {
            if credited != address {
                return Err(ApiError::conflict(
                    "this transaction already credited another wallet",
                ));
            }
            Ok(Json(json!({
                "creditId": b.apple_transaction_id, "appleTransactionId": b.apple_transaction_id,
                "credits": credits, "alreadyExisted": already,
            })))
        }
        CreditOutcome::Refunded => Err(ApiError::conflict("this transaction was refunded")),
        CreditOutcome::Revoked => Err(ApiError::conflict("this transaction was revoked")),
        CreditOutcome::UnknownProduct => {
            Err(ApiError::bad_request("unknown appleProductId or quantity"))
        }
        CreditOutcome::Capped(r) => Err(ApiError::unprocessable(format!(
            "over a cap: {}",
            r.as_str()
        ))),
    }
}

#[derive(Debug, Deserialize)]
pub struct AdminRefundBody {
    #[serde(rename = "appleTransactionId")]
    apple_transaction_id: String,
    #[serde(rename = "refundId")]
    refund_id: String,
    reason: String,
}

pub async fn admin_refund(
    _admin: RequireAdmin,
    State(state): State<SharedState>,
    body: Result<Json<AdminRefundBody>, JsonRejection>,
) -> Result<Json<JsonValue>, ApiError> {
    configured(&state)?;
    let Json(b) = body.map_err(|e| ApiError::bad_request(e.body_text()))?;
    validate_id("appleTransactionId", &b.apple_transaction_id)?;
    validate_id("refundId", &b.refund_id)?;
    if b.reason.is_empty() || b.reason.len() > 200 {
        return Err(ApiError::bad_request("reason must be 1 to 200 characters"));
    }
    match state
        .credits
        .refund_iap(&b.apple_transaction_id, None, &b.refund_id, &b.reason)
        .await?
    {
        RefundOutcome::Reversed {
            removed,
            shortfall,
            revocations,
            ..
        } => Ok(Json(json!({
            "creditId": b.apple_transaction_id, "appleTransactionId": b.apple_transaction_id,
            "refundId": b.refund_id, "removed": removed, "shortfall": shortfall, "revocations": revocations,
        }))),
        RefundOutcome::AlreadyRefunded => {
            Err(ApiError::conflict("this transaction was already refunded"))
        }
        RefundOutcome::NotCredited => Err(ApiError::not_found(
            "no credited App Store transaction with this id",
        )),
    }
}

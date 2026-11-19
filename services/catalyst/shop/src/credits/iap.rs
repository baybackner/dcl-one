// from: credits-server@d9c6a261f:src/logic/iap-mint.ts (mintIapCreditWithCapCheck, mintIapUsdTopUp), src/logic/iap-refund.ts (processRefund, refundUsd), src/logic/iap-quote.ts (evaluateIapPurchase), src/logic/iap-product-catalog.ts (IAP_PRODUCT_CATALOG_USD)
//! App Store purchases of Credits (`credits.iap_transactions`,
//! `credits.iap_account_tokens`).
//!
//! A purchase is credited from Apple's signed transaction only: the product
//! decides the Credits (`IAP_PRODUCT_CATALOG_USD`, product id to whole
//! Credits per unit, times the signed quantity), never the client. One
//! transaction id credits once, whichever path delivers it first (the
//! server notification, the device's `/iap/verify`, an operator), and a
//! transaction Apple revoked or refunded is never credited. The per-wallet
//! caps (`IAP_WALLET_TOTAL_LIMIT_CENTS`, `IAP_WALLET_DAILY_LIMIT_CENTS`, 0
//! uncapped) and the node's daily ceiling (`IAP_DAILY_LIMIT_CENTS`, 0
//! closed) are checked under one advisory lock with the insert, over the
//! last 24 hours; an operator credit, which reconciles a paid purchase a
//! wallet cap refused, is held to the node's ceiling only.
//!
//! A refund charges the purchase's Credits back from the wallet in the
//! refund's transaction (`Credits::revoke_in_tx`, floored at zero, the wallet
//! blocked when the balance net of live pledges cannot cover it). When it
//! cannot, every live signed credit of the wallet is queued for
//! `revokeCredits` on chain (`credits.revocations`), so Credits already
//! signed cannot be spent after the refund. A refund of a transaction never
//! credited leaves a `refunded` row that keeps it from being credited later.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use sqlx::Row;

use super::admin_store::GrantRequest;
use super::apple::{Environment, Verifier};
use super::errors::ApiError;
use super::settlement::enqueue_revocation;
use super::store::Credits;
use crate::types::UsdCents;

pub const IAP_ACTOR: &str = "apple";
/// The most units one signed transaction is credited for.
pub const MAX_QUANTITY: i64 = 10;
const DAILY_LOCK: &str = "apple-iap-daily";

/// The raw variables (`APPLE_*`, `IAP_*`).
#[derive(Clone, Default)]
pub struct RawSettings {
    /// `APPLE_BUNDLE_ID`: unset, the App Store routes answer 503.
    pub bundle_id: Option<String>,
    /// `APPLE_APP_APPLE_ID`: required in production.
    pub app_apple_id: Option<i64>,
    /// `APPLE_ENVIRONMENT` (sandbox).
    pub environment: Option<String>,
    /// `APPLE_ROOT_CERTS_DIR`: Apple's root certificates, DER `.cer`/`.der` files.
    pub root_certs_dir: Option<String>,
    /// `IAP_PRODUCT_CATALOG_USD`: `{"<productId>": <credits>}`.
    pub catalog: Option<String>,
    /// `IAP_WALLET_TOTAL_LIMIT_CENTS` (0: uncapped).
    pub wallet_total_limit_cents: i64,
    /// `IAP_WALLET_DAILY_LIMIT_CENTS` (0: uncapped).
    pub wallet_daily_limit_cents: i64,
    /// `IAP_DAILY_LIMIT_CENTS`: the node's ceiling (0: closed); required with `APPLE_BUNDLE_ID`.
    pub daily_limit_cents: Option<i64>,
}

#[derive(Clone)]
pub struct Iap {
    pub verifier: Verifier,
    /// Product id to whole Credits per unit.
    pub catalog: HashMap<String, i64>,
    pub limits: Limits,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    pub wallet_total_cents: i64,
    pub wallet_daily_cents: i64,
    pub daily_cents: i64,
}

impl Iap {
    /// `None` without `APPLE_BUNDLE_ID`; a partial configuration refuses to start.
    pub fn parse(raw: RawSettings) -> anyhow::Result<Option<Arc<Self>>> {
        let Some(bundle_id) = raw.bundle_id.filter(|s| !s.trim().is_empty()) else {
            return Ok(None);
        };
        let environment = Environment::parse(raw.environment.as_deref())?;
        let dir = raw
            .root_certs_dir
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("APPLE_ROOT_CERTS_DIR is required with APPLE_BUNDLE_ID")
            })?;
        let roots = load_roots(Path::new(dir.trim()))?;
        let verifier = Verifier::new(roots, bundle_id, raw.app_apple_id, environment)?;
        let catalog = parse_catalog(raw.catalog.as_deref().unwrap_or("{}"))?;
        let daily_cents = raw.daily_limit_cents.ok_or_else(|| {
            anyhow::anyhow!("IAP_DAILY_LIMIT_CENTS is required with APPLE_BUNDLE_ID")
        })?;
        if daily_cents < 0 || raw.wallet_total_limit_cents < 0 || raw.wallet_daily_limit_cents < 0 {
            anyhow::bail!("the IAP_*_LIMIT_CENTS values must not be negative");
        }
        Ok(Some(Arc::new(Self {
            verifier,
            catalog,
            limits: Limits {
                wallet_total_cents: raw.wallet_total_limit_cents,
                wallet_daily_cents: raw.wallet_daily_limit_cents,
                daily_cents,
            },
        })))
    }

    /// Whole Credits per unit of `product_id`.
    pub fn credits_per_unit(&self, product_id: &str) -> Option<i64> {
        self.catalog.get(product_id).copied()
    }
}

fn load_roots(dir: &Path) -> anyhow::Result<Vec<Vec<u8>>> {
    let mut roots = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("APPLE_ROOT_CERTS_DIR cannot be read: {e}"))?
    {
        let path = entry?.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if matches!(ext.as_deref(), Some("cer" | "der")) {
            roots.push(std::fs::read(&path)?);
        }
    }
    Ok(roots)
}

pub fn parse_catalog(raw: &str) -> anyhow::Result<HashMap<String, i64>> {
    let parsed: HashMap<String, serde_json::Value> = serde_json::from_str(raw)
        .map_err(|_| anyhow::anyhow!("IAP_PRODUCT_CATALOG_USD must be a JSON object"))?;
    parsed
        .into_iter()
        .map(|(product, credits)| match credits.as_i64() {
            Some(c) if c > 0 && !product.is_empty() => Ok((product, c)),
            _ => anyhow::bail!(
                "IAP_PRODUCT_CATALOG_USD maps each product id to a positive whole number of Credits"
            ),
        })
        .collect()
}

/// Which path credited a transaction (`credits.iap_transactions.credited_by`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Notification,
    Device,
    Operator,
}

impl Channel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Notification => "notification",
            Self::Device => "device",
            Self::Operator => "operator",
        }
    }
}

pub struct Purchase<'a> {
    pub address: &'a str,
    pub transaction_id: &'a str,
    pub original_transaction_id: Option<&'a str>,
    pub product_id: &'a str,
    pub quantity: i64,
    pub revoked: bool,
    pub channel: Channel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapReason {
    TotalLimit,
    DailyLimit,
    ServiceDailyLimit,
}

impl CapReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TotalLimit => "total_limit",
            Self::DailyLimit => "daily_limit",
            Self::ServiceDailyLimit => "service_daily_limit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreditOutcome {
    /// Credited now, or earlier by another path (`already`), to `address`.
    Credited {
        address: String,
        credits: i64,
        already: bool,
    },
    /// Apple's signed transaction is revoked.
    Revoked,
    /// The transaction was refunded; it is never credited.
    Refunded,
    UnknownProduct,
    Capped(CapReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefundOutcome {
    Reversed {
        address: String,
        removed: String,
        shortfall: String,
        /// Live signed credits queued for on-chain revocation.
        revocations: usize,
    },
    AlreadyRefunded,
    /// No credited purchase: a `refunded` row now keeps it from being credited.
    NotCredited,
}

/// What a wallet bought and may still buy (cents; `None` uncapped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    pub total_cents: i64,
    pub daily_cents: i64,
    pub service_daily_cents: i64,
}

impl Totals {
    pub fn decide(&self, limits: &Limits, requested_cents: i64) -> Option<CapReason> {
        if limits.wallet_total_cents > 0
            && self.total_cents + requested_cents > limits.wallet_total_cents
        {
            return Some(CapReason::TotalLimit);
        }
        if limits.wallet_daily_cents > 0
            && self.daily_cents + requested_cents > limits.wallet_daily_cents
        {
            return Some(CapReason::DailyLimit);
        }
        if self.service_daily_cents + requested_cents > limits.daily_cents {
            return Some(CapReason::ServiceDailyLimit);
        }
        None
    }
}

async fn lock(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, key: &str) -> Result<(), ApiError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn grant_key(transaction_id: &str) -> String {
    format!("apple-iap:{transaction_id}")
}

impl Credits {
    pub async fn iap_totals<'e, E: sqlx::PgExecutor<'e>>(
        executor: E,
        address: &str,
    ) -> Result<Totals, ApiError> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(amount_cents) FILTER (WHERE address = $1), 0)::bigint AS total, \
                    COALESCE(SUM(amount_cents) FILTER (WHERE address = $1 AND created_at > timezone('UTC', now()) - interval '24 hours'), 0)::bigint AS daily, \
                    COALESCE(SUM(amount_cents) FILTER (WHERE created_at > timezone('UTC', now()) - interval '24 hours'), 0)::bigint AS service \
             FROM credits.iap_transactions WHERE address IS NOT NULL \
               AND (address = $1 OR created_at > timezone('UTC', now()) - interval '24 hours')",
        )
        .bind(address)
        .fetch_one(executor)
        .await?;
        Ok(Totals {
            total_cents: row.get("total"),
            daily_cents: row.get("daily"),
            service_daily_cents: row.get("service"),
        })
    }

    /// Credits one App Store transaction once; see the module docs.
    pub async fn credit_iap(&self, iap: &Iap, p: Purchase<'_>) -> Result<CreditOutcome, ApiError> {
        if p.revoked {
            return Ok(CreditOutcome::Revoked);
        }
        let mut tx = self.pool.begin().await?;
        lock(&mut tx, &grant_key(p.transaction_id)).await?;
        let existing = sqlx::query(
            "SELECT status, address, floor(credits)::bigint AS credits FROM credits.iap_transactions \
             WHERE transaction_id = $1",
        )
        .bind(p.transaction_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = existing {
            tx.commit().await?;
            let status: String = row.get("status");
            return Ok(
                match (status.as_str(), row.get::<Option<String>, _>("address")) {
                    ("paid", Some(address)) => CreditOutcome::Credited {
                        address,
                        credits: row.get::<Option<i64>, _>("credits").unwrap_or(0),
                        already: true,
                    },
                    _ => CreditOutcome::Refunded,
                },
            );
        }
        let Some(per_unit) = iap.credits_per_unit(p.product_id) else {
            return Ok(CreditOutcome::UnknownProduct);
        };
        if !(1..=MAX_QUANTITY).contains(&p.quantity) {
            return Ok(CreditOutcome::UnknownProduct);
        }
        let credits = per_unit * p.quantity;
        let cents = UsdCents::from_credits(credits)
            .ok_or_else(|| ApiError::bad_request("credits out of range"))?
            .get();

        // An operator credit is how a purchase refused by a wallet cap is reconciled.
        let limits = match p.channel {
            Channel::Operator => Limits {
                wallet_total_cents: 0,
                wallet_daily_cents: 0,
                ..iap.limits
            },
            _ => iap.limits,
        };
        lock(&mut tx, DAILY_LOCK).await?;
        let totals = Self::iap_totals(&mut *tx, p.address).await?;
        if let Some(reason) = totals.decide(&limits, cents) {
            return Ok(CreditOutcome::Capped(reason));
        }

        sqlx::query(
            "INSERT INTO credits.iap_transactions \
                 (transaction_id, original_transaction_id, product_id, address, credits, amount_cents, status, credited_by) \
             VALUES ($1, $2, $3, $4, $5::numeric, $6, 'paid', $7)",
        )
        .bind(p.transaction_id)
        .bind(p.original_transaction_id)
        .bind(p.product_id)
        .bind(p.address)
        .bind(credits.to_string())
        .bind(cents)
        .bind(p.channel.as_str())
        .execute(&mut *tx)
        .await?;
        let detail = json!({
            "source": "apple",
            "transactionId": p.transaction_id,
            "productId": p.product_id,
            "quantity": p.quantity,
            "creditedBy": p.channel.as_str(),
        });
        let key = grant_key(p.transaction_id);
        self.admin_grant_credits_in_tx(
            &mut tx,
            GrantRequest {
                address: p.address,
                amount: &credits.to_string(),
                kind: "purchase",
                reason: Some("app store purchase"),
                actor: Some(IAP_ACTOR),
                idempotency_key: Some(&key),
                detail: &detail,
                studio_name: None,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(CreditOutcome::Credited {
            address: p.address.to_string(),
            credits,
            already: false,
        })
    }

    /// Reverses one App Store transaction once; see the module docs.
    /// `product_id` is `None` for an operator refund, which records nothing
    /// for a transaction never credited.
    pub async fn refund_iap(
        &self,
        transaction_id: &str,
        product_id: Option<&str>,
        refund_ref: &str,
        reason: &str,
    ) -> Result<RefundOutcome, ApiError> {
        let mut tx = self.pool.begin().await?;
        lock(&mut tx, &grant_key(transaction_id)).await?;
        let row = sqlx::query(
            "SELECT status, address, credits::text AS credits FROM credits.iap_transactions \
             WHERE transaction_id = $1 FOR UPDATE",
        )
        .bind(transaction_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            if let Some(product_id) = product_id {
                sqlx::query(
                    "INSERT INTO credits.iap_transactions \
                         (transaction_id, product_id, status, refund_ref, refund_reason, refunded_at) \
                     VALUES ($1, $2, 'refunded', $3, $4, timezone('UTC', now()))",
                )
                .bind(transaction_id)
                .bind(product_id)
                .bind(refund_ref)
                .bind(reason)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
            }
            return Ok(RefundOutcome::NotCredited);
        };
        let status: String = row.get("status");
        let (Some(address), Some(credits)) = (
            row.get::<Option<String>, _>("address"),
            row.get::<Option<String>, _>("credits"),
        ) else {
            return Ok(RefundOutcome::AlreadyRefunded);
        };
        if status != "paid" {
            return Ok(RefundOutcome::AlreadyRefunded);
        }
        sqlx::query(
            "UPDATE credits.iap_transactions \
             SET status = 'refunded', revoked_credits = credits, refund_ref = $2, refund_reason = $3, \
                 refunded_at = timezone('UTC', now()), updated_at = timezone('UTC', now()) \
             WHERE transaction_id = $1",
        )
        .bind(transaction_id)
        .bind(refund_ref)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
        let detail =
            json!({ "source": "apple", "transactionId": transaction_id, "refundRef": refund_ref });
        let revoked = self
            .revoke_in_tx(
                &mut tx,
                &address,
                &credits,
                &grant_key(transaction_id),
                reason,
                &detail,
            )
            .await?;
        let revocations = if revoked.uncovered {
            revoke_live_credits_in_tx(&mut tx, &address).await?
        } else {
            0
        };
        tx.commit().await?;
        if revoked.has_shortfall {
            tracing::error!(address = %address, transaction_id = %transaction_id, shortfall = %revoked.shortfall, "app store refund could not charge back every credit: the rest was already spent");
        }
        Ok(RefundOutcome::Reversed {
            address,
            removed: revoked.removed,
            shortfall: revoked.shortfall,
            revocations,
        })
    }

    /// Records `address` as the wallet behind its account token.
    pub async fn link_iap_token(&self, address: &str) -> Result<String, ApiError> {
        let token = super::apple::account_token(address);
        // A no-op update returns the existing row, waiting for a concurrent insert of it.
        let owner: String = sqlx::query_scalar(
            "INSERT INTO credits.iap_account_tokens AS t (token, address) VALUES ($1, $2) \
             ON CONFLICT (token) DO UPDATE SET address = t.address RETURNING t.address",
        )
        .bind(&token)
        .bind(address)
        .fetch_one(&self.pool)
        .await?;
        if owner != address {
            return Err(ApiError::conflict(
                "the account token belongs to another wallet",
            ));
        }
        Ok(token)
    }

    /// The wallet a transaction's `appAccountToken` names: a registered token, or a wallet address itself.
    pub async fn wallet_for_iap_token(
        &self,
        token: Option<&str>,
    ) -> Result<Option<String>, ApiError> {
        let Some(token) = token.map(str::to_ascii_lowercase) else {
            return Ok(None);
        };
        if token.len() == 42
            && token.starts_with("0x")
            && token[2..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Ok(Some(token));
        }
        Ok(
            sqlx::query_scalar("SELECT address FROM credits.iap_account_tokens WHERE token = $1")
                .bind(&token)
                .fetch_optional(&self.pool)
                .await?,
        )
    }
}

/// Queues every unexpired signed credit of `address` for `revokeCredits`.
async fn revoke_live_credits_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    address: &str,
) -> Result<usize, ApiError> {
    let rows = sqlx::query(
        "SELECT id, credit_hash, chain_id, contract FROM credits.authorizations \
         WHERE address = $1 AND status IN ('authorized', 'retired') \
           AND expires_at > timezone('UTC', now()) \
           AND credit_hash IS NOT NULL AND chain_id IS NOT NULL AND contract IS NOT NULL",
    )
    .bind(address)
    .fetch_all(&mut **tx)
    .await?;
    let mut queued = 0;
    for r in rows {
        let (id, hash, chain_id, contract): (String, String, i64, String) = (
            r.get("id"),
            r.get("credit_hash"),
            r.get("chain_id"),
            r.get("contract"),
        );
        if enqueue_revocation(tx, &hash, &id, chain_id, &contract, "reversed").await? {
            queued += 1;
        }
    }
    if queued > 0 {
        tracing::warn!(address = %address, queued, "app store refund exceeds the unpledged balance: live signed credits queued for revocation");
    }
    Ok(queued)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_takes_positive_whole_credits_only() {
        assert_eq!(parse_catalog(r#"{"p1": 100}"#).unwrap()["p1"], 100);
        assert!(parse_catalog(r#"{"p1": 0}"#).is_err());
        assert!(parse_catalog(r#"{"p1": 1.5}"#).is_err());
        assert!(parse_catalog("[]").is_err());
    }

    #[test]
    fn caps_apply_in_order_and_zero_closes_the_node() {
        let limits = Limits {
            wallet_total_cents: 1000,
            wallet_daily_cents: 500,
            daily_cents: 10_000,
        };
        let t = Totals {
            total_cents: 900,
            daily_cents: 0,
            service_daily_cents: 0,
        };
        assert_eq!(t.decide(&limits, 200), Some(CapReason::TotalLimit));
        let t = Totals {
            total_cents: 0,
            daily_cents: 400,
            service_daily_cents: 0,
        };
        assert_eq!(t.decide(&limits, 200), Some(CapReason::DailyLimit));
        let t = Totals {
            total_cents: 0,
            daily_cents: 0,
            service_daily_cents: 9_900,
        };
        assert_eq!(t.decide(&limits, 200), Some(CapReason::ServiceDailyLimit));
        assert_eq!(t.decide(&limits, 100), None);
        let closed = Limits {
            daily_cents: 0,
            ..Limits::default()
        };
        assert_eq!(
            Totals {
                total_cents: 0,
                daily_cents: 0,
                service_daily_cents: 0
            }
            .decide(&closed, 10),
            Some(CapReason::ServiceDailyLimit)
        );
    }
}

//! The builder's request budgets, one token bucket per key
//! (`shared::limits::RateLimiter`), kept in process memory and spent by the
//! node's clock (`State.now`).
//!
//! - Every request from a source address ([`source`]) spends from that
//!   source's read (`GET`, `HEAD`) or write budget before anything else
//!   ([`per_source`]); a preflight never reaches it.
//! - A verified signer spends from its wallet's read or write budget right
//!   after its signature verifies, before the body or the database
//!   ([`spend_wallet`]).
//! - A listing whose rows a request can't bound spends one more read token
//!   per [`ROWS_PER_TOKEN`] rows it answered, from both, once answered
//!   ([`charge_rows`]): a bucket may run into debt that later requests wait out.
//!   An item listing pays for its rows and per [`BYTES_PER_TOKEN`] of their
//!   documents before it loads them, and answers at most [`LISTING_BYTES`]
//!   of documents ([`Listing::admit`]).
//!   `GET /v1/addresses` and the public collection search spend
//!   [`ADDRESSES_TOKENS`] more ([`charge`]).
//! - The review routes also spend from a per-wallet budget of their own
//!   ([`spend_review`]); the newsletter from a per-source
//!   ([`spend_newsletter_source`]), per-email and node-wide
//!   ([`spend_newsletter_email`]) one.
//!
//! A spent budget answers 429 with `Retry-After`, the seconds until it
//! holds a token again. Apart from the budgets, a wallet's uploads may hold
//! at most [`WALLET_BYTES`] and [`WALLET_FILES`] that no published item
//! names, all wallets' together [`NODE_BYTES`] and [`NODE_FILES`], and a source
//! address may send at most [`SOURCE_UPLOAD_MIB`] of uploads a day
//! ([`within_storage`], [`charge_upload`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use futures::TryStreamExt;

use crate::SharedState;
use crate::http::ApiError;
use crate::shared::client_ip::{client_ip, is_internal, source_key};
use crate::shared::limits::RateLimiter;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3_600);
const DAY: Duration = Duration::from_secs(86_400);

/// Reads of one source address a minute.
pub const SOURCE_READS: u32 = 3_000;
/// Writes of one source address a minute.
pub const SOURCE_WRITES: u32 = 2_400;
/// Reads of one signer a minute.
pub const WALLET_READS: u32 = 2_400;
/// Writes of one signer a minute.
pub const WALLET_WRITES: u32 = 1_200;
/// Review writes (retry, appeal, comment) of one signer a minute, as builder-server.
pub const REVIEW_WRITES: u32 = 30;
/// Review timeline reads of one signer a minute, as builder-server.
pub const REVIEW_READS: u32 = 120;
/// Newsletter subscriptions from one source address an hour.
pub const NEWSLETTER_PER_SOURCE: u32 = 10;
/// Newsletter subscriptions of one email a day.
pub const NEWSLETTER_PER_EMAIL: u32 = 3;
/// Newsletter subscriptions through the node an hour.
pub const NEWSLETTER_PER_NODE: u32 = 1_000;
/// Read tokens `GET /v1/addresses` and the public collection search spend
/// beyond their first: their query takes some 50 times the database time of
/// a common read.
pub const ADDRESSES_TOKENS: u32 = 50;
/// Rows of an unbounded listing one read token pays for.
pub const ROWS_PER_TOKEN: usize = 20;
/// Bytes of item documents one read token pays for.
pub const BYTES_PER_TOKEN: i64 = 64 * 1024;
/// Bytes of item documents one listing answers: three times builder-server's
/// largest collection (12,665 items, 19.8 MB).
pub const LISTING_BYTES: i64 = 64 * 1024 * 1024;
/// The bytes one wallet's uploads may hold in the store while no published
/// item names them.
pub const WALLET_BYTES: u64 = 5_000_000_000;
/// Upload records one wallet's uploads may hold while no published item
/// names them: three times builder-server's largest creator's files.
pub const WALLET_FILES: u64 = 100_000;
/// The bytes every wallet's uploads together may hold while no published
/// item names them (`BUILDER_UNPUBLISHED_MAX_BYTES`).
pub const NODE_BYTES: u64 = 1_000_000_000_000;
/// The upload records every wallet's uploads together may hold while no
/// published item names them (`BUILDER_UNPUBLISHED_MAX_FILES`): 35 times
/// the files builder-server's items name.
pub const NODE_FILES: u64 = 10_000_000;
/// MiB of item files and videos one source address may send a day: four
/// wallets' unpublished bound.
pub const SOURCE_UPLOAD_MIB: u32 = 20 * 1024;
/// How long a wallet's unpublished bytes, once counted, answer for it.
pub const UNPUBLISHED_KEPT: Duration = MINUTE;
const MIB: u64 = 1024 * 1024;
const KEYS: usize = 50_000;

/// Every budget, each a pub field so a test can set its own.
pub struct Limits {
    epoch: (Instant, DateTime<Utc>),
    pub source_reads: RateLimiter,
    pub source_writes: RateLimiter,
    pub wallet_reads: RateLimiter,
    pub wallet_writes: RateLimiter,
    pub review_writes: RateLimiter,
    pub review_reads: RateLimiter,
    pub newsletter_sources: RateLimiter,
    pub newsletter_emails: RateLimiter,
    pub newsletter_node: RateLimiter,
    /// [`WALLET_BYTES`], or a test's own.
    pub wallet_bytes: u64,
    /// [`WALLET_FILES`], or a test's own.
    pub wallet_files: u64,
    /// [`NODE_BYTES`], or `BUILDER_UNPUBLISHED_MAX_BYTES`.
    pub node_bytes: u64,
    /// [`NODE_FILES`], or `BUILDER_UNPUBLISHED_MAX_FILES`.
    pub node_files: u64,
    /// [`SOURCE_UPLOAD_MIB`] a day, or a test's own.
    pub source_upload_mib: RateLimiter,
    /// [`LISTING_BYTES`], or a test's own.
    pub listing_bytes: i64,
    unpublished: Mutex<HashMap<String, Unpublished>>,
}

struct Unpublished {
    at: Instant,
    held: Held<i64>,
    uploaded: Held<i64>,
}

/// Bytes and files of upload records.
#[derive(Clone, Copy)]
pub struct Held<T> {
    pub bytes: T,
    pub files: T,
}

const NODE_KEY: &str = "node";

/// `per` tokens every `window`, refilled continuously.
pub fn budget(per: u32, window: Duration) -> RateLimiter {
    RateLimiter::new(per, window).with_max_buckets(KEYS)
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            epoch: (Instant::now(), Utc::now()),
            source_reads: budget(SOURCE_READS, MINUTE),
            source_writes: budget(SOURCE_WRITES, MINUTE),
            wallet_reads: budget(WALLET_READS, MINUTE),
            wallet_writes: budget(WALLET_WRITES, MINUTE),
            review_writes: budget(REVIEW_WRITES, MINUTE),
            review_reads: budget(REVIEW_READS, MINUTE),
            newsletter_sources: budget(NEWSLETTER_PER_SOURCE, HOUR),
            newsletter_emails: budget(NEWSLETTER_PER_EMAIL, DAY),
            newsletter_node: budget(NEWSLETTER_PER_NODE, HOUR),
            wallet_bytes: WALLET_BYTES,
            wallet_files: WALLET_FILES,
            node_bytes: NODE_BYTES,
            node_files: NODE_FILES,
            source_upload_mib: budget(SOURCE_UPLOAD_MIB, DAY),
            listing_bytes: LISTING_BYTES,
            unpublished: Mutex::default(),
        }
    }
}

impl Limits {
    /// A million tokens a minute in every budget.
    pub fn unbounded() -> Self {
        let all = || budget(1_000_000, MINUTE);
        Self {
            epoch: (Instant::now(), Utc::now()),
            source_reads: all(),
            source_writes: all(),
            wallet_reads: all(),
            wallet_writes: all(),
            review_writes: all(),
            review_reads: all(),
            newsletter_sources: all(),
            newsletter_emails: all(),
            newsletter_node: all(),
            wallet_bytes: u64::MAX,
            wallet_files: u64::MAX,
            node_bytes: u64::MAX,
            node_files: u64::MAX,
            source_upload_mib: all(),
            listing_bytes: LISTING_BYTES,
            unpublished: Mutex::default(),
        }
    }

    fn instant(&self, now: DateTime<Utc>) -> Instant {
        let (at, wall) = self.epoch;
        match (now - wall).to_std() {
            Ok(ahead) => at + ahead,
            Err(_) => (wall - now)
                .to_std()
                .ok()
                .and_then(|behind| at.checked_sub(behind))
                .unwrap_or(at),
        }
    }

    fn take(&self, budget: &RateLimiter, key: &str, now: DateTime<Utc>) -> Result<(), ApiError> {
        self.take_cost(budget, key, 1, now)
    }

    fn take_cost(
        &self,
        budget: &RateLimiter,
        key: &str,
        cost: u32,
        now: DateTime<Utc>,
    ) -> Result<(), ApiError> {
        budget
            .take_at(key, cost, self.instant(now))
            .map_err(ApiError::Limited)
    }
}

/// The source address a request came from, by [`source_key`]; `None` when
/// it names no single client: none at all, or an internal address, which is
/// what every request carries behind a gateway the node doesn't trust
/// (`HTTP_TRUSTED_PROXY_CIDRS`).
pub fn source(headers: &HeaderMap) -> Option<String> {
    let ip = client_ip(headers);
    let parsed: std::net::IpAddr = ip.parse().ok()?;
    (!is_internal(parsed)).then(|| source_key(&ip))
}

fn reads(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD
}

/// The source's read or write budget, before the route runs.
pub async fn per_source(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    if let Some(source) = source(req.headers()) {
        let limits = &state.limits;
        let budget = if reads(req.method()) {
            &limits.source_reads
        } else {
            &limits.source_writes
        };
        if let Err(refused) = limits.take(budget, &source, (state.now)()) {
            return refused.into_response();
        }
    }
    next.run(req).await
}

/// A verified signer's read or write budget.
pub fn spend_wallet(state: &SharedState, signer: &str, method: &Method) -> Result<(), ApiError> {
    let limits = &state.limits;
    let budget = if reads(method) {
        &limits.wallet_reads
    } else {
        &limits.wallet_writes
    };
    limits.take(budget, signer, (state.now)())
}

/// The review routes' own budget for `signer`: its timeline reads or its writes.
pub fn spend_review(state: &SharedState, signer: &str, read: bool) -> Result<(), ApiError> {
    let limits = &state.limits;
    let budget = if read {
        &limits.review_reads
    } else {
        &limits.review_writes
    };
    limits.take(budget, signer, (state.now)())
}

/// A subscription's budget of its source address, before its body is read.
pub fn spend_newsletter_source(state: &SharedState, source: &str) -> Result<(), ApiError> {
    let limits = &state.limits;
    limits.take(&limits.newsletter_sources, source, (state.now)())
}

/// A valid subscription's budgets of its email and of the node.
pub fn spend_newsletter_email(state: &SharedState, email: &str) -> Result<(), ApiError> {
    let limits = &state.limits;
    let now = (state.now)();
    limits.take(&limits.newsletter_emails, email, now)?;
    limits.take(&limits.newsletter_node, "node", now)
}

/// Refuses an upload by `wallet` before its body is read: 429 while its
/// source address has sent [`SOURCE_UPLOAD_MIB`] in the last day (what it
/// sends is charged once received, [`charge_upload`]); 413 when the
/// wallet's unpublished uploads hold its bound of files, or could take its
/// bound of bytes with this body ([`WALLET_FILES`], [`WALLET_BYTES`]); 507
/// when every wallet's together hold the node's ([`NODE_FILES`],
/// [`NODE_BYTES`]). The body's declared length, else `most`, the route's
/// largest body, counts as stored. Past a bound by the cheap sum of the
/// records, the unpublished uploads are counted at most once every
/// [`UNPUBLISHED_KEPT`], and moved meanwhile by what the records gained or
/// lost.
pub async fn within_storage(
    state: &SharedState,
    wallet: &str,
    headers: &HeaderMap,
    most: u64,
) -> Result<(), ApiError> {
    let incoming = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
        .unwrap_or(most);
    let limits = &state.limits;
    let now = (state.now)();
    if let Some(source) = source(headers) {
        limits.take(&limits.source_upload_mib, &source, now)?;
        limits.source_upload_mib.refund(&source, 1);
    }
    let wallet_bound = Held {
        bytes: limits.wallet_bytes,
        files: limits.wallet_files,
    };
    if let Some(over) = held_past(state, Some(wallet), incoming, wallet_bound).await? {
        return Err(
            ApiError::http(413, "The wallet's unpublished files reached their limit")
                .with_data(over),
        );
    }
    let node_bound = Held {
        bytes: limits.node_bytes,
        files: limits.node_files,
    };
    if let Some(over) = held_past(state, None, incoming, node_bound).await? {
        return Err(
            ApiError::http(507, "The node's unpublished files reached their limit").with_data(over),
        );
    }
    Ok(())
}

/// The bound `held` by `wallet`'s unpublished uploads (every wallet's with
/// `None`) passes with `incoming` more bytes, as `{maxBytes}` or
/// `{maxFiles}`; `None` within it.
async fn held_past(
    state: &SharedState,
    wallet: Option<&str>,
    incoming: u64,
    bound: Held<u64>,
) -> Result<Option<serde_json::Value>, ApiError> {
    let past = |held: Held<i64>| {
        let (bytes, files) = (
            u64::try_from(held.bytes).unwrap_or(0),
            u64::try_from(held.files).unwrap_or(0),
        );
        if bytes.saturating_add(incoming) > bound.bytes {
            Some(serde_json::json!({ "maxBytes": bound.bytes }))
        } else if files >= bound.files {
            Some(serde_json::json!({ "maxFiles": bound.files }))
        } else {
            None
        }
    };
    let pool = state.items.pool();
    let uploaded = crate::access::uploaded(pool, wallet).await?;
    if past(uploaded).is_none() {
        return Ok(None);
    }
    let limits = &state.limits;
    let key = wallet.unwrap_or(NODE_KEY);
    let at = limits.instant((state.now)());
    let fresh = |kept: &Unpublished| at.saturating_duration_since(kept.at) < UNPUBLISHED_KEPT;
    let kept = limits
        .unpublished
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
        .filter(|kept| fresh(kept))
        .map(|kept| Held {
            bytes: kept.held.bytes + uploaded.bytes - kept.uploaded.bytes,
            files: kept.held.files + uploaded.files - kept.uploaded.files,
        });
    let unpublished = match kept {
        Some(held) => held,
        None => {
            let held = crate::access::unpublished(pool, wallet).await?;
            let mut map = limits.unpublished.lock().unwrap_or_else(|e| e.into_inner());
            map.retain(|_, kept| fresh(kept));
            map.insert(key.to_string(), Unpublished { at, held, uploaded });
            held
        }
    };
    Ok(past(unpublished))
}

/// The reader of a signed item listing.
pub struct Listing<'a> {
    pub state: &'a SharedState,
    pub headers: &'a HeaderMap,
    pub signer: &'a str,
}

impl Listing<'_> {
    /// Before `rows` holding `bytes` of item documents load: 413 past the
    /// node's [`LISTING_BYTES`], else their read tokens from the signer's and
    /// the source's read budgets, 429 when either can't pay them now.
    pub fn admit(&self, rows: usize, bytes: i64) -> Result<(), ApiError> {
        let limits = &self.state.limits;
        if bytes > limits.listing_bytes {
            return Err(
                ApiError::http(413, "The listing is too large, ask for it in pages")
                    .with_data(serde_json::json!({ "maxBytes": limits.listing_bytes })),
            );
        }
        let tokens = rows / ROWS_PER_TOKEN + usize::try_from(bytes / BYTES_PER_TOKEN).unwrap_or(0);
        let tokens = u32::try_from(tokens).unwrap_or(u32::MAX);
        if tokens == 0 {
            return Ok(());
        }
        let now = (self.state.now)();
        limits.take_cost(&limits.wallet_reads, self.signer, tokens, now)?;
        if let Some(source) = source(self.headers) {
            limits.take_cost(&limits.source_reads, &source, tokens, now)?;
        }
        Ok(())
    }
}

/// The request with a body that counts the bytes it hands on.
pub fn counted(request: Request) -> (Request, Arc<AtomicU64>) {
    let received = Arc::new(AtomicU64::new(0));
    let counter = received.clone();
    let request = request.map(|body| {
        axum::body::Body::from_stream(body.into_data_stream().inspect_ok(move |chunk| {
            counter.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }))
    });
    (request, received)
}

/// The bytes an upload from `source` (by [`source`]) sent, whatever came of
/// it, charged to its source address's day.
pub fn charge_upload(state: &SharedState, source: Option<&str>, received: &AtomicU64) {
    let Some(source) = source else {
        return;
    };
    let mib = received.load(Ordering::Relaxed).div_ceil(MIB);
    let limits = &state.limits;
    limits.source_upload_mib.charge_at(
        source,
        u32::try_from(mib).unwrap_or(u32::MAX),
        limits.instant((state.now)()),
    );
}

/// `rows` answered by an unbounded listing, charged to the signer's and the
/// source's read budgets.
pub fn charge_rows(state: &SharedState, headers: &HeaderMap, signer: Option<&str>, rows: usize) {
    charge(
        state,
        headers,
        signer,
        u32::try_from(rows / ROWS_PER_TOKEN).unwrap_or(u32::MAX),
    );
}

/// `tokens` more spent by a read already answered, from the signer's and
/// the source's read budgets.
pub fn charge(state: &SharedState, headers: &HeaderMap, signer: Option<&str>, tokens: u32) {
    if tokens == 0 {
        return;
    }
    let limits = &state.limits;
    let now = limits.instant((state.now)());
    if let Some(signer) = signer {
        limits.wallet_reads.charge_at(signer, tokens, now);
    }
    if let Some(source) = source(headers) {
        limits.source_reads.charge_at(&source, tokens, now);
    }
}

//! Builder: the pre-publish draft store behind `builder-api.decentraland.org`
//! (the old `catalyrst-builder` member of the `catalyrst-create` bundle):
//! item and collection drafts, uploaded content, the committee's item
//! curation, standard collection publication (Polygon factory parameters
//! prepared here, signed and submitted by the client, the receipt verified
//! here) and linked (third-party) publication (frozen snapshot, stable
//! nonce, the owner-signed provider-slot cheque, the Foundation review)
//! and the newsletter opt-in. It keeps no scenes: no projects, manifests,
//! asset packs or scene files. Schema `builder` (`schemas/50_create.sql`).
//!
//! What it reuses: the ONE pool; the ONE `http::Outbound` client for the
//! Polygon RPC and the newsletter forward; the ONE blob store (`storage`)
//! for uploaded item files, by hash, which it serves itself (who
//! may read a file is [`access::read_file`]); signed
//! fetch through `src/auth` and the shared verifier the explore and social
//! routes use (`shared::signed_fetch`, [`auth`]); the marketplace subgraph
//! readers (`marketplace::graph`) where the old code read the squid
//! ([`onchain`]); the shop's copy of the Polygon collections subgraph,
//! read by shared SQL, for a collection's published and approved state
//! ([`chain_state`]: 503 while the copy can't tell) and the chain facts a
//! collection's body and listings merge ([`collections`]). The one
//! client it does not share is the Foundation review's
//! (`linked_publication_review::foundation_client`: guarded as the shared
//! one, but no redirects; the reason is in that module), which the
//! terms-of-service forward to the warehouse ([`terms`]) takes too. Every write goes behind
//! `CREATE_WRITES_ENABLED` ([`writes`], default false, 503 with
//! `Retry-After`, built the way `social::writes` gates the social writes).
//!
//! Authorization: [`access`] decides every read and write of a collection or
//! item (owner, on-chain manager, current committee member, third-party
//! manager, as builder-server defines them; 503 when a source it needs can't
//! tell), and the store functions take the grant it returns. The committee is
//! [`committee`]'s read of the Polygon collections subgraph, kept an hour.
//! The curation routes need the admin bearer (`CATALYRST_BUILDER_ADMIN_TOKEN`,
//! constant-time, fails closed) or a `BUILDER_ADMIN_ADDRESSES` signer; chain
//! writes are prepared here and signed and submitted by the client, the
//! receipt verified over the Polygon RPC; the linked cheque is verified only;
//! v2 never holds a signing key.
//!
//! Deviations from the old crate, all deliberate: the signed-fetch window is
//! the shared verifier's 5 minutes (the old required path allowed 30); the
//! subgraph readers replace the squid side pool (empty curation listings
//! without a collections subgraph, as the old node answered without its
//! pool); `BUILDER_POLYGON_RPC_URL`
//! must be https to a public host (a plaintext verifier could be fed a fake
//! receipt).
//!
//! Not carried, by the plan: `dcl-scene-cli` and what only it consumes
//! (`dcl-scene-authoring`, `dcl-preview-tunnel`,
//! `dcl-preview-tunnel-protocol`) and `catalyrst-deploy-signer` (a local
//! one-shot tool). Left out with the port: the old `ts-rs` bindings export
//! (the sites generate their types from the old crate until the web tier
//! moves), and the per-service `/ping` at the root (it answers under the
//! `/builder-api` prefix; the content server owns the root).
//!
//! Mount: `serve` merges [`root_router`] at the root (the old
//! `builder-api.` vhost paths) and nests [`router`] under `/builder-api`
//! (the old gateway prefix, stripped by nginx before it reached the bundle).

pub(crate) use catalyst_chain_index as chain_index;
pub(crate) use catalyst_content as content;
#[cfg(test)]
pub(crate) use catalyst_db as db;
pub(crate) use catalyst_shared as shared;
pub(crate) use catalyst_storage as storage;

pub mod access;
pub mod auth;
pub mod chain_state;
pub mod collections;
pub mod committee;
pub mod curation;
pub mod drafts;
pub mod handlers;
pub mod http;
pub mod items;
pub mod kept;
pub mod linked_publication;
pub mod linked_publication_review;
pub mod linked_publication_store;
pub mod onchain;
pub mod publication;
pub mod publication_chain;
pub mod publication_store;
pub mod rarities;
pub mod terms;
pub mod third_parties;
pub mod writes;

#[cfg(test)]
pub(crate) mod tests;

use std::sync::Arc;

use anyhow::bail;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, patch, post};
use sqlx::PgPool;

use crate::chain_index::ChainIndex;
use crate::storage::Storage;
use dcl_auth::eip1654::Eip1654Validator;
use dcl_http::Outbound;

use items::{ItemsComponent, NewsletterComponent};

pub const ADMIN_TOKEN_ENV: &str = "CATALYRST_BUILDER_ADMIN_TOKEN";
/// One 20 MiB upload with its framing.
const UPLOAD_BODY_LIMIT: usize = 21 * 1024 * 1024;

/// The newsletter SaaS forward (`NEWSLETTER_SERVICE_URL`,
/// `NEWSLETTER_PUBLICATION_ID`, `NEWSLETTER_SERVICE_API_KEY`), configured
/// only when all three are set.
#[derive(Clone, Debug)]
pub struct NewsletterForward {
    pub url: String,
    pub publication_id: String,
    pub api_key: String,
}

/// The warehouse a terms-of-service acceptance is forwarded to
/// (`WAREHOUSE_URL`, `WAREHOUSE_TOKEN` and the optional
/// `WAREHOUSE_CONTEXT_PREFIX`), configured only when the URL and the token
/// are set.
#[derive(Clone)]
pub struct WarehouseForward {
    pub url: String,
    pub token: String,
    pub context_prefix: Option<String>,
}

impl std::fmt::Debug for WarehouseForward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WarehouseForward")
            .field("context_prefix", &self.context_prefix)
            .finish_non_exhaustive()
    }
}

/// The environment as `Config` hands it over (the old env names).
#[derive(Clone, Debug, Default)]
pub struct RawSettings {
    /// `CREATE_WRITES_ENABLED` (false).
    pub writes_enabled: bool,
    /// `BUILDER_POLYGON_RPC_URL`: the Polygon JSON-RPC that verifies
    /// publication receipts and reads the rarities' MANA prices
    /// ([`rarities`]); unset refuses begin/claim/verify and the rarities
    /// routes with 503.
    pub polygon_rpc_url: Option<String>,
    /// `BUILDER_ADMIN_ADDRESSES`: comma-separated admin wallets.
    pub admin_addresses: String,
    /// `CATALYRST_BUILDER_ADMIN_TOKEN`: bearer of the curation routes.
    pub admin_token: Option<String>,
    pub newsletter_service_url: Option<String>,
    pub newsletter_publication_id: Option<String>,
    pub newsletter_api_key: Option<String>,
    /// `BUILDER_FOUNDATION_URL`: the Foundation builder API a linked
    /// publication is verified against; unset answers 503 there.
    pub foundation_url: Option<String>,
    pub warehouse_url: Option<String>,
    pub warehouse_token: Option<String>,
    pub warehouse_context_prefix: Option<String>,
    /// `SLACK_BOT_TOKEN`: the bot the curation notices are posted as.
    pub slack_bot_token: Option<String>,
    /// `SLACK_CURATION_CHANNEL`: the curators' channel; notices need both.
    pub slack_curation_channel: Option<String>,
    /// `CREATORS_URL`: the creators site a notice links to.
    pub creators_url: Option<String>,
    /// `BUILDER_URL`: the legacy builder a notice links to without
    /// `CREATORS_URL`.
    pub builder_url: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub writes_enabled: bool,
    pub polygon_rpc_url: Option<String>,
    pub admin_addresses: Vec<String>,
    pub admin_token: Option<String>,
    pub newsletter: Option<NewsletterForward>,
    pub foundation_url: Option<String>,
    pub warehouse: Option<WarehouseForward>,
    /// The curators' Slack channel; none sends no notice.
    pub slack: Option<curation::slack::Slack>,
    pub links: curation::slack::Links,
}

fn non_empty(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Settings {
    /// Lowercased, trimmed, blanks dropped: the old `Config::from_env` rule.
    pub fn admin_addresses_from(raw: &str) -> Vec<String> {
        raw.split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// The old `Config::from_env` checks over [`RawSettings`], plus the
    /// upstream guards every configured endpoint passes in v2.
    pub fn parse(raw: RawSettings) -> anyhow::Result<Self> {
        let polygon_rpc_url = non_empty(&raw.polygon_rpc_url);
        if let Some(url) = &polygon_rpc_url {
            let parsed = url::Url::parse(url)
                .map_err(|e| anyhow::anyhow!("BUILDER_POLYGON_RPC_URL is not a valid URL: {e}"))?;
            if parsed.scheme() != "https" || !dcl_http::is_safe_http_url(url) {
                bail!(
                    "BUILDER_POLYGON_RPC_URL is {}:// to {}; publication receipts must be verified over a trusted TLS (https://) endpoint",
                    parsed.scheme(),
                    parsed.host_str().unwrap_or("?")
                );
            }
        }
        let newsletter = match (
            non_empty(&raw.newsletter_service_url),
            non_empty(&raw.newsletter_publication_id),
            non_empty(&raw.newsletter_api_key),
        ) {
            (Some(url), Some(publication_id), Some(api_key)) => {
                if !dcl_http::is_safe_http_url(&url) {
                    bail!("NEWSLETTER_SERVICE_URL must be a public http(s) URL");
                }
                Some(NewsletterForward {
                    url,
                    publication_id,
                    api_key,
                })
            }
            _ => None,
        };
        let foundation_url =
            non_empty(&raw.foundation_url).map(|u| u.trim_end_matches('/').to_string());
        if let Some(url) = &foundation_url
            && !dcl_http::is_safe_http_url(url)
        {
            bail!("BUILDER_FOUNDATION_URL must be a public http(s) URL");
        }
        let warehouse = match (
            non_empty(&raw.warehouse_url),
            non_empty(&raw.warehouse_token),
        ) {
            (Some(url), Some(token)) => {
                if !dcl_http::is_safe_http_url(&url) {
                    bail!("WAREHOUSE_URL must be a public http(s) URL");
                }
                Some(WarehouseForward {
                    url,
                    token,
                    context_prefix: non_empty(&raw.warehouse_context_prefix),
                })
            }
            _ => None,
        };
        let slack = match (
            non_empty(&raw.slack_bot_token),
            non_empty(&raw.slack_curation_channel),
        ) {
            (Some(token), Some(channel)) => Some(curation::slack::Slack {
                token,
                channel,
                api: curation::slack::API.to_string(),
            }),
            _ => None,
        };
        let links = curation::slack::Links {
            creators_url: non_empty(&raw.creators_url),
            builder_url: non_empty(&raw.builder_url),
        };
        for (name, url) in [
            ("CREATORS_URL", &links.creators_url),
            ("BUILDER_URL", &links.builder_url),
        ] {
            if let Some(url) = url
                && !url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
            {
                bail!("{name} must be an http(s) URL");
            }
        }
        Ok(Self {
            slack,
            links,
            writes_enabled: raw.writes_enabled,
            polygon_rpc_url,
            admin_addresses: Self::admin_addresses_from(&raw.admin_addresses),
            admin_token: non_empty(&raw.admin_token),
            newsletter,
            foundation_url,
            warehouse,
        })
    }
}

pub struct State {
    pub items: ItemsComponent,
    pub newsletter: NewsletterComponent,
    /// The marketplace readers the on-chain routes read; `None` (or one
    /// without a collections subgraph) answers 503 there.
    pub market: Option<Arc<ChainIndex>>,
    pub settings: Settings,
    pub http: Outbound,
    /// The guarded, no-redirect client: the Foundation review reads and the
    /// warehouse forward, whose requests carry a credential.
    pub foundation: Outbound,
    pub eip1654: Option<Arc<dyn Eip1654Validator>>,
    /// The content state's denylist (the same `Arc`): a hash the camera reel
    /// withheld (rejected or deleted photo) is a 404 on the storage routes
    /// here too, not only on the content server's routes.
    pub denylist: Arc<crate::content::Denylist>,
    /// The current curation committee (`committee`), kept an hour.
    pub committee: committee::Committee,
    /// The rarities and their prices (`rarities`), kept a minute.
    pub rarities: rarities::Rarities,
    /// The third-party registry (`third_parties`), read per decision.
    pub third_parties: third_parties::ThirdParties,
    /// The node's clock, which a collection's lock is set and compared by.
    pub now: fn() -> chrono::DateTime<chrono::Utc>,
    /// What the Polygon RPC says of collection contracts, kept per contract.
    pub deployments: collections::deployment::Deployments,
    /// The curators' Slack notices.
    pub notices: curation::slack::Notices,
}

pub type SharedState = Arc<State>;

impl State {
    pub fn new(
        pool: PgPool,
        blobs: Arc<Storage>,
        denylist: Arc<crate::content::Denylist>,
        http: Outbound,
        market: Option<Arc<ChainIndex>>,
        eip1654: Option<Arc<dyn Eip1654Validator>>,
        settings: Settings,
    ) -> anyhow::Result<Self> {
        let notices = curation::slack::Notices::new(settings.slack.clone(), settings.links.clone());
        Ok(Self {
            items: ItemsComponent::new(pool.clone(), blobs),
            newsletter: NewsletterComponent::new(pool),
            market,
            settings,
            http,
            foundation: linked_publication_review::foundation_client(),
            eip1654,
            denylist,
            committee: committee::Committee::default(),
            rarities: rarities::Rarities::default(),
            third_parties: third_parties::ThirdParties::default(),
            now: chrono::Utc::now,
            deployments: collections::deployment::Deployments::default(),
            notices,
        })
    }

    /// What `serve` logs at startup.
    pub fn sources_label(&self) -> String {
        format!(
            "polygon_rpc={} collections_reader={} admins={} newsletter={} warehouse={} slack={}",
            self.settings.polygon_rpc_url.is_some(),
            onchain::configured(self.market.as_deref()),
            self.settings.admin_addresses.len(),
            self.settings.newsletter.is_some(),
            self.settings.warehouse.is_some(),
            self.settings.slack.is_some(),
        )
    }
}

fn api_routes() -> Router<SharedState> {
    Router::new()
        .route(
            "/v1/items/{id}/files",
            post(handlers::drafts::post_item_files).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)),
        )
        .route(
            "/v1/collections/{id}/publication",
            get(handlers::drafts::get_publication)
                .post(handlers::publication::begin)
                .put(handlers::publication::transaction)
                .delete(handlers::publication::cancel)
                .patch(handlers::publication::claim),
        )
        .route(
            "/v1/collections/{id}/publication/status",
            get(handlers::publication::status),
        )
        .route(
            "/v1/collections/{id}/linked-publication",
            get(handlers::linked_publication::prepare)
                .post(handlers::linked_publication::begin)
                .patch(handlers::linked_publication::claim)
                .put(handlers::linked_publication::authorize)
                .delete(handlers::linked_publication::cancel),
        )
        .route(
            "/v1/collections/{id}/linked-publication/status",
            get(handlers::linked_publication::status),
        )
        .route(
            "/v1/collections/{id}/linked-publication/verify",
            post(handlers::linked_publication::verify),
        )
        .route(
            "/v1/drafts/collections",
            get(handlers::drafts::get_collection_drafts),
        )
        .route("/v1/drafts/items", get(handlers::drafts::get_item_drafts))
        .route("/v1/items", get(handlers::collections::get_items))
        .route(
            "/v1/items/{id}",
            get(handlers::drafts::get_item)
                .put(handlers::drafts::put_item)
                .delete(handlers::drafts::delete_item),
        )
        .route(
            "/v1/collections/{id}/items",
            get(handlers::collections::get_collection_items),
        )
        .route(
            "/v1/collections/{id}",
            get(handlers::collections::get_collection)
                .put(handlers::drafts::put_collection)
                .delete(handlers::lifecycle::delete),
        )
        .route("/v1/collections/{id}/lock", post(handlers::lifecycle::lock))
        .route(
            "/v1/collections/{id}/owner/sync",
            post(handlers::lifecycle::owner_sync),
        )
        .route("/v1/collections/{id}/tos", post(handlers::lifecycle::terms))
        .route(
            "/v1/collections/{id}/publish",
            post(handlers::lifecycle::publish),
        )
        .route(
            "/v1/collections/curation",
            get(handlers::curation::get_curation_collections),
        )
        .route("/v1/curations", get(handlers::curation::curations))
        .route(
            "/v1/collections/{id}/curation",
            get(handlers::curation::collection_curation)
                .patch(handlers::curation::update_collection_curation)
                .post(handlers::curation::request_collection_curation),
        )
        .route(
            "/v1/collections/{id}/itemCurations",
            get(handlers::curation::collection_item_curations),
        )
        .route(
            "/v1/items/{id}/curation",
            get(handlers::curation::item_curation)
                .patch(handlers::curation::update_item_curation)
                .post(handlers::curation::request_item_curation),
        )
        .route(
            "/v1/{address}/collections",
            get(handlers::collections::get_address_collections),
        )
        .route(
            "/v1/{address}/items",
            get(handlers::collections::get_address_items),
        )
        .route(
            "/v1/collections/{id}/items/{item}/status",
            patch(handlers::curation::patch_item_status),
        )
        .route(
            "/v1/collections/{id}/items/status",
            patch(handlers::curation::patch_items_status_bulk),
        )
        .route("/v1/thirdParties", get(handlers::third_parties::list))
        .route(
            "/v1/thirdParties/{id}",
            get(handlers::third_parties::get)
                .delete(handlers::third_parties::delete)
                .patch(handlers::third_parties::update),
        )
        .route(
            "/v1/thirdParties/{id}/slots",
            get(handlers::third_parties::slots),
        )
        .route_layer(axum::middleware::map_response(http::private_response))
        .route(
            "/v1/collections",
            get(handlers::collections::get_collections),
        )
        .route("/v1/committee", get(handlers::collections::get_committee))
        .route("/v1/info", get(handlers::small_reads::get_info))
        .route("/v1/rarities", get(handlers::small_reads::get_rarities))
        .route(
            "/v1/rarities/{name}",
            get(handlers::small_reads::get_rarity),
        )
        .route(
            "/v1/published-collections/{address}/items/{id}/utility",
            get(handlers::small_reads::get_item_utility),
        )
        .route(
            "/v1/items/{id}/{item}/contents",
            get(handlers::small_reads::get_item_contents),
        )
        .route("/v1/addresses", get(handlers::small_reads::get_addresses))
        .route(
            "/v1/storage/contents/{hash}",
            get(handlers::storage::get_storage_content)
                .head(handlers::storage::get_storage_content),
        )
        .route(
            "/v1/storage/contents/{hash}/exists",
            get(handlers::storage::get_storage_content_exists),
        )
        .route(
            "/v1/newsletter",
            post(handlers::newsletter::post_newsletter),
        )
}

fn finish(routes: Router<SharedState>, state: SharedState) -> Router {
    let writes_enabled = state.settings.writes_enabled;
    let router = routes
        .route_layer(axum::middleware::map_response(http::enveloped))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state);
    writes::guard(router, writes_enabled)
}

/// The whole builder surface, for the `/builder-api` nest: the API routes
/// and `/ping`.
pub fn router(state: SharedState) -> Router {
    finish(api_routes().route("/ping", get(handlers::ping)), state)
}

/// The routes safe to merge at the root beside the content server (which
/// owns the root liveness route there).
pub fn root_router(state: SharedState) -> Router {
    finish(api_routes(), state)
}

#[cfg(test)]
mod router_tests {
    use super::*;

    #[test]
    fn settings_parse_the_old_env_rules_and_refuse_unsafe_endpoints() {
        let s = Settings::parse(RawSettings {
            admin_addresses: " 0xABC, ,0xdef ".into(),
            admin_token: Some("  ".into()),
            polygon_rpc_url: Some("https://polygon-rpc.com".into()),
            newsletter_service_url: Some("https://api.beehiiv.com/v2".into()),
            newsletter_publication_id: Some("pub_1".into()),
            newsletter_api_key: None,
            ..RawSettings::default()
        })
        .unwrap();
        assert_eq!(s.admin_addresses, vec!["0xabc", "0xdef"]);
        assert_eq!(s.admin_token, None, "a blank token is unset");
        assert!(
            s.newsletter.is_none(),
            "the forward needs all three settings"
        );
        assert!(!s.writes_enabled);
        let err = Settings::parse(RawSettings {
            polygon_rpc_url: Some("http://rpc.example/v3/SECRET".into()),
            ..RawSettings::default()
        })
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("http:// to rpc.example") && !err.contains("SECRET"),
            "{err}"
        );
        assert!(
            Settings::parse(RawSettings {
                polygon_rpc_url: Some("https://127.0.0.1/".into()),
                ..RawSettings::default()
            })
            .is_err()
        );
    }

    #[test]
    fn the_warehouse_needs_its_url_and_token_and_a_public_url() {
        let warehouse = |url: &str, token: Option<&str>, prefix: Option<&str>| {
            Settings::parse(RawSettings {
                warehouse_url: Some(url.into()),
                warehouse_token: token.map(str::to_string),
                warehouse_context_prefix: prefix.map(str::to_string),
                ..RawSettings::default()
            })
        };
        let forward = warehouse(
            "https://warehouse.example/events",
            Some("sk-example123"),
            Some(" prd "),
        )
        .unwrap()
        .warehouse
        .unwrap();
        assert_eq!(
            (
                forward.url.as_str(),
                forward.token.as_str(),
                forward.context_prefix.as_deref()
            ),
            (
                "https://warehouse.example/events",
                "sk-example123",
                Some("prd")
            )
        );
        assert!(
            warehouse("https://warehouse.example/events", None, None)
                .unwrap()
                .warehouse
                .is_none()
        );
        assert!(
            warehouse("https://warehouse.example/events", Some(" "), None)
                .unwrap()
                .warehouse
                .is_none()
        );
        assert!(
            warehouse(
                "https://warehouse.example/events",
                Some("sk-example123"),
                Some("")
            )
            .unwrap()
            .warehouse
            .unwrap()
            .context_prefix
            .is_none()
        );
        let settings = warehouse(
            "https://warehouse.example/events?key=sk-example456",
            Some("sk-example123"),
            None,
        )
        .unwrap();
        let printed = format!("{settings:?}");
        assert!(
            !printed.contains("sk-example123") && !printed.contains("sk-example456"),
            "{printed}"
        );
        let err = warehouse("http://127.0.0.1/events", Some("sk-example123"), None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("WAREHOUSE_URL") && !err.contains("sk-example123"),
            "{err}"
        );
    }
}

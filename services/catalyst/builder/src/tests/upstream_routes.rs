//! The route ledger: every route `github.com/decentraland/builder-server`
//! registers under `/v1` at `98897af692f26063a088aebb2ea5bb3f683168a2`, and
//! whether this crate's router answers its method and path. A `Routed` route
//! reaches a handler, which says nothing about matching upstream's
//! behaviour; a `Todo` route is still to add; a `Removed` route is outside
//! the port (scene templates, scene reads, assets, asset pack files,
//! deployments, analytics and NFT reads) and stays unanswered. Adding a route moves its row to
//! `Routed` first.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use sqlx::postgres::PgPoolOptions;

use super::support::temp_storage;
use crate::shared::test_http::call;
use crate::{Settings, State};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Route {
    Routed,
    Todo,
    Removed,
}
use Route::{Removed, Routed, Todo};

const UPSTREAM: &[(&str, &str, Route)] = &[
    ("GET", "/info", Todo),
    ("GET", "/assetPacks", Routed),
    ("GET", "/assetPacks/:id", Removed),
    ("GET", "/assets/:id", Removed),
    ("GET", "/assets", Removed),
    ("GET", "/templates", Removed),
    ("GET", "/projects", Routed),
    ("GET", "/projects/:id", Routed),
    ("DELETE", "/projects/:id", Routed),
    ("DELETE", "/projects/:coords/coords", Routed),
    ("GET", "/projects/:id/public", Routed),
    ("GET", "/projects/:id/media/:filename", Removed),
    ("GET", "/projects/:id/contents/:content", Removed),
    ("GET", "/projects/:id/about", Removed),
    ("GET", "/projects/:id/crdt", Removed),
    ("DELETE", "/projects_by_coords/:coords/coords", Removed),
    ("GET", "/items", Routed),
    ("GET", "/:address/items", Routed),
    ("GET", "/items/:id", Routed),
    (
        "GET",
        "/published-collections/:address/items/:id/utility",
        Todo,
    ),
    ("GET", "/collections/:id/items", Routed),
    ("PUT", "/items/:idOrURN", Routed),
    ("DELETE", "/items/:id", Routed),
    ("POST", "/items/:id/files", Routed),
    ("POST", "/items/:id/videos", Todo),
    ("GET", "/items/:collectionAddress/:itemId/contents", Todo),
    ("GET", "/collections", Routed),
    ("GET", "/:address/collections", Routed),
    ("GET", "/collections/:id", Routed),
    ("POST", "/collections/:id/publish", Todo),
    ("POST", "/collections/:id/owner/sync", Todo),
    ("POST", "/collections/:id/tos", Todo),
    ("POST", "/collections/:id/lock", Todo),
    ("GET", "/collections/:id/approvalData", Todo),
    ("PUT", "/collections/:id", Routed),
    ("DELETE", "/collections/:id", Todo),
    ("GET", "/addresses", Todo),
    ("GET", "/curations", Todo),
    ("GET", "/collections/:id/itemCurations", Todo),
    ("GET", "/collections/:id/curation", Todo),
    ("PATCH", "/collections/:id/curation", Todo),
    ("POST", "/collections/:id/curation", Todo),
    ("POST", "/collections/:id/curation/post", Todo),
    ("GET", "/items/:id/curation", Todo),
    ("PATCH", "/items/:id/curation", Todo),
    ("POST", "/items/:id/curation", Todo),
    ("POST", "/collections/:id/validations", Todo),
    ("POST", "/collections/:id/curation/appeal", Todo),
    ("GET", "/collections/:id/events", Todo),
    ("POST", "/collections/:id/comments", Todo),
    ("POST", "/collections/:id/validation-result", Todo),
    ("GET", "/committee", Todo),
    ("GET", "/thirdParties", Todo),
    ("GET", "/thirdParties/:id/slots", Todo),
    ("GET", "/thirdParties/:id", Todo),
    ("DELETE", "/thirdParties/:id", Todo),
    ("PATCH", "/thirdParties/:id", Todo),
    ("GET", "/rarities", Todo),
    ("GET", "/rarities/:name", Todo),
    ("POST", "/collections/:id/post", Todo),
    ("GET", "/projects/:id/manifest", Routed),
    ("GET", "/manifests", Routed),
    ("GET", "/publics/:id/manifest", Removed),
    ("GET", "/templates/:id/manifest", Removed),
    ("DELETE", "/projects/:id/manifest", Routed),
    ("GET", "/deployments", Removed),
    ("GET", "/projects/:id/deployment", Removed),
    ("DELETE", "/projects/:id/deployment", Removed),
    ("GET", "/storage/assetPacks/:filename", Removed),
    ("GET", "/storage/contents/:filename", Routed),
    ("HEAD", "/storage/contents/:filename", Routed),
    ("GET", "/storage/contents/:filename/exists", Routed),
    ("GET", "/analytics/weekly", Removed),
    ("GET", "/analytics/status", Removed),
    ("GET", "/nfts", Removed),
    ("GET", "/nfts/:contractAddress/:tokenId", Removed),
    ("POST", "/newsletter", Routed),
];

const PARAMETER: &str = "6f1c0a52-5d0e-4c4b-9c55-0d6a1a3b7e21";

fn request_path(upstream: &str) -> String {
    let segments: Vec<&str> = upstream
        .split('/')
        .map(|segment| {
            if segment.starts_with(':') {
                PARAMETER
            } else {
                segment
            }
        })
        .collect();
    format!("/v1{}", segments.join("/"))
}

#[tokio::test]
async fn the_ledger_matches_the_router() {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(100))
        .connect_lazy("postgres://127.0.0.1:1/route_ledger")
        .unwrap();
    let state = Arc::new(
        State::new(
            pool,
            temp_storage().await,
            Arc::default(),
            dcl_http::Outbound::for_tests(),
            None,
            None,
            Settings {
                writes_enabled: true,
                ..Settings::default()
            },
        )
        .unwrap(),
    );
    let app = Router::new()
        .merge(crate::root_router(state))
        .fallback(|| async { StatusCode::IM_A_TEAPOT });

    let mut disagreements = Vec::new();
    for &(method, path, route) in UPSTREAM {
        let (status, _) = call(&app, method, &request_path(path)).await;
        let routed = status != StatusCode::IM_A_TEAPOT && status != StatusCode::METHOD_NOT_ALLOWED;
        if routed != (route == Routed) {
            disagreements.push(format!(
                "{method} {path}: {route:?} in the ledger, answered {status}"
            ));
        }
    }
    assert!(disagreements.is_empty(), "{}", disagreements.join("\n"));
}

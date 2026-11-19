use axum::Json;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::SharedState;
use crate::access;
use crate::auth::{Signed, signed_path, try_extract_signer};
use crate::committee::Member;
use crate::http::{ApiData, ApiError};
use crate::items::{CollectionMetaOut, FullItemOut, ItemQuery};
use crate::shared::signed_fetch::AUTH_CHAIN_HEADER_PREFIX;

use super::onchain::ItemsParams;

const CURATION_STATUSES: [&str; 3] = ["pending", "approved", "rejected"];

const DEFAULT_LIMIT: i64 = 100_000;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "builder/"))]
pub struct PaginatedFullItemsOut {
    pub total: i64,
    pub limit: i64,
    pub pages: i64,
    pub page: i64,
    pub results: Vec<FullItemOut>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "builder/"))]
pub enum CollectionItemsOut {
    Plain(Vec<FullItemOut>),
    Paginated(PaginatedFullItemsOut),
}

fn should_paginate(page: Option<i64>, limit: Option<i64>) -> bool {
    matches!((page, limit), (Some(p), Some(l)) if p != 0 && l != 0 && l < DEFAULT_LIMIT)
}

/// `GET /v1/collections/:id`, for whoever `access::read_collection` admits.
pub async fn get_collection(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
) -> Result<Json<ApiData<CollectionMetaOut>>, ApiError> {
    let read = access::read_collection(&state, &signer, &id).await?;
    let meta = state
        .items
        .collection_by_id(&read)
        .await?
        .ok_or_else(|| ApiError::not_found("Not found"))?;
    Ok(Json(ApiData::ok(CollectionMetaOut::from(&meta))))
}

#[derive(Debug, Default, Deserialize)]
pub struct CollectionItemsParams {
    pub status: Option<String>,
    #[serde(rename = "mappingStatus")]
    pub mapping_status: Option<String>,
    pub synced: Option<bool>,
    pub name: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

/// The explorer's primary route: the collection's items as `FullItem`s,
/// every item for a role holder or the committee and the published ones for
/// any other signer of a published collection (`access::collection_items`);
/// paginated only when `page` and `limit` are both truthy and `limit` is
/// below the upstream default.
pub async fn get_collection_items(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(id): Path<String>,
    Query(params): Query<CollectionItemsParams>,
) -> Result<Json<ApiData<Value>>, ApiError> {
    if let Some(status) = &params.status
        && !CURATION_STATUSES.contains(&status.as_str())
    {
        return Err(ApiError::bad_request("Invalid Status provided"));
    }

    let read = access::collection_items(&state, &signer, &id).await?;

    let paginate = should_paginate(params.page, params.limit);

    let q = ItemQuery {
        status: params.status,
        mapping_status: params.mapping_status,
        synced: params.synced,
        name: params.name,
        page: params.page,
        limit: params.limit,
    };

    let (items, total) = state.items.items_for_collection(&read, &q).await?;
    let results: Vec<FullItemOut> = items.iter().map(|i| i.to_out()).collect();

    let data = if paginate {
        let limit = params.limit.unwrap_or(0);
        let page = params.page.unwrap_or(0);
        let pages = if limit > 0 {
            (total + limit - 1) / limit
        } else {
            0
        };
        CollectionItemsOut::Paginated(PaginatedFullItemsOut {
            total,
            limit,
            pages,
            page,
            results,
        })
    } else {
        CollectionItemsOut::Plain(results)
    };
    let mut data = serde_json::to_value(data).map_err(|e| ApiError::internal(e.to_string()))?;
    if !read.full() {
        let items = match data.get_mut("results") {
            Some(results) => results,
            None => &mut data,
        };
        for item in items.as_array_mut().into_iter().flatten() {
            if let Some(fields) = item.as_object_mut() {
                fields.remove("local_content_hash");
            }
        }
    }
    Ok(Json(ApiData::ok(data)))
}

/// builder-server's `HTTPError('Unauthorized', { eth_address }, 401)`: a
/// listing refusal names the wallet the request was about (the path's
/// address, the signer's, `""` anonymously).
fn unauthorized_for(eth_address: &str) -> ApiError {
    ApiError::unauthorized("Unauthorized").with_data(json!({ "eth_address": eth_address }))
}

#[derive(Debug, Default, Deserialize)]
pub struct CollectionsParams {
    pub q: Option<String>,
    pub is_published: Option<String>,
}

/// `GET /v1/collections` (`getCollections`): `q` with `is_published=true`
/// is the public search over the published collections, anonymous;
/// otherwise the committee lists every wallet's drafts and anyone else,
/// signed or not, is refused with the signer (`""` anonymously). A wallet
/// reads its own set at `/v1/{address}/collections`.
pub async fn get_collections(
    State(state): State<SharedState>,
    Query(params): Query<CollectionsParams>,
    OriginalUri(original): OriginalUri,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let signed = headers.contains_key(format!("{AUTH_CHAIN_HEADER_PREFIX}0"));
    let answer = list_collections(&state, params, &original, &uri, &headers)
        .await
        .into_response();
    if signed {
        crate::http::private_response(answer).await
    } else {
        answer
    }
}

async fn list_collections(
    state: &SharedState,
    params: CollectionsParams,
    original: &Uri,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let q = params.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    if let Some(q) = q
        && params.is_published.as_deref() == Some("true")
    {
        return Ok(Json(ApiData::ok(
            state.items.published_collections_named(q).await?,
        ))
        .into_response());
    }
    match try_extract_signer(
        headers,
        "get",
        &signed_path(original, uri),
        state.eip1654.as_deref(),
    )
    .await
    {
        Some(signer) => {
            let committee = access::committee(state, &signer).await?;
            Ok(Json(ApiData::ok(
                state
                    .items
                    .collection_drafts_of_everyone(&committee)
                    .await?,
            ))
            .into_response())
        }
        None => Err(unauthorized_for("")),
    }
}

/// `GET /v1/items` (`getItems`): the committee lists every wallet's item
/// drafts; a signed wallet that is not on it is refused with its address.
pub async fn get_items(
    State(state): State<SharedState>,
    Signed(signer): Signed,
) -> Result<Response, ApiError> {
    let committee = access::committee(&state, &signer).await?;
    Ok(Json(ApiData::ok(
        state.items.item_drafts_of_everyone(&committee).await?,
    ))
    .into_response())
}

/// `GET /v1/{address}/collections` (`getAddressCollections`): the wallet
/// itself, signed; anyone else is refused with the path's address. The set
/// is the on-chain reader's (`onchain::get_address_collections`).
pub async fn get_address_collections(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(address): Path<String>,
) -> Result<Response, ApiError> {
    let wallet = access::own_address(&signer, &address)?;
    Ok(super::onchain::get_address_collections(&state, &wallet)
        .await?
        .into_response())
}

/// `GET /v1/{address}/items` (`getAddressItems`): the same rule over the
/// on-chain items (`onchain::get_address_items`).
pub async fn get_address_items(
    State(state): State<SharedState>,
    Signed(signer): Signed,
    Path(address): Path<String>,
    Query(params): Query<ItemsParams>,
) -> Result<Response, ApiError> {
    let wallet = access::own_address(&signer, &address)?;
    Ok(super::onchain::get_address_items(&state, &wallet, params)
        .await?
        .into_response())
}

/// `GET /v1/committee`: the current committee members, unauthenticated;
/// 503 while the subgraph can't tell.
pub async fn get_committee(
    State(state): State<SharedState>,
) -> Result<Json<ApiData<Vec<Member>>>, ApiError> {
    let members = state.committee.members(state.market.as_ref()).await?;
    Ok(Json(ApiData::ok(members.as_ref().clone())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::granted;
    use std::sync::Arc;
    use uuid::Uuid;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::shared::signed_fetch::test_support::signed_headers_by;
    use crate::tests::support::{
        FakeCommittee, OWNER_KEY, cleanup, committee_index, temp_storage, wallet,
    };

    const STRANGER_KEY: &str = "0x2222222222222222222222222222222222222222222222222222222222222222";
    const COMMITTEE_KEY: &str =
        "0x3333333333333333333333333333333333333333333333333333333333333333";

    fn address_of(key: &str) -> String {
        format!("{:#x}", wallet(key).address())
    }

    async fn state(committee: &str) -> Option<SharedState> {
        let pool = crate::tests::support::scratch_pool(4).await?;
        let market = committee_index(&FakeCommittee::with(&[committee])).await;
        Some(Arc::new(
            crate::State::new(
                pool,
                temp_storage().await,
                Arc::default(),
                dcl_http::Outbound::for_tests(),
                Some(market),
                None,
                crate::Settings::default(),
            )
            .unwrap(),
        ))
    }

    async fn get(app: &Router, path: &str, key: Option<&str>) -> (StatusCode, Value) {
        let mut request = Request::get(path);
        if let Some(key) = key {
            let now = chrono::Utc::now().timestamp_millis().to_string();
            let local = path.split('?').next().unwrap();
            for (name, value) in signed_headers_by(&wallet(key), "get", local, "{}", true, &now)
                .await
                .iter()
            {
                request = request.header(name, value);
            }
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let cache = response.headers().get(axum::http::header::CACHE_CONTROL);
        if path.starts_with("/v1/drafts/") {
            assert_eq!(cache.unwrap(), "private, no-store");
        }
        if path.starts_with("/v1/collections?") && key.is_none() {
            assert_eq!(cache, None, "an unsigned search stays cacheable");
        }
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    fn refusal_of(address: &str) -> Value {
        json!({ "ok": false, "data": { "eth_address": address }, "error": "Unauthorized" })
    }

    fn ids(body: &Value) -> Vec<String> {
        body["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The four listings, builder-server's rule: a wallet that owns nothing
    /// and is not on the committee gets the upstream 401 envelope naming the
    /// wallet the request was about, the owner its own set, the committee
    /// every wallet's drafts but no other wallet's address listing, the
    /// anonymous published search its matches.
    #[tokio::test]
    async fn listings_are_visible_to_the_owner_and_the_committee_only() {
        let (owner, stranger, committee) = (
            address_of(OWNER_KEY),
            address_of(STRANGER_KEY),
            address_of(COMMITTEE_KEY),
        );
        let Some(state) = state(&committee).await else {
            eprintln!("skipping: CATALYST_TEST_DATABASE_URL not set");
            return;
        };
        let app = crate::router(state.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let (draft, published) = (Uuid::new_v4(), Uuid::new_v4());
        let first: crate::drafts::CollectionDraft =
            serde_json::from_value(json!({ "id": draft, "name": format!("draft {tag}") })).unwrap();
        let saved = state
            .items
            .save_collection_draft(&granted::edit(&owner, draft), &first)
            .await
            .unwrap();
        assert_eq!(saved.eth_address, owner);
        let second: crate::drafts::CollectionDraft =
            serde_json::from_value(json!({ "id": published, "name": format!("SheFi {tag}%_") }))
                .unwrap();
        state
            .items
            .save_collection_draft(&granted::edit(&owner, published), &second)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO builder.items (id, collection_id, eth_address, name, type, blockchain_item_id) \
             VALUES ($1, $2, $3, 'Minted', 'wearable', '0')",
        )
        .bind(Uuid::new_v4())
        .bind(published)
        .bind(&owner)
        .execute(state.items.pool())
        .await
        .unwrap();
        let item = Uuid::new_v4();
        let item_draft: crate::drafts::ItemDraft = serde_json::from_value(json!({
            "id": item, "name": "My wearable", "collection_id": draft,
            "type": "wearable", "data": { "category": "upper_body" }, "rarity": "common", "price": "0",
            "metrics": { "triangles": 42, "meshes": 1 }
        }))
        .unwrap();
        state
            .items
            .save_item_draft(
                &granted::save(&owner, item_draft.id, item_draft.collection_id),
                &item_draft,
            )
            .await
            .unwrap();

        for (kind, expected) in [("items", item), ("collections", draft)] {
            let path = format!("/v1/drafts/{kind}");
            let (status, body) = get(&app, &path, Some(OWNER_KEY)).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "local drafts need no upstream reader: {body}"
            );
            assert!(ids(&body).contains(&expected.to_string()), "{body}");
            for key in [STRANGER_KEY, COMMITTEE_KEY] {
                let (status, body) = get(&app, &path, Some(key)).await;
                assert_eq!(status, StatusCode::OK);
                assert!(
                    !ids(&body).contains(&expected.to_string()),
                    "local drafts are always signer-scoped: {body}"
                );
            }
            let (status, body) = get(&app, &path, None).await;
            assert_eq!(
                (status, body),
                (
                    StatusCode::UNAUTHORIZED,
                    json!({ "ok": false, "data": { "message": "Invalid auth chain" }, "error": "Unauthenticated" })
                ),
                "missing signed-fetch headers are upstream's Unauthenticated"
            );
        }

        let (status, body) = get(&app, "/v1/items", Some(STRANGER_KEY)).await;
        assert_eq!(
            (status, body),
            (StatusCode::UNAUTHORIZED, refusal_of(&stranger))
        );
        let (status, body) = get(&app, "/v1/items", Some(OWNER_KEY)).await;
        assert_eq!(
            (status, body),
            (StatusCode::UNAUTHORIZED, refusal_of(&owner)),
            "an owner is not the committee either"
        );
        let (status, body) = get(&app, "/v1/items", Some(COMMITTEE_KEY)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ids(&body).contains(&item.to_string()),
            "the committee sees another wallet's item draft: {body}"
        );
        let (status, body) = get(&app, "/v1/items", None).await;
        assert_ne!(
            status,
            StatusCode::OK,
            "unsigned is the signer's error: {body}"
        );
        assert_eq!(body["ok"], false);

        let (status, body) = get(&app, "/v1/collections", None).await;
        assert_eq!((status, body), (StatusCode::UNAUTHORIZED, refusal_of("")));
        let (status, body) = get(&app, "/v1/collections", Some(STRANGER_KEY)).await;
        assert_eq!(
            (status, body),
            (StatusCode::UNAUTHORIZED, refusal_of(&stranger))
        );
        let (status, body) = get(&app, "/v1/collections", Some(COMMITTEE_KEY)).await;
        assert_eq!(status, StatusCode::OK);
        let listed = ids(&body);
        assert!(
            listed.contains(&draft.to_string()) && !listed.contains(&published.to_string()),
            "{body}"
        );
        let search = format!("/v1/collections?q={tag}&is_published=true");
        let (status, body) = get(&app, &search, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let found = ids(&body);
        assert!(
            found.contains(&published.to_string()) && !found.contains(&draft.to_string()),
            "{body}"
        );
        let (status, body) = get(
            &app,
            &format!("/v1/collections?q={tag}%25_&is_published=true"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            ids(&body).contains(&published.to_string()),
            "the pattern characters of q are literal: {body}"
        );
        let (status, body) = get(
            &app,
            &format!("/v1/collections?q=nobody-{tag}&is_published=true"),
            None,
        )
        .await;
        assert_eq!((status, ids(&body)), (StatusCode::OK, vec![]));
        let (status, body) = get(&app, &format!("/v1/collections?q={tag}"), None).await;
        assert_eq!(
            (status, body),
            (StatusCode::UNAUTHORIZED, refusal_of("")),
            "q without is_published=true is no search"
        );

        for route in ["collections", "items"] {
            let path = format!("/v1/{owner}/{route}");
            let (status, body) = get(&app, &path, None).await;
            assert_ne!(status, StatusCode::OK, "unsigned {route}: {body}");
            assert_eq!(body["ok"], false);
            for key in [STRANGER_KEY, COMMITTEE_KEY] {
                let (status, body) = get(&app, &path, Some(key)).await;
                assert_eq!(
                    (status, body),
                    (StatusCode::UNAUTHORIZED, refusal_of(&owner)),
                    "{route}: the refusal names the path's wallet; the committee is no exception"
                );
            }
            let (status, body) = get(&app, &path, Some(OWNER_KEY)).await;
            assert_eq!(status, StatusCode::OK, "{route}: {body}");
            let (status, _) = get(
                &app,
                &format!("/v1/{}/{route}", owner.to_uppercase().replace("0X", "0x")),
                Some(OWNER_KEY),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{route}: the path's case does not matter"
            );
        }

        cleanup(&state.items, &[draft, published], &[]).await;
    }

    #[test]
    fn pagination_truthiness_matches_upstream_js() {
        assert!(should_paginate(Some(1), Some(10)));
        assert!(should_paginate(Some(1), Some(99_999)));
        assert!(!should_paginate(None, Some(10)));
        assert!(!should_paginate(Some(1), None));
        assert!(!should_paginate(None, None));
        assert!(!should_paginate(Some(0), Some(10)));
        assert!(!should_paginate(Some(1), Some(0)));
        assert!(!should_paginate(Some(1), Some(100_000)));
        assert!(!should_paginate(Some(1), Some(100_001)));
    }

    #[test]
    fn collection_items_union_arms_serialize_correctly() {
        let plain = ApiData::ok(CollectionItemsOut::Plain(vec![]));
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            json!({ "ok": true, "data": [] }),
        );

        let paginated = ApiData::ok(CollectionItemsOut::Paginated(PaginatedFullItemsOut {
            total: 5,
            limit: 2,
            pages: 3,
            page: 1,
            results: vec![],
        }));
        assert_eq!(
            serde_json::to_value(&paginated).unwrap(),
            json!({
                "ok": true,
                "data": { "total": 5, "limit": 2, "pages": 3, "page": 1, "results": [] },
            }),
        );
    }
}

//! The item reads as builder-server answers them: one item by id, the items
//! of a collection (`getCollectionItems`), an address's items
//! (`getAddressItems`) and every item (`getItems`, the committee's).
//!
//! A listing resolves the page's ids first, ordered by creation and id, with
//! the total of what matched, and builds bodies for that page only. Filters
//! are bound values over fixed SQL; nothing from the request becomes SQL
//! text. Bounds: a collection's items and an address's items answer at most
//! [`ROWS_CAP`] rows per response, `GET /v1/items` at most [`EVERY_ITEM_CAP`].
//! A collection's curation filters read each item's newest item curation:
//! `synced`, its approved hash against the item's own; a third-party
//! collection's `status`, whatever status it names, a pending one; and
//! `mappingStatus=unpublished_mapping`, mappings it doesn't mark complete.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::json;
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::access::{CollectionRead, ItemRead, OnCommittee, Wallet};
use crate::chain_state;
use crate::collections::listing::{ListQuery, Page, page};
use crate::http::ApiError;

use super::ItemsComponent;
use super::body::{FullItemOut, ItemRow, Reached, by_id_columns, columns, from};

/// The rows a collection's or an address's items answer per response: above
/// the largest collection (12,665 items) and the largest owner (12,669) in
/// the reference data.
pub const ROWS_CAP: i64 = 20_000;

/// The rows `GET /v1/items` answers, oldest first.
pub const EVERY_ITEM_CAP: i64 = 1_000;

/// A listing: a plain array, or a page when `page` and `limit` both ask for
/// one.
#[derive(Debug, Serialize)]
#[serde(untagged)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "builder/"))]
pub enum ItemsOut {
    Plain(Vec<FullItemOut>),
    Paginated(ItemsPageOut),
}

impl ItemsOut {
    /// The items it answers.
    pub fn len(&self) -> usize {
        match self {
            Self::Plain(items) => items.len(),
            Self::Paginated(page) => page.results.len(),
        }
    }

    /// Whether it answers no item.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "builder/"))]
pub struct ItemsPageOut {
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub total: Option<i64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub limit: i64,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub pages: Option<i64>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub page: i64,
    pub results: Vec<FullItemOut>,
}

/// The item `id`, as `reached`; 404 when gone.
pub(crate) async fn item(
    store: &ItemsComponent,
    id: Uuid,
    reached: Reached,
    full: bool,
) -> Result<FullItemOut, ApiError> {
    let mut conn = store.pool.acquire().await?;
    let view = chain_state::for_read(&mut conn).await?;
    let row = sqlx::query_as::<_, ItemRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM {} WHERE i.id = $1",
        by_id_columns(),
        from(&view)
    )))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ApiError::not_found("Item not found"))?;
    row.withheld(&store.denylist).out(reached, full)
}

/// The statement that reads the rows of a list's bodies, the ids as `$1`.
pub(crate) fn list_statement(view: &chain_state::ReadView) -> String {
    format!(
        "SELECT {} FROM {} WHERE i.id = ANY($1)",
        columns(),
        from(view)
    )
}

/// The bodies of `ids`, in that order, read in one read-only transaction
/// without JIT (a page's bodies are a few thousand rows at most).
pub(crate) async fn bodies(
    store: &ItemsComponent,
    ids: &[Uuid],
    reached: Reached,
    full: bool,
) -> Result<Vec<FullItemOut>, ApiError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut tx = store.pool.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL jit = off").execute(&mut *tx).await?;
    let view = chain_state::for_read(&mut tx).await?;
    let rows = sqlx::query_as::<_, ItemRow>(sqlx::AssertSqlSafe(list_statement(&view)))
        .bind(ids)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    let mut rows: HashMap<Uuid, ItemRow> = rows.into_iter().map(|row| (row.id, row)).collect();
    ids.iter()
        .filter_map(|id| rows.remove(id))
        .map(|row| row.withheld(&store.denylist).out(reached, full))
        .collect()
}

/// A statement whose scope the caller pushes next: a `SELECT` of `id`,
/// `created_at` and `tp` (a third-party item) per item.
fn scope() -> QueryBuilder<Postgres> {
    QueryBuilder::new("WITH scope AS (")
}

/// The page's ids, oldest first, with whether each is a third-party item,
/// and, when `counted`, the total matched; after the scope [`scope`] began.
async fn page_ids(
    pool: &PgPool,
    mut sql: QueryBuilder<Postgres>,
    page: &Page,
    counted: bool,
) -> Result<(Vec<(Uuid, bool)>, Option<i64>), ApiError> {
    sql.push(if counted {
        ") SELECT id, tp, count(*) OVER () FROM scope ORDER BY created_at, id LIMIT "
    } else {
        ") SELECT id, tp, 0::bigint FROM scope ORDER BY created_at, id LIMIT "
    });
    sql.push_bind(page.limit)
        .push(" OFFSET ")
        .push_bind(page.offset);
    let rows: Vec<(Uuid, bool, i64)> = sql.build_query_as().fetch_all(pool).await?;
    let total = counted
        .then(|| rows.first().map(|(_, _, total)| *total))
        .flatten();
    Ok((
        rows.into_iter().map(|(id, tp, _)| (id, tp)).collect(),
        total,
    ))
}

fn answer(
    page: &Page,
    total: Option<i64>,
    results: Vec<FullItemOut>,
    empty_total: Option<i64>,
) -> ItemsOut {
    match page.page {
        Some(number) => {
            let total = total.or(empty_total);
            ItemsOut::Paginated(ItemsPageOut {
                total,
                limit: page.limit,
                pages: total.map(|t| (t + page.limit - 1) / page.limit),
                page: number,
                results,
            })
        }
        None => ItemsOut::Plain(results),
    }
}

/// Standard items first, then third-party ones, each in page order:
/// builder-server merges the two kinds apart and answers them in that order.
fn standard_first(ids: Vec<(Uuid, bool)>) -> Vec<Uuid> {
    let (standard, third_party): (Vec<_>, Vec<_>) = ids.into_iter().partition(|(_, tp)| !tp);
    standard
        .into_iter()
        .chain(third_party)
        .map(|(id, _)| id)
        .collect()
}

/// `getCollectionItems`' filters, read from the query string.
struct CollectionFilters {
    name: Option<String>,
    synced: Option<bool>,
    status: bool,
    mapping: Option<bool>,
}

impl CollectionFilters {
    fn of(q: &ListQuery) -> Result<Self, ApiError> {
        Ok(Self {
            name: q.non_empty("name")?.map(str::to_string),
            synced: q.non_empty("synced")?.map(|s| s == "true"),
            status: q.non_empty("status")?.is_some(),
            mapping: match q.non_empty("mappingStatus")? {
                Some("missing_mapping") => Some(false),
                Some("unpublished_mapping") => Some(true),
                _ => None,
            },
        })
    }
}

impl ItemsComponent {
    /// `GET /v1/items/:id`.
    pub async fn item(&self, read: &ItemRead) -> Result<FullItemOut, ApiError> {
        item(self, read.id(), Reached::ById, read.full()).await
    }

    /// `GET /v1/collections/:id/items`: every item for a full read, else the
    /// published ones (minted, or submitted for a third-party item).
    pub async fn collection_items(
        &self,
        read: &CollectionRead,
        q: &ListQuery,
    ) -> Result<ItemsOut, ApiError> {
        let filters = CollectionFilters::of(q)?;
        let page = page(q, ROWS_CAP)?;
        let third_party: bool = sqlx::query_scalar(
            "SELECT third_party_id IS NOT NULL AND urn_suffix IS NOT NULL FROM builder.collections WHERE id = $1",
        )
        .bind(read.id())
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(false);
        let status = third_party && filters.status;
        let mapping = filters.mapping.filter(|_| third_party);
        let mut scope = scope();
        scope.push("SELECT i.id, i.created_at, false AS tp FROM builder.items i ");
        if filters.synced.is_some() || status || mapping.is_some() {
            scope.push(
                "LEFT JOIN LATERAL (SELECT k.status, k.content_hash, k.is_mapping_complete \
                 FROM builder.item_curations k WHERE k.item_id = i.id ORDER BY k.created_at DESC LIMIT 1) nk ON true ",
            );
        }
        scope.push("WHERE i.collection_id = ").push_bind(read.id());
        match filters.synced {
            Some(true) => {
                scope
                    .push(" AND nk.status = 'approved' AND nk.content_hash = i.local_content_hash");
            }
            Some(false) => {
                scope.push(
                    " AND nk.status = 'approved' AND nk.content_hash <> i.local_content_hash",
                );
            }
            None => {}
        }
        if status {
            scope.push(" AND nk.status = 'pending'");
        }
        if let Some(name) = &filters.name {
            scope
                .push(" AND i.name ILIKE '%' || ")
                .push_bind(name.clone())
                .push(" || '%'");
        }
        if let Some(with_mappings) = mapping {
            scope.push(if with_mappings {
                " AND i.mappings IS NOT NULL AND NOT COALESCE(nk.is_mapping_complete, false)"
            } else {
                " AND i.mappings IS NULL"
            });
        }
        if !read.full() {
            scope
                .push(" AND (i.blockchain_item_id IS NOT NULL OR ")
                .push(crate::curation::ITEM_CURATED)
                .push(")");
        }
        let (ids, total) = page_ids(&self.pool, scope, &page, true).await?;
        let ids: Vec<Uuid> = ids.into_iter().map(|(id, _)| id).collect();
        let results = bodies(self, &ids, Reached::InList, read.full()).await?;
        Ok(answer(&page, total, results, Some(0)))
    }

    /// `GET /v1/{address}/items`: the wallet's own items that are not
    /// third-party items, and the items of the collections of the providers
    /// it manages; `collectionId` narrows them to one collection, `null` to
    /// items with none.
    pub async fn address_items(
        &self,
        wallet: &Wallet,
        q: &ListQuery,
    ) -> Result<ItemsOut, ApiError> {
        let page = page(q, ROWS_CAP)?;
        let collection = match q.non_empty("collectionId")? {
            None => None,
            Some("null") => Some(None),
            Some(raw) => Some(Some(Uuid::parse_str(raw).map_err(|_| {
                ApiError::bad_request("Invalid collectionId")
                    .with_data(json!({ "collectionId": raw }))
            })?)),
        };
        let narrow = |sql: &mut QueryBuilder<Postgres>| match collection {
            None => {}
            Some(None) => {
                sql.push(" AND i.collection_id IS NULL");
            }
            Some(Some(id)) => {
                sql.push(" AND i.collection_id = ").push_bind(id);
            }
        };
        let mut scope = scope();
        scope.push(
            "SELECT i.id, i.created_at, false AS tp FROM builder.items i \
             LEFT JOIN builder.collections c ON c.id = i.collection_id WHERE i.eth_address = ",
        );
        scope
            .push_bind(wallet.address().to_string())
            .push(" AND (c.third_party_id IS NULL OR i.urn_suffix IS NULL)");
        narrow(&mut scope);
        scope.push(
            " UNION ALL SELECT i.id, i.created_at, true FROM builder.collections c \
             JOIN builder.items i ON i.collection_id = c.id \
             WHERE i.urn_suffix IS NOT NULL AND c.third_party_id = ANY(",
        );
        scope.push_bind(wallet.provider_ids()).push(")");
        narrow(&mut scope);
        let (ids, total) = page_ids(&self.pool, scope, &page, true).await?;
        let results = bodies(self, &standard_first(ids), Reached::InList, true).await?;
        Ok(answer(&page, total, results, None))
    }

    /// `GET /v1/items`: every item, at most [`EVERY_ITEM_CAP`], standard ones
    /// first.
    pub async fn every_item(&self, _committee: &OnCommittee) -> Result<Vec<FullItemOut>, ApiError> {
        let page = Page {
            limit: EVERY_ITEM_CAP,
            offset: 0,
            page: None,
        };
        let mut sql = scope();
        sql.push(
            "SELECT i.id, i.created_at, (c.third_party_id IS NOT NULL AND i.urn_suffix IS NOT NULL) AS tp \
             FROM builder.items i LEFT JOIN builder.collections c ON c.id = i.collection_id",
        );
        let (ids, _) = page_ids(&self.pool, sql, &page, false).await?;
        bodies(self, &standard_first(ids), Reached::InList, true).await
    }
}

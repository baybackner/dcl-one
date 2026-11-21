//! The place catalogue derived from this node's own scene deployments (the
//! old `place-catalog-sync`, `PLACES_DERIVE_FROM_CONTENT`), a job over
//! `content.deployments` that composes with the places mirror:
//!
//! - [`DeriveScope::All`] (`PLACES_DERIVE_FROM_CONTENT` without
//!   `PLACES_MIRROR_UPSTREAM`, the old production mode): every active scene
//!   becomes a place row, `raw.source = "content"`, keyed by the UUIDv5 of
//!   its base parcel so a scene keeps one id across redeployments.
//! - [`DeriveScope::Local`] (every other node): only the scenes this node
//!   accepted itself (`content.local_entities`, not tombstoned), so a local
//!   deploy appears among the places whether or not the upstream catalogue
//!   is mirrored beside it. These rows are `origin = 'local'`, which the
//!   places mirror's retire and sweep spare (the document keeps the older
//!   marker, `raw.local = true`, by which the mirror reclassifies a row an
//!   older binary derived).
//!
//! Either scope also lists the worlds this node publishes itself in
//! `explore.place_world_local` (`raw.source = "worlds"`, keyed by the world's
//! name), from `worlds.worlds` and its local `worlds.world_scenes`; the
//! mirrored row of such a world goes, and the places mirror skips it.
//!
//! Source: `content.deployments` (active, `entity_type = 'scene'`) with
//! `content.content_files` (the navmap thumbnail) and `content.local_entities`.
//! Freshness: one pass per `PLACES_DERIVE_INTERVAL_SECS`; a pass stamps
//! `fetched_at` on every row it derives and prunes the derived rows it did not
//! stamp (a scene undeployed, overwritten or unpublished). Rebuild: the same
//! pass (every derived row is recomputed from its deployment; the operator's
//! keys in `raw` and the moderation overlay survive). A derived row never
//! overwrites a mirrored one on an id collision: the upstream is the
//! authority for the rows it lists.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

use super::moderation::{self, CreatorRating, RatedRow};
use super::query::{EXCLUDE_FROM_RANKING_SQL, raw_float8_sql};
use crate::jobs::{Jobs, PeriodicCfg};
use crate::land_authz::LandAuthzState;

pub const JOB_NAME: &str = "places-derive";
pub const PAGE: i64 = 1000;
/// deployments.id is a plain `integer` sequence, so the walk starts below every
/// row it can hold.
pub const BEFORE_FIRST_SCENE: i32 = i32::MIN;
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeriveScope {
    /// Every active scene deployment (the old derive mode).
    All,
    /// The scenes this node accepted itself.
    Local,
}

impl DeriveScope {
    /// The old rule: the node derives its whole catalogue from its content
    /// when asked to and when no upstream catalogue is mirrored; otherwise it
    /// derives its own scenes only.
    pub fn resolve(derive_from_content: bool, places_mirrored: bool) -> Self {
        if derive_from_content && !places_mirrored {
            Self::All
        } else {
            Self::Local
        }
    }
}

pub struct DerivedPlace {
    pub id: String,
    pub base_position: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator_address: Option<String>,
    pub content_rating: Option<String>,
    pub deployed_at: Option<NaiveDateTime>,
    pub raw: Value,
    /// A scene this node accepted itself (`explore.place.origin = 'local'`).
    pub local: bool,
}

fn scene_json(metadata: &Value) -> &Value {
    metadata.get("v").unwrap_or(metadata)
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn str_array(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Creator categories follow the upstream active catalogue: at most three,
/// deduplicated in tag order. Editorial categories are operator authority.
fn creator_categories(tags: &[String]) -> Vec<String> {
    let mut categories = Vec::new();
    for tag in tags {
        if tag != "poi"
            && tag != "featured"
            && super::handlers::category_i18n_en(tag).is_some()
            && !categories.contains(tag)
        {
            categories.push(tag.clone());
            if categories.len() == 3 {
                break;
            }
        }
    }
    categories
}

pub fn derive(
    deployer_address: &str,
    pointers: &[String],
    deployed_at: Option<NaiveDateTime>,
    metadata: &Value,
    thumbnail_hash: Option<&str>,
    content_public_url: &str,
    local: bool,
) -> Option<DerivedPlace> {
    let m = scene_json(metadata);
    let scene = m.get("scene")?;
    let base = non_empty(scene.get("base").and_then(Value::as_str))
        .or_else(|| pointers.first().cloned())?;
    let display = m.get("display");
    let title = non_empty(display.and_then(|d| d.get("title")).and_then(Value::as_str));
    let description = non_empty(
        display
            .and_then(|d| d.get("description"))
            .and_then(Value::as_str),
    );
    let contact = m.get("contact");
    let contact_name = non_empty(contact.and_then(|c| c.get("name")).and_then(Value::as_str));
    let contact_email = non_empty(contact.and_then(|c| c.get("email")).and_then(Value::as_str));
    // Stored as a rating code (`M` as `A`); an unknown code reads as none.
    let content_rating = non_empty(
        m.get("policy")
            .and_then(|p| p.get("contentRating"))
            .and_then(Value::as_str),
    )
    .and_then(|c| moderation::creator_code(&c))
    .map(str::to_string);
    let mut positions = str_array(scene.get("parcels"));
    if positions.is_empty() {
        positions.push(base.clone());
    }
    let tags = str_array(m.get("tags"));
    let image = thumbnail_hash.map(|h| {
        format!(
            "{}/contents/{}",
            content_public_url.trim_end_matches('/'),
            h
        )
    });
    // Curation identity comes from the signed deployment, never scene-authored metadata.
    let creator_address = non_empty(Some(deployer_address)).map(|s| s.to_lowercase());
    let owner = creator_address.clone();
    let categories = creator_categories(&tags);
    let id = super::parcel_place_id(&base);
    let at = deployed_at.map(|d| DateTime::<Utc>::from_naive_utc_and_offset(d, Utc));
    let mut raw = json!({
        "id": id,
        "title": title,
        "description": description,
        "owner": owner,
        "metadata_owner": non_empty(m.get("owner").and_then(Value::as_str)),
        "metadata_creator": non_empty(m.get("creator").and_then(Value::as_str)),
        "creator_address": creator_address,
        "sdk": non_empty(m.get("runtimeVersion").and_then(Value::as_str)),
        "positions": positions,
        "base_position": base,
        "contact_name": contact_name,
        "contact_email": contact_email,
        "image": image,
        "tags": tags,
        "categories": categories,
        "disabled": false,
        "world": false,
        "deployed_at": at,
        "created_at": at,
        "updated_at": at,
        "source": "content",
    });
    if local {
        raw["local"] = json!(true);
    }
    Some(DerivedPlace {
        id,
        base_position: base,
        title,
        description,
        creator_address,
        content_rating,
        deployed_at,
        raw,
        local,
    })
}

/// Keyset, never OFFSET: an undeployment sets deleter_deployment mid-scan and
/// shifts every later OFFSET window back a row, so a still-served scene goes
/// unvisited and the prune then deletes it on its stale fetched_at. `$3`
/// false walks every active scene, true only the local ones; a tombstoned
/// local scene (unpublished) is never derived.
pub const SELECT_SCENES: &str = r#"
    SELECT
        d.id,
        d.entity_id,
        d.deployer_address,
        d.entity_pointers,
        d.entity_timestamp AS deployed_at,
        (d.entity_metadata::jsonb) AS meta,
        (le.entity_id IS NOT NULL) AS local,
        (SELECT cf.content_hash FROM content.content_files cf
          WHERE cf.deployment = d.id
            AND cf.key = (d.entity_metadata::jsonb)->'v'->'display'->>'navmapThumbnail'
          LIMIT 1) AS thumbnail_hash,
        (SELECT cf.content_hash FROM content.content_files cf
          WHERE cf.deployment = d.id AND cf.key = 'scene.json'
          LIMIT 1) AS scene_json_hash
    FROM content.deployments d
    LEFT JOIN content.local_entities le ON le.entity_id = d.entity_id
    WHERE d.entity_type = 'scene' AND d.deleter_deployment IS NULL AND d.id > $2
      AND (le.entity_id IS NOT NULL OR NOT $3)
      AND le.tombstoned_at IS NULL
    ORDER BY d.id
    LIMIT $1
"#;

/// Every raw key an operator writes outside this rebuild has to be named in the
/// preservation object: the pass rebuilds `raw` wholesale from the deployment,
/// so a key it does not carry forward is erased by the next pass. The
/// moderation overlay (`place_moderation`) decides `disabled`, `highlighted`
/// and `content_rating` of a new row; an existing row keeps its own. The
/// creator rating rule then settles the rating of a new deployment or a new
/// row ([`upsert_page`]). A row the
/// places mirror wrote is left to the mirror.
const UPSERT: &str = r#"
    INSERT INTO explore.place
        (id, base_position, title, description, creator_address, content_rating,
         categories, likes, dislikes, favorites, deployed_at, disabled, highlighted,
         raw, fetched_at, origin)
    SELECT u.id, u.base_position, u.title, u.description, u.creator_address,
           -- a scene's policy.contentRating may spell pending PR; the stored
           -- code is RP (places::moderation)
           COALESCE(m.content_rating, CASE WHEN u.content_rating = 'PR' THEN 'RP' ELSE u.content_rating END),
           ARRAY(SELECT jsonb_array_elements_text(u.raw->'categories')), 0, 0, 0,
           u.deployed_at, COALESCE(m.disabled, false), COALESCE(m.highlighted, false),
           u.raw || COALESCE(r.overlay, '{}'::jsonb)
               -- a new row disabled by an operator carries the disable's instant and reason
               || CASE WHEN m.disabled IS TRUE THEN jsonb_build_object(
                           'disabled_at', to_char(m.disabled_at, 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
                           'disabled_reason', m.reason)
                       ELSE '{}'::jsonb END,
           timezone('UTC', now()), u.origin
    FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[],
                $7::timestamp[], $8::jsonb[], $9::text[])
         AS u(id, base_position, title, description, creator_address, content_rating,
              deployed_at, raw, origin)
    LEFT JOIN explore.place_moderation m ON m.place_id = u.id
    LEFT JOIN explore.place_ranking r ON r.place_id = u.id
    ON CONFLICT (id) DO UPDATE SET
        base_position   = EXCLUDED.base_position,
        title           = EXCLUDED.title,
        description     = EXCLUDED.description,
        creator_address = EXCLUDED.creator_address,
        categories = CASE WHEN cardinality(EXCLUDED.categories) = 0 THEN explore.place.categories ELSE
            ARRAY(SELECT c FROM unnest(EXCLUDED.categories || explore.place.categories) WITH ORDINALITY AS tags(c, position)
                  WHERE c IN ('poi', 'featured') OR c = ANY(EXCLUDED.categories)
                  GROUP BY c ORDER BY min(position)) END,
        deployed_at     = EXCLUDED.deployed_at,
        raw = EXCLUDED.raw || jsonb_strip_nulls(jsonb_build_object(
            'ranking',              explore.place.raw->'ranking',
            'exclude_from_ranking', explore.place.raw->'exclude_from_ranking',
            'highlighted_image',    explore.place.raw->'highlighted_image',
            'like_score',           explore.place.raw->'like_score',
            'like_rate',            explore.place.raw->'like_rate',
            'created_at',           explore.place.raw->'created_at',
            'disabled_at',          explore.place.raw->'disabled_at',
            'disabled_reason',      explore.place.raw->'disabled_reason'
        )),
        fetched_at = timezone('UTC', now()),
        origin     = EXCLUDED.origin
    WHERE explore.place.raw->>'source' = 'content'
    RETURNING id, (xmax = 0) AS inserted
"#;

/// Overlap candidates for every fresh row of a page at once: `$1` the fresh ids, `$2`
/// each one's positions as a jsonb array.
pub(crate) fn overlapping_places_sql() -> &'static str {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        let ranking = raw_float8_sql("ranking");
        format!(
            r#"
    SELECT f.id AS fresh_id, p.id, p.highlighted, p.creator_address,
           {ranking} AS ranking,
           {EXCLUDE_FROM_RANKING_SQL} AS exclude_from_ranking,
           raw->>'highlighted_image' AS highlighted_image
    FROM unnest($1::text[], $2::jsonb[]) AS f(id, positions)
    JOIN explore.place p
      ON p.id <> f.id AND p.disabled IS FALSE AND p.world IS FALSE
     AND p.raw->'positions' ?| ARRAY(SELECT jsonb_array_elements_text(f.positions))
"#
        )
    });
    &SQL
}

const INHERIT_CURATION: &str = r#"
    UPDATE explore.place
    SET highlighted = u.highlighted,
        raw = raw || jsonb_build_object(
            'ranking',              u.ranking,
            'highlighted_image',    u.highlighted_image,
            'exclude_from_ranking', u.exclude_from_ranking
        )
    FROM unnest($1::text[], $2::boolean[], $3::float8[], $4::text[], $5::boolean[])
         AS u(id, highlighted, ranking, highlighted_image, exclude_from_ranking)
    WHERE explore.place.id = u.id
"#;

/// The derived rows a pass did not stamp: `$1` the pass start.
const PRUNE: &str =
    "DELETE FROM explore.place WHERE raw->>'source' = 'content' AND fetched_at < $1";

/// One `explore.place_world_local` row per unblocked world this node publishes
/// itself (no mirror source and a local scene, `worlds::core::store::deploy_scene`;
/// a blocked world is hidden as the worlds listing hides it), keyed
/// by the lowercase name as upstream keys its worlds: the world row's settings
/// with its newest local scene's base parcel, contact and deploy time. `$1` the
/// content public URL the thumbnail is served under. The moderation and ranking
/// overlays apply as in the places mirror; a row this pass did not write is
/// left alone.
const UPSERT_LOCAL_WORLDS: &str = r#"
    INSERT INTO explore.place_world_local AS l
        (id, base_position, title, description, creator_address, content_rating,
         categories, deployed_at, disabled, highlighted, raw, fetched_at, world_name, world)
    SELECT u.id, u.base_position, u.title, u.description, u.owner,
           COALESCE(m.content_rating, u.content_rating), u.categories, u.deployed_at,
           COALESCE(m.disabled, false), COALESCE(m.highlighted, false),
           u.raw || COALESCE(r.overlay, '{}'::jsonb)
               -- an operator's disable carries its instant and reason (places::moderation)
               || CASE WHEN m.disabled IS TRUE THEN jsonb_build_object(
                           'disabled_at', to_char(m.disabled_at, 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
                           'disabled_reason', m.reason)
                       ELSE '{}'::jsonb END,
           timezone('UTC', now()), u.world_name, true
    FROM (
        SELECT w.name AS id, w.description, w.owner, w.content_rating,
               COALESCE(w.categories, '{}') AS categories, s.created_at AS deployed_at,
               COALESCE(NULLIF(btrim(s.meta->'scene'->>'base'), ''), s.parcels[1], '0,0') AS base_position,
               COALESCE(w.title, NULLIF(btrim(s.meta->'display'->>'title'), '')) AS title,
               CASE WHEN lower(btrim(s.meta->'worldConfiguration'->>'name')) = w.name
                    THEN btrim(s.meta->'worldConfiguration'->>'name') ELSE w.name END AS world_name,
               jsonb_build_object(
                   'title', COALESCE(w.title, NULLIF(btrim(s.meta->'display'->>'title'), '')),
                   'description', w.description,
                   'image', CASE WHEN COALESCE(w.thumbnail_hash, s.thumbnail_hash) IS NOT NULL
                                 THEN rtrim($1, '/') || '/contents/' || COALESCE(w.thumbnail_hash, s.thumbnail_hash) END,
                   'owner', w.owner,
                   'content_rating', w.content_rating,
                   'categories', to_jsonb(COALESCE(w.categories, '{}')),
                   'show_in_places', w.show_in_places,
                   'single_player', w.single_player,
                   'skybox_time', w.skybox_time,
                   'is_private', w.access_type <> 'unrestricted',
                   'settings_version', w.settings_version,
                   'contact_name', NULLIF(btrim(s.meta->'contact'->>'name'), ''),
                   'contact_email', NULLIF(btrim(s.meta->'contact'->>'email'), ''),
                   'sdk', NULLIF(btrim(s.meta->>'runtimeVersion'), ''),
                   'base_position', COALESCE(NULLIF(btrim(s.meta->'scene'->>'base'), ''), s.parcels[1], '0,0'),
                   'disabled', false,
                   'world', true,
                   'deployed_at', to_char(s.created_at, 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
                   'created_at', to_char(w.created_at, 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
                   'updated_at', to_char(GREATEST(w.updated_at, s.created_at), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"'),
                   'source', 'worlds',
                   'local', true
               ) AS raw
        FROM worlds.worlds w
        CROSS JOIN LATERAL (
            SELECT ws.entity->'metadata' AS meta, ws.parcels, ws.created_at,
                   (SELECT file->>'hash' FROM jsonb_array_elements(ws.entity->'content') file
                    WHERE file->>'file' = ws.entity->'metadata'->'display'->>'navmapThumbnail'
                    LIMIT 1) AS thumbnail_hash
            FROM worlds.world_scenes ws
            WHERE ws.world_name = w.name AND ws.mirror_source IS NULL
            ORDER BY ws.created_at DESC, ws.entity_id
            LIMIT 1
        ) s
        WHERE w.mirror_source IS NULL AND w.blocked_since IS NULL
    ) u
    LEFT JOIN explore.place_moderation m ON m.place_id = u.id
    LEFT JOIN explore.place_ranking r ON r.place_id = u.id
    ON CONFLICT (id) DO UPDATE SET
        base_position   = EXCLUDED.base_position,
        title           = EXCLUDED.title,
        description     = EXCLUDED.description,
        creator_address = EXCLUDED.creator_address,
        content_rating  = EXCLUDED.content_rating,
        categories      = EXCLUDED.categories,
        deployed_at     = EXCLUDED.deployed_at,
        disabled        = EXCLUDED.disabled,
        highlighted     = EXCLUDED.highlighted,
        raw             = EXCLUDED.raw,
        fetched_at      = EXCLUDED.fetched_at,
        world_name      = EXCLUDED.world_name
    WHERE l.raw->>'source' = 'worlds'
"#;

/// A world this node publishes is listed once, from the local leg: the mirrored
/// row of the same world goes (the places mirror skips it while the local row
/// stands, `mirror::UPSERT`).
const DISPLACE_MIRRORED_WORLDS: &str = r#"
    DELETE FROM explore.place p USING explore.place_world_local l
    WHERE l.raw->>'source' = 'worlds' AND p.world IS TRUE
      AND (p.id = l.id OR lower(p.world_name) = l.id)
"#;

/// The local world rows a pass did not stamp (every local scene unpublished,
/// the world blocked, or mirrored again): `$1` the pass start.
const PRUNE_LOCAL_WORLDS: &str =
    "DELETE FROM explore.place_world_local WHERE raw->>'source' = 'worlds' AND fetched_at < $1";

#[derive(Debug, Clone, PartialEq)]
pub struct Curation {
    pub highlighted: bool,
    pub ranking: Option<f64>,
    pub exclude_from_ranking: bool,
    pub highlighted_image: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CurationCandidate {
    pub creator_address: Option<String>,
    pub curation: Curation,
}

/// A fresh place inherits the curation of the one curated place its parcels
/// overlap when the same creator deployed both (a redeploy under a new base
/// parcel keeps its highlight); two curated predecessors, or none, inherit
/// nothing.
pub fn inherited_curation(
    candidates: &[CurationCandidate],
    creator: Option<&str>,
) -> Option<Curation> {
    let creator = creator.filter(|c| !c.is_empty())?;
    let mut curated = candidates.iter().filter(|c| {
        (c.curation.highlighted
            || c.curation.exclude_from_ranking
            || c.curation.ranking.unwrap_or(0.0) > 0.0)
            && c.creator_address.as_deref() == Some(creator)
    });
    let predecessor = curated.next()?;
    if curated.next().is_some() {
        return None;
    }
    Some(predecessor.curation.clone())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PassSummary {
    pub derived: usize,
    pub pruned: u64,
}

/// One pass: walk the scenes of `scope` by id, upsert each page, prune the
/// derived rows not stamped; then the local worlds, in one transaction.
pub async fn run_once(
    pool: &PgPool,
    scope: DeriveScope,
    content_public_url: &str,
) -> Result<PassSummary> {
    run_once_with_storage(pool, scope, content_public_url, None, None, None).await
}

/// The stored, hash-addressed scene.json supplies creator/runtimeVersion.
/// Limit each read to 2 MiB; missing, withheld or malformed metadata keeps
/// the catalogue available without turning a storage fault into an empty pass.
async fn scene_file(
    storage: &crate::storage::Storage,
    entity: &str,
    hash: &str,
) -> Result<Option<Value>> {
    const MAX_SCENE_JSON_BYTES: u64 = 2 * 1024 * 1024;
    if storage.withholds(entity) {
        return Ok(None);
    }
    let Some(mut file) = storage.open(hash).await? else {
        return Ok(None);
    };
    if file.size > MAX_SCENE_JSON_BYTES {
        tracing::warn!(
            entity,
            hash,
            size = file.size,
            "place scene.json exceeds metadata read limit"
        );
        return Ok(None);
    }
    let last = file.size.saturating_sub(1);
    let bytes = file.read_range(0, last).await?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) if value.is_object() => Ok(Some(value)),
        Ok(_) => Ok(None),
        Err(error) => {
            tracing::debug!(entity, hash, %error, "place scene.json is not JSON metadata");
            Ok(None)
        }
    }
}

fn apply_scene_file(place: &mut DerivedPlace, scene: &Value, metadata: &Value) {
    place.raw["metadata_creator"] = json!(
        non_empty(scene.get("creator").and_then(Value::as_str))
            .or_else(|| non_empty(scene_json(metadata).get("creator").and_then(Value::as_str)))
    );
    place.raw["sdk"] = json!(
        non_empty(scene.get("runtimeVersion").and_then(Value::as_str)).or_else(|| non_empty(
            scene_json(metadata)
                .get("runtimeVersion")
                .and_then(Value::as_str)
        ))
    );
}

async fn run_once_with_storage(
    pool: &PgPool,
    scope: DeriveScope,
    content_public_url: &str,
    storage: Option<&crate::storage::Storage>,
    notify: Option<&str>,
    land: Option<&LandAuthzState>,
) -> Result<PassSummary> {
    let started: NaiveDateTime = sqlx::query_scalar("SELECT timezone('UTC', clock_timestamp())")
        .fetch_one(pool)
        .await?;
    let mut last_id = BEFORE_FIRST_SCENE;
    let mut derived = 0usize;
    loop {
        let rows = sqlx::query(SELECT_SCENES)
            .bind(PAGE)
            .bind(last_id)
            .bind(scope == DeriveScope::Local)
            .fetch_all(pool)
            .await?;
        let fetched = rows.len() as i64;
        let mut candidates = Vec::with_capacity(rows.len());
        for row in &rows {
            last_id = row.try_get("id")?;
            let deployer: String = row.try_get("deployer_address").unwrap_or_default();
            let pointers: Vec<String> = row.try_get("entity_pointers").unwrap_or_default();
            let deployed_at: Option<NaiveDateTime> = row.try_get("deployed_at").unwrap_or(None);
            let meta: Value = row.try_get("meta").unwrap_or(Value::Null);
            let local: bool = row.try_get("local").unwrap_or(false);
            let thumb: Option<String> = row.try_get("thumbnail_hash").unwrap_or(None);
            if let Some(place) = derive(
                &deployer,
                &pointers,
                deployed_at,
                &meta,
                thumb.as_deref(),
                content_public_url,
                local,
            ) {
                candidates.push((
                    place,
                    row.try_get::<String, _>("entity_id")?,
                    row.try_get::<Option<String>, _>("scene_json_hash")?,
                    meta,
                ));
            }
        }
        // One primary-key lookup per bounded page. Immutable scene blobs are re-read only
        // when their deployment or hash changes; a failed read is retried next pass.
        let ids: Vec<&str> = candidates
            .iter()
            .map(|(place, ..)| place.id.as_str())
            .collect();
        let cached: HashMap<String, Value> = sqlx::query_as::<_, (String, Value)>(
            "SELECT id, raw FROM explore.place WHERE id = ANY($1) AND raw->>'source' = 'content'",
        )
        .bind(&ids)
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect();
        let mut page = Vec::with_capacity(candidates.len());
        let mut failed = Vec::new();
        for (mut place, entity, hash, meta) in candidates {
            if let (Some(storage), Some(hash)) = (storage, hash) {
                let previous = cached.get(&place.id).filter(|raw| {
                    raw["scene_metadata_entity"].as_str() == Some(entity.as_str())
                        && raw["scene_metadata_hash"].as_str() == Some(hash.as_str())
                });
                if let Some(previous) = previous {
                    for key in ["sdk", "metadata_creator"] {
                        place.raw[key] = previous[key].clone();
                    }
                } else {
                    match scene_file(storage, &entity, &hash).await {
                        Ok(Some(scene)) => apply_scene_file(&mut place, &scene, &meta),
                        Ok(None) => {
                            // Missing or withheld blobs may become readable without redeployment.
                            page.push(place);
                            derived += 1;
                            continue;
                        }
                        Err(error) => {
                            tracing::warn!(%entity, %hash, %error, "skipping place with unreadable scene metadata");
                            failed.push(place.id);
                            continue;
                        }
                    }
                }
                place.raw["scene_metadata_entity"] = json!(entity);
                place.raw["scene_metadata_hash"] = json!(hash);
            }
            page.push(place);
            derived += 1;
        }
        // Preserve the last usable catalogue row on transient I/O failures; pruning still
        // removes deployments absent from this pass, and failed rows remain due next pass.
        if !failed.is_empty() {
            sqlx::query("UPDATE explore.place SET fetched_at = timezone('UTC', clock_timestamp()) WHERE id = ANY($1) AND raw->>'source' = 'content'")
                .bind(&failed).execute(pool).await?;
        }
        upsert_page(pool, page, notify, land).await?;
        if fetched < PAGE {
            break;
        }
    }
    let mut pruned = sqlx::query(PRUNE)
        .bind(started)
        .execute(pool)
        .await?
        .rows_affected();
    let mut tx = pool.begin().await?;
    derived += sqlx::query(UPSERT_LOCAL_WORLDS)
        .bind(content_public_url)
        .execute(&mut *tx)
        .await?
        .rows_affected() as usize;
    sqlx::query(DISPLACE_MIRRORED_WORLDS)
        .execute(&mut *tx)
        .await?;
    pruned += sqlx::query(PRUNE_LOCAL_WORLDS)
        .bind(started)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(PassSummary { derived, pruned })
}

fn positions_of(p: &DerivedPlace) -> Vec<String> {
    p.raw["positions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A derived row's rating before this page: `(content_rating, deployed_at)`
/// by id, locked until the page commits.
const LOCK_RATINGS: &str = "SELECT id, content_rating, deployed_at FROM explore.place \
     WHERE id = ANY($1) AND raw->>'source' = 'content' ORDER BY id FOR UPDATE";

/// The ratings a new deployment or a new row is held to: its own operator
/// rating; the rating recorded on each of its parcels by the same place (the
/// same id or exactly the same parcel set), the same creator or under the same
/// LAND owner; and the rating of every other derived place by the same creator
/// its parcels overlap, or with exactly its parcels (the rows it replaces,
/// which `PRUNE` removes; that row's operator rating first), disabled ones
/// included. A place by another creator on another owner's LAND and another
/// parcel set starts fresh; an empty creator or owner matches none. `$1` the
/// ids, `$2` each one's positions; `$3` to `$7` one entry per parcel: the id,
/// the position, the lowercase creator, the parcel's LAND owner and the
/// place's parcel set ([`parcel_set`]). The overlapping places are found one
/// parcel at a time (`@>` on one position, behind `OFFSET 0`), so each probe
/// uses `place_positions_path_gin`.
const HELD_RATINGS: &str = r#"
    SELECT f.id, m.content_rating AS rating
    FROM unnest($1::text[]) AS f(id)
    JOIN explore.place_moderation m ON m.place_id = f.id AND m.content_rating IS NOT NULL
    UNION ALL
    SELECT c.id, r.content_rating
    FROM unnest($3::text[], $4::text[], $5::text[], $6::text[], $7::text[])
         AS c(id, position, creator, owner, parcel_set)
    JOIN explore.place_parcel_rating r ON r.position = c.position
     AND (r.place_id = c.id OR r.parcel_set_hash = c.parcel_set
          OR (c.creator <> '' AND r.creator_address = c.creator)
          OR (c.owner <> '' AND r.owner_address = c.owner))
    UNION ALL
    SELECT c.id, COALESCE(m.content_rating, p.content_rating)
    FROM unnest($3::text[], $4::text[], $5::text[]) AS c(id, position, creator)
    CROSS JOIN LATERAL (
        SELECT p.id, p.creator_address, p.content_rating, p.raw FROM explore.place p
        WHERE p.raw->'positions' @> jsonb_build_array(c.position)
          AND p.world IS FALSE AND p.raw->>'source' = 'content'
        OFFSET 0) p
    JOIN unnest($1::text[], $2::jsonb[]) AS f(id, positions) ON f.id = c.id
    LEFT JOIN explore.place_moderation m ON m.place_id = p.id
    WHERE p.id <> c.id
      AND ((c.creator <> '' AND lower(btrim(p.creator_address)) = c.creator)
           OR ((p.raw->'positions') @> f.positions AND f.positions @> (p.raw->'positions')))
"#;

/// The page's parcel ratings, locked after its places as an operator's
/// rating locks them (`moderation::apply_with`).
const LOCK_PARCEL_RATINGS: &str = "SELECT 1 FROM explore.place_parcel_rating \
     WHERE position = ANY($1) ORDER BY position FOR UPDATE";

/// Of the parcels `$1` of the unchanged places `$2`: those with no recorded
/// rating of a rated place (`$4`; `missing`, which the page fills), and with
/// a LAND index (`$3`) those recorded with an unknown owner by their own
/// place, or with a known owner by another place (`missing` false). Either
/// means the place was settled while a parcel's owner was unknown; once the
/// index knows it the page settles the place again. An unlocked read: the
/// page then locks only what it writes.
const PARCEL_STATES: &str = "SELECT u.pos, u.id, r.position IS NULL AS missing \
     FROM unnest($1::text[], $2::text[], $4::bool[]) AS u(pos, id, rated) \
     LEFT JOIN explore.place_parcel_rating r ON r.position = u.pos \
     WHERE (r.position IS NULL AND u.rated) \
        OR ($3 AND ((r.place_id = u.id AND r.owner_address IS NULL) \
                 OR (r.place_id <> u.id AND r.owner_address IS NOT NULL)))";

/// Locks the parcels a page writes, recorded or not, as an operator's rating
/// does (`moderation::LOCK_PARCELS`), then the recorded rows.
async fn lock_page_parcels(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    parcels: &[String],
) -> Result<()> {
    for sql in [moderation::LOCK_PARCELS, LOCK_PARCEL_RATINGS] {
        sqlx::query(sql).bind(parcels).execute(&mut **tx).await?;
    }
    Ok(())
}

/// Records `$2` on each parcel `$1` for the place `$3` by the creator `$4`
/// under the LAND owner `$6`, with the place's parcel set `$7`: `$5` true
/// overwrites a recorded rating, false only fills a parcel with none. A place
/// with an operator rating records an operator's rating. An operator's record
/// is replaced only by a place held to it ([`HELD_RATINGS`], which keeps it an
/// operator's) or under a known owner other than its known one; another
/// place with an unknown owner replaces no record with a known owner, which
/// the page settles again once the owner is known ([`PARCEL_STATES`]). An
/// unknown owner keeps the owner the same place (or parcel set) recorded, as
/// `moderation::OPERATOR_PARCEL_RATINGS` does.
const RECORD_PARCEL_RATINGS: &str = r#"
    INSERT INTO explore.place_parcel_rating AS r
        (position, content_rating, place_id, parcel_set_hash, creator_address, owner_address, operator)
    SELECT DISTINCT ON (u.position) u.position, u.rating, u.place_id, u.parcel_set,
           NULLIF(lower(btrim(u.creator)), ''), NULLIF(u.owner, ''),
           EXISTS (SELECT 1 FROM explore.place_moderation m
                   WHERE m.place_id = u.place_id AND m.content_rating IS NOT NULL)
    FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $6::text[], $7::text[])
         AS u(position, rating, place_id, creator, owner, parcel_set)
    ORDER BY u.position
    ON CONFLICT (position) DO UPDATE
    SET content_rating = EXCLUDED.content_rating, place_id = EXCLUDED.place_id,
        parcel_set_hash = EXCLUDED.parcel_set_hash,
        creator_address = EXCLUDED.creator_address,
        owner_address = COALESCE(EXCLUDED.owner_address,
            CASE WHEN r.place_id = EXCLUDED.place_id OR r.parcel_set_hash = EXCLUDED.parcel_set_hash
                 THEN r.owner_address END),
        operator = EXCLUDED.operator OR (r.operator AND COALESCE(
            r.place_id = EXCLUDED.place_id OR r.parcel_set_hash = EXCLUDED.parcel_set_hash
            OR r.creator_address = EXCLUDED.creator_address
            OR r.owner_address = EXCLUDED.owner_address, false)),
        updated_at = timezone('UTC', now())
    WHERE $5
      AND (r.place_id = EXCLUDED.place_id OR r.parcel_set_hash = EXCLUDED.parcel_set_hash
           OR r.creator_address = EXCLUDED.creator_address
           OR (r.owner_address IS NOT NULL AND EXCLUDED.owner_address IS NOT NULL)
           OR (NOT r.operator AND r.owner_address IS NULL))
      AND (r.content_rating, r.place_id, r.parcel_set_hash, r.creator_address, r.owner_address)
          IS DISTINCT FROM (EXCLUDED.content_rating, EXCLUDED.place_id, EXCLUDED.parcel_set_hash,
                            EXCLUDED.creator_address,
                            COALESCE(EXCLUDED.owner_address,
                                CASE WHEN r.place_id = EXCLUDED.place_id
                                       OR r.parcel_set_hash = EXCLUDED.parcel_set_hash
                                     THEN r.owner_address END))
"#;

/// Clears the parcels `$1` of a place left with no rating, so a later place
/// on them is held to none it did not hold. Such a place is held to no
/// record. A record with a known owner stays while `$2`, the parcel's LAND
/// owner, is unknown; an operator's record stays unless both owners are known
/// and differ (the LAND changed owner) and the record is of another parcel
/// set than `$3`.
const CLEAR_PARCEL_RATINGS: &str = "DELETE FROM explore.place_parcel_rating r \
     USING unnest($1::text[], $2::text[], $3::text[]) AS c(position, owner, parcel_set) \
     WHERE r.position = c.position \
       AND ((NOT r.operator AND (r.owner_address IS NULL OR c.owner <> '')) \
            OR (r.owner_address IS NOT NULL AND c.owner <> '' \
                AND r.owner_address <> c.owner AND r.parcel_set_hash <> c.parcel_set))";

/// The page's operator rows, locked before its places as an operator's
/// decision locks them (`moderation::apply_with`).
const LOCK_OVERLAYS: &str = "SELECT 1 FROM explore.place_moderation \
     WHERE place_id = ANY($1) ORDER BY place_id FOR UPDATE";

const SET_RATINGS: &str = "UPDATE explore.place SET content_rating = u.rating \
     FROM unnest($1::text[], $2::text[]) AS u(id, rating) WHERE explore.place.id = u.id";

/// An operator's rating follows the rating the row takes, so a later
/// re-insert of the row (`UPSERT`'s overlay) keeps it.
const FOLLOW_RATINGS: &str = "UPDATE explore.place_moderation m SET content_rating = u.rating \
     FROM unnest($1::text[], $2::text[]) AS u(id, rating) \
     WHERE m.place_id = u.id AND m.content_rating IS NOT NULL \
       AND m.content_rating IS DISTINCT FROM u.rating";

/// One transaction per page: a multi-row upsert, the creator rating rule
/// ([`settle_ratings`]), one overlap query for the fresh rows, and one batched
/// curation update. Later duplicates of an id win, as they did row by row.
async fn upsert_page(
    pool: &PgPool,
    page: Vec<DerivedPlace>,
    notify: Option<&str>,
    land: Option<&LandAuthzState>,
) -> Result<()> {
    let mut slot_of: HashMap<&str, usize> = HashMap::new();
    let mut order: Vec<usize> = Vec::with_capacity(page.len());
    for (i, p) in page.iter().enumerate() {
        match slot_of.get(p.id.as_str()) {
            Some(&slot) => order[slot] = i,
            None => {
                slot_of.insert(p.id.as_str(), order.len());
                order.push(i);
            }
        }
    }
    let rows: Vec<&DerivedPlace> = order.iter().map(|&i| &page[i]).collect();
    if rows.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    sqlx::query(LOCK_OVERLAYS)
        .bind(rows.iter().map(|p| p.id.clone()).collect::<Vec<_>>())
        .execute(&mut *tx)
        .await?;
    let before: HashMap<String, (Option<String>, Option<NaiveDateTime>)> =
        sqlx::query(LOCK_RATINGS)
            .bind(rows.iter().map(|p| p.id.clone()).collect::<Vec<_>>())
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|r| {
                (
                    r.try_get::<String, _>("id").unwrap_or_default(),
                    (
                        r.try_get("content_rating").unwrap_or(None),
                        r.try_get("deployed_at").unwrap_or(None),
                    ),
                )
            })
            .collect();
    let PageParcels {
        locked: parcels,
        owners,
        recheck,
    } = page_parcels(&mut tx, &rows, &before, land).await?;
    lock_page_parcels(&mut tx, &parcels).await?;
    let inserted: HashMap<String, bool> = sqlx::query(UPSERT)
        .bind(rows.iter().map(|p| p.id.clone()).collect::<Vec<_>>())
        .bind(
            rows.iter()
                .map(|p| p.base_position.clone())
                .collect::<Vec<_>>(),
        )
        .bind(rows.iter().map(|p| p.title.clone()).collect::<Vec<_>>())
        .bind(
            rows.iter()
                .map(|p| p.description.clone())
                .collect::<Vec<_>>(),
        )
        .bind(
            rows.iter()
                .map(|p| p.creator_address.clone())
                .collect::<Vec<_>>(),
        )
        .bind(
            rows.iter()
                .map(|p| p.content_rating.clone())
                .collect::<Vec<_>>(),
        )
        .bind(rows.iter().map(|p| p.deployed_at).collect::<Vec<_>>())
        .bind(rows.iter().map(|p| p.raw.clone()).collect::<Vec<_>>())
        .bind(
            rows.iter()
                .map(|p| if p.local { "local" } else { "upstream" })
                .collect::<Vec<_>>(),
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|r| {
            (
                r.try_get::<String, _>("id").unwrap_or_default(),
                r.try_get::<bool, _>("inserted").unwrap_or(false),
            )
        })
        .collect();
    let page = Page {
        before: &before,
        inserted: &inserted,
        locked: &parcels,
        owners: &owners,
        recheck: &recheck,
    };
    settle_ratings(&mut tx, &rows, &page, notify).await?;
    let fresh: Vec<(&DerivedPlace, Vec<String>)> = rows
        .iter()
        .filter(|p| inserted.get(&p.id).copied().unwrap_or(false))
        .map(|p| (*p, positions_of(p)))
        .filter(|(_, positions)| !positions.is_empty())
        .collect();
    if fresh.is_empty() {
        tx.commit().await?;
        return Ok(());
    }
    let mut candidates: HashMap<String, Vec<(String, CurationCandidate)>> = HashMap::new();
    let found = sqlx::query(sqlx::AssertSqlSafe(overlapping_places_sql()))
        .bind(fresh.iter().map(|(p, _)| p.id.clone()).collect::<Vec<_>>())
        .bind(
            fresh
                .iter()
                .map(|(_, positions)| Value::from(positions.clone()))
                .collect::<Vec<Value>>(),
        )
        .fetch_all(&mut *tx)
        .await?;
    for r in found {
        let fresh_id: String = r.try_get("fresh_id").unwrap_or_default();
        let id: String = r.try_get("id").unwrap_or_default();
        candidates.entry(fresh_id).or_default().push((
            id,
            CurationCandidate {
                creator_address: r.try_get("creator_address").unwrap_or(None),
                curation: Curation {
                    highlighted: r.try_get("highlighted").unwrap_or(false),
                    ranking: r.try_get("ranking").unwrap_or(None),
                    exclude_from_ranking: r.try_get("exclude_from_ranking").unwrap_or(false),
                    highlighted_image: r.try_get("highlighted_image").unwrap_or(None),
                },
            },
        ));
    }
    // Fresh rows earlier in the page that just inherited count as curated for later ones,
    // exactly as when each row was committed before the next was examined.
    let mut inherited: Vec<(String, Curation)> = Vec::new();
    for (p, _) in &fresh {
        let mine: Vec<CurationCandidate> = candidates
            .remove(&p.id)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, mut c)| {
                if let Some((_, cur)) = inherited.iter().find(|(i, _)| *i == id) {
                    c.curation = cur.clone();
                }
                c
            })
            .collect();
        if let Some(curation) = inherited_curation(&mine, p.creator_address.as_deref()) {
            inherited.push((p.id.clone(), curation));
        }
    }
    if !inherited.is_empty() {
        sqlx::query(INHERIT_CURATION)
            .bind(
                inherited
                    .iter()
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>(),
            )
            .bind(
                inherited
                    .iter()
                    .map(|(_, c)| c.highlighted)
                    .collect::<Vec<_>>(),
            )
            .bind(inherited.iter().map(|(_, c)| c.ranking).collect::<Vec<_>>())
            .bind(
                inherited
                    .iter()
                    .map(|(_, c)| c.highlighted_image.clone())
                    .collect::<Vec<_>>(),
            )
            .bind(
                inherited
                    .iter()
                    .map(|(_, c)| c.exclude_from_ranking)
                    .collect::<Vec<_>>(),
            )
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// What a page writes and reads before it settles: the parcels it may
/// write, sorted and locked; their LAND owners (one lookup per page); and the
/// unchanged places it settles again.
struct PageParcels {
    locked: Vec<String>,
    owners: HashMap<String, String>,
    recheck: HashSet<String>,
}

/// The parcels a page may write: those of a new row or a new deployment;
/// the unrecorded ones of a rated row it fills; and every parcel of an
/// unchanged row it settles again ([`PARCEL_STATES`]: one settled while a
/// parcel's owner was unknown, once the index knows it). Only these are
/// locked, so a page of unchanged rows does not hold an operator's rating of
/// another place. One owner lookup covers them and the parcels whose owner
/// decides a second settle; a parcel the index never knows is looked up
/// again on each pass.
async fn page_parcels(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: &[&DerivedPlace],
    before: &HashMap<String, (Option<String>, Option<NaiveDateTime>)>,
    land: Option<&LandAuthzState>,
) -> Result<PageParcels> {
    let changed = |p: &DerivedPlace| before.get(&p.id).is_none_or(|(_, at)| *at != p.deployed_at);
    let mut parcels: Vec<String> = rows
        .iter()
        .filter(|p| changed(p))
        .flat_map(|p| positions_of(p))
        .collect();
    let (mut positions, mut places, mut rated) = (Vec::new(), Vec::new(), Vec::new());
    for p in rows.iter().filter(|p| !changed(p)) {
        let is_rated = before
            .get(&p.id)
            .and_then(|(rating, _)| rating.as_deref())
            .and_then(moderation::creator_code)
            .is_some();
        if !is_rated && land.is_none() {
            continue;
        }
        for position in positions_of(p) {
            positions.push(position);
            places.push(p.id.clone());
            rated.push(is_rated);
        }
    }
    let mut suspect: HashMap<String, Vec<String>> = HashMap::new();
    if !positions.is_empty() {
        for r in sqlx::query(PARCEL_STATES)
            .bind(&positions)
            .bind(&places)
            .bind(land.is_some())
            .bind(&rated)
            .fetch_all(&mut **tx)
            .await?
        {
            let position: String = r.try_get("pos")?;
            if r.try_get::<bool, _>("missing")? {
                parcels.push(position);
            } else {
                suspect.entry(r.try_get("id")?).or_default().push(position);
            }
        }
    }
    let of = |id: &str| {
        rows.iter()
            .find(|p| p.id == id)
            .map(|p| positions_of(p))
            .unwrap_or_default()
    };
    let mut lookup = parcels.clone();
    for id in suspect.keys() {
        lookup.extend(of(id));
    }
    lookup.sort();
    lookup.dedup();
    let owners = moderation::parcel_owners(tx, land, &lookup).await?;
    let recheck: HashSet<String> = suspect
        .into_iter()
        .filter(|(_, at)| at.iter().any(|p| owners.contains_key(p)))
        .map(|(id, _)| id)
        .collect();
    for id in &recheck {
        parcels.extend(of(id));
    }
    parcels.sort();
    parcels.dedup();
    Ok(PageParcels {
        locked: parcels,
        owners,
        recheck,
    })
}

/// A page's state before its ratings settle: each row's rating and
/// deployment time before it, which rows the upsert inserted, the locked
/// parcels, their LAND owners and the unchanged rows settled again.
struct Page<'a> {
    before: &'a HashMap<String, (Option<String>, Option<NaiveDateTime>)>,
    inserted: &'a HashMap<String, bool>,
    locked: &'a [String],
    owners: &'a HashMap<String, String>,
    recheck: &'a HashSet<String>,
}

/// The creator rating rule (`places::moderation::CreatorRating`) over a page:
/// a new deployment of an existing row, or a new row, takes the scene's rating
/// when it raises the one held, never a laxer one. It is held to its stored
/// or operator rating, and to the ratings of the same place (the same id or
/// exactly the same parcels, whoever deploys it), the same creator or the
/// same LAND owner (`owners`) on its parcels: those recorded there and those
/// of the places it replaces; a place by another creator on another owner's
/// LAND and other parcels starts fresh. The rating it takes is recorded on its
/// parcels, and a place left with no rating clears them, an operator's
/// record only once the LAND changed owner. A pass over the same deployment
/// leaves the stored rating, an operator's included, and only fills its
/// parcels' missing records among `locked`, unless the page settles it again
/// (`recheck`: settled while a parcel's owner was unknown), which may raise
/// it to a rating now held by owner, and then reports only a change. A scene
/// that declares no rating keeps the held one, with no notice.
async fn settle_ratings(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: &[&DerivedPlace],
    page: &Page<'_>,
    notify: Option<&str>,
) -> Result<()> {
    let Page {
        before,
        inserted,
        locked,
        owners,
        recheck,
    } = *page;
    let redeployed = |p: &DerivedPlace| match inserted.get(&p.id) {
        Some(true) => true,
        Some(false) => before
            .get(&p.id)
            .is_some_and(|(_, at)| *at != p.deployed_at),
        None => false,
    };
    let settled: Vec<&DerivedPlace> = rows
        .iter()
        .copied()
        .filter(|p| redeployed(p) || (inserted.contains_key(&p.id) && recheck.contains(&p.id)))
        .collect();
    let mut held: HashMap<String, Vec<String>> = HashMap::new();
    if !settled.is_empty() {
        let ids: Vec<String> = settled.iter().map(|p| p.id.clone()).collect();
        let positions: Vec<Value> = settled
            .iter()
            .map(|p| Value::from(positions_of(p)))
            .collect();
        let (mut cell_ids, mut cells, mut cell_creators, mut cell_owners, mut cell_sets) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for p in &settled {
            let set = parcel_set(p);
            for position in positions_of(p) {
                cell_ids.push(p.id.clone());
                cell_creators.push(creator_of(p));
                cell_owners.push(owners.get(&position).cloned().unwrap_or_default());
                cell_sets.push(set.clone());
                cells.push(position);
            }
        }
        for r in sqlx::query(HELD_RATINGS)
            .bind(&ids)
            .bind(&positions)
            .bind(&cell_ids)
            .bind(&cells)
            .bind(&cell_creators)
            .bind(&cell_owners)
            .bind(&cell_sets)
            .fetch_all(&mut **tx)
            .await?
        {
            let entry = held
                .entry(r.try_get::<String, _>("id").unwrap_or_default())
                .or_default();
            if let Some(rating) = r.try_get::<Option<String>, _>("rating").unwrap_or(None) {
                entry.push(rating);
            }
        }
    }
    let (mut ids, mut ratings) = (Vec::new(), Vec::new());
    let (mut recorded, mut missing) = (Records::default(), Records::default());
    let (mut cleared, mut cleared_sets): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for p in rows {
        let fresh = inserted.get(&p.id) == Some(&true);
        if !settled.iter().any(|s| s.id == p.id) {
            // A row stored before the rule may hold any code: `M` is recorded
            // as `A`, an off-scale code not at all.
            if let Some(rating) = before
                .get(&p.id)
                .filter(|_| inserted.contains_key(&p.id))
                .and_then(|(rating, _)| rating.as_deref())
                .and_then(moderation::creator_code)
            {
                let set = parcel_set(p);
                for position in positions_of(p) {
                    if locked.binary_search(&position).is_ok() {
                        missing.push(position, rating, p, &set, owners);
                    }
                }
            }
            continue;
        }
        // The rating held, and whether a creator's change to it is reported.
        let held_to = held.remove(&p.id);
        let is_held = held_to.is_some();
        let mut codes = held_to.unwrap_or_default();
        let own = if fresh {
            None
        } else {
            before.get(&p.id).and_then(|(rating, _)| rating.clone())
        };
        codes.extend(own.clone());
        let stored = moderation::strictest(codes.iter().map(String::as_str)).map(str::to_string);
        let declared = p.content_rating.as_deref();
        let rating = match declared {
            Some(d) if CreatorRating::of(stored.as_deref(), d).applies() => Some(d.to_string()),
            _ => stored.clone(),
        };
        let code = |r: &Option<String>| r.as_deref().and_then(moderation::creator_code);
        let report = (!fresh || is_held) && (redeployed(p) || code(&rating) != code(&own));
        if let (Some(base), Some(stored), Some(declared), true) =
            (notify, stored.as_deref(), declared, report)
        {
            let row = RatedRow::Place {
                id: &p.id,
                base_position: &p.base_position,
            };
            moderation::capture_creator_rating(tx, base, row, stored, declared).await?;
        }
        // A held `M` (stored before the rule) is taken as `A`.
        let Some(rating) = rating
            .as_deref()
            .and_then(moderation::creator_code)
            .map(str::to_string)
        else {
            let set = parcel_set(p);
            for position in positions_of(p) {
                cleared.push(position);
                cleared_sets.push(set.clone());
            }
            continue;
        };
        let set = parcel_set(p);
        for position in positions_of(p) {
            recorded.push(position, &rating, p, &set, owners);
        }
        if fresh || Some(&rating) != own.as_ref() {
            ids.push(p.id.clone());
            ratings.push(rating);
        }
    }
    if !ids.is_empty() {
        for sql in [SET_RATINGS, FOLLOW_RATINGS] {
            sqlx::query(sql)
                .bind(&ids)
                .bind(&ratings)
                .execute(&mut **tx)
                .await?;
        }
    }
    if !cleared.is_empty() {
        let cleared_owners: Vec<String> = cleared
            .iter()
            .map(|p| owners.get(p).cloned().unwrap_or_default())
            .collect();
        sqlx::query(CLEAR_PARCEL_RATINGS)
            .bind(&cleared)
            .bind(&cleared_owners)
            .bind(&cleared_sets)
            .execute(&mut **tx)
            .await?;
    }
    for (records, overwrite) in [(&recorded, true), (&missing, false)] {
        if !records.positions.is_empty() {
            sqlx::query(RECORD_PARCEL_RATINGS)
                .bind(&records.positions)
                .bind(&records.ratings)
                .bind(&records.places)
                .bind(&records.creators)
                .bind(overwrite)
                .bind(&records.owners)
                .bind(&records.sets)
                .execute(&mut **tx)
                .await?;
        }
    }
    Ok(())
}

/// A place's creator as its parcel records keep it: the lowercase deployer
/// address, empty when unknown (which matches no record).
fn creator_of(p: &DerivedPlace) -> String {
    p.creator_address
        .as_deref()
        .map(|c| c.trim().to_lowercase())
        .unwrap_or_default()
}

/// A place's whole parcel set as its parcel records keep it
/// ([`super::parcel_set_hash`]).
fn parcel_set(p: &DerivedPlace) -> String {
    super::parcel_set_hash(&positions_of(p))
}

/// Parcel records to write: each parcel's rating, place, creator, LAND
/// owner (empty when unknown) and the place's parcel set.
#[derive(Default)]
struct Records {
    positions: Vec<String>,
    ratings: Vec<String>,
    places: Vec<String>,
    creators: Vec<String>,
    owners: Vec<String>,
    sets: Vec<String>,
}

impl Records {
    fn push(
        &mut self,
        position: String,
        rating: &str,
        place: &DerivedPlace,
        set: &str,
        owners: &HashMap<String, String>,
    ) {
        self.owners
            .push(owners.get(&position).cloned().unwrap_or_default());
        self.positions.push(position);
        self.ratings.push(rating.to_string());
        self.places.push(place.id.clone());
        self.creators.push(creator_of(place));
        self.sets.push(set.to_string());
    }
}

/// Registers the periodic derive job (`places-derive`), reading parcel owners
/// from `land`.
#[allow(clippy::too_many_arguments)]
pub fn spawn_job(
    jobs: &Jobs,
    pool: PgPool,
    scope: DeriveScope,
    interval: Duration,
    content_public_url: String,
    storage: Option<Arc<crate::storage::Storage>>,
    notify: Option<String>,
    land: Option<Arc<LandAuthzState>>,
) {
    tracing::info!(scope = ?scope, interval_secs = interval.as_secs(), "place catalogue: deriving from content deployments");
    jobs.spawn_periodic_leased(
        JOB_NAME,
        PeriodicCfg::every(interval).with_error_backoff(Duration::from_secs(3600)),
        move |ctx| {
            let (pool, url, storage, notify, land) = (
                pool.clone(),
                content_public_url.clone(),
                storage.clone(),
                notify.clone(),
                land.clone(),
            );
            async move {
                let out = run_once_with_storage(
                    &pool,
                    scope,
                    &url,
                    storage.as_deref(),
                    notify.as_deref(),
                    land.as_deref(),
                )
                .await?;
                ctx.progress(format!(
                    "{} places derived, {} pruned",
                    out.derived, out.pruned
                ));
                Ok(())
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page with no LAND index attached.
    async fn upsert_page(
        pool: &PgPool,
        page: Vec<DerivedPlace>,
        notify: Option<&str>,
    ) -> Result<()> {
        super::upsert_page(pool, page, notify, None).await
    }

    async fn run_once_with_storage(
        pool: &PgPool,
        scope: DeriveScope,
        content_public_url: &str,
        storage: Option<&crate::storage::Storage>,
        notify: Option<&str>,
    ) -> Result<PassSummary> {
        super::run_once_with_storage(pool, scope, content_public_url, storage, notify, None).await
    }

    fn wrapped() -> Value {
        json!({ "v": {
            "scene": { "base": "135,2", "parcels": ["135,2", "135,3"] },
            "display": { "title": "My Scene", "description": "hello", "navmapThumbnail": "t.png" },
            "contact": { "name": "author", "email": "a@b.c" },
            "policy": { "contentRating": "E" },
            "tags": ["game", "art"]
        }})
    }

    #[test]
    fn derives_from_v_wrapped_scene() {
        let p = derive(
            "0xABC",
            &["135,2".to_string()],
            None,
            &wrapped(),
            Some("hash123"),
            "/content",
            false,
        )
        .expect("derives");
        assert_eq!(p.base_position, "135,2");
        assert_eq!(p.title.as_deref(), Some("My Scene"));
        assert_eq!(p.creator_address.as_deref(), Some("0xabc"));
        assert_eq!(p.content_rating.as_deref(), Some("E"));
        assert_eq!(p.raw["image"], "/content/contents/hash123");
        assert_eq!(p.raw["positions"], json!(["135,2", "135,3"]));
        assert_eq!(p.raw["source"], "content");
        assert_eq!(p.raw["world"], false);
        assert!(p.raw.get("local").is_none());
        let local = derive("0xABC", &[], None, &wrapped(), None, "/content", true).unwrap();
        assert_eq!(local.raw["local"], true);
    }

    #[test]
    fn scene_file_metadata_cannot_replace_the_signed_deployer_identity() {
        let mut metadata = wrapped();
        metadata["v"]["owner"] = json!("0xLAND");
        metadata["v"]["runtimeVersion"] = json!("6");
        metadata["v"]["tags"] =
            json!(["poi", "game", "game", "art", "social", "music", "featured"]);
        let mut place = derive("0xDEPLOYER", &[], None, &metadata, None, "/content", true).unwrap();
        apply_scene_file(
            &mut place,
            &json!({ "creator": "0xAUTHOR", "runtimeVersion": "7.1" }),
            &metadata,
        );
        assert_eq!(place.creator_address.as_deref(), Some("0xdeployer"));
        assert_eq!(place.raw["owner"], "0xdeployer");
        assert_eq!(place.raw["metadata_owner"], "0xLAND");
        assert_eq!(place.raw["metadata_creator"], "0xAUTHOR");
        assert_eq!(place.raw["sdk"], "7.1");
        assert_eq!(place.raw["categories"], json!(["game", "art", "social"]));
        apply_scene_file(&mut place, &json!({}), &metadata);
        assert_eq!(place.creator_address.as_deref(), Some("0xdeployer"));
        assert_eq!(place.raw["sdk"], "6");
    }

    #[tokio::test]
    async fn creator_categories_survive_rebuild_and_preserve_editorial_categories() {
        let Some(pool) = crate::db::scratch::pool(2).await else {
            return;
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let mut meta = wrapped();
        let base = format!(
            "{},{}",
            u16::from_str_radix(&token[..4], 16).unwrap(),
            u16::from_str_radix(&token[4..8], 16).unwrap()
        );
        meta["v"]["scene"]["base"] = json!(base);
        meta["v"]["scene"]["parcels"] = json!([base]);
        let place = derive("0xauthor", &[], None, &meta, None, "/content", true).unwrap();
        let id = place.id.clone();
        upsert_page(&pool, vec![place], None).await.unwrap();
        sqlx::query("UPDATE explore.place SET categories = array_append(categories, 'featured') WHERE id = $1").bind(&id).execute(&pool).await.unwrap();
        meta["v"]["tags"] = json!(["music"]);
        upsert_page(
            &pool,
            vec![derive("0xauthor", &[], None, &meta, None, "/content", true).unwrap()],
            None,
        )
        .await
        .unwrap();
        let categories: Vec<String> =
            sqlx::query_scalar("SELECT categories FROM explore.place WHERE id = $1")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(categories, vec!["music", "featured"]);
        meta["v"]["tags"] = json!(["unknown", "poi"]);
        upsert_page(
            &pool,
            vec![derive("0xauthor", &[], None, &meta, None, "/content", true).unwrap()],
            None,
        )
        .await
        .unwrap();
        let unchanged: Vec<String> =
            sqlx::query_scalar("SELECT categories FROM explore.place WHERE id = $1")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(unchanged, categories);
    }

    #[tokio::test]
    async fn a_new_deployment_raises_the_rating_and_never_lowers_it() {
        let Some(pool) = crate::db::scratch::pool(2).await else {
            return;
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let base = format!(
            "{},{}",
            u16::from_str_radix(&token[..4], 16).unwrap(),
            u16::from_str_radix(&token[4..8], 16).unwrap()
        );
        let deploy = |rating: &str, minute: u32| {
            let mut meta = wrapped();
            meta["v"]["scene"]["base"] = json!(base);
            meta["v"]["scene"]["parcels"] = json!([base]);
            meta["v"]["policy"] = json!({ "contentRating": rating });
            let at = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, minute, 0);
            vec![derive("0xauthor", &[], at, &meta, None, "/content", true).unwrap()]
        };
        let id = deploy("T", 0)[0].id.clone();
        let rating = || async {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT content_rating FROM explore.place WHERE id = $1",
            )
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        let notices = || async {
            sqlx::query_as::<_, (String, Value)>(
                "SELECT kind, payload FROM explore.moderation_hooks \
                 WHERE channel = 'places' AND subject = $1 ORDER BY created_at",
            )
            .bind(&id)
            .fetch_all(&pool)
            .await
            .unwrap()
        };
        let site = Some("https://decentraland.org/places");

        upsert_page(&pool, deploy("T", 0), site).await.unwrap();
        assert_eq!(rating().await.as_deref(), Some("T"));
        upsert_page(&pool, deploy("A", 1), site).await.unwrap();
        assert_eq!(rating().await.as_deref(), Some("A"), "a redeploy raises");
        upsert_page(&pool, deploy("A", 1), site).await.unwrap();
        upsert_page(&pool, deploy("E", 2), site).await.unwrap();
        assert_eq!(
            rating().await.as_deref(),
            Some("A"),
            "a redeploy never lowers"
        );
        upsert_page(&pool, deploy("E", 2), site).await.unwrap();
        let sent = notices().await;
        let kinds: Vec<&str> = sent.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            ["rating_upgraded", "rating_downgrade_refused"],
            "one notice per new deployment"
        );
        let text = sent[0].1.to_string();
        assert!(text.contains("upgraded by Content Creator"), "{text}");
        assert!(
            text.contains("Previous rating: T - New rating: A"),
            "{text}"
        );
        let text = sent[1].1.to_string();
        assert!(
            text.contains("Actual rating: A - Proposed rating: E"),
            "{text}"
        );

        // An operator lowers it; passes over the same deployment keep that.
        moderation::apply(
            &pool,
            moderation::Target::Place(&id),
            &moderation::Decision::Rating("T".into()),
            "0xmoderator",
            None,
        )
        .await
        .unwrap()
        .unwrap();
        upsert_page(&pool, deploy("E", 2), site).await.unwrap();
        assert_eq!(rating().await.as_deref(), Some("T"));
        upsert_page(&pool, deploy("R", 3), None).await.unwrap();
        assert_eq!(
            rating().await.as_deref(),
            Some("R"),
            "a later raise applies"
        );
        assert_eq!(notices().await.len(), 2, "no notice without the webhook");
    }

    #[tokio::test]
    async fn a_new_row_is_held_to_the_ratings_of_the_rows_it_replaces() {
        let Some(pool) = crate::db::scratch::pool(2).await else {
            return;
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let (x, y) = (
            u16::from_str_radix(&token[..4], 16).unwrap() as i32,
            u16::from_str_radix(&token[4..8], 16).unwrap() as i32 % 30000,
        );
        let (a, b, c) = (
            format!("{x},{y}"),
            format!("{x},{}", y + 1),
            format!("{x},{}", y + 2),
        );
        let deploy = |base: &str, parcels: &[&String], rating: Option<&str>, minute: u32| {
            let mut meta = wrapped();
            meta["v"]["scene"]["base"] = json!(base);
            meta["v"]["scene"]["parcels"] = json!(parcels);
            meta["v"]["policy"] = json!({ "contentRating": rating });
            let at = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, minute, 0);
            vec![derive("0xauthor", &[], at, &meta, None, "/content", true).unwrap()]
        };
        let (id_a, id_b) = (
            super::super::parcel_place_id(&a),
            super::super::parcel_place_id(&b),
        );
        let rating = |id: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT content_rating FROM explore.place WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let operator_rating = |id: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT content_rating FROM explore.place_moderation WHERE place_id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let kinds = |id: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, String>(
                    "SELECT kind FROM explore.moderation_hooks \
                     WHERE channel = 'places' AND subject = $1 ORDER BY created_at",
                )
                .bind(id)
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };
        let prune = |id: String| {
            let pool = pool.clone();
            async move {
                sqlx::query("DELETE FROM explore.place WHERE id = $1")
                    .bind(id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };
        let operator = |id: String, code: &'static str| {
            let pool = pool.clone();
            async move {
                moderation::apply(
                    &pool,
                    moderation::Target::Place(&id),
                    &moderation::Decision::Rating(code.into()),
                    "0xmoderator",
                    None,
                )
                .await
                .unwrap()
                .unwrap();
            }
        };
        let site = Some("https://decentraland.org/places");

        // Moving the base parcel keeps the stricter rating of the row it replaces.
        upsert_page(&pool, deploy(&a, &[&a, &b], Some("A"), 0), site)
            .await
            .unwrap();
        upsert_page(&pool, deploy(&b, &[&a, &b], Some("T"), 1), site)
            .await
            .unwrap();
        assert_eq!(rating(id_b.clone()).await.as_deref(), Some("A"));
        assert_eq!(kinds(id_b.clone()).await, ["rating_downgrade_refused"]);

        // An operator's rating of the replaced row holds too.
        prune(id_a.clone()).await;
        operator(id_b.clone(), "R").await;
        upsert_page(&pool, deploy(&a, &[&a, &b], Some("T"), 2), site)
            .await
            .unwrap();
        assert_eq!(rating(id_a.clone()).await.as_deref(), Some("R"));
        assert_eq!(kinds(id_a.clone()).await, ["rating_downgrade_refused"]);
        prune(id_b.clone()).await;

        upsert_page(&pool, deploy(&a, &[&a, &b], None, 3), site)
            .await
            .unwrap();
        assert_eq!(rating(id_a.clone()).await.as_deref(), Some("R"));

        // A raise moves the operator's rating, so a re-inserted row keeps it.
        operator(id_a.clone(), "T").await;
        upsert_page(&pool, deploy(&a, &[&a, &b], Some("A"), 4), site)
            .await
            .unwrap();
        assert_eq!(rating(id_a.clone()).await.as_deref(), Some("A"));
        assert_eq!(operator_rating(id_a.clone()).await.as_deref(), Some("A"));
        prune(id_a.clone()).await;
        prune(id_b.clone()).await;
        upsert_page(&pool, deploy(&a, &[&a, &b], Some("T"), 5), site)
            .await
            .unwrap();
        assert_eq!(rating(id_a.clone()).await.as_deref(), Some("A"));
        // A stricter scene rating on a new row is taken over the operator's.
        prune(id_a.clone()).await;
        upsert_page(&pool, deploy(&a, &[&a, &b], Some("R"), 6), site)
            .await
            .unwrap();
        assert_eq!(rating(id_a.clone()).await.as_deref(), Some("R"));
        assert_eq!(operator_rating(id_a.clone()).await.as_deref(), Some("R"));

        // Another code of the same rank keeps the stored code; `M` is stored as `A`.
        operator(id_a.clone(), "T").await;
        upsert_page(&pool, deploy(&a, &[&a, &b], Some("E"), 7), site)
            .await
            .unwrap();
        assert_eq!(rating(id_a.clone()).await.as_deref(), Some("T"));
        let id_c = super::super::parcel_place_id(&c);
        upsert_page(&pool, deploy(&c, &[&c], Some("M"), 7), site)
            .await
            .unwrap();
        assert_eq!(rating(id_c.clone()).await.as_deref(), Some("A"));
        assert_eq!(
            kinds(id_a).await,
            [
                "rating_downgrade_refused",
                "rating_upgraded",
                "rating_upgraded"
            ],
            "a new row on rated parcels reports, an unsent downgrade takes the newer message"
        );
        assert!(
            kinds(id_c.clone()).await.is_empty(),
            "a new row on unrated parcels reports nothing"
        );
        // A scene that declares no rating keeps the stored one, with no notice.
        upsert_page(&pool, deploy(&c, &[&c], None, 8), site)
            .await
            .unwrap();
        assert_eq!(rating(id_c.clone()).await.as_deref(), Some("A"));
        assert!(kinds(id_c).await.is_empty());
    }

    /// A page that has locked its parcels, one of them unrecorded, and an
    /// operator rating a place on those parcels wait on each other in turn,
    /// never in a cycle.
    #[tokio::test]
    async fn a_page_and_an_operator_rating_take_parcel_locks_in_one_order() {
        let Some(pool) = crate::db::scratch::pool(4).await else {
            return;
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let x = u16::from_str_radix(&token[..4], 16).unwrap() as i32;
        let y = u16::from_str_radix(&token[4..8], 16).unwrap() as i32 % 30000;
        let (a, b) = (format!("{x},{y}"), format!("{x},{}", y + 1));
        let mut meta = wrapped();
        meta["v"]["scene"]["base"] = json!(a);
        meta["v"]["scene"]["parcels"] = json!([a, b]);
        meta["v"]["policy"] = json!({ "contentRating": "T" });
        let page = vec![derive("0xauthor", &[], None, &meta, None, "/content", true).unwrap()];
        upsert_page(&pool, page, None).await.unwrap();
        sqlx::query("DELETE FROM explore.place_parcel_rating WHERE position = $1")
            .bind(&a)
            .execute(&pool)
            .await
            .unwrap();

        // A page holds `b` (recorded) and will insert `a` (not recorded).
        let parcels = vec![a.clone(), b.clone()];
        let mut page = pool.begin().await.unwrap();
        lock_page_parcels(&mut page, &parcels).await.unwrap();
        let id = super::super::parcel_place_id(&a);
        let operator = tokio::spawn({
            let (pool, id) = (pool.clone(), id.clone());
            async move {
                moderation::apply(
                    &pool,
                    moderation::Target::Place(&id),
                    &moderation::Decision::Rating("R".into()),
                    "0xmoderator",
                    None,
                )
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!operator.is_finished(), "the operator waits for the page");
        sqlx::query(RECORD_PARCEL_RATINGS)
            .bind(&parcels)
            .bind(vec!["A".to_string(); 2])
            .bind(vec![id.clone(); 2])
            .bind(vec!["0xauthor".to_string(); 2])
            .bind(true)
            .bind(vec![String::new(); 2])
            .bind(vec!["test".to_string(); 2])
            .execute(&mut *page)
            .await
            .expect("the page is not chosen as a deadlock victim");
        page.commit().await.unwrap();
        operator.await.unwrap().unwrap().unwrap();
        let recorded: Vec<String> = sqlx::query_scalar(
            "SELECT content_rating FROM explore.place_parcel_rating \
             WHERE position = ANY($1) ORDER BY position",
        )
        .bind(&parcels)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(recorded, ["R", "R"], "the operator's later rating lands");
        sqlx::query("DELETE FROM explore.place_parcel_rating WHERE position = ANY($1)")
            .bind(&parcels)
            .execute(&pool)
            .await
            .unwrap();
        for table in ["explore.place_moderation", "explore.place_content_ratings"] {
            let column = if table.ends_with("ratings") {
                "entity_id"
            } else {
                "place_id"
            };
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE {column} = $1"
            )))
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("DELETE FROM explore.place WHERE id = $1")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_page_locks_only_the_parcels_it_may_write() {
        let Some(pool) = crate::db::scratch::pool(4).await else {
            return;
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let x = u16::from_str_radix(&token[..4], 16).unwrap() as i32;
        let y0 = u16::from_str_radix(&token[4..8], 16).unwrap() as i32 % 30000;
        let parcel = |n: i32| format!("{x},{}", y0 + n);
        let deploy = |parcels: &[&String], rating: Option<&str>, minute| {
            let mut meta = wrapped();
            meta["v"]["scene"]["base"] = json!(parcels[0]);
            meta["v"]["scene"]["parcels"] = json!(parcels);
            meta["v"]["policy"] = json!({ "contentRating": rating });
            let at = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, minute, 0);
            derive("0xauthor", &[], at, &meta, None, "/content", true).unwrap()
        };
        let (a, b, c, d) = (parcel(0), parcel(1), parcel(3), parcel(5));
        // Another place on a parcel in `a`'s lock bucket, so a page that
        // locked its unchanged rows' parcels would hold the operator.
        let e: String = sqlx::query_scalar(
            "SELECT $1 || ',' || ($2 + n) FROM generate_series(7, 5000) AS n \
             WHERE hashtext($1 || ',' || ($2 + n)) & 127 = hashtext($3) & 127 LIMIT 1",
        )
        .bind(x.to_string())
        .bind(y0)
        .bind(&a)
        .fetch_one(&pool)
        .await
        .unwrap();
        let rated = deploy(&[&a, &b], Some("A"), 0);
        let unrated = deploy(&[&c], None, 0);
        let other = deploy(&[&e], Some("T"), 0);
        upsert_page(&pool, vec![rated, unrated, other], None)
            .await
            .unwrap();
        let before: HashMap<String, (Option<String>, Option<NaiveDateTime>)> =
            sqlx::query_as::<_, (String, Option<String>, Option<NaiveDateTime>)>(
                "SELECT id, content_rating, deployed_at FROM explore.place WHERE id = ANY($1)",
            )
            .bind(vec![
                super::super::parcel_place_id(&a),
                super::super::parcel_place_id(&c),
            ])
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|(id, rating, at)| (id, (rating, at)))
            .collect();
        let (rated, unrated, new) = (
            deploy(&[&a, &b], Some("A"), 0),
            deploy(&[&c], None, 0),
            deploy(&[&d], Some("T"), 0),
        );
        async fn locked(
            pool: &PgPool,
            rows: &[&DerivedPlace],
            before: &HashMap<String, (Option<String>, Option<NaiveDateTime>)>,
        ) -> Vec<String> {
            let mut tx = pool.begin().await.unwrap();
            page_parcels(&mut tx, rows, before, None)
                .await
                .unwrap()
                .locked
        }

        // Unchanged rows whose parcels are recorded lock nothing, and an
        // operator's rating of another place does not wait for the page.
        assert!(locked(&pool, &[&rated, &unrated], &before).await.is_empty());
        let mut page = pool.begin().await.unwrap();
        let parcels = page_parcels(&mut page, &[&rated, &unrated], &before, None)
            .await
            .unwrap()
            .locked;
        lock_page_parcels(&mut page, &parcels).await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            moderation::apply(
                &pool,
                moderation::Target::Place(&super::super::parcel_place_id(&e)),
                &moderation::Decision::Rating("R".into()),
                "0xmoderator",
                None,
            ),
        )
        .await
        .expect("the operator does not wait for the page")
        .unwrap()
        .unwrap();
        page.rollback().await.unwrap();

        // A new row's parcels, and a rated row's unrecorded one, are locked.
        assert_eq!(
            locked(&pool, &[&rated, &unrated, &new], &before).await,
            vec![d.clone()]
        );
        sqlx::query("DELETE FROM explore.place_parcel_rating WHERE position = $1")
            .bind(&b)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            locked(&pool, &[&rated, &unrated], &before).await,
            vec![b.clone()]
        );
        let redeployed = deploy(&[&a, &b], Some("A"), 1);
        let mut expected = vec![a.clone(), b.clone()];
        expected.sort();
        assert_eq!(locked(&pool, &[&redeployed], &before).await, expected);
    }

    #[tokio::test]
    async fn a_rating_outlives_the_row_on_its_parcels() {
        let Some(pool) = crate::db::scratch::pool(2).await else {
            return;
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let x = u16::from_str_radix(&token[..4], 16).unwrap() as i32;
        let y0 = u16::from_str_radix(&token[4..8], 16).unwrap() as i32 % 30000;
        let parcel = |n: i32| format!("{x},{}", y0 + n);
        let deploy = |base: &str, parcels: &[&String], rating: &str, minute: u32| {
            let mut meta = wrapped();
            meta["v"]["scene"]["base"] = json!(base);
            meta["v"]["scene"]["parcels"] = json!(parcels);
            meta["v"]["policy"] = json!({ "contentRating": rating });
            let at = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, minute, 0);
            vec![derive("0xauthor", &[], at, &meta, None, "/content", true).unwrap()]
        };
        let id = |position: &String| super::super::parcel_place_id(position);
        let rating = |position: &String| {
            let (pool, id) = (pool.clone(), id(position));
            async move {
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT content_rating FROM explore.place WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let kinds = |position: &String| {
            let (pool, id) = (pool.clone(), id(position));
            async move {
                sqlx::query_scalar::<_, String>(
                    "SELECT kind FROM explore.moderation_hooks \
                     WHERE channel = 'places' AND subject = $1 ORDER BY created_at",
                )
                .bind(id)
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };
        // What the pass does once a scene is unpublished or replaced.
        let prune = |position: &String| {
            let (pool, id) = (pool.clone(), id(position));
            async move {
                sqlx::query("DELETE FROM explore.place WHERE id = $1")
                    .bind(id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };
        let operator = |position: &String, code: &'static str| {
            let (pool, id) = (pool.clone(), id(position));
            async move {
                moderation::apply(
                    &pool,
                    moderation::Target::Place(&id),
                    &moderation::Decision::Rating(code.into()),
                    "0xmoderator",
                    None,
                )
                .await
                .unwrap()
                .unwrap();
            }
        };
        let site = Some("https://decentraland.org/places");

        // An operator's rating holds after its row is pruned and the scene
        // returns on another base parcel.
        let (a, b) = (parcel(0), parcel(1));
        upsert_page(&pool, deploy(&a, &[&a, &b], "T", 0), site)
            .await
            .unwrap();
        operator(&a, "R").await;
        prune(&a).await;
        upsert_page(&pool, deploy(&b, &[&a, &b], "T", 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&b).await.as_deref(), Some("R"));
        assert_eq!(kinds(&b).await, ["rating_downgrade_refused"]);

        // An operator's lower rating of the current row wins over an older
        // one under a former base.
        operator(&b, "T").await;
        prune(&b).await;
        upsert_page(&pool, deploy(&b, &[&a, &b], "T", 2), site)
            .await
            .unwrap();
        assert_eq!(rating(&b).await.as_deref(), Some("T"));

        // A creator's raise holds with no operator rating, on the same base.
        let c = parcel(3);
        upsert_page(&pool, deploy(&c, &[&c], "T", 0), site)
            .await
            .unwrap();
        upsert_page(&pool, deploy(&c, &[&c], "A", 1), site)
            .await
            .unwrap();
        prune(&c).await;
        upsert_page(&pool, deploy(&c, &[&c], "T", 2), site)
            .await
            .unwrap();
        assert_eq!(rating(&c).await.as_deref(), Some("A"));

        // A republish on only some of the parcels, with another base, holds.
        let (d, e) = (parcel(5), parcel(6));
        upsert_page(&pool, deploy(&d, &[&d, &e], "A", 0), site)
            .await
            .unwrap();
        prune(&d).await;
        upsert_page(&pool, deploy(&e, &[&e], "T", 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&e).await.as_deref(), Some("A"));

        // Two moves: one parcel alone, then the other with a third as base.
        let (f, g, h) = (parcel(8), parcel(9), parcel(10));
        upsert_page(&pool, deploy(&f, &[&f, &g], "A", 0), site)
            .await
            .unwrap();
        upsert_page(&pool, deploy(&g, &[&g], "T", 1), site)
            .await
            .unwrap();
        prune(&f).await;
        upsert_page(&pool, deploy(&h, &[&f, &h], "T", 2), site)
            .await
            .unwrap();
        assert_eq!(rating(&g).await.as_deref(), Some("A"));
        assert_eq!(rating(&h).await.as_deref(), Some("A"));

        // A row redeployed over another rated place's parcels is held to it.
        let (i, j) = (parcel(12), parcel(13));
        upsert_page(&pool, deploy(&i, &[&i], "R", 0), site)
            .await
            .unwrap();
        upsert_page(&pool, deploy(&j, &[&j], "T", 0), site)
            .await
            .unwrap();
        upsert_page(&pool, deploy(&j, &[&i, &j], "T", 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&j).await.as_deref(), Some("R"));
        assert_eq!(kinds(&j).await, ["rating_downgrade_refused"]);

        // A pass over an unchanged deployment fills its parcels' missing ratings.
        let k = parcel(15);
        upsert_page(&pool, deploy(&k, &[&k], "A", 0), site)
            .await
            .unwrap();
        sqlx::query("DELETE FROM explore.place_parcel_rating WHERE position = $1")
            .bind(&k)
            .execute(&pool)
            .await
            .unwrap();
        upsert_page(&pool, deploy(&k, &[&k], "A", 0), site)
            .await
            .unwrap();
        prune(&k).await;
        upsert_page(&pool, deploy(&k, &[&k], "T", 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&k).await.as_deref(), Some("A"));

        // A row stored before the rule with `M`, or an off-scale code, is
        // recorded on the scale (or not at all), and the pass goes on.
        let recorded = |position: &String| {
            let (pool, position) = (pool.clone(), position.clone());
            async move {
                sqlx::query_scalar::<_, String>(
                    "SELECT content_rating FROM explore.place_parcel_rating WHERE position = $1",
                )
                .bind(position)
                .fetch_optional(&pool)
                .await
                .unwrap()
            }
        };
        let legacy = |position: &String, code: &'static str| {
            let (pool, position) = (pool.clone(), position.clone());
            async move {
                sqlx::query("UPDATE explore.place SET content_rating = $2 WHERE id = $1")
                    .bind(super::super::parcel_place_id(&position))
                    .bind(code)
                    .execute(&pool)
                    .await
                    .unwrap();
                sqlx::query("DELETE FROM explore.place_parcel_rating WHERE position = $1")
                    .bind(&position)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };
        let (m, x) = (parcel(17), parcel(19));
        for (position, code) in [(&m, "M"), (&x, "X")] {
            upsert_page(&pool, deploy(position, &[position], "A", 0), site)
                .await
                .unwrap();
            legacy(position, code).await;
            upsert_page(&pool, deploy(position, &[position], "A", 0), site)
                .await
                .unwrap();
        }
        assert_eq!(recorded(&m).await.as_deref(), Some("A"));
        assert_eq!(recorded(&x).await, None);
        legacy(&m, "M").await;
        upsert_page(&pool, deploy(&m, &[&m], "T", 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&m).await.as_deref(), Some("A"));
        assert_eq!(recorded(&m).await.as_deref(), Some("A"));
        upsert_page(&pool, deploy(&x, &[&x], "T", 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&x).await.as_deref(), Some("T"));
        assert_eq!(recorded(&x).await.as_deref(), Some("T"));

        // A legacy code is shown on the scale in the notices.
        let payloads = |position: &String| {
            let (pool, id) = (pool.clone(), id(position));
            async move {
                sqlx::query_scalar::<_, String>(
                    "SELECT payload::text FROM explore.moderation_hooks \
                     WHERE channel = 'places' AND subject = $1 ORDER BY created_at",
                )
                .bind(id)
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };
        let last = payloads(&m).await.pop().unwrap_or_default();
        assert!(
            last.contains("Actual rating: A - Proposed rating: T"),
            "{last}"
        );

        // A predecessor holding `M` holds a new row on its parcels as `A`.
        let (n, o) = (parcel(21), parcel(22));
        upsert_page(&pool, deploy(&n, &[&n], "A", 0), site)
            .await
            .unwrap();
        legacy(&n, "M").await;
        upsert_page(&pool, deploy(&o, &[&n, &o], "T", 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&o).await.as_deref(), Some("A"));
        assert_eq!(recorded(&o).await.as_deref(), Some("A"));
        assert_eq!(kinds(&o).await, ["rating_downgrade_refused"]);

        // An own `M` raised to `R` takes `R` and reports the raise from `A`.
        let q = parcel(24);
        upsert_page(&pool, deploy(&q, &[&q], "A", 0), site)
            .await
            .unwrap();
        legacy(&q, "M").await;
        upsert_page(&pool, deploy(&q, &[&q], "R", 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&q).await.as_deref(), Some("R"));
        assert_eq!(recorded(&q).await.as_deref(), Some("R"));
        let last = payloads(&q).await.pop().unwrap_or_default();
        assert!(last.contains("upgraded by Content Creator"), "{last}");
        assert!(
            last.contains("Previous rating: A - New rating: R"),
            "{last}"
        );

        // A lowercase or padded code counts as its rating.
        let (l, w) = (parcel(26), parcel(28));
        for (position, code) in [(&l, "r"), (&w, " A")] {
            upsert_page(&pool, deploy(position, &[position], "A", 0), site)
                .await
                .unwrap();
            legacy(position, code).await;
            upsert_page(&pool, deploy(position, &[position], "T", 1), site)
                .await
                .unwrap();
        }
        assert_eq!(rating(&l).await.as_deref(), Some("R"));
        assert_eq!(recorded(&l).await.as_deref(), Some("R"));
        let last = payloads(&l).await.pop().unwrap_or_default();
        assert!(
            last.contains("Actual rating: R - Proposed rating: T"),
            "{last}"
        );
        assert_eq!(rating(&w).await.as_deref(), Some("A"));
        assert_eq!(recorded(&w).await.as_deref(), Some("A"));
    }

    #[tokio::test]
    async fn a_rating_holds_only_for_the_same_place_or_its_creator() {
        let Some(pool) = crate::db::scratch::pool(2).await else {
            return;
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let x = u16::from_str_radix(&token[..4], 16).unwrap() as i32;
        let y0 = u16::from_str_radix(&token[4..8], 16).unwrap() as i32 % 30000;
        let parcel = |n: i32| format!("{x},{}", y0 + n);
        let deploy = |by: &str, base: &str, parcels: &[&String], rating: Option<&str>, minute| {
            let mut meta = wrapped();
            meta["v"]["scene"]["base"] = json!(base);
            meta["v"]["scene"]["parcels"] = json!(parcels);
            meta["v"]["policy"] = json!({ "contentRating": rating });
            let at = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, minute, 0);
            vec![derive(by, &[], at, &meta, None, "/content", true).unwrap()]
        };
        let id = |position: &String| super::super::parcel_place_id(position);
        let rating = |position: &String| {
            let (pool, id) = (pool.clone(), id(position));
            async move {
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT content_rating FROM explore.place WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let kinds = |position: &String| {
            let (pool, id) = (pool.clone(), id(position));
            async move {
                sqlx::query_scalar::<_, String>(
                    "SELECT kind FROM explore.moderation_hooks \
                     WHERE channel = 'places' AND subject = $1 ORDER BY created_at",
                )
                .bind(id)
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };
        let prune = |position: &String| {
            let (pool, id) = (pool.clone(), id(position));
            async move {
                sqlx::query("DELETE FROM explore.place WHERE id = $1")
                    .bind(id)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };
        let site = Some("https://decentraland.org/places");
        let (owner, other) = ("0xOwner", "0xother");

        // The same place keeps its rating, whoever deploys it.
        let a = parcel(0);
        upsert_page(&pool, deploy(owner, &a, &[&a], Some("A"), 0), site)
            .await
            .unwrap();
        prune(&a).await;
        upsert_page(&pool, deploy(other, &a, &[&a], Some("T"), 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&a).await.as_deref(), Some("A"));

        // A base moved by the same creator (address in any case) keeps it.
        let (b, c) = (parcel(2), parcel(3));
        upsert_page(&pool, deploy(owner, &b, &[&b, &c], Some("A"), 0), site)
            .await
            .unwrap();
        prune(&b).await;
        upsert_page(&pool, deploy("0xOWNER", &c, &[&b, &c], Some("T"), 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&c).await.as_deref(), Some("A"));
        assert_eq!(kinds(&c).await, ["rating_downgrade_refused"]);

        // Another creator on exactly the same parcels, under a new base, is
        // the same place and keeps its rating, with a notice: after the row
        // is pruned, while it is still listed, and over an operator's rating
        // of the former place, which stays an operator's on the parcels.
        let t = Ratings { pool: pool.clone() };
        let (d, e) = (parcel(5), parcel(6));
        upsert_page(&pool, deploy(owner, &d, &[&d, &e], Some("A"), 0), site)
            .await
            .unwrap();
        prune(&d).await;
        upsert_page(&pool, deploy(other, &e, &[&d, &e], Some("T"), 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&e).await.as_deref(), Some("A"));
        assert_eq!(kinds(&e).await, ["rating_downgrade_refused"]);
        let (f, g) = (parcel(8), parcel(9));
        upsert_page(&pool, deploy(owner, &f, &[&f, &g], Some("A"), 0), site)
            .await
            .unwrap();
        upsert_page(&pool, deploy(other, &g, &[&g, &f], Some("T"), 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&g).await.as_deref(), Some("A"));
        let (h, i) = (parcel(11), parcel(12));
        upsert_page(&pool, deploy(owner, &h, &[&h, &i], Some("T"), 0), site)
            .await
            .unwrap();
        moderation::apply(
            &pool,
            moderation::Target::Place(&id(&h)),
            &moderation::Decision::Rating("R".into()),
            "0xmoderator",
            None,
        )
        .await
        .unwrap()
        .unwrap();
        prune(&h).await;
        upsert_page(&pool, deploy(other, &i, &[&h, &i], Some("T"), 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&i).await.as_deref(), Some("R"));
        assert_eq!(kinds(&i).await, ["rating_downgrade_refused"]);
        assert_eq!(t.record(&h).await, Some(("R".into(), true)));
        assert_eq!(t.record(&i).await, Some(("R".into(), true)));

        // A deployment with no known creator matches no place on other parcels.
        let (j, k) = (parcel(14), parcel(15));
        upsert_page(&pool, deploy("", &j, &[&j, &k], Some("A"), 0), site)
            .await
            .unwrap();
        prune(&j).await;
        upsert_page(&pool, deploy(" ", &k, &[&k], Some("T"), 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&k).await.as_deref(), Some("T"));

        // Another creator's place with one parcel more, or one fewer, starts
        // fresh, with no notice.
        let (l, m, n) = (parcel(17), parcel(18), parcel(19));
        upsert_page(&pool, deploy(owner, &l, &[&l, &m], Some("A"), 0), site)
            .await
            .unwrap();
        prune(&l).await;
        upsert_page(&pool, deploy(other, &n, &[&l, &m, &n], Some("T"), 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&n).await.as_deref(), Some("T"));
        assert!(kinds(&n).await.is_empty());
        let (o, q, r) = (parcel(21), parcel(22), parcel(23));
        upsert_page(&pool, deploy(owner, &o, &[&o, &q, &r], Some("A"), 0), site)
            .await
            .unwrap();
        prune(&o).await;
        upsert_page(&pool, deploy(other, &r, &[&q, &r], Some("T"), 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&r).await.as_deref(), Some("T"));
        assert!(kinds(&r).await.is_empty());

        // A rated place replaced by another creator's unrated one on other
        // parcels leaves its creator nothing to be held to on those parcels.
        let (u, v, w, z) = (parcel(25), parcel(26), parcel(27), parcel(28));
        upsert_page(&pool, deploy(owner, &u, &[&u, &v], Some("A"), 0), site)
            .await
            .unwrap();
        prune(&u).await;
        upsert_page(&pool, deploy(other, &v, &[&u, &v, &w], None, 1), site)
            .await
            .unwrap();
        assert_eq!(rating(&v).await, None);
        prune(&v).await;
        upsert_page(&pool, deploy(owner, &z, &[&u, &v, &z], Some("T"), 2), site)
            .await
            .unwrap();
        assert_eq!(rating(&z).await.as_deref(), Some("T"));
    }

    /// The rows, notices and parcel records the operator-rating tests read.
    struct Ratings {
        pool: PgPool,
    }

    impl Ratings {
        fn deploy(
            by: &str,
            base: &str,
            parcels: &[&String],
            rating: Option<&str>,
            minute: u32,
        ) -> Vec<DerivedPlace> {
            let mut meta = wrapped();
            meta["v"]["scene"]["base"] = json!(base);
            meta["v"]["scene"]["parcels"] = json!(parcels);
            meta["v"]["policy"] = json!({ "contentRating": rating });
            let at = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, minute, 0);
            vec![derive(by, &[], at, &meta, None, "/content", true).unwrap()]
        }

        async fn rating(&self, position: &str) -> Option<String> {
            sqlx::query_scalar("SELECT content_rating FROM explore.place WHERE id = $1")
                .bind(super::super::parcel_place_id(position))
                .fetch_one(&self.pool)
                .await
                .unwrap()
        }

        async fn kinds(&self, position: &str) -> Vec<String> {
            sqlx::query_scalar(
                "SELECT kind FROM explore.moderation_hooks \
                 WHERE channel = 'places' AND subject = $1 ORDER BY created_at",
            )
            .bind(super::super::parcel_place_id(position))
            .fetch_all(&self.pool)
            .await
            .unwrap()
        }

        /// `(rating, operator)` recorded on a parcel.
        async fn record(&self, position: &str) -> Option<(String, bool)> {
            sqlx::query_as(
                "SELECT content_rating, operator FROM explore.place_parcel_rating WHERE position = $1",
            )
            .bind(position)
            .fetch_optional(&self.pool)
            .await
            .unwrap()
        }

        /// The LAND owner recorded on a parcel.
        async fn owner(&self, position: &str) -> Option<String> {
            sqlx::query_scalar(
                "SELECT owner_address FROM explore.place_parcel_rating WHERE position = $1",
            )
            .bind(position)
            .fetch_optional(&self.pool)
            .await
            .unwrap()
            .flatten()
        }

        async fn prune(&self, position: &str) {
            sqlx::query("DELETE FROM explore.place WHERE id = $1")
                .bind(super::super::parcel_place_id(position))
                .execute(&self.pool)
                .await
                .unwrap();
        }

        /// An operator's rating through [`moderation::apply`], as the ops
        /// console and the explore routes set it.
        async fn rate(&self, position: &str, rating: &str, land: Option<&LandAuthzState>) {
            moderation::apply(
                &self.pool,
                moderation::Target::Place(&super::super::parcel_place_id(position)),
                &moderation::Decision::Rating(rating.into()),
                "0xmoderator",
                land,
            )
            .await
            .unwrap()
            .unwrap();
        }
    }

    #[tokio::test]
    async fn another_creator_never_clears_or_replaces_an_operators_parcel_rating() {
        let Some(pool) = crate::db::scratch::pool(2).await else {
            return;
        };
        let t = Ratings { pool: pool.clone() };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let x = u16::from_str_radix(&token[..4], 16).unwrap() as i32;
        let y0 = u16::from_str_radix(&token[4..8], 16).unwrap() as i32 % 30000;
        let parcel = |n: i32| format!("{x},{}", y0 + n);
        let site = Some("https://decentraland.org/places");
        let (owner, other) = ("0xowner", "0xother");

        // An unrated place by another creator on other parcels leaves the
        // operator's records.
        let (l, m, n) = (parcel(0), parcel(1), parcel(2));
        upsert_page(
            &pool,
            Ratings::deploy(owner, &l, &[&l, &m], Some("A"), 0),
            site,
        )
        .await
        .unwrap();
        t.rate(&l, "R", None).await;
        assert_eq!(t.record(&m).await, Some(("R".into(), true)));
        t.prune(&l).await;
        upsert_page(&pool, Ratings::deploy(other, &m, &[&m], None, 1), site)
            .await
            .unwrap();
        assert_eq!(t.rating(&m).await, None);
        assert_eq!(t.record(&l).await, Some(("R".into(), true)));
        t.prune(&m).await;
        upsert_page(
            &pool,
            Ratings::deploy(owner, &n, &[&l, &m, &n], Some("T"), 2),
            site,
        )
        .await
        .unwrap();
        assert_eq!(t.rating(&n).await.as_deref(), Some("R"));
        assert_eq!(t.record(&l).await, Some(("R".into(), true)));

        // A rated one on other parcels starts fresh and leaves them too.
        let (p, q, r) = (parcel(4), parcel(5), parcel(6));
        upsert_page(
            &pool,
            Ratings::deploy(owner, &p, &[&p, &q], Some("T"), 0),
            site,
        )
        .await
        .unwrap();
        t.rate(&p, "R", None).await;
        t.prune(&p).await;
        upsert_page(&pool, Ratings::deploy(other, &q, &[&q], Some("E"), 1), site)
            .await
            .unwrap();
        assert_eq!(t.rating(&q).await.as_deref(), Some("E"));
        assert!(t.kinds(&q).await.is_empty());
        assert_eq!(t.record(&p).await, Some(("R".into(), true)));
        t.prune(&q).await;
        upsert_page(
            &pool,
            Ratings::deploy(owner, &r, &[&p, &q, &r], Some("T"), 2),
            site,
        )
        .await
        .unwrap();
        assert_eq!(t.rating(&r).await.as_deref(), Some("R"));
    }

    #[tokio::test]
    async fn a_filled_parcel_keeps_an_operators_rating_and_a_padded_creator_matches() {
        let Some(pool) = crate::db::scratch::pool(2).await else {
            return;
        };
        let t = Ratings { pool: pool.clone() };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let x = u16::from_str_radix(&token[..4], 16).unwrap() as i32;
        let y0 = u16::from_str_radix(&token[4..8], 16).unwrap() as i32 % 30000;
        let parcel = |n: i32| format!("{x},{}", y0 + n);
        let site = Some("https://decentraland.org/places");
        let forget = |position: String| {
            let pool = pool.clone();
            async move {
                sqlx::query("DELETE FROM explore.place_parcel_rating WHERE position = $1")
                    .bind(position)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };

        // A parcel recorded before the table fills as the operator's.
        let a = parcel(0);
        upsert_page(
            &pool,
            Ratings::deploy("0xowner", &a, &[&a], Some("T"), 0),
            site,
        )
        .await
        .unwrap();
        t.rate(&a, "R", None).await;
        forget(a.clone()).await;
        upsert_page(
            &pool,
            Ratings::deploy("0xowner", &a, &[&a], Some("T"), 0),
            site,
        )
        .await
        .unwrap();
        assert_eq!(t.record(&a).await, Some(("R".into(), true)));

        // A listed place whose stored creator is padded still holds a new
        // place by that creator on its parcels.
        let (b, c) = (parcel(2), parcel(3));
        upsert_page(
            &pool,
            Ratings::deploy("0xowner", &b, &[&b, &c], Some("A"), 0),
            site,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE explore.place SET creator_address = ' 0xOwner ' WHERE id = $1")
            .bind(super::super::parcel_place_id(&b))
            .execute(&pool)
            .await
            .unwrap();
        forget(b.clone()).await;
        forget(c.clone()).await;
        upsert_page(
            &pool,
            Ratings::deploy("0xowner", &c, &[&b, &c], Some("T"), 1),
            site,
        )
        .await
        .unwrap();
        assert_eq!(t.rating(&c).await.as_deref(), Some("A"));
    }

    /// Publishes a ready LAND index on an isolated database, with `owner` on
    /// each parcel.
    async fn land_index(pool: &PgPool, parcels: &[&String], owner: &str) -> LandAuthzState {
        let settings = crate::land_authz::Settings::for_tests();
        sqlx::query(
            "INSERT INTO land_authz.index_cursor \
             (network, land_registry, estate_registry, last_block, folded_block, folded_at, full_replay_started) \
             VALUES ($1, $2, $3, 100, 100, timezone('UTC', now()), true) \
             ON CONFLICT (network) DO UPDATE SET folded_at = EXCLUDED.folded_at, folded_block = 100, \
             last_block = 100, full_replay_started = true",
        )
        .bind(&settings.network)
        .bind(&settings.land_registry)
        .bind(&settings.estate_registry)
        .execute(pool)
        .await
        .unwrap();
        for position in parcels {
            let (x, y) = crate::auth::parse_pointer(position).unwrap();
            sqlx::query(
                "INSERT INTO land_authz.token_right (token_address, token_id, x, y, owner, updated_block, updated_log) \
                 VALUES ($1, $2::numeric * 340282366920938463463374607431768211456 + $3, $2, $3, $4, 100, 0) \
                 ON CONFLICT (token_address, token_id) DO UPDATE SET owner = EXCLUDED.owner",
            )
            .bind(&settings.land_registry)
            .bind(x)
            .bind(y)
            .bind(owner)
            .execute(pool)
            .await
            .unwrap();
        }
        LandAuthzState::new(
            pool.clone(),
            crate::http::Outbound::new(Default::default()).unwrap(),
            settings,
            None,
        )
    }

    #[tokio::test]
    async fn a_rating_holds_for_the_same_land_owner_from_another_wallet() {
        let Some(pool) = crate::db::scratch::isolated_pool(2).await else {
            return;
        };
        let t = Ratings { pool: pool.clone() };
        let parcel = |n: i32| format!("10,{n}");
        let site = Some("https://decentraland.org/places");
        let (owner, wallet, buyer) = (
            "0x00000000000000000000000000000000000000a1",
            "0x00000000000000000000000000000000000000b2",
            "0x00000000000000000000000000000000000000c3",
        );
        let (a, b, c, d, e, f) = (
            parcel(0),
            parcel(1),
            parcel(3),
            parcel(4),
            parcel(6),
            parcel(7),
        );
        let land = land_index(&pool, &[&a, &b, &c, &d, &e, &f], owner).await;
        let page = |rows| {
            let (pool, land) = (pool.clone(), &land);
            async move {
                super::upsert_page(&pool, rows, site, Some(land))
                    .await
                    .unwrap()
            }
        };

        // An operator's rating of an unrated place holds a moved base
        // deployed from a wallet the owner authorized.
        page(Ratings::deploy(owner, &a, &[&a, &b], None, 0)).await;
        t.rate(&a, "R", Some(&land)).await;
        t.prune(&a).await;
        page(Ratings::deploy(wallet, &b, &[&a, &b], Some("T"), 1)).await;
        assert_eq!(t.rating(&b).await.as_deref(), Some("R"));
        assert_eq!(t.kinds(&b).await, ["rating_downgrade_refused"]);

        // So does a creator's raise.
        page(Ratings::deploy(owner, &c, &[&c, &d], Some("A"), 0)).await;
        t.prune(&c).await;
        page(Ratings::deploy(wallet, &d, &[&c, &d], Some("T"), 1)).await;
        assert_eq!(t.rating(&d).await.as_deref(), Some("A"));

        // A shifted parcel set from that wallet matches only by owner: an
        // operator's rating (one parcel dropped, one added) and a creator's
        // raise (one parcel more, one fewer) hold.
        let q: Vec<String> = (10..20).map(parcel).collect();
        let all: Vec<&String> = q.iter().collect();
        let land = land_index(&pool, &all, owner).await;
        let page = |rows| {
            let (pool, land) = (pool.clone(), &land);
            async move {
                super::upsert_page(&pool, rows, site, Some(land))
                    .await
                    .unwrap()
            }
        };
        page(Ratings::deploy(owner, &q[0], &[&q[0], &q[1]], None, 0)).await;
        t.rate(&q[0], "R", Some(&land)).await;
        t.prune(&q[0]).await;
        page(Ratings::deploy(
            wallet,
            &q[2],
            &[&q[1], &q[2]],
            Some("T"),
            1,
        ))
        .await;
        assert_eq!(t.rating(&q[2]).await.as_deref(), Some("R"));
        assert_eq!(t.kinds(&q[2]).await, ["rating_downgrade_refused"]);
        page(Ratings::deploy(owner, &q[3], &[&q[3], &q[4]], Some("A"), 0)).await;
        t.prune(&q[3]).await;
        page(Ratings::deploy(
            wallet,
            &q[5],
            &[&q[3], &q[4], &q[5]],
            Some("T"),
            1,
        ))
        .await;
        assert_eq!(t.rating(&q[5]).await.as_deref(), Some("A"));
        page(Ratings::deploy(
            owner,
            &q[6],
            &[&q[6], &q[7], &q[8]],
            Some("A"),
            0,
        ))
        .await;
        t.prune(&q[6]).await;
        page(Ratings::deploy(
            wallet,
            &q[8],
            &[&q[7], &q[8]],
            Some("T"),
            1,
        ))
        .await;
        assert_eq!(t.rating(&q[8]).await.as_deref(), Some("A"));

        // Once the LAND changes owner, the new owner's place on other parcels
        // starts fresh and replaces the operator's records.
        let g = parcel(8);
        page(Ratings::deploy(owner, &e, &[&e, &f], Some("T"), 0)).await;
        t.rate(&e, "R", Some(&land)).await;
        t.prune(&e).await;
        land_index(&pool, &[&e, &f, &g], buyer).await;
        page(Ratings::deploy(buyer, &f, &[&e, &f, &g], None, 1)).await;
        assert_eq!(t.rating(&f).await, None);
        assert_eq!(t.record(&e).await, None);
    }

    /// A record written while the owner is unknown takes it on a later pass
    /// over the same place, and then holds a shifted place from another wallet
    /// of that owner; a record of a pruned place keeps it unknown.
    #[tokio::test]
    async fn a_later_pass_fills_an_unknown_land_owner() {
        let Some(pool) = crate::db::scratch::isolated_pool(2).await else {
            return;
        };
        let t = Ratings { pool: pool.clone() };
        let parcel = |n: i32| format!("30,{n}");
        let site = Some("https://decentraland.org/places");
        let (owner, wallet) = (
            "0x00000000000000000000000000000000000000a1",
            "0x00000000000000000000000000000000000000b2",
        );
        let p: Vec<String> = (0..8).map(parcel).collect();
        let creator = || Ratings::deploy(owner, &p[0], &[&p[0], &p[1]], Some("A"), 0);
        let operator = || Ratings::deploy(owner, &p[3], &[&p[3], &p[4]], None, 0);
        let pruned = Ratings::deploy(owner, &p[6], &[&p[6], &p[7]], Some("A"), 0);
        for rows in [creator(), operator(), pruned] {
            upsert_page(&pool, rows, site).await.unwrap();
        }
        t.rate(&p[3], "R", None).await;
        t.prune(&p[6]).await;
        assert_eq!(t.owner(&p[0]).await, None);
        assert_eq!(t.owner(&p[3]).await, None);

        let all: Vec<&String> = p.iter().collect();
        let land = land_index(&pool, &all, owner).await;
        super::upsert_page(
            &pool,
            creator().into_iter().chain(operator()).collect(),
            site,
            Some(&land),
        )
        .await
        .unwrap();
        for position in [&p[0], &p[1], &p[3], &p[4]] {
            assert_eq!(
                t.owner(position).await.as_deref(),
                Some(owner),
                "{position}"
            );
        }
        assert_eq!(t.owner(&p[6]).await, None, "a pruned place's record");
        assert_eq!(t.record(&p[3]).await, Some(("R".into(), true)));

        t.prune(&p[0]).await;
        t.prune(&p[3]).await;
        for (base, parcels) in [(&p[2], [&p[1], &p[2]]), (&p[5], [&p[4], &p[5]])] {
            super::upsert_page(
                &pool,
                Ratings::deploy(wallet, base, &parcels, Some("T"), 1),
                site,
                Some(&land),
            )
            .await
            .unwrap();
        }
        assert_eq!(t.rating(&p[2]).await.as_deref(), Some("A"));
        assert_eq!(t.rating(&p[5]).await.as_deref(), Some("R"));
    }

    /// A redeploy of the same place while the owner is unknown keeps the owner
    /// its records know, so a later shifted place from another wallet of that
    /// owner is still held.
    #[tokio::test]
    async fn a_redeploy_with_an_unknown_owner_keeps_the_known_one() {
        let Some(pool) = crate::db::scratch::isolated_pool(2).await else {
            return;
        };
        let t = Ratings { pool: pool.clone() };
        let parcel = |n: i32| format!("50,{n}");
        let site = Some("https://decentraland.org/places");
        let (owner, wallet) = (
            "0x00000000000000000000000000000000000000a1",
            "0x00000000000000000000000000000000000000b2",
        );
        let (a, b, c) = (parcel(0), parcel(1), parcel(2));
        let land = land_index(&pool, &[&a, &b, &c], owner).await;
        super::upsert_page(
            &pool,
            Ratings::deploy(owner, &a, &[&a, &b], None, 0),
            site,
            Some(&land),
        )
        .await
        .unwrap();
        t.rate(&a, "R", Some(&land)).await;
        assert_eq!(t.owner(&b).await.as_deref(), Some(owner));

        upsert_page(&pool, Ratings::deploy(owner, &a, &[&a, &b], None, 1), site)
            .await
            .unwrap();
        assert_eq!(t.owner(&b).await.as_deref(), Some(owner));

        t.prune(&a).await;
        super::upsert_page(
            &pool,
            Ratings::deploy(wallet, &c, &[&b, &c], Some("T"), 2),
            site,
            Some(&land),
        )
        .await
        .unwrap();
        assert_eq!(t.rating(&c).await.as_deref(), Some("R"));
    }

    /// A place settled while its parcels' owner was unknown is settled again
    /// once the index knows it: one by the same owner is raised to the rating
    /// it is held to (one notice), whether it recorded parcels of its own, sits
    /// within the earlier place's parcels or declares no rating; one by
    /// another owner keeps its rating and replaces the records. A record with
    /// a known owner outlives the first settle, and a later pass changes
    /// nothing more.
    #[tokio::test]
    async fn a_place_settled_with_an_unknown_owner_is_settled_again() {
        let Some(pool) = crate::db::scratch::isolated_pool(2).await else {
            return;
        };
        let t = Ratings { pool: pool.clone() };
        let parcel = |n: i32| format!("70,{n}");
        let site = Some("https://decentraland.org/places");
        let (owner, wallet, buyer) = (
            "0x00000000000000000000000000000000000000a1",
            "0x00000000000000000000000000000000000000b2",
            "0x00000000000000000000000000000000000000c3",
        );
        let p: Vec<String> = (0..16).map(parcel).collect();
        let all: Vec<&String> = p.iter().collect();
        let land = land_index(&pool, &all, owner).await;
        let known = |rows| {
            let (pool, land) = (pool.clone(), &land);
            async move {
                super::upsert_page(&pool, rows, site, Some(land))
                    .await
                    .unwrap()
            }
        };
        let unknown = |rows| {
            let pool = pool.clone();
            async move { upsert_page(&pool, rows, site).await.unwrap() }
        };
        // The earlier places, all under the owner: three operator's ratings,
        // a creator's raise and one more operator's rating.
        for (base, parcels) in [
            (&p[0], vec![&p[0], &p[1]]),
            (&p[3], vec![&p[3], &p[4]]),
            (&p[6], vec![&p[6], &p[7]]),
            (&p[12], vec![&p[12], &p[13]]),
        ] {
            known(Ratings::deploy(owner, base, &parcels, None, 0)).await;
            t.rate(base, "R", Some(&land)).await;
            t.prune(base).await;
        }
        known(Ratings::deploy(
            owner,
            &p[9],
            &[&p[9], &p[10]],
            Some("A"),
            0,
        ))
        .await;
        t.prune(&p[9]).await;

        // Another wallet's places while the owner is unknown: none is held.
        let later = || {
            [
                Ratings::deploy(wallet, &p[2], &[&p[1], &p[2]], Some("T"), 1),
                Ratings::deploy(wallet, &p[4], &[&p[4]], Some("T"), 1),
                Ratings::deploy(wallet, &p[7], &[&p[7]], None, 1),
                Ratings::deploy(wallet, &p[11], &[&p[10], &p[11]], Some("T"), 1),
                Ratings::deploy(buyer, &p[14], &[&p[13], &p[14]], Some("T"), 1),
            ]
        };
        for rows in later() {
            unknown(rows).await;
        }
        assert_eq!(t.rating(&p[2]).await.as_deref(), Some("T"));
        assert_eq!(t.rating(&p[4]).await.as_deref(), Some("T"));
        assert_eq!(t.rating(&p[7]).await, None);
        assert_eq!(t.rating(&p[11]).await.as_deref(), Some("T"));
        assert_eq!(t.record(&p[10]).await, Some(("A".into(), false)));
        assert_eq!(t.owner(&p[10]).await.as_deref(), Some(owner));

        // A later pass with the owner known.
        land_index(&pool, &[&p[13], &p[14]], buyer).await;
        known(later().into_iter().flatten().collect()).await;
        assert_eq!(t.rating(&p[2]).await.as_deref(), Some("R"));
        assert_eq!(t.kinds(&p[2]).await, ["rating_downgrade_refused"]);
        assert_eq!(t.owner(&p[2]).await.as_deref(), Some(owner));
        assert_eq!(t.rating(&p[4]).await.as_deref(), Some("R"));
        assert_eq!(t.rating(&p[7]).await.as_deref(), Some("R"));
        assert!(t.kinds(&p[7]).await.is_empty());
        assert_eq!(t.rating(&p[11]).await.as_deref(), Some("A"));
        assert_eq!(t.rating(&p[14]).await.as_deref(), Some("T"));
        assert!(t.kinds(&p[14]).await.is_empty());
        assert_eq!(t.record(&p[13]).await, Some(("T".into(), false)));

        // Settled: another pass reports nothing more.
        known(later().into_iter().flatten().collect()).await;
        assert_eq!(t.kinds(&p[2]).await, ["rating_downgrade_refused"]);
        assert_eq!(t.kinds(&p[4]).await, ["rating_downgrade_refused"]);
        assert_eq!(t.kinds(&p[11]).await, ["rating_downgrade_refused"]);
    }

    /// A new row held only to its own operator rating (no parcel record, no
    /// row it replaces) reports a laxer scene rating, as any held row does.
    #[tokio::test]
    async fn a_new_row_held_only_by_its_own_operator_rating_reports_a_laxer_one() {
        let Some(pool) = crate::db::scratch::pool(2).await else {
            return;
        };
        let t = Ratings { pool: pool.clone() };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let x = u16::from_str_radix(&token[..4], 16).unwrap() as i32;
        let y = u16::from_str_radix(&token[4..8], 16).unwrap() as i32 % 30000;
        let a = format!("{x},{y}");
        let site = Some("https://decentraland.org/places");
        upsert_page(&pool, Ratings::deploy("0xowner", &a, &[&a], None, 0), site)
            .await
            .unwrap();
        t.rate(&a, "R", None).await;
        t.prune(&a).await;
        sqlx::query("DELETE FROM explore.place_parcel_rating WHERE position = $1")
            .bind(&a)
            .execute(&pool)
            .await
            .unwrap();
        upsert_page(
            &pool,
            Ratings::deploy("0xother", &a, &[&a], Some("T"), 1),
            site,
        )
        .await
        .unwrap();
        assert_eq!(t.rating(&a).await.as_deref(), Some("R"));
        assert_eq!(t.kinds(&a).await, ["rating_downgrade_refused"]);
    }

    #[test]
    fn a_parcel_set_hash_ignores_order_and_repeats() {
        let set = |p: &[&str]| {
            super::super::parcel_set_hash(&p.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(set(&["1,2", "-1,0", "1,2"]), set(&["-1,0", "1,2"]));
        assert_ne!(set(&["-1,0", "1,2"]), set(&["-1,0"]));
        // The digest of "-1,0;1,2".
        assert_eq!(
            set(&["1,2", "-1,0"]),
            "97be2b158fe28859d4a4efdec7884cad93a036e2ec76eb2f34de8fac2141066d"
        );
    }

    /// The overlap lookup of [`HELD_RATINGS`] can probe the positions index.
    #[tokio::test]
    async fn held_ratings_can_use_the_positions_index() {
        let Some(pool) = crate::db::scratch::pool(2).await else {
            return;
        };
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL enable_seqscan = off")
            .execute(&mut *tx)
            .await
            .unwrap();
        let plan: Vec<String> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("EXPLAIN {HELD_RATINGS}")))
                .bind(vec!["p".to_string()])
                .bind(vec![json!(["1,1", "1,2"])])
                .bind(vec!["p".to_string(), "p".to_string()])
                .bind(vec!["1,1".to_string(), "1,2".to_string()])
                .bind(vec!["0xa".to_string(), "0xa".to_string()])
                .bind(vec![String::new(), String::new()])
                .bind(vec!["s".to_string(), "s".to_string()])
                .fetch_all(&mut *tx)
                .await
                .unwrap();
        assert!(
            plan.iter().any(|l| l.contains("place_positions_path_gin")),
            "{}",
            plan.join("\n")
        );
    }

    /// Renting the LAND out does not change its owner: a tenant's place on a
    /// shifted parcel set is held to the operator's rating of the lessor's.
    #[tokio::test]
    async fn a_rented_parcel_keeps_its_lessor_as_owner() {
        let Some(pool) = crate::db::scratch::isolated_pool(2).await else {
            return;
        };
        let t = Ratings { pool: pool.clone() };
        let parcel = |n: i32| format!("60,{n}");
        let site = Some("https://decentraland.org/places");
        let (owner, tenant) = (
            "0x00000000000000000000000000000000000000a1",
            "0x00000000000000000000000000000000000000e5",
        );
        let (a, b, c) = (parcel(0), parcel(1), parcel(2));
        let land = land_index(&pool, &[&a, &b, &c], owner).await;
        super::upsert_page(
            &pool,
            Ratings::deploy(owner, &a, &[&a, &b], None, 0),
            site,
            Some(&land),
        )
        .await
        .unwrap();
        t.rate(&a, "R", Some(&land)).await;
        t.prune(&a).await;

        let rentals = crate::land_authz::RENTALS_MAINNET;
        sqlx::query(
            "INSERT INTO land_authz.authz_event (block_number, log_index, block_time, token_address, kind, token_id, account) \
             SELECT 90 + n, y, 1, token_address, 'transfer', token_id, CASE n WHEN 0 THEN $1 ELSE $2 END \
             FROM land_authz.token_right, generate_series(0, 1) AS n WHERE x = 60",
        )
        .bind(owner)
        .bind(rentals)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE land_authz.token_right SET owner = $1 WHERE x = 60")
            .bind(rentals)
            .execute(&pool)
            .await
            .unwrap();
        super::upsert_page(
            &pool,
            Ratings::deploy(tenant, &c, &[&b, &c], Some("T"), 1),
            site,
            Some(&land),
        )
        .await
        .unwrap();
        assert_eq!(t.rating(&c).await.as_deref(), Some("R"));
        assert_eq!(t.record(&b).await, Some(("R".into(), true)));
        assert_eq!(t.owner(&c).await.as_deref(), Some(owner));
    }

    /// A LAND index that cannot be read leaves owners unknown: the derive page
    /// and an operator's rating still commit, as with an unpublished index.
    #[tokio::test]
    async fn a_failing_land_index_leaves_owners_unknown() {
        let Some(pool) = crate::db::scratch::isolated_pool(2).await else {
            return;
        };
        let t = Ratings { pool: pool.clone() };
        let parcel = |n: i32| format!("40,{n}");
        let site = Some("https://decentraland.org/places");
        let owner = "0x00000000000000000000000000000000000000a1";
        let (a, b, c, d) = (parcel(0), parcel(1), parcel(2), parcel(3));
        let land = land_index(&pool, &[&a, &b, &c, &d], owner).await;
        sqlx::query("ALTER TABLE land_authz.token_right RENAME TO token_right_unreadable")
            .execute(&pool)
            .await
            .unwrap();

        super::upsert_page(
            &pool,
            Ratings::deploy(owner, &a, &[&a, &b], Some("A"), 0),
            site,
            Some(&land),
        )
        .await
        .unwrap();
        assert_eq!(t.rating(&a).await.as_deref(), Some("A"));
        assert_eq!(t.record(&b).await, Some(("A".into(), false)));
        assert_eq!(t.owner(&b).await, None);

        upsert_page(&pool, Ratings::deploy(owner, &c, &[&c, &d], None, 0), site)
            .await
            .unwrap();
        t.rate(&c, "R", Some(&land)).await;
        assert_eq!(t.record(&d).await, Some(("R".into(), true)));
        assert_eq!(t.owner(&d).await, None);
    }

    #[tokio::test]
    async fn a_rating_holds_for_exactly_the_same_parcels_on_another_owners_land() {
        let Some(pool) = crate::db::scratch::isolated_pool(2).await else {
            return;
        };
        let t = Ratings { pool: pool.clone() };
        let parcel = |n: i32| format!("20,{n}");
        let site = Some("https://decentraland.org/places");
        let (owner, buyer, wallet) = (
            "0x00000000000000000000000000000000000000a1",
            "0x00000000000000000000000000000000000000c3",
            "0x00000000000000000000000000000000000000d4",
        );
        let p: Vec<String> = (0..12).map(parcel).collect();
        let all: Vec<&String> = p.iter().collect();
        let land = land_index(&pool, &all, owner).await;
        let page = |rows| {
            let (pool, land) = (pool.clone(), &land);
            async move {
                super::upsert_page(&pool, rows, site, Some(land))
                    .await
                    .unwrap()
            }
        };

        // The owner's places: a creator's raise, two operator's ratings and
        // two more raises; then the LAND changes owner and the rows go.
        page(Ratings::deploy(owner, &p[0], &[&p[0], &p[1]], Some("A"), 0)).await;
        page(Ratings::deploy(owner, &p[2], &[&p[2], &p[3]], Some("T"), 0)).await;
        t.rate(&p[2], "R", Some(&land)).await;
        page(Ratings::deploy(owner, &p[4], &[&p[4], &p[5]], Some("A"), 0)).await;
        page(Ratings::deploy(
            owner,
            &p[7],
            &[&p[7], &p[8], &p[9]],
            Some("A"),
            0,
        ))
        .await;
        page(Ratings::deploy(
            owner,
            &p[10],
            &[&p[10], &p[11]],
            Some("T"),
            0,
        ))
        .await;
        t.rate(&p[10], "R", Some(&land)).await;
        for base in [&p[0], &p[2], &p[4], &p[7], &p[10]] {
            t.prune(base).await;
        }
        land_index(&pool, &all, buyer).await;

        // Another wallet redeploying exactly those parcels under a new base is
        // held to the creator's and the operator's ratings, and the operator's
        // record moves to the new place.
        page(Ratings::deploy(
            wallet,
            &p[1],
            &[&p[1], &p[0]],
            Some("T"),
            1,
        ))
        .await;
        assert_eq!(t.rating(&p[1]).await.as_deref(), Some("A"));
        assert_eq!(t.kinds(&p[1]).await, ["rating_downgrade_refused"]);
        page(Ratings::deploy(
            wallet,
            &p[3],
            &[&p[2], &p[3]],
            Some("E"),
            1,
        ))
        .await;
        assert_eq!(t.rating(&p[3]).await.as_deref(), Some("R"));
        assert_eq!(t.kinds(&p[3]).await, ["rating_downgrade_refused"]);
        assert_eq!(t.record(&p[2]).await, Some(("R".into(), true)));
        let place: String = sqlx::query_scalar(
            "SELECT place_id FROM explore.place_parcel_rating WHERE position = $1",
        )
        .bind(&p[2])
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(place, super::super::parcel_place_id(&p[3]));

        // So is an unrated one, which keeps the operator's records.
        page(Ratings::deploy(wallet, &p[11], &[&p[11], &p[10]], None, 1)).await;
        assert_eq!(t.rating(&p[11]).await.as_deref(), Some("R"));
        assert!(t.kinds(&p[11]).await.is_empty());
        assert_eq!(t.record(&p[10]).await, Some(("R".into(), true)));
        assert_eq!(t.record(&p[11]).await, Some(("R".into(), true)));

        // One parcel more, or one fewer, starts fresh with no notice.
        page(Ratings::deploy(
            wallet,
            &p[6],
            &[&p[4], &p[5], &p[6]],
            Some("T"),
            1,
        ))
        .await;
        assert_eq!(t.rating(&p[6]).await.as_deref(), Some("T"));
        assert!(t.kinds(&p[6]).await.is_empty());
        page(Ratings::deploy(
            wallet,
            &p[9],
            &[&p[8], &p[9]],
            Some("T"),
            1,
        ))
        .await;
        assert_eq!(t.rating(&p[9]).await.as_deref(), Some("T"));
        assert!(t.kinds(&p[9]).await.is_empty());
    }

    #[test]
    fn the_deployment_time_fills_both_timestamps_upstream_keeps_not_null() {
        let at = DateTime::parse_from_rfc3339("2026-03-01T10:00:00Z")
            .expect("parse")
            .naive_utc();
        let p = derive("0x1", &[], Some(at), &wrapped(), None, "/content", false).expect("derives");
        let wire = json!(DateTime::<Utc>::from_naive_utc_and_offset(at, Utc));
        assert_eq!(p.raw["deployed_at"], wire);
        assert_eq!(p.raw["created_at"], wire);
        assert_eq!(p.raw["updated_at"], wire);
    }

    #[test]
    fn id_is_stable_for_base_position() {
        let a = derive("0x1", &[], None, &wrapped(), None, "/content", false).unwrap();
        let b = derive("0x2", &[], None, &wrapped(), None, "/content", false).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.id, super::super::parcel_place_id("135,2"));
    }

    #[test]
    fn no_scene_yields_none() {
        assert!(derive("0x1", &[], None, &json!({"v": {}}), None, "/content", false).is_none());
    }

    #[test]
    fn falls_back_to_pointer_when_base_missing() {
        let meta = json!({ "v": { "scene": { "parcels": [] } } });
        let p = derive(
            "0x1",
            &["10,10".to_string()],
            None,
            &meta,
            None,
            "/content",
            false,
        )
        .unwrap();
        assert_eq!(p.base_position, "10,10");
        assert_eq!(p.raw["positions"], json!(["10,10"]));
    }

    #[test]
    fn the_scope_follows_the_old_mode_and_the_mirror() {
        assert_eq!(DeriveScope::resolve(true, false), DeriveScope::All);
        assert_eq!(DeriveScope::resolve(true, true), DeriveScope::Local);
        assert_eq!(DeriveScope::resolve(false, false), DeriveScope::Local);
        assert_eq!(DeriveScope::resolve(false, true), DeriveScope::Local);
    }

    const CREATOR: &str = "0x4f7fe261619141ffa63fefee35bba886581292f4";

    fn banner() -> Curation {
        Curation {
            highlighted: true,
            ranking: Some(1900.0),
            exclude_from_ranking: false,
            highlighted_image: Some("/images/places/banner.jpg".to_string()),
        }
    }

    fn candidate(creator: &str, curation: Curation) -> CurationCandidate {
        CurationCandidate {
            creator_address: Some(creator.to_string()),
            curation,
        }
    }

    #[test]
    fn carries_curation_from_a_predecessor_by_the_same_creator() {
        assert_eq!(
            inherited_curation(&[candidate(CREATOR, banner())], Some(CREATOR)),
            Some(banner())
        );
    }

    #[test]
    fn a_ranked_but_unhighlighted_predecessor_counts_as_curated() {
        let ranked = Curation {
            highlighted: false,
            ranking: Some(12.0),
            exclude_from_ranking: false,
            highlighted_image: None,
        };
        assert_eq!(
            inherited_curation(&[candidate(CREATOR, ranked.clone())], Some(CREATOR)),
            Some(ranked)
        );
    }

    #[test]
    fn an_excluded_but_unranked_predecessor_counts_as_curated() {
        let excluded = Curation {
            highlighted: false,
            ranking: Some(0.0),
            exclude_from_ranking: true,
            highlighted_image: None,
        };
        assert_eq!(
            inherited_curation(&[candidate(CREATOR, excluded.clone())], Some(CREATOR)),
            Some(excluded)
        );
    }

    #[test]
    fn a_predecessor_by_another_creator_inherits_nothing() {
        assert_eq!(
            inherited_curation(
                &[candidate(CREATOR, banner())],
                Some("0x0000000000000000000000000000000000000001")
            ),
            None
        );
    }

    #[test]
    fn more_than_one_curated_predecessor_bails_out() {
        assert_eq!(
            inherited_curation(
                &[candidate(CREATOR, banner()), candidate(CREATOR, banner())],
                Some(CREATOR)
            ),
            None
        );
    }

    #[test]
    fn uncurated_neighbours_do_not_count_towards_the_bail_out() {
        let plain = Curation {
            highlighted: false,
            ranking: Some(0.0),
            exclude_from_ranking: false,
            highlighted_image: None,
        };
        assert_eq!(
            inherited_curation(
                &[candidate(CREATOR, banner()), candidate(CREATOR, plain)],
                Some(CREATOR)
            ),
            Some(banner())
        );
    }

    #[test]
    fn a_deployment_without_a_known_creator_inherits_nothing() {
        assert_eq!(
            inherited_curation(&[candidate(CREATOR, banner())], None),
            None
        );
        assert_eq!(
            inherited_curation(&[candidate(CREATOR, banner())], Some("")),
            None
        );
    }

    async fn scene(
        pool: &PgPool,
        entity_id: &str,
        base: &str,
        parcels: &[&str],
        deleter: Option<i32>,
    ) -> i32 {
        let meta = json!({ "v": { "scene": { "base": base, "parcels": parcels }, "display": { "title": format!("scene {entity_id}") } } });
        sqlx::query_scalar(
            "INSERT INTO content.deployments (deployer_address, version, entity_type, entity_id, entity_metadata,
                 entity_timestamp, entity_pointers, local_timestamp, auth_chain, deleter_deployment)
             VALUES ('0xAbC', 'v3', 'scene', $1, $2::json, timezone('UTC', now()), $3, timezone('UTC', now()), '[]', $4)
             RETURNING id",
        )
        .bind(entity_id)
        .bind(meta.to_string())
        .bind(parcels.iter().map(|p| p.to_string()).collect::<Vec<_>>())
        .bind(deleter)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn scene_metadata_cache_avoids_reads_and_isolates_storage_faults() {
        let Ok(base) = std::env::var("CATALYST_TEST_DATABASE_URL") else {
            return;
        };
        let opts: sqlx::postgres::PgConnectOptions = base.parse().unwrap();
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone().database("postgres"))
            .await
            .unwrap();
        let token = uuid::Uuid::new_v4().simple().to_string();
        let name = format!("catalyst_z_scene_cache_{token}");
        crate::db::scratch::create_database_with_schema(&admin, &name).await;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(opts.database(&name))
            .await
            .unwrap();
        let root = std::env::temp_dir().join(format!("scene-cache-{token}"));
        let storage = crate::storage::Storage::from_config(&root).await.unwrap();
        let first = scene(&pool, "first", "1,1", &["1,1"], None).await;
        let second = scene(&pool, "second", "2,2", &["2,2"], None).await;
        let bytes = bytes::Bytes::from_static(br#"{"runtimeVersion":"7","creator":"0xforged"}"#);
        let hash = crate::auth::hash_bytes(&bytes);
        storage.store(&hash, bytes).await.unwrap();
        for deployment in [first, second] {
            sqlx::query("INSERT INTO content.content_files (deployment, content_hash, key) VALUES ($1, $2, 'scene.json')")
                .bind(deployment).bind(&hash).execute(&pool).await.unwrap();
        }
        run_once_with_storage(&pool, DeriveScope::All, "/content", Some(&storage), None)
            .await
            .unwrap();
        let path = storage.file_path(&hash).await.unwrap().unwrap();
        tokio::fs::remove_file(&path).await.unwrap();
        tokio::fs::create_dir(&path).await.unwrap();
        assert!(scene_file(&storage, "first", &hash).await.is_err());
        // Both unchanged deployments use the cached immutable blob despite an I/O fault.
        assert_eq!(
            run_once_with_storage(&pool, DeriveScope::All, "/content", Some(&storage), None)
                .await
                .unwrap()
                .derived,
            2
        );
        sqlx::query("UPDATE content.deployments SET entity_id = 'first-redeploy' WHERE id = $1")
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        let fresh = bytes::Bytes::from_static(br#"{"runtimeVersion":"8"}"#);
        let fresh_hash = crate::auth::hash_bytes(&fresh);
        storage.store(&fresh_hash, fresh).await.unwrap();
        sqlx::query("UPDATE content.content_files SET content_hash = $1 WHERE deployment = $2")
            .bind(&fresh_hash)
            .bind(second)
            .execute(&pool)
            .await
            .unwrap();
        // One changed deployment faults; the other still refreshes and pruning retains the old usable row.
        assert_eq!(
            run_once_with_storage(&pool, DeriveScope::All, "/content", Some(&storage), None)
                .await
                .unwrap()
                .derived,
            1
        );
        let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT base_position, raw->>'sdk', creator_address FROM explore.place ORDER BY base_position")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(
            rows,
            vec![
                ("1,1".into(), "7".into(), "0xabc".into()),
                ("2,2".into(), "8".into(), "0xabc".into())
            ]
        );
        tokio::fs::remove_dir(&path).await.unwrap();
        storage
            .store(
                &hash,
                bytes::Bytes::from_static(br#"{"runtimeVersion":"7"}"#),
            )
            .await
            .unwrap();
        assert_eq!(
            run_once_with_storage(&pool, DeriveScope::All, "/content", Some(&storage), None)
                .await
                .unwrap()
                .derived,
            2
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
        pool.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE \"{name}\" WITH (FORCE)"
        )))
        .execute(&admin)
        .await
        .unwrap();
        admin.close().await;
    }

    /// The local scope derives this node's own scenes (not a synced one, not
    /// a tombstoned one), a mirrored row at the same id is left alone, the
    /// places mirror's retire keeps the local rows, and an unpublish prunes
    /// the place on the next pass.
    #[tokio::test]
    async fn local_scenes_become_places_beside_the_mirror_and_leave_when_unpublished() {
        let Ok(base) = std::env::var("CATALYST_TEST_DATABASE_URL") else {
            return;
        };
        let opts: sqlx::postgres::PgConnectOptions = base.parse().expect("a postgres DSN");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone().database("postgres"))
            .await
            .unwrap();
        let name = format!(
            "catalyst_z_derive_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        crate::db::scratch::create_database_with_schema(&admin, &name).await;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(opts.database(&name))
            .await
            .unwrap();
        let t = uuid::Uuid::new_v4().simple().to_string();
        let (x, y) = (
            i64::from_str_radix(&t[..4], 16).unwrap() % 150,
            i64::from_str_radix(&t[4..8], 16).unwrap() % 150,
        );
        let (local_base, synced_base, gone_base) = (
            format!("{x},{y}"),
            format!("{x},{}", -y - 1),
            format!("{},{y}", -x - 1),
        );
        let (local_id, synced_id, gone_id) = (
            format!("bafylocal{t}"),
            format!("bafysynced{t}"),
            format!("bafygone{t}"),
        );
        scene(&pool, &local_id, &local_base, &[&local_base], None).await;
        scene(&pool, &synced_id, &synced_base, &[&synced_base], None).await;
        scene(&pool, &gone_id, &gone_base, &[&gone_base], None).await;
        for id in [&local_id, &gone_id] {
            sqlx::query(
                "INSERT INTO content.local_entities (entity_id, signer) VALUES ($1, '0xabc')",
            )
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
        let place = |base: &str| super::super::parcel_place_id(base);
        let row = |id: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (String, String)>(
                    "SELECT raw->>'source', origin FROM explore.place WHERE id = $1",
                )
                .bind(id)
                .fetch_optional(&pool)
                .await
                .unwrap()
            }
        };

        let out = run_once(&pool, DeriveScope::Local, "https://node.example/content")
            .await
            .unwrap();
        assert_eq!(
            out,
            PassSummary {
                derived: 2,
                pruned: 0
            }
        );
        assert_eq!(
            row(place(&local_base)).await,
            Some(("content".into(), "local".into()))
        );
        assert_eq!(
            row(place(&gone_base)).await,
            Some(("content".into(), "local".into()))
        );
        assert_eq!(
            row(place(&synced_base)).await,
            None,
            "a synced scene is the mirror's or nobody's in the local scope"
        );

        // a binary from before `origin` derived one too: the default made it
        // 'upstream', and its raw.local is what the mirror goes by
        let legacy = format!("legacy-derived-{t}");
        sqlx::query(
            "INSERT INTO explore.place (id, base_position, raw, fetched_at) \
             VALUES ($1, '-150,-150', '{\"source\": \"content\", \"local\": true}', \
                     timezone('UTC', now()) - interval '1 day')",
        )
        .bind(&legacy)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            row(legacy.clone()).await,
            Some(("content".into(), "upstream".into()))
        );

        // the places mirror's retire keeps the local rows, and so does its
        // sweep however stale the row is (a stalled derive job)
        super::super::mirror::retire_derived(&pool).await.unwrap();
        assert!(row(place(&local_base)).await.is_some());
        assert_eq!(
            row(legacy.clone()).await,
            Some(("content".into(), "local".into())),
            "reclassified, not retired"
        );
        sqlx::query(
            "UPDATE explore.place SET fetched_at = fetched_at - interval '1 day' WHERE id = $1",
        )
        .bind(place(&local_base))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE explore.place SET origin = 'upstream' WHERE id = $1")
            .bind(&legacy)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            super::super::mirror::sweep_removed(
                &pool,
                super::super::mirror::Leg::Places,
                Utc::now() - chrono::Duration::hours(1)
            )
            .await
            .unwrap(),
            super::super::mirror::SweepOutcome::Swept(0)
        );
        assert!(row(place(&local_base)).await.is_some());
        assert_eq!(
            row(legacy.clone()).await,
            Some(("content".into(), "local".into())),
            "reclassified, not swept"
        );

        // unpublished: the next pass prunes it
        sqlx::query("UPDATE content.local_entities SET tombstoned_at = timezone('UTC', now()) WHERE entity_id = $1").bind(&gone_id).execute(&pool).await.unwrap();
        run_once(&pool, DeriveScope::Local, "https://node.example/content")
            .await
            .unwrap();
        assert_eq!(row(place(&gone_base)).await, None);
        assert!(row(place(&local_base)).await.is_some());

        // the whole-catalogue scope derives the synced scene too
        run_once(&pool, DeriveScope::All, "https://node.example/content")
            .await
            .unwrap();
        assert_eq!(
            row(place(&synced_base)).await,
            Some(("content".into(), "upstream".into()))
        );

        // a mirrored row at a derived id is the mirror's
        sqlx::query("UPDATE explore.place SET raw = raw || '{\"source\": \"mirror\"}'::jsonb, title = 'upstream', origin = 'upstream' WHERE id = $1").bind(place(&local_base)).execute(&pool).await.unwrap();
        run_once(&pool, DeriveScope::Local, "https://node.example/content")
            .await
            .unwrap();
        let title: String = sqlx::query_scalar("SELECT title FROM explore.place WHERE id = $1")
            .bind(place(&local_base))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(title, "upstream");

        let _ = (&synced_id, &local_id);
        pool.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE \"{name}\" WITH (FORCE)"
        )))
        .execute(&admin)
        .await
        .unwrap();
    }

    async fn world(pool: &PgPool, name: &str, mirror: Option<&str>, access: &str) {
        sqlx::query(
            "INSERT INTO worlds.worlds (name, owner, title, description, content_rating, categories,
                 show_in_places, thumbnail_hash, access_type, access_secret_hash, mirror_source)
             VALUES ($1, '0x00000000000000000000000000000000000000aa', 'Title ' || $1, 'about it',
                     'T', '{game}', true, 'bafythumb', $2,
                     CASE WHEN $2 = 'shared-secret' THEN 'hash' END, $3)",
        )
        .bind(name)
        .bind(access)
        .bind(mirror)
        .execute(pool)
        .await
        .unwrap();
        let entity = json!({ "metadata": {
            "scene": { "base": "1,1", "parcels": ["1,1", "1,2"] },
            "contact": { "name": "builder" },
            "worldConfiguration": { "name": name.to_uppercase() },
        }});
        sqlx::query(
            "INSERT INTO worlds.world_scenes (world_name, entity_id, deployment_auth_chain, entity,
                 deployer, parcels, size, mirror_source)
             VALUES ($1, 'bafy' || $1, '[]', $2, '0xaa', '{\"1,1\",\"1,2\"}', 1, $3)",
        )
        .bind(name)
        .bind(entity)
        .bind(mirror)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A world this node publishes is a catalogue row of the local leg, keyed by
    /// its name as upstream keys worlds, and the only row of that world: the
    /// mirrored one goes and the places mirror does not bring it back. A world
    /// only mirrored is never derived; blocking the world prunes the row until
    /// it is unblocked, as the worlds listing hides it; unpublishing the last
    /// local scene prunes the row and the mirror lists the world again.
    #[tokio::test]
    async fn local_worlds_reach_the_catalogue_once_and_leave_when_blocked_or_unpublished() {
        let Ok(base) = std::env::var("CATALYST_TEST_DATABASE_URL") else {
            return;
        };
        let opts: sqlx::postgres::PgConnectOptions = base.parse().expect("a postgres DSN");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone().database("postgres"))
            .await
            .unwrap();
        let name = format!(
            "catalyst_z_derive_worlds_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        crate::db::scratch::create_database_with_schema(&admin, &name).await;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(opts.database(&name))
            .await
            .unwrap();

        let upstream = |id: &str| json!({ "id": id, "world_name": id, "title": "upstream" });
        for id in ["local.dcl.eth", "mirrored.dcl.eth"] {
            super::super::mirror::upsert_world(&pool, &upstream(id))
                .await
                .unwrap();
        }
        world(&pool, "local.dcl.eth", None, "unrestricted").await;
        world(&pool, "secret.dcl.eth", None, "shared-secret").await;
        world(
            &pool,
            "mirrored.dcl.eth",
            Some("https://worlds.example"),
            "unrestricted",
        )
        .await;
        let listed = |world: &str| {
            let pool = pool.clone();
            let world = world.to_string();
            async move {
                sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
                    "SELECT id, raw->>'source', title FROM explore.place_indexed
                     WHERE world IS TRUE AND lower(world_name) = $1 ORDER BY id",
                )
                .bind(world)
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };
        let local = |title: &str| {
            vec![(
                "local.dcl.eth".to_string(),
                Some("worlds".to_string()),
                Some(title.to_string()),
            )]
        };
        let mirrored = |id: &str| vec![(id.to_string(), None, Some("upstream".to_string()))];

        run_once(&pool, DeriveScope::Local, "https://node.example/content/")
            .await
            .unwrap();
        assert_eq!(listed("local.dcl.eth").await, local("Title local.dcl.eth"));
        assert_eq!(
            listed("mirrored.dcl.eth").await,
            mirrored("mirrored.dcl.eth")
        );
        let (world_name, owner, image, private, base): (String, String, String, bool, String) =
            sqlx::query_as(
                "SELECT world_name, creator_address, raw->>'image', (raw->>'is_private')::boolean,
                        base_position
                 FROM explore.place_world_local WHERE id = 'local.dcl.eth'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            (
                world_name.as_str(),
                owner.as_str(),
                image.as_str(),
                private,
                base.as_str()
            ),
            (
                "LOCAL.DCL.ETH",
                "0x00000000000000000000000000000000000000aa",
                "https://node.example/content/contents/bafythumb",
                false,
                "1,1"
            )
        );
        let private: bool = sqlx::query_scalar(
            "SELECT (raw->>'is_private')::boolean FROM explore.place_world_local WHERE id = 'secret.dcl.eth'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(private, "an access-restricted world is private");

        // the places mirror's next pass leaves the local world alone
        super::super::mirror::upsert_world(&pool, &upstream("local.dcl.eth"))
            .await
            .unwrap();
        assert_eq!(listed("local.dcl.eth").await, local("Title local.dcl.eth"));

        // a settings change shows on the next pass
        sqlx::query("UPDATE worlds.worlds SET title = 'Renamed' WHERE name = 'local.dcl.eth'")
            .execute(&pool)
            .await
            .unwrap();
        run_once(&pool, DeriveScope::Local, "https://node.example/content")
            .await
            .unwrap();
        assert_eq!(listed("local.dcl.eth").await, local("Renamed"));

        // a blocked world leaves the catalogue until it is unblocked, and the
        // places mirror's passes meanwhile do not list upstream's copy of it
        for (blocked, expected) in [
            ("timezone('UTC', now())", vec![]),
            ("NULL", local("Renamed")),
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE worlds.worlds SET blocked_since = {blocked} WHERE name = 'local.dcl.eth'"
            )))
            .execute(&pool)
            .await
            .unwrap();
            run_once(&pool, DeriveScope::Local, "https://node.example/content")
                .await
                .unwrap();
            super::super::mirror::upsert_world(&pool, &upstream("local.dcl.eth"))
                .await
                .unwrap();
            assert_eq!(
                listed("local.dcl.eth").await,
                expected,
                "blocked_since {blocked}"
            );
        }

        // the last local scene unpublished: pruned, and the mirror lists it again
        sqlx::query("DELETE FROM worlds.world_scenes WHERE world_name = 'local.dcl.eth'")
            .execute(&pool)
            .await
            .unwrap();
        run_once(&pool, DeriveScope::Local, "https://node.example/content")
            .await
            .unwrap();
        assert_eq!(listed("local.dcl.eth").await, vec![]);
        super::super::mirror::upsert_world(&pool, &upstream("local.dcl.eth"))
            .await
            .unwrap();
        assert_eq!(listed("local.dcl.eth").await, mirrored("local.dcl.eth"));

        pool.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE \"{name}\" WITH (FORCE)"
        )))
        .execute(&admin)
        .await
        .unwrap();
    }
}

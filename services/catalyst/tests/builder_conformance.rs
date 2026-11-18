//! The builder's parity ledger (`tests/fixtures/conformance/builder/parity.json`)
//! checked against the route ledger (`builder/src/tests/upstream_routes.rs`),
//! and its captures replayed against the builder routers as `serve` composes
//! them, with the social family's replay rules
//! (`social::test_fixtures::replay_signed`). Every capture is evidence of
//! exactly one operation and carries the replay notes it must produce: none
//! for a match, the recorded difference for a known gap. The replay needs
//! CATALYST_TEST_DATABASE_URL; one scratch database for the file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy::signers::{Signer, local::PrivateKeySigner};
use alloy_primitives::keccak256;
use axum::Router;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use catalyst_testing::conformance::diff::REDACTION_MARK;
use catalyst_testing::conformance::fixture::{Fixture, VOLATILE_SENTINEL};
use catalyst_testing::conformance::volatility::Volatility;
use serde::Deserialize;
use serde_json::Value;

use catalyst::auth::{AuthLink, AuthLinkType};
use catalyst::builder::{Settings, State, root_router, router};
use catalyst::http::Outbound;
use catalyst::shared::signed_fetch::{
    AUTH_CHAIN_HEADER_PREFIX, AUTH_METADATA_HEADER, AUTH_TIMESTAMP_HEADER, build_payload_v6,
};
use catalyst::social::test_fixtures::replay_with;
use catalyst::storage::Storage;

#[path = "support/db.rs"]
mod db;

const FIXTURES: &str = "services/catalyst/tests/fixtures/conformance/builder";
const ROUTE_LEDGER: &str = "services/catalyst/builder/src/tests/upstream_routes.rs";
const GATEWAY_PREFIX: &str = "/builder-api";
const STATUSES: &[&str] = &["not_reviewed", "reviewed", "known_gap"];

#[derive(Deserialize)]
struct Ledger {
    schema_version: u32,
    #[serde(default)]
    volatile_fields: Vec<String>,
    operations: Vec<Operation>,
}

#[derive(Deserialize)]
struct Operation {
    id: String,
    owner: String,
    status: String,
    result: String,
    evidence: Vec<Evidence>,
    gaps: Vec<String>,
    remaining_verification: String,
}

#[derive(Deserialize)]
struct Evidence {
    path: String,
    #[serde(default)]
    test: Option<String>,
    scope: String,
    #[serde(default)]
    replay: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Seed {
    wallets: BTreeMap<String, String>,
    collections: Vec<SeedCollection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedCollection {
    id: uuid::Uuid,
    name: String,
    owner: String,
    salt: String,
    contract_address: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

fn parse_seed(text: &str) -> Result<Seed, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

fn root() -> PathBuf {
    PathBuf::from(env!("DCL_ONE_ROOT"))
}

fn ledger() -> Ledger {
    let path = root().join(FIXTURES).join("parity.json");
    serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| panic!("{path:?}: {e}")))
        .unwrap()
}

fn in_scope_routes() -> BTreeSet<String> {
    let source = std::fs::read_to_string(root().join(ROUTE_LEDGER)).unwrap();
    let table = &source[source.find("const UPSTREAM").unwrap()..];
    let table = &table[..table.find("];").unwrap()];
    let compact: String = table.chars().filter(|c| !c.is_whitespace()).collect();
    let rows: Vec<(String, String, String)> = compact
        .split("(\"")
        .skip(1)
        .map(|row| {
            let (method, rest) = row.split_once("\",\"").unwrap();
            let (path, rest) = rest.split_once("\",").unwrap();
            let status = rest.split(')').next().unwrap().trim_end_matches(',');
            assert!(
                ["Routed", "Todo", "Removed"].contains(&status),
                "unparsed route ledger row {row}"
            );
            (method.to_string(), path.to_string(), status.to_string())
        })
        .collect();
    assert!(
        rows.len() > 70,
        "the route ledger parsed to {} rows",
        rows.len()
    );
    rows.into_iter()
        .filter(|(_, _, status)| status != "Removed")
        .map(|(method, path, _)| format!("api_http:{method}:/v1{path}"))
        .collect()
}

fn raw_capture(path: &str) -> Value {
    let bytes = std::fs::read(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn checked_capture(path: &str, volatile_fields: &[String]) -> Value {
    let raw = raw_capture(path);
    let relative = path.strip_prefix(&format!("{FIXTURES}/")).unwrap_or(path);
    let problems = capture_errors(relative, &raw, volatile_fields);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    raw
}

fn fixture_files() -> BTreeSet<String> {
    fn walk(dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                let relative = path.strip_prefix(root()).unwrap();
                out.insert(relative.to_str().unwrap().to_string());
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(&root().join(FIXTURES), &mut out);
    out
}

fn exercises(id: &str, method: &str, path: &str) -> bool {
    let Some((id_method, template)) = id
        .strip_prefix("api_http:")
        .and_then(|rest| rest.split_once(':'))
    else {
        return false;
    };
    let path = path.split('?').next().unwrap_or_default();
    let path = path.strip_prefix(GATEWAY_PREFIX).unwrap_or(path);
    let path = if path.len() > 1 {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        path
    };
    let segments: Vec<&str> = path.split('/').collect();
    let expected: Vec<&str> = template.split('/').collect();
    id_method == method
        && segments.len() == expected.len()
        && segments.iter().zip(&expected).all(|(segment, want)| {
            if want.starts_with(':') {
                !segment.is_empty()
            } else {
                segment == want
            }
        })
}

fn volatile_notes(raw: &Value, candidate: &[u8]) -> Vec<String> {
    let Ok(body) = serde_json::from_slice::<Value>(candidate) else {
        return Vec::new();
    };
    let at = |path: &str| values_at(&body, &path.split('.').collect::<Vec<_>>());
    let mut notes = Vec::new();
    for path in strings(raw, "volatile_paths") {
        if at(&path).iter().any(|v| !(v.is_string() || v.is_number())) {
            notes.push(format!("{path} volatile value is not a timestamp"));
        }
    }
    for paths in equalities(raw) {
        let values: Vec<Vec<&Value>> = paths.iter().map(|p| at(p)).collect();
        if values.windows(2).any(|pair| pair[0] != pair[1]) {
            notes.push(format!("{} differ", paths.join(" and ")));
        }
    }
    notes
}

fn request_of(raw: &Value) -> Option<(String, String)> {
    Some((
        raw["request"]["method"].as_str()?.to_string(),
        raw["request"]["path"].as_str()?.to_string(),
    ))
}

fn strings(raw: &Value, key: &str) -> Vec<String> {
    serde_json::from_value(raw[key].clone()).unwrap_or_default()
}

fn equalities(raw: &Value) -> Vec<Vec<String>> {
    serde_json::from_value(raw["volatile_equal"].clone()).unwrap_or_default()
}

struct Marker {
    path: String,
    at: *const Value,
    redacted: bool,
}

fn markers(value: &Value, path: String, out: &mut Vec<Marker>) {
    match value {
        Value::String(text)
            if text.contains(REDACTION_MARK) || text.contains(VOLATILE_SENTINEL) =>
        {
            out.push(Marker {
                path,
                at: value,
                redacted: text.contains(REDACTION_MARK),
            })
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                markers(item, format!("{path}[{i}]"), out);
            }
        }
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                markers(child, child_path, out);
            }
        }
        _ => {}
    }
}

fn capture_errors(relative: &str, raw: &Value, volatile_fields: &[String]) -> Vec<String> {
    let mut errors = Vec::new();
    let volatile_paths = strings(raw, "volatile_paths");
    let body = &raw["response"]["body_json"];
    let mut reached = BTreeSet::new();
    for volatile in &volatile_paths {
        let field = volatile.rsplit('.').next().unwrap_or_default();
        if !volatile_fields.iter().any(|allowed| allowed == field) {
            errors.push(format!(
                "{relative}: volatile path {volatile} is not one of the volatile_fields"
            ));
            continue;
        }
        let found = values_at(body, &volatile.split('.').collect::<Vec<_>>());
        if found.is_empty() || found.iter().any(|v| v.as_str() != Some(VOLATILE_SENTINEL)) {
            errors.push(format!(
                "{relative}: {volatile} is not recorded as {VOLATILE_SENTINEL}"
            ));
        }
        reached.extend(found.into_iter().map(|v| v as *const Value));
    }
    let mut found = Vec::new();
    markers(raw, String::new(), &mut found);
    for marker in found {
        let path = marker.path;
        if marker.redacted {
            errors.push(format!("{relative}: <<REDACTED>> at {path}"));
        } else if !reached.contains(&marker.at) {
            errors.push(format!(
                "{relative}: {VOLATILE_SENTINEL} at {path} is not reached by a volatile path"
            ));
        }
    }
    for path in equalities(raw).iter().flatten() {
        if !volatile_paths.contains(path) {
            errors.push(format!(
                "{relative}: volatile_equal names {path}, which is not a volatile path"
            ));
        }
    }
    errors
}

fn values_at<'a>(value: &'a Value, parts: &[&str]) -> Vec<&'a Value> {
    match value {
        Value::Array(items) => items
            .iter()
            .flat_map(|item| values_at(item, parts))
            .collect(),
        _ if parts.is_empty() => vec![value],
        Value::Object(map) => map
            .get(parts[0])
            .map(|child| values_at(child, &parts[1..]))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn captures(files: &BTreeSet<String>) -> BTreeSet<String> {
    files
        .iter()
        .filter(|path| {
            ["source", "live"].iter().any(|dir| {
                path.strip_prefix(&format!("{FIXTURES}/{dir}/"))
                    .is_some_and(|name| name.ends_with(".json") && !name.contains('/'))
            })
        })
        .cloned()
        .collect()
}

fn ledger_errors(
    ledger: &Ledger,
    routes: &BTreeSet<String>,
    files: &BTreeSet<String>,
    captured: &BTreeMap<String, Value>,
) -> Vec<String> {
    let mut errors = Vec::new();
    if ledger.schema_version != 1 {
        errors.push(format!("schema_version {}", ledger.schema_version));
    }
    let named: BTreeSet<String> = ledger.operations.iter().map(|o| o.id.clone()).collect();
    if named.len() != ledger.operations.len() {
        errors.push("duplicate operations in parity.json".to_string());
    }
    for id in routes.difference(&named) {
        errors.push(format!("{id}: missing from parity.json"));
    }
    for id in named.difference(routes) {
        errors.push(format!("{id}: not an in-scope route"));
    }
    let replayed = captures(files);
    let mut cited = BTreeMap::new();
    for op in &ledger.operations {
        let id = &op.id;
        if !STATUSES.contains(&op.status.as_str()) {
            errors.push(format!("{id}: status {}", op.status));
        }
        if op.owner.is_empty() || op.result.is_empty() || op.remaining_verification.is_empty() {
            errors.push(format!(
                "{id}: owner, result and remaining_verification are required"
            ));
        }
        if op.status != "not_reviewed" && op.evidence.is_empty() {
            errors.push(format!("{id}: a review needs evidence"));
        }
        if op.status == "known_gap" && op.gaps.is_empty() {
            errors.push(format!("{id}: a known gap names its difference"));
        }
        for evidence in &op.evidence {
            let path = &evidence.path;
            let exists = if path.starts_with(FIXTURES) {
                files.contains(path)
            } else {
                root().join(path).is_file()
            };
            if !exists {
                errors.push(format!("{id}: missing evidence {path}"));
                continue;
            }
            if evidence.scope.is_empty() {
                errors.push(format!("{id}: evidence {path} states no scope"));
            }
            if let Some(test) = &evidence.test {
                let text = std::fs::read_to_string(root().join(path)).unwrap_or_default();
                if !text.contains(&format!("fn {test}(")) {
                    errors.push(format!("{id}: no test {test} in {path}"));
                }
            }
            let under_fixtures = path.strip_prefix(&format!("{FIXTURES}/"));
            if let Some(relative) = under_fixtures {
                if !replayed.contains(path) {
                    errors.push(format!("{relative} is not a replayed capture"));
                }
                if evidence.replay.is_none() {
                    errors.push(format!("{id} cites {relative} without its replay"));
                }
            }
            if let (Some(_), Some((method, request_path))) =
                (&evidence.replay, captured.get(path).and_then(request_of))
                && !exercises(id, &method, &request_path)
            {
                let relative = under_fixtures.unwrap_or(path);
                errors.push(format!(
                    "{relative} requests {method} {request_path}, not {id}"
                ));
            }
            if let Some(notes) = &evidence.replay {
                if under_fixtures.is_none() {
                    errors.push(format!("{id}: replay of a non-capture {path}"));
                }
                if op.status == "reviewed" && !notes.is_empty() {
                    errors.push(format!("{id}: reviewed with replay differences"));
                }
                if let Some(other) = cited.insert(path.clone(), id.clone()) {
                    errors.push(format!("{path} is evidence of {other} and {id}"));
                }
            }
        }
        let replays_a_capture = op
            .evidence
            .iter()
            .any(|e| e.replay.is_some() && replayed.contains(&e.path));
        if op.status == "reviewed" && !replays_a_capture {
            errors.push(format!("{id}: reviewed without a replayed capture"));
        }
    }
    for path in files {
        let relative = path.strip_prefix(&format!("{FIXTURES}/")).unwrap_or(path);
        let data = ["parity.json", "seed.json"].contains(&relative);
        if path.ends_with(".json") && !data && !replayed.contains(path) {
            errors.push(format!("{relative} is not a replayed capture"));
        }
    }
    for path in &replayed {
        let relative = path.strip_prefix(&format!("{FIXTURES}/")).unwrap_or(path);
        let Some(raw) = captured.get(path) else {
            errors.push(format!("{relative}: not parsed"));
            continue;
        };
        if let Err(e) = serde_json::from_value::<Fixture>(raw.clone()) {
            errors.push(format!("{relative}: {e}"));
        }
        errors.extend(capture_errors(relative, raw, &ledger.volatile_fields));
        if let Some((method, request_path)) = request_of(raw) {
            let matching: Vec<&str> = routes
                .iter()
                .filter(|route| exercises(route, &method, &request_path))
                .map(String::as_str)
                .collect();
            if matching.len() > 1 {
                errors.push(format!(
                    "{relative} requests {method} {request_path}, which matches several operations: {}",
                    matching.join(", ")
                ));
            }
        }
        if !cited.contains_key(path) {
            errors.push(format!("{path}: no operation cites this capture"));
        }
    }
    errors.sort();
    errors.dedup();
    errors
}

#[test]
fn the_parity_ledger_names_every_in_scope_route() {
    let files = fixture_files();
    let fixtures = captures(&files)
        .into_iter()
        .map(|path| {
            let raw = raw_capture(&path);
            (path, raw)
        })
        .collect();
    let errors = ledger_errors(&ledger(), &in_scope_routes(), &files, &fixtures);
    assert!(errors.is_empty(), "parity.json:\n{}", errors.join("\n"));
}

const INFO: &str = "api_http:GET:/v1/info";

fn fixture(name: &str) -> String {
    format!("{FIXTURES}/{name}")
}

fn capture(method: &str, path: &str, body: Value, volatile: &[&str]) -> Value {
    serde_json::json!({
        "request": {"method": method, "path": path},
        "response": {"status": 200, "body_json": body},
        "volatile_paths": volatile
    })
}

fn info_captures(files: &BTreeSet<String>) -> BTreeMap<String, Value> {
    captures(files)
        .into_iter()
        .map(|path| (path, capture("GET", "/v1/info", serde_json::json!({}), &[])))
        .collect()
}

fn ledger_value(operations: Value) -> Ledger {
    serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "volatile_fields": ["created_at", "updated_at"],
        "operations": operations
    }))
    .unwrap()
}

fn checked(operations: Value, files: &[&str]) -> Vec<String> {
    let files = files.iter().map(|name| fixture(name)).collect();
    let routes = BTreeSet::from([INFO.to_string()]);
    ledger_errors(
        &ledger_value(operations),
        &routes,
        &files,
        &info_captures(&files),
    )
}

fn operation(status: &str, evidence: Value) -> Value {
    serde_json::json!({
        "id": INFO, "owner": "small-reads", "status": status, "result": "compared",
        "evidence": evidence, "gaps": if status == "known_gap" { vec!["differs"] } else { vec![] },
        "remaining_verification": "more cases"
    })
}

fn refused(errors: &[String], expected: &str) {
    assert!(
        errors.iter().any(|e| e.contains(expected)),
        "expected an error containing {expected:?}, got {errors:#?}"
    );
}

#[test]
fn a_reviewed_operation_with_a_matching_capture_is_accepted() {
    let evidence =
        serde_json::json!([{"path": fixture("source/01-info.json"), "scope": "s", "replay": []}]);
    let errors = checked(
        serde_json::json!([operation("reviewed", evidence)]),
        &["parity.json", "seed.json", "source/01-info.json"],
    );
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn a_capture_cited_without_its_replay_is_refused() {
    let mut gap = operation(
        "known_gap",
        serde_json::json!([{"path": fixture("source/01-info.json"), "scope": "s", "replay": ["status: fixture=200 candidate=404"]}]),
    );
    gap["id"] = "api_http:GET:/v1/rarities".into();
    let reviewed = operation(
        "reviewed",
        serde_json::json!([{"path": fixture("source/01-info.json"), "scope": "s"}]),
    );
    let ledger = ledger_value(serde_json::json!([reviewed, gap]));
    let routes = BTreeSet::from([INFO.to_string(), "api_http:GET:/v1/rarities".to_string()]);
    let files = BTreeSet::from([fixture("source/01-info.json")]);
    refused(
        &ledger_errors(&ledger, &routes, &files, &info_captures(&files)),
        "cites source/01-info.json without its replay",
    );
}

#[test]
fn a_replay_of_a_file_that_is_not_a_capture_is_refused() {
    let evidence = serde_json::json!([{"path": fixture("seed.json"), "scope": "s", "replay": []}]);
    let errors = checked(
        serde_json::json!([operation("reviewed", evidence)]),
        &["parity.json", "seed.json"],
    );
    refused(&errors, "seed.json is not a replayed capture");
}

#[test]
fn a_capture_outside_source_and_live_is_refused() {
    let evidence = serde_json::json!([{"path": fixture("source/extra/01-info.json"), "scope": "s", "replay": []}]);
    let errors = checked(
        serde_json::json!([operation("reviewed", evidence)]),
        &[
            "parity.json",
            "seed.json",
            "source/extra/01-info.json",
            "captures/02-info.json",
        ],
    );
    refused(
        &errors,
        "source/extra/01-info.json is not a replayed capture",
    );
    refused(&errors, "captures/02-info.json is not a replayed capture");
}

#[test]
fn a_reviewed_operation_without_a_replayed_capture_is_refused() {
    let evidence = serde_json::json!([{
        "path": "services/catalyst/tests/builder_conformance.rs",
        "test": "the_parity_ledger_names_every_in_scope_route",
        "scope": "s"
    }]);
    let errors = checked(
        serde_json::json!([operation("reviewed", evidence)]),
        &["parity.json", "seed.json"],
    );
    refused(&errors, "reviewed without a replayed capture");
}

#[test]
fn a_capture_of_another_operation_is_refused() {
    let collections = "api_http:GET:/v1/collections";
    let path = fixture("source/01-collections-public-search.json");
    let mut moved = operation(
        "reviewed",
        serde_json::json!([{"path": path, "scope": "s", "replay": []}]),
    );
    moved["id"] = INFO.into();
    let mut left = operation("not_reviewed", serde_json::json!([]));
    left["id"] = collections.into();
    let routes = BTreeSet::from([INFO.to_string(), collections.to_string()]);
    let files = BTreeSet::from([path.clone()]);
    let fixtures = BTreeMap::from([(
        path,
        capture(
            "GET",
            "/v1/collections",
            serde_json::json!({"ok": true, "data": []}),
            &[],
        ),
    )]);
    let ledger = ledger_value(serde_json::json!([moved, left]));
    refused(
        &ledger_errors(&ledger, &routes, &files, &fixtures),
        "source/01-collections-public-search.json requests GET /v1/collections, not api_http:GET:/v1/info",
    );
}

#[test]
fn a_capture_matches_its_operation_by_method_and_path_template() {
    let id = "api_http:GET:/v1/collections/:id/items";
    assert!(exercises(id, "GET", "/v1/collections/c011ec71/items"));
    assert!(exercises(id, "GET", "/v1/collections/c011ec71/items/"));
    assert!(exercises(
        id,
        "GET",
        "/builder-api/v1/collections/c011ec71/items"
    ));
    assert!(exercises(
        id,
        "GET",
        "/v1/collections/c011ec71/items?page=1"
    ));
    assert!(!exercises(id, "PUT", "/v1/collections/c011ec71/items"));
    assert!(!exercises(id, "GET", "/v1/collections//items"));
    assert!(!exercises(
        id,
        "GET",
        "/v1/collections/c011ec71/items/extra"
    ));
    assert!(!exercises(id, "GET", "/v1/collections/c011ec71"));
    assert!(!exercises(
        id,
        "GET",
        "/builder-api/collections/c011ec71/items"
    ));
    assert!(exercises(
        "api_http:GET:/v1/collections",
        "GET",
        "/v1/collections/"
    ));
    assert!(!exercises(
        "api_http:GET:/v1/collections",
        "GET",
        "/v1/collectionsx"
    ));
    assert!(exercises(
        "api_http:HEAD:/v1/storage/contents/:filename",
        "HEAD",
        "/v1/storage/contents/Qm1"
    ));
}

#[test]
fn a_volatile_path_outside_the_allowed_fields_is_refused() {
    let path = fixture("source/01-info.json");
    let evidence = serde_json::json!([{"path": path, "scope": "s", "replay": []}]);
    let ledger = ledger_value(serde_json::json!([operation("reviewed", evidence)]));
    let routes = BTreeSet::from([INFO.to_string()]);
    let files = BTreeSet::from([path.clone()]);
    let check = |fx: Value| {
        ledger_errors(
            &ledger,
            &routes,
            &files,
            &BTreeMap::from([(path.clone(), fx)]),
        )
    };
    let masked =
        serde_json::json!({"data": "<<VOLATILE>>", "created": {"created_at": "<<VOLATILE>>"}});
    let only_created = serde_json::json!({"created": {"created_at": "<<VOLATILE>>"}});
    let allowed = check(capture(
        "GET",
        "/v1/info",
        only_created,
        &["created.created_at"],
    ));
    assert!(allowed.is_empty(), "{allowed:#?}");
    refused(
        &check(capture("GET", "/v1/info", masked.clone(), &["data"])),
        "source/01-info.json: volatile path data is not one of the volatile_fields",
    );
    refused(
        &check(capture("GET", "/v1/info", masked, &["data[]"])),
        "source/01-info.json: volatile path data[] is not one of the volatile_fields",
    );
    let recorded = serde_json::json!({"created": {"created_at": "2025-12-01T00:00:00.000Z"}});
    refused(
        &check(capture(
            "GET",
            "/v1/info",
            recorded,
            &["created.created_at"],
        )),
        "source/01-info.json: created.created_at is not recorded as <<VOLATILE>>",
    );
    let missing = serde_json::json!({"created": {}});
    refused(
        &check(capture("GET", "/v1/info", missing, &["created.created_at"])),
        "source/01-info.json: created.created_at is not recorded as <<VOLATILE>>",
    );
}

fn one_capture(fx: Value, extra_routes: &[&str]) -> Vec<String> {
    let path = fixture("source/01-info.json");
    let evidence = serde_json::json!([{"path": path, "scope": "s", "replay": []}]);
    let ledger = ledger_value(serde_json::json!([operation("reviewed", evidence)]));
    let mut routes = BTreeSet::from([INFO.to_string()]);
    routes.extend(extra_routes.iter().map(|r| r.to_string()));
    let mut ledger = ledger;
    for route in extra_routes {
        let mut other = operation("not_reviewed", serde_json::json!([]));
        other["id"] = (*route).into();
        ledger
            .operations
            .push(serde_json::from_value(other).unwrap());
    }
    let files = BTreeSet::from([path.clone()]);
    let fixtures = BTreeMap::from([(path, fx)]);
    ledger_errors(&ledger, &routes, &files, &fixtures)
}

#[test]
fn a_marker_written_into_a_capture_is_refused() {
    let unreached = serde_json::json!({
        "request": {"method": "GET", "path": "/v1/info"},
        "response": {"status": 200, "body_json": {"ok": true, "data": "<<VOLATILE>>"}},
        "volatile_paths": []
    });
    refused(
        &one_capture(unreached, &[]),
        "source/01-info.json: <<VOLATILE>> at response.body_json.data is not reached by a volatile path",
    );
    let redacted = serde_json::json!({
        "request": {"method": "GET", "path": "/v1/info"},
        "response": {"status": 200, "body_json": {"ok": true, "data": "<<REDACTED>>"}}
    });
    refused(
        &one_capture(redacted, &[]),
        "source/01-info.json: <<REDACTED>> at response.body_json.data",
    );
    let in_request = serde_json::json!({
        "request": {"method": "GET", "path": "/v1/info", "headers": {"x-identity-auth-chain-0": "<<REDACTED:jwt>>"}},
        "response": {"status": 200, "body_json": {"ok": true}}
    });
    refused(
        &one_capture(in_request, &[]),
        "source/01-info.json: <<REDACTED>> at request.headers.x-identity-auth-chain-0",
    );
}

#[test]
fn a_capture_that_matches_two_operations_is_refused() {
    let by_id = "api_http:GET:/v1/collections/:id";
    let by_address = "api_http:GET:/v1/:address/items";
    let path = fixture("source/01-collection.json");
    let mut cited = operation(
        "reviewed",
        serde_json::json!([{"path": path, "scope": "s", "replay": []}]),
    );
    cited["id"] = by_id.into();
    let mut other = operation("not_reviewed", serde_json::json!([]));
    other["id"] = by_address.into();
    let ledger = ledger_value(serde_json::json!([cited, other]));
    let routes = BTreeSet::from([by_id.to_string(), by_address.to_string()]);
    let files = BTreeSet::from([path.clone()]);
    let fixtures = BTreeMap::from([(
        path,
        capture(
            "GET",
            "/v1/collections/items",
            serde_json::json!({"ok": true}),
            &[],
        ),
    )]);
    refused(
        &ledger_errors(&ledger, &routes, &files, &fixtures),
        "source/01-collection.json requests GET /v1/collections/items, which matches several operations: api_http:GET:/v1/:address/items, api_http:GET:/v1/collections/:id",
    );
}

#[test]
fn a_volatile_value_must_be_a_timestamp_and_keep_recorded_equalities() {
    let raw = serde_json::json!({
        "volatile_paths": ["data.created_at", "data.updated_at"],
        "volatile_equal": [["data.created_at", "data.updated_at"]]
    });
    let candidate = |body: Value| volatile_notes(&raw, &serde_json::to_vec(&body).unwrap());
    let same = "2026-11-18T00:00:00.000Z";
    assert!(
        candidate(serde_json::json!({"data": {"created_at": same, "updated_at": same}})).is_empty()
    );
    assert!(candidate(serde_json::json!({"data": {"created_at": 1, "updated_at": 1}})).is_empty());
    assert_eq!(
        candidate(
            serde_json::json!({"data": {"created_at": {"nested": true}, "updated_at": same}})
        ),
        vec![
            "data.created_at volatile value is not a timestamp".to_string(),
            "data.created_at and data.updated_at differ".to_string()
        ]
    );
    assert_eq!(
        candidate(
            serde_json::json!({"data": {"created_at": same, "updated_at": "2026-11-18T00:00:01.000Z"}})
        ),
        vec!["data.created_at and data.updated_at differ".to_string()]
    );
}

#[test]
fn a_seed_field_the_replay_cannot_apply_is_refused() {
    let collection = r#"{"id": "c011ec71-0000-4000-8000-000000000001", "name": "n", "owner": "owner",
        "salt": "0x00", "created_at": "2025-12-01T00:00:00Z", "updated_at": "2025-12-01T00:00:00Z""#;
    let committee =
        format!(r#"{{"wallets": {{}}, "committee": ["owner"], "collections": [{collection}}}]}}"#);
    let error = parse_seed(&committee).err().unwrap_or_default();
    assert!(error.contains("committee"), "{error:?}");
    let locked = format!(r#"{{"wallets": {{}}, "collections": [{collection}, "lock": null}}]}}"#);
    let error = parse_seed(&locked).err().unwrap_or_default();
    assert!(error.contains("lock"), "{error:?}");
}

struct Wallet {
    root: PrivateKeySigner,
    ephemeral: PrivateKeySigner,
}

impl Wallet {
    fn derived(role: &str) -> Self {
        let key = |label: String| {
            PrivateKeySigner::from_bytes(&keccak256(
                format!("dcl-one builder conformance {label}").as_bytes(),
            ))
            .unwrap()
        };
        Self {
            root: key(role.to_string()),
            ephemeral: key(format!("{role} ephemeral")),
        }
    }

    fn address(&self) -> String {
        format!("{:#x}", self.root.address())
    }

    async fn sign(&self, method: &str, path: &str, metadata: &str) -> HeaderMap {
        let timestamp = chrono::Utc::now().timestamp_millis().to_string();
        let payload = build_payload_v6(method, path, &timestamp, metadata);
        let ephemeral_payload = format!(
            "Decentraland Login\nEphemeral address: {:#x}\nExpiration: 2099-01-01T00:00:00.000Z",
            self.ephemeral.address()
        );
        let chain = [
            AuthLink {
                link_type: AuthLinkType::SIGNER,
                payload: self.address(),
                signature: None,
            },
            AuthLink {
                link_type: AuthLinkType::EcdsaEphemeral,
                signature: Some(
                    self.root
                        .sign_message(ephemeral_payload.as_bytes())
                        .await
                        .unwrap()
                        .to_string(),
                ),
                payload: ephemeral_payload,
            },
            AuthLink {
                link_type: AuthLinkType::EcdsaSignedEntity,
                signature: Some(
                    self.ephemeral
                        .sign_message(payload.as_bytes())
                        .await
                        .unwrap()
                        .to_string(),
                ),
                payload,
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
                HeaderName::from_bytes(format!("{AUTH_CHAIN_HEADER_PREFIX}{i}").as_bytes())
                    .unwrap(),
                HeaderValue::from_str(&serde_json::to_string(link).unwrap()).unwrap(),
            );
        }
        headers
    }
}

async fn fresh_signature(
    fx: &Fixture,
    wallets: &BTreeMap<String, Wallet>,
    prefix: &str,
) -> HeaderMap {
    let Some(first) = fx
        .request
        .headers
        .get(&format!("{AUTH_CHAIN_HEADER_PREFIX}0"))
    else {
        return HeaderMap::new();
    };
    let link: Value = serde_json::from_str(first).unwrap();
    let signer = link["payload"].as_str().unwrap().to_lowercase();
    let wallet = wallets
        .get(&signer)
        .unwrap_or_else(|| panic!("{signer} is not a seed.json wallet"));
    let metadata = fx
        .request
        .headers
        .get(AUTH_METADATA_HEADER)
        .map_or("{}", String::as_str);
    let path = format!("{prefix}{}", fx.request.path);
    wallet.sign(&fx.request.method, &path, metadata).await
}

const DRAFT_CONTRACT_REFUSED: &str = "collections_contract_published_check";

async fn insert_collection(
    pool: &sqlx::PgPool,
    c: &SeedCollection,
    owner: &str,
    contract_address: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO builder.collections \
         (id, name, eth_address, salt, contract_address, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(c.id)
    .bind(&c.name)
    .bind(owner)
    .bind(&c.salt)
    .bind(contract_address)
    .bind(c.created_at.naive_utc())
    .bind(c.updated_at.naive_utc())
    .execute(pool)
    .await
    .map(|_| ())
}

async fn seed(pool: &sqlx::PgPool, seed: &Seed) {
    for c in &seed.collections {
        let owner = &seed.wallets[&c.owner];
        let Err(error) = insert_collection(pool, c, owner, c.contract_address.as_deref()).await
        else {
            continue;
        };
        let refused_draft_contract = error
            .as_database_error()
            .and_then(|e| e.constraint())
            .is_some_and(|name| name == DRAFT_CONTRACT_REFUSED);
        assert!(
            refused_draft_contract,
            "seed.json collection {}: {error}",
            c.id
        );
        insert_collection(pool, c, owner, None).await.unwrap();
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mounted(state: Arc<State>) -> Router {
    Router::new()
        .merge(root_router(state.clone()))
        .nest("/builder-api", router(state))
}

struct Replay {
    scratch: db::ScratchDb,
    app: Router,
    wallets: BTreeMap<String, Wallet>,
    _dir: TempDir,
}

async fn replay_setup() -> Option<Replay> {
    let seed_data =
        parse_seed(&std::fs::read_to_string(root().join(FIXTURES).join("seed.json")).unwrap())
            .unwrap_or_else(|e| panic!("seed.json: {e}"));
    let scratch = db::scratch_database().await?;
    let mut wallets = BTreeMap::new();
    for (role, address) in &seed_data.wallets {
        let wallet = Wallet::derived(role);
        assert_eq!(&wallet.address(), address, "seed.json wallet {role}");
        wallets.insert(address.clone(), wallet);
    }
    seed(&scratch.pool, &seed_data).await;
    let dir = TempDir(std::env::temp_dir().join(format!(
        "catalyst-builder-conformance-{}",
        uuid::Uuid::new_v4()
    )));
    let storage = Arc::new(Storage::from_config(&dir.0).await.unwrap());
    let state = Arc::new(
        State::new(
            scratch.pool.clone(),
            storage.clone(),
            storage.denylist().clone(),
            Outbound::for_tests(),
            None,
            None,
            Settings {
                writes_enabled: true,
                ..Settings::default()
            },
        )
        .unwrap(),
    );
    Some(Replay {
        scratch,
        app: mounted(state),
        wallets,
        _dir: dir,
    })
}

async fn replayed(replay: &Replay, path: &str, raw: &Value, prefix: &str) -> Vec<String> {
    let fx: Fixture = serde_json::from_value(raw.clone()).unwrap_or_else(|e| panic!("{path}: {e}"));
    let name = Path::new(path).file_stem().unwrap().to_str().unwrap();
    let auth = fresh_signature(&fx, &replay.wallets, prefix).await;
    let every_key = Volatility::default().with_ignored_fields(&[]);
    let (mut notes, body) = replay_with(&replay.app, prefix, name, &fx, &auth, &every_key).await;
    notes.extend(volatile_notes(raw, &body));
    notes
}

#[tokio::test]
async fn a_changed_id_fails_the_replay() {
    let path = fixture("source/02-collection-owner.json");
    let mut raw = checked_capture(&path, &ledger().volatile_fields);
    raw["response"]["body_json"]["data"]["id"] = "c011ec71-0000-4000-8000-0000000000ff".into();
    let Some(replay) = replay_setup().await else {
        return;
    };
    let notes = replayed(&replay, &path, &raw, "").await;
    replay.scratch.teardown().await;
    assert!(notes.contains(&"data.id value".to_string()), "{notes:#?}");
}

#[tokio::test]
async fn the_builder_captures_replay_against_the_mounted_router() {
    let ledger = ledger();
    let expected: BTreeMap<String, (String, Vec<String>)> = ledger
        .operations
        .into_iter()
        .flat_map(|op| {
            let id = op.id;
            op.evidence
                .into_iter()
                .filter_map(move |e| e.replay.map(|notes| (e.path, (id.clone(), notes))))
        })
        .collect();
    let mut paths = captures(&fixture_files());
    paths.extend(expected.keys().cloned());
    let loaded: Vec<(String, Value)> = paths
        .into_iter()
        .map(|path| {
            let raw = checked_capture(&path, &ledger.volatile_fields);
            (path, raw)
        })
        .collect();
    let Some(replay) = replay_setup().await else {
        return;
    };
    let mut mismatches = Vec::new();
    for (path, raw) in loaded {
        let notes = replayed(&replay, &path, &raw, "").await;
        let Some((operation, recorded)) = expected.get(&path) else {
            mismatches.push(format!(
                "{path}: no operation cites it; it replays as {notes:#?}"
            ));
            continue;
        };
        if &notes != recorded {
            mismatches.push(format!(
                "{path} ({operation}): recorded {recorded:#?}, replayed {notes:#?}"
            ));
        }
        if ["GET", "HEAD"].contains(&raw["request"]["method"].as_str().unwrap_or_default()) {
            let prefixed = replayed(&replay, &path, &raw, GATEWAY_PREFIX).await;
            if &prefixed != recorded {
                mismatches.push(format!(
                    "{path} ({operation}) under {GATEWAY_PREFIX}: recorded {recorded:#?}, replayed {prefixed:#?}"
                ));
            }
        }
    }
    replay.scratch.teardown().await;
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

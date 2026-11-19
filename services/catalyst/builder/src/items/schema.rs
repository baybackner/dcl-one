//! The body of `PUT /v1/items/:idOrURN`, checked as builder-server checks it:
//! `upsertItemSchema` under its validator (ajv with `removeAdditional`), so a
//! key the schema doesn't name is dropped where the schema closes an object,
//! and everything else keeps the order and the values `JSON.parse` gives it.

use std::cmp::Ordering;

use crate::content::JsJson;
use crate::http::ApiError;

type Fields = Vec<(String, JsJson)>;

const MAX_CONTENTS: usize = 256;
const RARITIES: &[&str] = &[
    "unique",
    "mythic",
    "exotic",
    "legendary",
    "epic",
    "rare",
    "uncommon",
    "common",
];
const WEARABLE_CATEGORIES: &[&str] = &[
    "eyebrows",
    "eyes",
    "facial_hair",
    "hair",
    "body_shape",
    "mouth",
    "upper_body",
    "lower_body",
    "feet",
    "earring",
    "eyewear",
    "hat",
    "helmet",
    "mask",
    "tiara",
    "top_head",
    "skin",
    "hands_wear",
];
const EMOTE_CATEGORIES: &[&str] = &[
    "dance",
    "stunt",
    "greetings",
    "fun",
    "poses",
    "reactions",
    "horror",
    "miscellaneous",
];
const BODY_SHAPES: &[&str] = &[
    "urn:decentraland:off-chain:base-avatars:BaseMale",
    "urn:decentraland:off-chain:base-avatars:BaseFemale",
];
const URN_NETWORKS: &[&str] = &["mainnet", "goerli", "sepolia", "matic", "mumbai", "amoy"];
const MAPPING_NETWORKS: &[&str] = &[
    "mainnet",
    "base_mainnet",
    "base_sepolia",
    "monad_mainnet",
    "monad_testnet",
    "ape_mainnet",
    "ape_caldera",
    "matic",
    "sepolia",
    "amoy",
];
const ITEM_KEYS: &[&str] = &[
    "id",
    "urn",
    "name",
    "description",
    "thumbnail",
    "video",
    "eth_address",
    "collection_id",
    "blockchain_item_id",
    "price",
    "beneficiary",
    "rarity",
    "total_supply",
    "is_published",
    "is_approved",
    "created_at",
    "updated_at",
    "contents",
    "utility",
    "content_hash",
    "mappings",
    "type",
    "data",
    "metrics",
];
const ITEM_REQUIRED: &[&str] = &[
    "name",
    "description",
    "eth_address",
    "data",
    "type",
    "metrics",
    "contents",
];
const WEARABLE_DATA_KEYS: &[&str] = &[
    "category",
    "representations",
    "replaces",
    "hides",
    "removesDefaultHiding",
    "tags",
    "requiredPermissions",
    "blockVrmExport",
    "outlineCompatible",
    "springBones",
];
const EMOTE_DATA_KEYS: &[&str] = &[
    "category",
    "loop",
    "representations",
    "tags",
    "startAnimation",
    "randomizeOutcomes",
    "outcomes",
];
const WEARABLE_REPRESENTATION_KEYS: &[&str] = &[
    "bodyShapes",
    "mainFile",
    "contents",
    "overrideReplaces",
    "overrideHides",
];
const MODEL_METRICS: &[&str] = &[
    "meshes",
    "bodies",
    "materials",
    "textures",
    "triangles",
    "entities",
];
const ANIMATION_METRICS: &[&str] = &[
    "sequences",
    "duration",
    "frames",
    "fps",
    "props",
    "additionalArmatures",
];
const CLIPS: &[&str] = &[
    "Armature",
    "Armature_Prop",
    "Armature_Other",
    "[object Object]",
];

/// The kind of item a body saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemType {
    Wearable,
    Emote,
}

impl ItemType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wearable => "wearable",
            Self::Emote => "emote",
        }
    }
}

/// The `item` of a body the schema accepts, without the keys the save ignores.
#[derive(Debug, Clone)]
pub struct ItemInput {
    pub id: Option<String>,
    pub urn: Option<Option<String>>,
    pub name: String,
    pub description: Option<String>,
    pub thumbnail: Option<String>,
    pub video: Option<Option<String>>,
    pub collection_id: Option<Option<String>>,
    pub price: Option<Option<String>>,
    pub beneficiary: Option<Option<String>>,
    pub rarity: Option<Option<String>>,
    pub utility: Option<Option<String>>,
    pub mappings: Option<Option<JsJson>>,
    pub item_type: ItemType,
    pub data: JsJson,
    pub metrics: JsJson,
    pub contents: Vec<(String, String)>,
}

/// The body checked against builder-server's `upsertItemSchema`, every bound
/// and pattern as its validator applies them, keys the schema doesn't name
/// dropped where it closes an object, and `400 Invalid request body` for the
/// rest. Beyond the schema, the node refuses: a `contents` value that isn't a
/// CID, a `contents` key that isn't a relative file path (a directory entry,
/// a path with one trailing `/`, is kept), more than 256 `contents` entries,
/// `mappings` under a network the schema doesn't name or a contract key that
/// isn't an address (upstream drops them, leaving an empty object), and a body
/// upstream's validator throws on (a range bound that isn't an integer).
pub fn parse(body: &[u8]) -> Result<ItemInput, ApiError> {
    let root: JsJson = serde_json::from_slice(body).map_err(|_| refused())?;
    let JsJson::Object(top) = root else {
        return Err(refused());
    };
    let mut item = top
        .into_iter()
        .find_map(|(key, value)| (key == "item").then_some(value))
        .ok_or_else(refused)?;
    let kind = valid_item(&mut item).ok_or_else(refused)?;
    let JsJson::Object(fields) = item else {
        return Err(refused());
    };
    input(fields, kind).ok_or_else(refused)
}

impl ItemInput {
    /// builder-server's `sanitizeItemContents`: drops directory entries and
    /// empty files from `contents` and the files they name from each
    /// representation's `contents`.
    pub fn sanitize(&mut self) {
        self.contents
            .retain(|(file, hash)| !file.ends_with('/') && hash != EMPTY_CONTENT_HASH);
        let files: Vec<&str> = self.contents.iter().map(|(f, _)| f.as_str()).collect();
        let Some(representations) = field_mut(&mut self.data, "representations") else {
            return;
        };
        let JsJson::Array(representations) = representations else {
            return;
        };
        for representation in representations {
            if let Some(JsJson::Array(names)) = field_mut(representation, "contents") {
                names.retain(
                    |name| matches!(name, JsJson::String(n) if files.contains(&n.as_str())),
                );
            }
        }
    }
}

/// The CIDv1 of a 0-byte file.
pub const EMPTY_CONTENT_HASH: &str = "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku";

fn refused() -> ApiError {
    ApiError::bad_request("Invalid request body")
}

fn field_mut<'a>(value: &'a mut JsJson, key: &str) -> Option<&'a mut JsJson> {
    match value {
        JsJson::Object(fields) => fields.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn input(fields: Fields, item_type: ItemType) -> Option<ItemInput> {
    let take = |key: &str| {
        fields
            .iter()
            .position(|(k, _)| k == key)
            .map(|at| fields[at].1.clone())
    };
    let text = |v: JsJson| match v {
        JsJson::String(s) => Some(s),
        _ => None,
    };
    let nullable = |v: Option<JsJson>| v.map(text);
    let contents = match take("contents")? {
        JsJson::Object(entries) => js_order(entries)
            .into_iter()
            .map(|(file, hash)| Some((file, text(hash)?)))
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    Some(ItemInput {
        id: take("id").and_then(text),
        urn: nullable(take("urn")),
        name: text(take("name")?)?,
        description: text(take("description")?),
        thumbnail: take("thumbnail").and_then(text),
        video: nullable(take("video")),
        collection_id: nullable(take("collection_id")),
        price: nullable(take("price")),
        beneficiary: nullable(take("beneficiary")),
        rarity: nullable(take("rarity")),
        utility: nullable(take("utility")),
        mappings: take("mappings").map(|m| (!matches!(m, JsJson::Null)).then_some(m)),
        item_type,
        data: take("data")?,
        metrics: take("metrics")?,
        contents,
    })
}

fn array_index(key: &str) -> Option<u32> {
    let n: u32 = key.parse().ok()?;
    (n != u32::MAX && n.to_string() == key).then_some(n)
}

fn js_order(entries: Fields) -> Fields {
    let (mut indexed, rest): (Fields, Fields) = entries
        .into_iter()
        .partition(|(k, _)| array_index(k).is_some());
    indexed.sort_by_key(|(k, _)| array_index(k));
    indexed.into_iter().chain(rest).collect()
}

fn object(value: &mut JsJson) -> Option<&mut Fields> {
    match value {
        JsJson::Object(fields) => Some(fields),
        _ => None,
    }
}

fn has(fields: &Fields, key: &str) -> bool {
    fields.iter().any(|(k, _)| k == key)
}

fn closed(fields: &mut Fields, keys: &[&str], required: &[&str]) -> bool {
    fields.retain(|(k, _)| keys.contains(&k.as_str()));
    required.iter().all(|key| has(fields, key))
}

fn number(value: &JsJson) -> Option<f64> {
    match value {
        JsJson::Number(n) => Some(*n),
        JsJson::Signed(n) => Some(*n as f64),
        JsJson::Unsigned(n) => Some(*n as f64),
        _ => None,
    }
}

fn number_in(value: &JsJson, min: f64, max: f64) -> bool {
    number(value).is_some_and(|n| n >= min && n <= max)
}

fn boolean(value: &JsJson) -> bool {
    matches!(value, JsJson::Bool(_))
}

fn null_or(value: &JsJson, check: impl Fn(&JsJson) -> bool) -> bool {
    matches!(value, JsJson::Null) || check(value)
}

fn string(value: &JsJson, check: impl Fn(&str) -> bool) -> bool {
    matches!(value, JsJson::String(s) if check(s))
}

fn any_string(value: &JsJson) -> bool {
    string(value, |_| true)
}

fn plain(max: usize) -> impl Fn(&str) -> bool {
    move |s| s.chars().count() <= max && !s.contains(':')
}

fn one_of(list: &'static [&'static str]) -> impl Fn(&str) -> bool {
    move |s| list.contains(&s)
}

fn hideable(value: &JsJson) -> bool {
    string(value, |s| {
        WEARABLE_CATEGORIES.contains(&s) || s == "head" || s == "hands"
    })
}

fn array_of(value: &JsJson, check: impl Fn(&JsJson) -> bool) -> bool {
    matches!(value, JsJson::Array(items) if items.iter().all(check))
}

fn array_of_mut(
    value: &mut JsJson,
    min: usize,
    mut check: impl FnMut(&mut JsJson) -> bool,
) -> bool {
    match value {
        JsJson::Array(items) => items.len() >= min && items.iter_mut().all(&mut check),
        _ => false,
    }
}

fn hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn address(s: &str) -> bool {
    s.strip_prefix("0x").is_some_and(|rest| hex(rest, 40))
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn uuid(s: &str) -> bool {
    let s = match s.get(..9) {
        Some(prefix) if prefix.eq_ignore_ascii_case("urn:uuid:") => &s[9..],
        _ => s,
    };
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(part, len)| hex(part, len))
}

fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn slot(c: char) -> bool {
    c != ':' && c != '|' && !js_space(c)
}

fn urn(s: &str) -> bool {
    fn after_network(rest: &str) -> Option<&str> {
        URN_NETWORKS
            .iter()
            .find_map(|network| rest.strip_prefix(network)?.strip_prefix(':'))
    }
    let suffix = |rest: &str| {
        if let Some(rest) = rest.strip_prefix("collections-thirdparty:") {
            let first = rest.find(|c| !slot(c)).unwrap_or(rest.len());
            return first > 0
                && rest[first..]
                    .strip_prefix(':')
                    .and_then(|r| r.chars().next())
                    .is_some_and(slot);
        }
        rest.strip_prefix("collections-v2:0x")
            .and_then(|r| r.get(..40))
            .is_some_and(|r| hex(r, 40))
    };
    s.match_indices("urn:decentraland:")
        .any(|(at, prefix)| after_network(&s[at + prefix.len()..]).is_some_and(suffix))
}

fn content_key(key: &str) -> bool {
    crate::drafts::valid_filename(key.strip_suffix('/').unwrap_or(key))
}

fn valid_item(item: &mut JsJson) -> Option<ItemType> {
    let fields = object(item)?;
    let kind = match fields.iter().find(|(k, _)| k == "type").map(|(_, v)| v) {
        Some(JsJson::String(t)) if t == "wearable" => ItemType::Wearable,
        Some(JsJson::String(t)) if t == "emote" => ItemType::Emote,
        _ => return None,
    };
    let mut ok =
        closed(fields, ITEM_KEYS, ITEM_REQUIRED) && (has(fields, "id") || has(fields, "urn"));
    for (key, value) in fields.iter_mut() {
        ok &= match key.as_str() {
            "id" => string(value, uuid),
            "urn" => null_or(value, |v| string(v, urn)),
            "name" => string(value, plain(32)),
            "description" => null_or(value, |v| string(v, plain(64))),
            "thumbnail" => string(value, plain(1000)),
            "video" => null_or(value, |v| string(v, plain(1000))),
            "eth_address" | "created_at" | "updated_at" => any_string(value),
            "collection_id" => null_or(value, |v| string(v, uuid)),
            "blockchain_item_id" | "content_hash" => null_or(value, any_string),
            "price" => null_or(value, |v| {
                string(v, |s| {
                    s.len() <= 78 && s.bytes().all(|b| b.is_ascii_digit())
                })
            }),
            "beneficiary" => null_or(value, |v| string(v, |s| s.is_empty() || address(s))),
            "rarity" => null_or(value, |v| string(v, one_of(RARITIES))),
            "total_supply" => number(value).is_some_and(|n| n >= 0.0),
            "is_published" | "is_approved" => boolean(value),
            "contents" => contents(value),
            "utility" => null_or(value, |v| string(v, plain(64))),
            "mappings" => mappings(value),
            "data" => match kind {
                ItemType::Wearable => wearable_data(value),
                ItemType::Emote => emote_data(value),
            },
            "metrics" => match kind {
                ItemType::Wearable => metrics(value, MODEL_METRICS, MODEL_METRICS, 0.0),
                ItemType::Emote => metrics(value, ANIMATION_METRICS, &ANIMATION_METRICS[..4], 1.0),
            },
            _ => true,
        };
    }
    ok.then_some(kind)
}

fn contents(value: &JsJson) -> bool {
    match value {
        JsJson::Object(entries) => {
            entries.len() <= MAX_CONTENTS
                && entries
                    .iter()
                    .all(|(k, v)| content_key(k) && string(v, dcl_auth::is_canonical_cid))
        }
        _ => false,
    }
}

fn metrics(value: &mut JsJson, keys: &[&str], required: &[&str], min_sequences: f64) -> bool {
    let Some(fields) = object(value) else {
        return false;
    };
    closed(fields, keys, required)
        && fields.iter().all(|(k, v)| {
            let min = if k == "sequences" { min_sequences } else { 0.0 };
            number(v).is_some_and(|n| n >= min)
        })
}

fn representation(value: &mut JsJson, wearable: bool) -> bool {
    let Some(fields) = object(value) else {
        return false;
    };
    let keys = if wearable {
        WEARABLE_REPRESENTATION_KEYS
    } else {
        &WEARABLE_REPRESENTATION_KEYS[..3]
    };
    closed(fields, keys, keys)
        && fields.iter().all(|(k, v)| match k.as_str() {
            "bodyShapes" => array_of(v, |s| string(s, one_of(BODY_SHAPES))),
            "mainFile" => any_string(v),
            "contents" => array_of(v, any_string) && matches!(v, JsJson::Array(a) if !a.is_empty()),
            _ => array_of(v, hideable),
        })
}

fn wearable_data(value: &mut JsJson) -> bool {
    let Some(fields) = object(value) else {
        return false;
    };
    let mut ok = closed(
        fields,
        WEARABLE_DATA_KEYS,
        &["category", "representations", "replaces", "hides", "tags"],
    );
    for (key, value) in fields.iter_mut() {
        ok &= match key.as_str() {
            "category" => string(value, one_of(WEARABLE_CATEGORIES)),
            "representations" => array_of_mut(value, 1, |r| representation(r, true)),
            "replaces" | "hides" | "removesDefaultHiding" => array_of(value, hideable),
            "tags" | "requiredPermissions" => array_of(value, any_string),
            "blockVrmExport" | "outlineCompatible" => null_or(value, boolean),
            _ => matches!(value, JsJson::Null) || spring_bones(value),
        };
    }
    ok
}

fn spring_bones(value: &mut JsJson) -> bool {
    let Some(fields) = object(value) else {
        return false;
    };
    closed(fields, &["version", "models"], &["version", "models"])
        && fields.iter().all(|(k, v)| match k.as_str() {
            "version" => number(v) == Some(1.0),
            _ => match v {
                JsJson::Object(models) => models.iter().all(|(_, bones)| match bones {
                    JsJson::Object(bones) => bones.iter().all(|(_, bone)| spring_bone(bone)),
                    _ => false,
                }),
                _ => false,
            },
        })
}

fn spring_bone(value: &JsJson) -> bool {
    let JsJson::Object(fields) = value else {
        return false;
    };
    ["stiffness", "gravityPower", "gravityDir", "drag"]
        .iter()
        .all(|key| has(fields, key))
        && fields.iter().all(|(k, v)| match k.as_str() {
            "stiffness" => number_in(v, 0.0, 4.0),
            "gravityPower" => number_in(v, 0.0, 2.0),
            "drag" => number_in(v, 0.0, 1.0),
            "gravityDir" => matches!(v, JsJson::Array(a) if a.len() == 3 && a.iter().all(|n| number_in(n, -10.0, 10.0))),
            "center" => null_or(v, any_string),
            "isRoot" => null_or(v, boolean),
            _ => true,
        })
}

fn emote_data(value: &mut JsJson) -> bool {
    let Some(fields) = object(value) else {
        return false;
    };
    let mut ok = closed(
        fields,
        EMOTE_DATA_KEYS,
        &["category", "loop", "representations", "tags"],
    );
    for (key, value) in fields.iter_mut() {
        ok &= match key.as_str() {
            "category" => string(value, one_of(EMOTE_CATEGORIES)),
            "loop" => boolean(value),
            "representations" => array_of_mut(value, 1, |r| representation(r, false)),
            "tags" => array_of(value, any_string),
            "randomizeOutcomes" => null_or(value, boolean),
            "startAnimation" => matches!(value, JsJson::Null) || start_animation(value),
            _ => matches!(value, JsJson::Null) || array_of_mut(value, 0, outcome),
        };
    }
    ok
}

fn armature(value: &mut JsJson) -> bool {
    let Some(fields) = object(value) else {
        return false;
    };
    closed(fields, &["animation"], &["animation"])
        && fields.iter().all(|(_, v)| string(v, |s| !s.is_empty()))
}

fn start_animation(value: &mut JsJson) -> bool {
    let Some(fields) = object(value) else {
        return false;
    };
    let mut ok = closed(
        fields,
        &["loop", "Armature", "Armature_Prop", "audio"],
        &["loop", "Armature"],
    );
    for (key, value) in fields.iter_mut() {
        ok &= match key.as_str() {
            "loop" => boolean(value),
            "Armature" => armature(value),
            "Armature_Prop" => matches!(value, JsJson::Null) || armature(value),
            _ => null_or(value, any_string),
        };
    }
    ok
}

fn outcome(value: &mut JsJson) -> bool {
    let Some(fields) = object(value) else {
        return false;
    };
    let mut ok = closed(
        fields,
        &["title", "loop", "clips", "audio"],
        &["title", "loop", "clips"],
    );
    for (key, value) in fields.iter_mut() {
        ok &= match key.as_str() {
            "title" => string(value, |s| !s.is_empty()),
            "loop" => boolean(value),
            "audio" => null_or(value, any_string),
            _ => match object(value) {
                Some(clips) => {
                    let mut ok = !clips.is_empty();
                    for (clip, value) in clips.iter_mut() {
                        if CLIPS.contains(&clip.as_str()) {
                            ok &= matches!(value, JsJson::Null) || armature(value);
                        }
                    }
                    ok
                }
                None => false,
            },
        };
    }
    ok
}

#[derive(Debug)]
enum Mapping {
    Single(String),
    Any,
    Range(String, String),
    Multiple(Vec<String>),
}

fn mappings(value: &mut JsJson) -> bool {
    let JsJson::Object(networks) = value else {
        return matches!(value, JsJson::Null);
    };
    let [(network, contracts)] = networks.as_mut_slice() else {
        return false;
    };
    let JsJson::Object(contracts) = contracts else {
        return false;
    };
    let [(contract, list)] = contracts.as_mut_slice() else {
        return false;
    };
    if !MAPPING_NETWORKS.contains(&network.as_str()) || !address(contract) {
        return false;
    }
    let JsJson::Array(list) = list else {
        return false;
    };
    let Some(parsed) = list.iter_mut().map(mapping).collect::<Option<Vec<_>>>() else {
        return false;
    };
    parsed
        .iter()
        .enumerate()
        .all(|(at, m)| parsed[..at].iter().all(|earlier| !overlaps(earlier, m)))
}

fn mapping(value: &mut JsJson) -> Option<Mapping> {
    let fields = object(value)?;
    let kind = match fields.iter().find(|(k, _)| k == "type")?.1 {
        JsJson::String(ref t) => t.clone(),
        _ => return None,
    };
    let get = |fields: &Fields, key: &str| -> Option<String> {
        match &fields.iter().find(|(k, _)| k == key)?.1 {
            JsJson::String(s) if digits(s) => Some(s.clone()),
            _ => None,
        }
    };
    match kind.as_str() {
        "single" => {
            closed(fields, &["type", "id"], &["type", "id"]).then_some(())?;
            Some(Mapping::Single(get(fields, "id")?))
        }
        "any" => {
            closed(fields, &["type"], &["type"]).then_some(())?;
            Some(Mapping::Any)
        }
        "range" => {
            let bound = |key: &str| match &fields.iter().find(|(k, _)| k == key)?.1 {
                JsJson::String(s) if !s.is_empty() => Some(s.clone()),
                _ => None,
            };
            let (from, to) = (bound("from")?, bound("to")?);
            if !digits(&from) || !digits(&to) || decimal_cmp(&from, &to) == Ordering::Greater {
                return None;
            }
            closed(fields, &["type", "from", "to"], &["type", "from", "to"]).then_some(())?;
            Some(Mapping::Range(from, to))
        }
        "multiple" => {
            closed(fields, &["type", "ids"], &["type", "ids"]).then_some(())?;
            let JsJson::Array(ids) = &fields.iter().find(|(k, _)| k == "ids")?.1 else {
                return None;
            };
            let ids = ids
                .iter()
                .map(|id| match id {
                    JsJson::String(s) if digits(s) => Some(s.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let distinct = ids
                .iter()
                .enumerate()
                .all(|(at, id)| !ids[..at].contains(id));
            (!ids.is_empty() && distinct).then_some(Mapping::Multiple(ids))
        }
        _ => None,
    }
}

fn decimal_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn in_range(id: &str, from: &str, to: &str) -> bool {
    decimal_cmp(id, from) != Ordering::Less && decimal_cmp(id, to) != Ordering::Greater
}

fn overlaps(a: &Mapping, b: &Mapping) -> bool {
    use Mapping::*;
    match (a, b) {
        (Any, _) | (_, Any) => true,
        (Single(x), Single(y)) => x == y,
        (Single(x), Multiple(ids)) | (Multiple(ids), Single(x)) => ids.contains(x),
        (Single(x), Range(f, t)) | (Range(f, t), Single(x)) => in_range(x, f, t),
        (Multiple(xs), Multiple(ys)) => xs.iter().any(|x| ys.contains(x)),
        (Multiple(ids), Range(f, t)) | (Range(f, t), Multiple(ids)) => {
            ids.iter().any(|id| in_range(id, f, t))
        }
        (Range(f1, t1), Range(f2, t2)) => {
            decimal_cmp(f1, t2) != Ordering::Greater && decimal_cmp(t1, f2) != Ordering::Less
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::js_stringify;
    use serde_json::Value;

    fn fields(input: &ItemInput) -> String {
        let text = |s: &str| JsJson::String(s.to_string());
        let nullable = |v: &Option<String>| v.as_deref().map_or(JsJson::Null, text);
        let mut out = Vec::new();
        let mut put = |key: &str, value: JsJson| out.push((key.to_string(), value));
        if let Some(id) = &input.id {
            put("id", text(id));
        }
        if let Some(urn) = &input.urn {
            put("urn", nullable(urn));
        }
        put("name", text(&input.name));
        put("description", nullable(&input.description));
        if let Some(thumbnail) = &input.thumbnail {
            put("thumbnail", text(thumbnail));
        }
        for (key, value) in [
            ("video", &input.video),
            ("collection_id", &input.collection_id),
            ("price", &input.price),
            ("beneficiary", &input.beneficiary),
            ("rarity", &input.rarity),
            ("utility", &input.utility),
        ] {
            if let Some(value) = value {
                put(key, nullable(value));
            }
        }
        put("type", text(input.item_type.as_str()));
        js_stringify(&JsJson::Object(out))
    }

    fn differences(input: &ItemInput, cleaned: &Value) -> Vec<String> {
        let mut out = Vec::new();
        let mut same = |part: &str, node: String, upstream: Option<&str>| {
            if upstream != Some(node.as_str()) {
                out.push(format!("{part}: node {node}, upstream {upstream:?}"));
            }
        };
        same("data", js_stringify(&input.data), cleaned["data"].as_str());
        same(
            "metrics",
            js_stringify(&input.metrics),
            cleaned["metrics"].as_str(),
        );
        same("fields", fields(input), cleaned["fields"].as_str());
        let mappings = match &input.mappings {
            None => Value::Null,
            Some(None) => Value::String("null".into()),
            Some(Some(m)) => Value::String(js_stringify(m)),
        };
        if mappings != cleaned["mappings"] {
            out.push(format!(
                "mappings: node {mappings}, upstream {}",
                cleaned["mappings"]
            ));
        }
        let contents: Vec<(String, String)> =
            serde_json::from_value(cleaned["contents"].clone()).unwrap();
        if input.contents != contents {
            out.push(format!(
                "contents: node {:?}, upstream {contents:?}",
                input.contents
            ));
        }
        out
    }

    #[test]
    fn every_body_of_the_corpus_gets_upstreams_verdict_and_cleaned_item() {
        let corpus: Value = serde_json::from_str(include_str!("schema_corpus.json")).unwrap();
        let cases = corpus["cases"].as_array().unwrap();
        assert!(cases.len() > 200, "{}", cases.len());
        let mut wrong = Vec::new();
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let node_refuses = case["node"] == "refuses" || case["upstream"] == "throws";
            let accepted = case["valid"] == true && !node_refuses;
            match parse(case["raw"].as_str().unwrap().as_bytes()) {
                Ok(input) if accepted => {
                    for difference in differences(&input, &case["cleaned"]) {
                        wrong.push(format!("{name}: {difference}"));
                    }
                }
                Ok(_) => wrong.push(format!(
                    "{name}: accepted, upstream valid={}",
                    case["valid"]
                )),
                Err(e) if accepted => wrong.push(format!("{name}: refused ({e})")),
                Err(e) if e.status() != 400 => {
                    wrong.push(format!("{name}: answered {}", e.status()))
                }
                Err(_) => {}
            }
        }
        assert!(
            wrong.is_empty(),
            "{} cases differ:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }

    #[test]
    fn sanitizing_drops_directory_entries_and_empty_files_from_contents_and_representations() {
        let cid = |c: char| format!("bafkrei{}", c.to_string().repeat(52));
        let body = serde_json::json!({"item": {
            "id": "00000000-0000-4000-8000-0000000000a1", "name": "Hat", "description": null,
            "eth_address": "0x1111111111111111111111111111111111111111", "type": "wearable",
            "metrics": {"meshes": 1, "bodies": 1, "materials": 1, "textures": 1, "triangles": 1, "entities": 1},
            "contents": {"male/": cid('a'), "male/model.glb": cid('b'), "empty.png": EMPTY_CONTENT_HASH},
            "data": {"category": "hat", "replaces": [], "hides": [], "tags": [], "representations": [{
                "bodyShapes": [], "mainFile": "male/model.glb", "overrideReplaces": [], "overrideHides": [],
                "contents": ["male/", "male/model.glb", "empty.png", "elsewhere.png"]}]}
        }});
        let mut input = parse(body.to_string().as_bytes()).unwrap();
        input.sanitize();
        assert_eq!(
            input.contents,
            vec![("male/model.glb".to_string(), cid('b'))]
        );
        assert_eq!(
            js_stringify(&input.data),
            r#"{"category":"hat","hides":[],"replaces":[],"representations":[{"bodyShapes":[],"contents":["male/model.glb"],"mainFile":"male/model.glb","overrideHides":[],"overrideReplaces":[]}],"tags":[]}"#
        );
    }
}

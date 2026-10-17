//! Counted overview of a collection: the map a caller reads before moving.
//!
//! Pure and deterministic. Counts are exact over the value given (the
//! persisted, redacted payload), and every listed value or bound is a
//! verbatim source leaf (I1). Fields keep first-seen order, top-level fields
//! before nested ones, so the same payload always yields the same overview.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::hash::{Hash, Hasher};

use chrono::{DateTime, FixedOffset};
use serde_json::{Map, Value};

use crate::{CollectionOverview, FieldOverview, ValueCount, pointer};

/// Fields listed in an overview; the rest are counted in `fields_omitted`.
pub const OVERVIEW_MAX_FIELDS: usize = 24;
/// A field lists its values only when it has at most this many distinct ones.
pub const OVERVIEW_MAX_VALUES: usize = 8;
/// Strings longer than this are never listed as values.
pub const OVERVIEW_VALUE_MAX_CHARS: usize = 40;
/// Smallest array that counts as a collection.
pub const OVERVIEW_MIN_ITEMS: usize = 2;
/// Strings longer than this cannot serve as record keys.
pub const KEY_MAX_CHARS: usize = 128;
/// Key candidates reported per overview.
const MAX_KEY_CANDIDATES: usize = 4;
/// Field pointers descend at most this many object levels into an item.
const FIELD_DEPTH: usize = 2;
/// Object levels searched below the start path for a collection.
const SEARCH_DEPTH: usize = 4;
/// Key field names preferred over first-seen order, most specific first.
const KEY_PRIORITY: &[&str] = &[
    "id", "uid", "uuid", "key", "number", "node_id", "name", "slug",
];

/// Locate the collection an overview or record comparison should describe:
/// the array at `start` itself, or else the largest array of objects reachable
/// through object members below it. Arrays are never searched inside, so the
/// result is always addressable from `start` without an index.
pub fn find_collection(value: &Value, start: &str) -> Option<String> {
    let root = pointer::get(value, start).ok()??;
    if let Value::Array(items) = root {
        return (items.len() >= OVERVIEW_MIN_ITEMS).then(|| start.to_string());
    }
    let mut best: Option<(usize, String)> = None;
    let mut queue = VecDeque::from([(start.to_string(), root, 0usize)]);
    while let Some((path, node, depth)) = queue.pop_front() {
        let Value::Object(map) = node else {
            continue;
        };
        for (key, child) in map {
            let child_path = pointer::push(&path, key);
            match child {
                Value::Array(items) if is_record_collection(items) => {
                    if best.as_ref().is_none_or(|(len, _)| items.len() > *len) {
                        best = Some((items.len(), child_path));
                    }
                }
                Value::Object(_) if depth + 1 < SEARCH_DEPTH => {
                    queue.push_back((child_path, child, depth + 1));
                }
                _ => {}
            }
        }
    }
    best.map(|(_, path)| path)
}

fn is_record_collection(items: &[Value]) -> bool {
    items.len() >= OVERVIEW_MIN_ITEMS
        && items.iter().filter(|item| item.is_object()).count() * 2 >= items.len()
}

/// Count the fields and values of the array at `path`. Returns `None` when
/// `path` does not hold an array.
pub fn overview(value: &Value, path: &str, max_fields: usize) -> Option<CollectionOverview> {
    let Value::Array(items) = pointer::get(value, path).ok()?? else {
        return None;
    };
    let mut item_kinds = BTreeMap::new();
    let mut fields = FieldTable::default();
    for item in items {
        *item_kinds.entry(kind_name(item).to_string()).or_insert(0) += 1;
        if let Value::Object(map) = item {
            fields.observe_object(map, "", 1);
        }
    }
    let item_count = items.len() as u64;
    let object_items = item_kinds.get("object").copied().unwrap_or(0);
    let key_candidates = fields.key_candidates(item_count, object_items);
    let ordered = fields.ordered();
    let fields_omitted = ordered.len().saturating_sub(max_fields) as u64;
    Some(CollectionOverview {
        path: path.to_string(),
        items: item_count,
        item_kinds,
        fields: ordered
            .into_iter()
            .take(max_fields)
            .map(|(field, stats)| stats.into_overview(field))
            .collect(),
        fields_omitted,
        key_candidates,
    })
}

/// Scalar key fields: present, unredacted, and unique in every item of each
/// collection, in priority order. Exact, so it can gate record matching.
pub fn key_candidates(collections: &[&[Value]]) -> Vec<String> {
    let mut candidates: Option<Vec<String>> = None;
    for items in collections {
        let mut table = FieldTable::default();
        let mut objects = 0u64;
        for item in *items {
            if let Value::Object(map) = item {
                objects += 1;
                table.observe_object(map, "", 1);
            }
        }
        let side = table.key_candidates(items.len() as u64, objects);
        candidates = Some(match candidates {
            None => side,
            Some(previous) => previous
                .into_iter()
                .filter(|field| side.contains(field))
                .collect(),
        });
    }
    candidates.unwrap_or_default()
}

#[derive(Default)]
struct FieldTable {
    order: Vec<String>,
    stats: BTreeMap<String, FieldStats>,
}

impl FieldTable {
    fn observe_object(&mut self, map: &Map<String, Value>, prefix: &str, depth: usize) {
        for (key, value) in map {
            let field = pointer::push(prefix, key);
            if !self.stats.contains_key(&field) {
                self.order.push(field.clone());
                self.stats.insert(field.clone(), FieldStats::new(depth));
            }
            self.stats
                .get_mut(&field)
                .expect("field stats were just inserted")
                .observe(value);
            if depth < FIELD_DEPTH
                && let Value::Object(child) = value
            {
                self.observe_object(child, &field, depth + 1);
            }
        }
    }

    fn ordered(mut self) -> Vec<(String, FieldStats)> {
        let mut fields = self
            .order
            .iter()
            .map(|field| {
                let stats = self.stats.remove(field).expect("ordered field has stats");
                (field.clone(), stats)
            })
            .collect::<Vec<_>>();
        // Stable: top-level fields first, each level in first-seen order.
        fields.sort_by_key(|(_, stats)| stats.depth);
        fields
    }

    fn key_candidates(&self, items: u64, object_items: u64) -> Vec<String> {
        if items == 0 || object_items != items {
            return Vec::new();
        }
        let mut candidates = self
            .order
            .iter()
            .filter(|field| {
                self.stats[*field]
                    .unique
                    .as_ref()
                    .is_some_and(|seen| seen.len() as u64 == items)
                    && self.stats[*field].present == items
            })
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by_key(|field| {
            let depth = self.stats[field].depth;
            let leaf = field.rsplit('/').next().unwrap_or_default();
            let priority = KEY_PRIORITY
                .iter()
                .position(|name| *name == leaf)
                .unwrap_or(KEY_PRIORITY.len());
            (depth, priority)
        });
        candidates.truncate(MAX_KEY_CANDIDATES);
        candidates
    }
}

struct FieldStats {
    depth: usize,
    kinds: BTreeSet<&'static str>,
    present: u64,
    /// Distinct short scalars with counts; `None` once the field stops being
    /// enumerable.
    values: Option<Vec<(Value, u64)>>,
    /// Hashes of key-shaped values; `None` once a value repeats or cannot be
    /// a key. A hash collision only drops a candidate, never adds one.
    unique: Option<HashSet<u64>>,
    bounds: Bounds,
}

impl FieldStats {
    fn new(depth: usize) -> Self {
        Self {
            depth,
            kinds: BTreeSet::new(),
            present: 0,
            values: Some(Vec::new()),
            unique: Some(HashSet::new()),
            bounds: Bounds::Unset,
        }
    }

    fn observe(&mut self, value: &Value) {
        self.present += 1;
        self.kinds.insert(kind_name(value));
        if let Some(values) = &mut self.values {
            if enumerable(value) {
                if let Some((_, count)) = values.iter_mut().find(|(seen, _)| seen == value) {
                    *count += 1;
                } else if values.len() < OVERVIEW_MAX_VALUES {
                    values.push((value.clone(), 1));
                } else {
                    self.values = None;
                }
            } else {
                self.values = None;
            }
        }
        if let Some(unique) = &mut self.unique {
            match key_hash(value) {
                Some(hash) if unique.insert(hash) => {}
                _ => self.unique = None,
            }
        }
        self.bounds.observe(value);
    }

    fn into_overview(self, field: String) -> FieldOverview {
        let mut values = self.values.unwrap_or_default();
        // Most frequent first; equal counts keep first-seen order.
        values.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        let (min, max) = if values.is_empty() {
            self.bounds.into_values()
        } else {
            (None, None)
        };
        FieldOverview {
            field,
            kinds: self.kinds.into_iter().map(str::to_string).collect(),
            present: self.present,
            values: values
                .into_iter()
                .map(|(value, count)| ValueCount { value, count })
                .collect(),
            min,
            max,
        }
    }
}

/// Smallest and largest leaf of a field whose non-null values are all numbers
/// or all RFC 3339 timestamps. Any other value disables bounds.
enum Bounds {
    Unset,
    Numbers {
        min: Value,
        max: Value,
    },
    Timestamps {
        min: (DateTime<FixedOffset>, Value),
        max: (DateTime<FixedOffset>, Value),
    },
    Mixed,
}

impl Bounds {
    fn observe(&mut self, value: &Value) {
        if value.is_null() {
            return;
        }
        let next = match (std::mem::replace(self, Self::Mixed), value) {
            (Self::Mixed, _) => Self::Mixed,
            (Self::Unset, Value::Number(_)) => Self::Numbers {
                min: value.clone(),
                max: value.clone(),
            },
            (Self::Numbers { min, max }, Value::Number(number)) => {
                let current = number.as_f64().unwrap_or(f64::NAN);
                let below = current < min.as_f64().unwrap_or(f64::NAN);
                let above = current > max.as_f64().unwrap_or(f64::NAN);
                Self::Numbers {
                    min: if below { value.clone() } else { min },
                    max: if above { value.clone() } else { max },
                }
            }
            (state @ (Self::Unset | Self::Timestamps { .. }), Value::String(text)) => {
                match DateTime::parse_from_rfc3339(text) {
                    Ok(time) => match state {
                        Self::Timestamps { min, max } => Self::Timestamps {
                            min: if time < min.0 {
                                (time, value.clone())
                            } else {
                                min
                            },
                            max: if time > max.0 {
                                (time, value.clone())
                            } else {
                                max
                            },
                        },
                        _ => Self::Timestamps {
                            min: (time, value.clone()),
                            max: (time, value.clone()),
                        },
                    },
                    Err(_) => Self::Mixed,
                }
            }
            _ => Self::Mixed,
        };
        *self = next;
    }

    fn into_values(self) -> (Option<Value>, Option<Value>) {
        match self {
            Self::Numbers { min, max } => (Some(min), Some(max)),
            Self::Timestamps { min, max } => (Some(min.1), Some(max.1)),
            Self::Unset | Self::Mixed => (None, None),
        }
    }
}

/// Short scalars are listed with counts. Timestamps are not: they are nearly
/// always distinct, and their range (`min`/`max`) says more.
fn enumerable(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(text) => {
            text.chars().count() <= OVERVIEW_VALUE_MAX_CHARS
                && DateTime::parse_from_rfc3339(text).is_err()
        }
        Value::Array(_) | Value::Object(_) => false,
    }
}

/// Hash of a value that can address a record: an integer or a short,
/// unredacted string. Integers and strings hash apart (`1` is not `"1"`).
fn key_hash(value: &Value) -> Option<u64> {
    let mut hasher = DefaultHasher::new();
    match value {
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            0u8.hash(&mut hasher);
            number.to_string().hash(&mut hasher);
        }
        Value::String(text) if is_key_string(text) => {
            1u8.hash(&mut hasher);
            text.hash(&mut hasher);
        }
        _ => return None,
    }
    Some(hasher.finish())
}

/// A string usable as a record key: non-empty, short, and not a redaction
/// marker (two redacted keys would compare equal while hiding real values).
pub fn is_key_string(text: &str) -> bool {
    !text.is_empty() && text.chars().count() <= KEY_MAX_CHARS && !text.starts_with("[REDACTED")
}

pub(crate) fn kind_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn issues() -> Value {
        json!([
            {"id": 11, "number": 1, "state": "open", "title": "a", "user": {"login": "x"}, "created_at": "2026-09-02T10:00:00Z"},
            {"id": 12, "number": 2, "state": "closed", "title": "b", "user": {"login": "y"}, "created_at": "2026-09-01T10:00:00Z"},
            {"id": 13, "number": 3, "state": "open", "title": "c", "user": {"login": "x"}, "created_at": "2026-09-03T10:00:00Z", "extra": null}
        ])
    }

    fn field<'a>(overview: &'a CollectionOverview, name: &str) -> &'a FieldOverview {
        overview
            .fields
            .iter()
            .find(|field| field.field == name)
            .unwrap_or_else(|| panic!("missing field {name}"))
    }

    #[test]
    fn overview_counts_values_presence_and_bounds_exactly() {
        let overview = overview(&issues(), "", OVERVIEW_MAX_FIELDS).expect("array");
        assert_eq!(overview.items, 3);
        assert_eq!(overview.item_kinds["object"], 3);
        let state = field(&overview, "/state");
        assert_eq!(state.present, 3);
        assert_eq!(
            state.values,
            vec![
                ValueCount {
                    value: json!("open"),
                    count: 2
                },
                ValueCount {
                    value: json!("closed"),
                    count: 1
                },
            ]
        );
        let created = field(&overview, "/created_at");
        assert_eq!(created.min, Some(json!("2026-09-01T10:00:00Z")));
        assert_eq!(created.max, Some(json!("2026-09-03T10:00:00Z")));
        assert_eq!(field(&overview, "/extra").present, 1);
        assert_eq!(field(&overview, "/user/login").values.len(), 2);
        // Top-level fields precede nested ones.
        let last_top = overview
            .fields
            .iter()
            .rposition(|field| field.field.matches('/').count() == 1);
        let first_nested = overview
            .fields
            .iter()
            .position(|field| field.field.matches('/').count() == 2);
        assert!(last_top < first_nested);
    }

    #[test]
    fn key_candidates_prefer_known_names_and_require_uniqueness() {
        let overview = overview(&issues(), "", OVERVIEW_MAX_FIELDS).expect("array");
        assert_eq!(
            overview.key_candidates[..2],
            ["/id".to_string(), "/number".to_string()]
        );
        assert!(!overview.key_candidates.contains(&"/state".to_string()));
        assert!(!overview.key_candidates.contains(&"/user/login".to_string()));
    }

    #[test]
    fn redacted_or_long_values_never_become_keys_or_values() {
        let value = json!([
            {"id": "[REDACTED:secret_field]", "body": "x".repeat(KEY_MAX_CHARS + 1)},
            {"id": "[REDACTED:secret_field]", "body": "y".repeat(KEY_MAX_CHARS + 1)}
        ]);
        let overview = overview(&value, "", OVERVIEW_MAX_FIELDS).expect("array");
        assert!(overview.key_candidates.is_empty());
        assert!(field(&overview, "/body").values.is_empty());
    }

    #[test]
    fn find_collection_prefers_the_largest_record_array() {
        let value = json!({"meta": {"count": 2}, "data": {"items": [{"a": 1}, {"a": 2}, {"a": 3}]}, "tags": [{"t": 1}, {"t": 2}]});
        assert_eq!(find_collection(&value, ""), Some("/data/items".to_string()));
        assert_eq!(find_collection(&value, "/tags"), Some("/tags".to_string()));
        assert_eq!(find_collection(&json!({"a": 1}), ""), None);
        assert_eq!(find_collection(&value, "/missing"), None);
    }

    #[test]
    fn field_cap_reports_the_omitted_count() {
        let item = (0..30)
            .map(|index| (format!("f{index:02}"), json!(index)))
            .collect::<Map<_, _>>();
        let value = Value::Array(vec![Value::Object(item.clone()), Value::Object(item)]);
        let overview = overview(&value, "", 10).expect("array");
        assert_eq!(overview.fields.len(), 10);
        assert_eq!(overview.fields_omitted, 20);
        assert_eq!(overview.fields[0].field, "/f00");
    }

    #[test]
    fn overview_is_deterministic() {
        let first = overview(&issues(), "", OVERVIEW_MAX_FIELDS);
        for _ in 0..5 {
            assert_eq!(overview(&issues(), "", OVERVIEW_MAX_FIELDS), first);
        }
    }

    #[test]
    fn key_candidates_must_hold_on_every_side() {
        let left = vec![json!({"id": 1, "name": "a"}), json!({"id": 2, "name": "b"})];
        let right = vec![json!({"id": 1, "name": "a"}), json!({"id": 3, "name": "a"})];
        assert_eq!(key_candidates(&[&left, &right]), vec!["/id".to_string()]);
    }
}

//! Filtered, projected, paged rows of one collection, each with its address.
//!
//! Pure and deterministic. Rows keep source order and carry the exact JSON
//! Pointer of the item they came from, so a later `expand` or `evidence` can
//! return to it. Row values are projections of the source item: selected
//! leaves keep their source paths, and long strings become labelled prefixes
//! (I1). An item whose filter field is redacted cannot be tested; it is
//! counted as unknown and never reported as matching or not matching.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::{CoreError, OmittedRegion, PreviewPolicy, Result, SelectRow, pointer, project};

/// How a filter compares the field with its operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    /// `field=value`: the scalar texts are equal (`17` matches `"17"`).
    Equals,
    /// `field!=value`: not equal; a missing field is not equal.
    NotEquals,
    /// `field~text`: case-insensitive substring of the value's text.
    Contains,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectFilter {
    /// JSON Pointer relative to each item.
    pub field: String,
    pub op: FilterOp,
    pub operand: String,
}

impl SelectFilter {
    /// Parse `field=value`, `field!=value`, or `field~text`. The field may be
    /// written with or without a leading `/`; a quoted operand is a JSON
    /// string, so `name="a=b"` is unambiguous.
    pub fn parse(text: &str) -> Result<Self> {
        let bad = |reason: &str| CoreError::BadArgs {
            operation: "select --where".to_string(),
            reason: format!("{reason}: {text:?} (use field=value, field!=value, or field~text)"),
        };
        let position = text
            .char_indices()
            .find(|(_, ch)| matches!(ch, '=' | '~' | '!'))
            .map(|(index, _)| index)
            .ok_or_else(|| bad("missing operator"))?;
        let (field, rest) = text.split_at(position);
        let (op, operand) = if let Some(operand) = rest.strip_prefix("!=") {
            (FilterOp::NotEquals, operand)
        } else if let Some(operand) = rest.strip_prefix('=') {
            (FilterOp::Equals, operand)
        } else if let Some(operand) = rest.strip_prefix('~') {
            (FilterOp::Contains, operand)
        } else {
            return Err(bad("unknown operator"));
        };
        let field = field.trim();
        if field.is_empty() {
            return Err(bad("missing field"));
        }
        let operand = match serde_json::from_str::<Value>(operand) {
            Ok(Value::String(text)) => text,
            _ => operand.to_string(),
        };
        Ok(Self {
            field: normalize_field(field),
            op,
            operand,
        })
    }

    fn test(&self, item: &Value) -> Option<bool> {
        let value = pointer::get(item, &self.field).ok().flatten();
        if value.is_some_and(is_redacted) {
            return None;
        }
        let text = value.map(scalar_text);
        Some(match self.op {
            FilterOp::Equals => text.as_deref() == Some(self.operand.as_str()),
            FilterOp::NotEquals => text.as_deref() != Some(self.operand.as_str()),
            FilterOp::Contains => {
                text.is_some_and(|text| text.to_lowercase().contains(&self.operand.to_lowercase()))
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct SelectRequest<'a> {
    pub filters: &'a [SelectFilter],
    /// Field pointers relative to each item; empty keeps the whole item.
    pub fields: &'a [String],
    pub offset: usize,
    pub limit: usize,
    /// Characters kept from each string before a labelled cut.
    pub string_chars: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectResult {
    pub total: u64,
    pub matched: u64,
    pub unknown: u64,
    pub rows: Vec<SelectRow>,
    pub omitted: Vec<OmittedRegion>,
    pub next_offset: Option<u64>,
    /// Item positions (array index or member key) of the returned rows.
    pub returned: Vec<String>,
}

/// Select rows from the array or object at `path` within `value`.
pub fn select_rows(value: &Value, path: &str, request: &SelectRequest<'_>) -> Result<SelectResult> {
    let collection = pointer::get(value, path)?.ok_or_else(|| CoreError::PathNotFound {
        path: path.to_string(),
        hint: pointer::siblings_hint(value, path),
    })?;
    let items: Vec<(String, &Value)> = match collection {
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| (index.to_string(), item))
            .collect(),
        Value::Object(map) => map.iter().map(|(key, item)| (key.clone(), item)).collect(),
        _ => {
            return Err(CoreError::BadArgs {
                operation: "select".to_string(),
                reason: format!("{path} is not an array or object"),
            });
        }
    };
    let policy = PreviewPolicy {
        array_items: 5,
        object_fields: 32,
        string_chars: request.string_chars,
        depth: 4,
        node_budget: 400,
        max_envelope_bytes: usize::MAX,
    };
    let mut matched = 0u64;
    let mut unknown = 0u64;
    let mut rows = Vec::new();
    let mut omitted = Vec::new();
    let mut returned = Vec::new();
    let mut next_offset = None;
    for (segment, item) in &items {
        let mut verdict = Some(true);
        for filter in request.filters {
            match filter.test(item) {
                Some(true) => {}
                Some(false) => {
                    verdict = Some(false);
                    break;
                }
                None => verdict = None,
            }
        }
        match verdict {
            Some(true) => {}
            Some(false) => continue,
            None => {
                unknown += 1;
                continue;
            }
        }
        let position = matched as usize;
        matched += 1;
        if position < request.offset {
            continue;
        }
        if rows.len() >= request.limit {
            if next_offset.is_none() {
                next_offset = Some(position as u64);
            }
            continue;
        }
        let item_path = pointer::push(path, segment);
        let selected = if request.fields.is_empty() {
            (*item).clone()
        } else {
            select_fields(item, request.fields)
        };
        let projection = project(&selected, &policy, &item_path);
        omitted.extend(projection.omitted);
        rows.push(SelectRow {
            path: item_path,
            value: projection.preview,
        });
        returned.push(segment.clone());
    }
    Ok(SelectResult {
        total: items.len() as u64,
        matched,
        unknown,
        rows,
        omitted,
        next_offset,
        returned,
    })
}

/// Encode item indices as inclusive ranges: `0-4,16,41-42`.
pub fn index_ranges(indices: &BTreeSet<u64>) -> String {
    let mut ranges = Vec::new();
    let mut iter = indices.iter().copied();
    let Some(mut start) = iter.next() else {
        return String::new();
    };
    let mut end = start;
    for index in iter {
        if index == end + 1 {
            end = index;
            continue;
        }
        ranges.push(range_text(start, end));
        start = index;
        end = index;
    }
    ranges.push(range_text(start, end));
    ranges.join(",")
}

/// Decode [`index_ranges`] output. Malformed parts are ignored, so a damaged
/// record can only under-count returned items, never over-count them.
pub fn parse_index_ranges(text: &str) -> BTreeSet<u64> {
    let mut indices = BTreeSet::new();
    for part in text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let parsed = match part.split_once('-') {
            Some((start, end)) => start.parse::<u64>().ok().zip(end.parse::<u64>().ok()),
            None => part.parse::<u64>().ok().map(|index| (index, index)),
        };
        if let Some((start, end)) = parsed
            && start <= end
            && end - start < 1_000_000
        {
            indices.extend(start..=end);
        }
    }
    indices
}

/// Inclusive ranges of `0..total` absent from `present`.
pub fn missing_index_ranges(total: u64, present: &BTreeSet<u64>) -> Vec<String> {
    let missing = (0..total)
        .filter(|index| !present.contains(index))
        .collect::<BTreeSet<_>>();
    let text = index_ranges(&missing);
    if text.is_empty() {
        Vec::new()
    } else {
        text.split(',').map(str::to_string).collect()
    }
}

fn range_text(start: u64, end: u64) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

/// Normalize a field to a JSON Pointer relative to an item.
pub fn normalize_field(field: &str) -> String {
    let field = field.trim();
    if field.is_empty() || field.starts_with('/') {
        field.to_string()
    } else {
        format!("/{field}")
    }
}

/// Copy only the named leaves, keeping each at its source pointer so the row
/// stays addressable with the same paths as the item.
fn select_fields(item: &Value, fields: &[String]) -> Value {
    if !item.is_object() {
        return item.clone();
    }
    let mut out = Value::Object(Map::new());
    let mut seen = BTreeSet::new();
    for field in fields {
        let field = normalize_field(field);
        if !seen.insert(field.clone()) {
            continue;
        }
        let Ok(segments) = pointer::parse(&field) else {
            continue;
        };
        if segments.is_empty() {
            return item.clone();
        }
        if let Ok(Some(leaf)) = pointer::get(item, &field) {
            insert_at(&mut out, &segments, leaf.clone());
        }
    }
    out
}

fn insert_at(target: &mut Value, segments: &[String], leaf: Value) {
    let Some((first, rest)) = segments.split_first() else {
        return;
    };
    let Value::Object(map) = target else {
        return;
    };
    if rest.is_empty() {
        map.insert(first.clone(), leaf);
        return;
    }
    let child = map
        .entry(first.clone())
        .or_insert_with(|| Value::Object(Map::new()));
    insert_at(child, rest, leaf);
}

fn is_redacted(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.starts_with("[REDACTED"))
}

/// The text a filter compares: strings verbatim, other scalars as JSON, and
/// containers as compact JSON (so `labels~bug` finds a label named "bug").
fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn items() -> Value {
        json!({"data": [
            {"number": 1, "state": "open", "title": "timeout in sync", "user": {"login": "a", "id": 9}},
            {"number": 2, "state": "closed", "title": "crash", "user": {"login": "b", "id": 8}},
            {"number": 3, "state": "open", "title": "x".repeat(300), "user": {"login": "a", "id": 7}},
            {"number": 4, "state": "[REDACTED:secret_field]", "title": "hidden", "user": {"login": "c", "id": 6}}
        ]})
    }

    fn request<'a>(filters: &'a [SelectFilter], fields: &'a [String]) -> SelectRequest<'a> {
        SelectRequest {
            filters,
            fields,
            offset: 0,
            limit: 10,
            string_chars: 40,
        }
    }

    #[test]
    fn parses_operators_fields_and_json_operands() {
        assert_eq!(
            SelectFilter::parse("state=open").unwrap(),
            SelectFilter {
                field: "/state".to_string(),
                op: FilterOp::Equals,
                operand: "open".to_string()
            }
        );
        assert_eq!(
            SelectFilter::parse("/user/login!=a").unwrap().op,
            FilterOp::NotEquals
        );
        assert_eq!(
            SelectFilter::parse("title~Sync").unwrap().op,
            FilterOp::Contains
        );
        assert_eq!(SelectFilter::parse("name=\"a=b\"").unwrap().operand, "a=b");
        assert!(SelectFilter::parse("state").is_err());
        assert!(SelectFilter::parse("=open").is_err());
    }

    #[test]
    fn filters_keep_addresses_and_count_redacted_fields_as_unknown() {
        let filters = [SelectFilter::parse("state=open").unwrap()];
        let fields = ["number".to_string(), "user/login".to_string()];
        let result = select_rows(&items(), "/data", &request(&filters, &fields)).unwrap();
        assert_eq!((result.total, result.matched, result.unknown), (4, 2, 1));
        assert_eq!(result.rows[0].path, "/data/0");
        assert_eq!(
            result.rows[0].value,
            json!({"number": 1, "user": {"login": "a"}})
        );
        assert_eq!(result.rows[1].path, "/data/2");
        assert_eq!(result.returned, ["0", "2"]);

        let not_open = [SelectFilter::parse("state!=open").unwrap()];
        let result = select_rows(&items(), "/data", &request(&not_open, &[])).unwrap();
        assert_eq!((result.matched, result.unknown), (1, 1));
    }

    #[test]
    fn numbers_match_their_text_and_contains_is_case_insensitive() {
        let filters = [SelectFilter::parse("number=2").unwrap()];
        assert_eq!(
            select_rows(&items(), "/data", &request(&filters, &[]))
                .unwrap()
                .matched,
            1
        );
        let filters = [SelectFilter::parse("title~SYNC").unwrap()];
        assert_eq!(
            select_rows(&items(), "/data", &request(&filters, &[]))
                .unwrap()
                .matched,
            1
        );
        let filters = [SelectFilter::parse("user~\"login\":\"a\"").unwrap()];
        assert_eq!(
            select_rows(&items(), "/data", &request(&filters, &[]))
                .unwrap()
                .matched,
            2
        );
    }

    #[test]
    fn paging_reports_the_next_offset_and_cuts_long_strings_with_a_label() {
        let mut page = request(&[], &[]);
        page.limit = 2;
        let first = select_rows(&items(), "/data", &page).unwrap();
        assert_eq!(first.rows.len(), 2);
        assert_eq!(first.next_offset, Some(2));
        page.offset = 2;
        let second = select_rows(&items(), "/data", &page).unwrap();
        assert_eq!(second.rows[0].path, "/data/2");
        assert_eq!(second.next_offset, None);
        assert!(
            second
                .omitted
                .iter()
                .any(|region| region.path == "/data/2/title")
        );
        let title = second.rows[0].value["title"].as_str().unwrap();
        assert!(title.ends_with('…') && title.chars().count() == 41);
    }

    #[test]
    fn index_ranges_round_trip_and_report_gaps() {
        let indices = [0, 1, 2, 3, 4, 16, 41, 42]
            .into_iter()
            .collect::<BTreeSet<u64>>();
        assert_eq!(index_ranges(&indices), "0-4,16,41-42");
        assert_eq!(parse_index_ranges("0-4,16,41-42"), indices);
        assert_eq!(parse_index_ranges("x,3-1,7"), [7].into_iter().collect());
        assert_eq!(
            missing_index_ranges(6, &[0, 2, 3].into_iter().collect()),
            ["1", "4-5"]
        );
        assert!(missing_index_ranges(2, &[0, 1].into_iter().collect()).is_empty());
    }

    #[test]
    fn objects_are_collections_keyed_by_member() {
        let value = json!({"svc": {"a": {"up": true}, "b": {"up": false}}});
        let filters = [SelectFilter::parse("up=false").unwrap()];
        let result = select_rows(&value, "/svc", &request(&filters, &[])).unwrap();
        assert_eq!(result.rows[0].path, "/svc/b");
        assert!(select_rows(&json!({"n": 1}), "/n", &request(&[], &[])).is_err());
    }
}

//! Keyed record comparison between two captures of the same collection.
//!
//! Pure and deterministic. A record is matched by one scalar key field that is
//! present, unredacted, and unique on both sides. The comparison runs over the
//! full persisted payloads, never a preview, so its counts are exact for the
//! captured collections. It refuses to run when either capture stopped early:
//! a record missing from a truncated capture proves nothing.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    CaptureCompleteness, CaptureStopReason, FieldChange, RecordChange, RecordCounts, RecordDelta,
    RecordDeltaStatus, RecordRef, overview, pointer,
};

/// Characters of a changed string shown before a labelled cut.
const CHANGE_VALUE_MAX_CHARS: usize = 80;

/// One side of a record comparison: a persisted payload plus the facts that
/// decide whether its collection can be trusted as complete.
#[derive(Debug, Clone, Copy)]
pub struct RecordSide<'a> {
    pub value: &'a Value,
    /// Where to look for a collection when no path is given.
    pub search_root: &'a str,
    pub capture: &'a CaptureCompleteness,
    /// The caller declared this capture's selection exhaustive.
    pub selection_exhaustive: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct RecordDeltaOptions<'a> {
    /// Collection pointer; inferred from the subject when absent.
    pub path: Option<&'a str>,
    /// Key field pointer relative to each record; inferred when absent.
    pub key: Option<&'a str>,
    /// Entries kept in each of `added`, `removed`, and `changed`.
    pub list_limit: usize,
    /// Changed leaves listed per record.
    pub field_limit: usize,
}

impl Default for RecordDeltaOptions<'_> {
    fn default() -> Self {
        Self {
            path: None,
            key: None,
            list_limit: 20,
            field_limit: 8,
        }
    }
}

/// Compare the records of one collection in two payloads. Returns `None` when
/// nothing was requested and neither side holds a collection, so automatic
/// callers add no noise; an explicit `path` or `key` always gets an answer.
pub fn compare_records(
    baseline: RecordSide<'_>,
    subject: RecordSide<'_>,
    options: &RecordDeltaOptions<'_>,
) -> Option<RecordDelta> {
    let explicit = options.path.is_some() || options.key.is_some();
    let path = match options.path {
        Some(path) => path.to_string(),
        None => match overview::find_collection(subject.value, subject.search_root)
            .or_else(|| overview::find_collection(baseline.value, baseline.search_root))
        {
            Some(path) => path,
            None if explicit => {
                return Some(unavailable(
                    RecordDeltaStatus::NoCollection,
                    None,
                    vec!["neither payload holds a collection of records".to_string()],
                ));
            }
            None => return None,
        },
    };
    let (Some(baseline_items), Some(subject_items)) = (
        array_at(baseline.value, &path),
        array_at(subject.value, &path),
    ) else {
        let reason = format!("{path} is not a collection in both payloads");
        return explicit.then(|| {
            unavailable(
                RecordDeltaStatus::NoCollection,
                Some(path.clone()),
                vec![reason],
            )
        });
    };

    let mut incomplete = Vec::new();
    capture_gaps("baseline", baseline.capture, &mut incomplete);
    capture_gaps("subject", subject.capture, &mut incomplete);
    if !incomplete.is_empty() {
        let mut delta = unavailable(RecordDeltaStatus::Incomplete, Some(path), incomplete);
        delta.baseline_items = baseline_items.len() as u64;
        delta.subject_items = subject_items.len() as u64;
        return Some(delta);
    }

    let (key, key_declared) = match options.key {
        Some(key) => (normalize_field(key), true),
        None => match overview::key_candidates(&[baseline_items, subject_items])
            .into_iter()
            .next()
        {
            Some(key) => (key, false),
            None if explicit => {
                let mut delta = unavailable(
                    RecordDeltaStatus::Unkeyed,
                    Some(path),
                    vec![
                        "no field is present, unredacted, and unique in every record on both sides; pass --key"
                            .to_string(),
                    ],
                );
                delta.baseline_items = baseline_items.len() as u64;
                delta.subject_items = subject_items.len() as u64;
                return Some(delta);
            }
            None => return None,
        },
    };

    let mut key_problems = Vec::new();
    let baseline_index = index_by_key(baseline_items, &key, "baseline", &mut key_problems);
    let subject_index = index_by_key(subject_items, &key, "subject", &mut key_problems);
    if !key_problems.is_empty() {
        let mut delta = unavailable(RecordDeltaStatus::Unkeyed, Some(path), key_problems);
        delta.key = Some(key);
        delta.key_declared = key_declared;
        delta.baseline_items = baseline_items.len() as u64;
        delta.subject_items = subject_items.len() as u64;
        return Some(delta);
    }

    let mut counts = RecordCounts::default();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut redacted_records = 0u64;
    for (index, item) in subject_items.iter().enumerate() {
        let key_text = canonical_key(item, &key).expect("indexed records have keys");
        match baseline_index.get(&key_text) {
            None => {
                counts.added += 1;
                push_capped(&mut added, options.list_limit, || RecordRef {
                    key: key_value(item, &key),
                    path: pointer::push(&path, &index.to_string()),
                });
            }
            Some(&baseline_position) => {
                let before = &baseline_items[baseline_position];
                if contains_redaction(before) || contains_redaction(item) {
                    redacted_records += 1;
                }
                if before == item {
                    counts.unchanged += 1;
                    continue;
                }
                counts.changed += 1;
                if changed.len() < options.list_limit {
                    let mut fields = Vec::new();
                    let mut total = 0u64;
                    diff_leaves(
                        before,
                        item,
                        "",
                        options.field_limit,
                        &mut fields,
                        &mut total,
                    );
                    changed.push(RecordChange {
                        key: key_value(item, &key),
                        baseline_path: pointer::push(&path, &baseline_position.to_string()),
                        subject_path: pointer::push(&path, &index.to_string()),
                        fields_omitted: total.saturating_sub(fields.len() as u64),
                        fields,
                    });
                }
            }
        }
    }
    for (index, item) in baseline_items.iter().enumerate() {
        let key_text = canonical_key(item, &key).expect("indexed records have keys");
        if !subject_index.contains_key(&key_text) {
            counts.removed += 1;
            push_capped(&mut removed, options.list_limit, || RecordRef {
                key: key_value(item, &key),
                path: pointer::push(&path, &index.to_string()),
            });
        }
    }

    let mut reasons = Vec::new();
    if !(baseline.selection_exhaustive && subject.selection_exhaustive) {
        reasons.push(
            "added and removed describe the captured collections only; records the source paged or filtered out are not counted"
                .to_string(),
        );
    }
    if redacted_records > 0 {
        reasons.push(format!(
            "{redacted_records} matched record(s) hold redacted values; a change inside a redacted value is not visible"
        ));
    }
    let truncated = counts.added > added.len() as u64
        || counts.removed > removed.len() as u64
        || counts.changed > changed.len() as u64;
    Some(RecordDelta {
        status: RecordDeltaStatus::Compared,
        path: Some(path),
        key: Some(key),
        key_declared,
        baseline_items: baseline_items.len() as u64,
        subject_items: subject_items.len() as u64,
        counts,
        added,
        removed,
        changed,
        truncated,
        reasons,
    })
}

fn unavailable(
    status: RecordDeltaStatus,
    path: Option<String>,
    reasons: Vec<String>,
) -> RecordDelta {
    RecordDelta {
        status,
        path,
        key: None,
        key_declared: false,
        baseline_items: 0,
        subject_items: 0,
        counts: RecordCounts::default(),
        added: Vec::new(),
        removed: Vec::new(),
        changed: Vec::new(),
        truncated: false,
        reasons,
    }
}

fn array_at<'v>(value: &'v Value, path: &str) -> Option<&'v [Value]> {
    match pointer::get(value, path).ok()?? {
        Value::Array(items) => Some(items.as_slice()),
        _ => None,
    }
}

/// A capture can stand behind "this record is absent" only when every byte
/// was captured and stored. Redaction and windowed finding derivation do not
/// hide records: this comparison reads the whole persisted payload, and keys
/// that were redacted are rejected separately.
fn capture_gaps(side: &str, capture: &CaptureCompleteness, reasons: &mut Vec<String>) {
    let stop_ok = matches!(
        capture.stop_reason,
        CaptureStopReason::Complete
            | CaptureStopReason::DerivationWindowed
            | CaptureStopReason::Redacted
    );
    if !stop_ok {
        reasons.push(format!(
            "{side} capture stopped early ({})",
            serde_json::to_value(capture.stop_reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string())
        ));
    } else if capture.total_bytes != Some(capture.captured_bytes) {
        reasons.push(format!(
            "{side} capture holds fewer bytes than the source produced"
        ));
    }
}

fn normalize_field(field: &str) -> String {
    let field = field.trim();
    if field.is_empty() || field.starts_with('/') {
        field.to_string()
    } else {
        format!("/{field}")
    }
}

fn canonical_key(item: &Value, key: &str) -> Option<String> {
    let value = pointer::get(item, key).ok()??;
    match value {
        Value::Number(number) if number.is_i64() || number.is_u64() => Some(format!("n:{number}")),
        Value::String(text) if overview::is_key_string(text) => Some(format!("s:{text}")),
        _ => None,
    }
}

fn key_value(item: &Value, key: &str) -> Value {
    pointer::get(item, key)
        .ok()
        .flatten()
        .cloned()
        .unwrap_or(Value::Null)
}

fn index_by_key(
    items: &[Value],
    key: &str,
    side: &str,
    problems: &mut Vec<String>,
) -> HashMap<String, usize> {
    let mut index = HashMap::with_capacity(items.len());
    for (position, item) in items.iter().enumerate() {
        let Some(key_text) = canonical_key(item, key) else {
            problems.push(format!(
                "{side} record {position} has no usable {key} (missing, redacted, too long, or not a string or integer)"
            ));
            return index;
        };
        if index.insert(key_text, position).is_some() {
            problems.push(format!(
                "{side} has more than one record with the same {key}"
            ));
            return index;
        }
    }
    index
}

fn push_capped<T>(list: &mut Vec<T>, limit: usize, make: impl FnOnce() -> T) {
    if list.len() < limit {
        list.push(make());
    }
}

fn contains_redaction(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains("[REDACTED"),
        Value::Array(items) => items.iter().any(contains_redaction),
        Value::Object(map) => map.values().any(contains_redaction),
        _ => false,
    }
}

/// Collect changed leaves in source order: baseline fields first, then fields
/// only the subject has. Arrays of equal length compare by index; otherwise
/// the array itself is the changed leaf.
fn diff_leaves(
    before: &Value,
    after: &Value,
    path: &str,
    limit: usize,
    out: &mut Vec<FieldChange>,
    total: &mut u64,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(left), Value::Object(right)) => {
            for (key, left_value) in left {
                let child = pointer::push(path, key);
                match right.get(key) {
                    Some(right_value) => {
                        diff_leaves(left_value, right_value, &child, limit, out, total);
                    }
                    None => record_change(out, total, limit, child, Some(left_value), None),
                }
            }
            for (key, right_value) in right {
                if !left.contains_key(key) {
                    record_change(
                        out,
                        total,
                        limit,
                        pointer::push(path, key),
                        None,
                        Some(right_value),
                    );
                }
            }
        }
        (Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
            for (index, (left_value, right_value)) in left.iter().zip(right).enumerate() {
                diff_leaves(
                    left_value,
                    right_value,
                    &pointer::push(path, &index.to_string()),
                    limit,
                    out,
                    total,
                );
            }
        }
        _ => record_change(
            out,
            total,
            limit,
            path.to_string(),
            Some(before),
            Some(after),
        ),
    }
}

fn record_change(
    out: &mut Vec<FieldChange>,
    total: &mut u64,
    limit: usize,
    field: String,
    before: Option<&Value>,
    after: Option<&Value>,
) {
    *total += 1;
    if out.len() < limit {
        out.push(FieldChange {
            field,
            before: before.map(display_value),
            after: after.map(display_value),
        });
    }
}

/// Scalars verbatim; long strings as a labelled prefix; containers as a
/// marker. Never a rewritten value (I1).
fn display_value(value: &Value) -> Value {
    match value {
        Value::String(text) if text.chars().count() > CHANGE_VALUE_MAX_CHARS => {
            let prefix = text
                .chars()
                .take(CHANGE_VALUE_MAX_CHARS)
                .collect::<String>();
            Value::String(format!("{prefix}…"))
        }
        Value::Array(items) => json!(format!("«array: {} items»", items.len())),
        Value::Object(map) => json!(format!("«object: {} fields»", map.len())),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn complete() -> CaptureCompleteness {
        CaptureCompleteness::complete(1)
    }

    fn side<'a>(value: &'a Value, capture: &'a CaptureCompleteness) -> RecordSide<'a> {
        RecordSide {
            value,
            search_root: "",
            capture,
            selection_exhaustive: false,
        }
    }

    fn issues(states: &[(u64, &str)]) -> Value {
        Value::Array(
            states
                .iter()
                .map(|(number, state)| json!({"number": number, "state": state, "title": format!("t{number}")}))
                .collect(),
        )
    }

    #[test]
    fn keyed_comparison_reports_exact_added_removed_and_changed_records() {
        let capture = complete();
        let before = issues(&[(1, "open"), (2, "open"), (3, "open")]);
        let after = issues(&[(1, "open"), (2, "closed"), (4, "open")]);
        let delta = compare_records(
            side(&before, &capture),
            side(&after, &capture),
            &RecordDeltaOptions::default(),
        )
        .expect("collection");
        assert_eq!(delta.status, RecordDeltaStatus::Compared);
        assert_eq!(delta.key.as_deref(), Some("/number"));
        assert!(!delta.key_declared);
        assert_eq!(
            delta.counts,
            RecordCounts {
                added: 1,
                removed: 1,
                changed: 1,
                unchanged: 1
            }
        );
        assert_eq!(
            delta.added[0],
            RecordRef {
                key: json!(4),
                path: "/2".to_string()
            }
        );
        assert_eq!(
            delta.removed[0],
            RecordRef {
                key: json!(3),
                path: "/2".to_string()
            }
        );
        assert_eq!(
            delta.changed[0].fields,
            vec![FieldChange {
                field: "/state".to_string(),
                before: Some(json!("open")),
                after: Some(json!("closed")),
            }]
        );
        assert!(delta.reasons[0].contains("captured collections only"));
    }

    #[test]
    fn reordered_records_are_unchanged() {
        let capture = complete();
        let before = issues(&[(1, "open"), (2, "open")]);
        let after = issues(&[(2, "open"), (1, "open")]);
        let delta = compare_records(
            side(&before, &capture),
            side(&after, &capture),
            &RecordDeltaOptions::default(),
        )
        .expect("collection");
        assert_eq!(
            delta.counts,
            RecordCounts {
                unchanged: 2,
                ..RecordCounts::default()
            }
        );
    }

    #[test]
    fn incomplete_captures_never_report_removed_records() {
        let complete = complete();
        let mut truncated = complete.clone();
        truncated.stop_reason = CaptureStopReason::ByteLimit;
        let before = issues(&[(1, "open"), (2, "open")]);
        let after = issues(&[(1, "open")]);
        for (baseline, subject) in [(&truncated, &complete), (&complete, &truncated)] {
            let delta = compare_records(
                side(&before, baseline),
                side(&after, subject),
                &RecordDeltaOptions::default(),
            )
            .expect("collection");
            assert_eq!(delta.status, RecordDeltaStatus::Incomplete);
            assert!(delta.removed.is_empty());
            assert_eq!(delta.counts, RecordCounts::default());
        }
        let mut short = complete.clone();
        short.total_bytes = Some(10);
        let delta = compare_records(
            side(&before, &complete),
            side(&after, &short),
            &RecordDeltaOptions::default(),
        )
        .expect("collection");
        assert_eq!(delta.status, RecordDeltaStatus::Incomplete);
    }

    #[test]
    fn duplicate_missing_or_redacted_declared_keys_refuse_to_compare() {
        let capture = complete();
        let before = issues(&[(1, "open"), (2, "open")]);
        let after = json!([{"number": 1, "state": "open"}, {"number": "[REDACTED:secret_field]", "state": "open"}]);
        let delta = compare_records(
            side(&before, &capture),
            side(&after, &capture),
            &RecordDeltaOptions {
                key: Some("number"),
                ..RecordDeltaOptions::default()
            },
        )
        .expect("collection");
        assert_eq!(delta.status, RecordDeltaStatus::Unkeyed);
        assert!(delta.added.is_empty() && delta.removed.is_empty());

        let duplicate = issues(&[(1, "open"), (1, "closed")]);
        let delta = compare_records(
            side(&before, &capture),
            side(&duplicate, &capture),
            &RecordDeltaOptions {
                key: Some("/number"),
                ..RecordDeltaOptions::default()
            },
        )
        .expect("collection");
        assert_eq!(delta.status, RecordDeltaStatus::Unkeyed);
    }

    #[test]
    fn automatic_mode_stays_silent_without_a_keyed_collection() {
        let capture = complete();
        let scalar = json!({"ok": true});
        assert!(
            compare_records(
                side(&scalar, &capture),
                side(&scalar, &capture),
                &RecordDeltaOptions::default()
            )
            .is_none()
        );
        let unkeyed = json!([{"state": "open"}, {"state": "open"}]);
        assert!(
            compare_records(
                side(&unkeyed, &capture),
                side(&unkeyed, &capture),
                &RecordDeltaOptions::default()
            )
            .is_none()
        );
        let delta = compare_records(
            side(&unkeyed, &capture),
            side(&unkeyed, &capture),
            &RecordDeltaOptions {
                path: Some(""),
                ..RecordDeltaOptions::default()
            },
        )
        .expect("explicit request answers");
        assert_eq!(delta.status, RecordDeltaStatus::Unkeyed);
    }

    #[test]
    fn redacted_values_and_selection_are_disclosed() {
        let capture = complete();
        let before =
            json!([{"id": 1, "token": "[REDACTED:secret_field]"}, {"id": 2, "token": "x"}]);
        let after = before.clone();
        let mut left = side(&before, &capture);
        let mut right = side(&after, &capture);
        left.selection_exhaustive = true;
        right.selection_exhaustive = true;
        let delta =
            compare_records(left, right, &RecordDeltaOptions::default()).expect("collection");
        assert_eq!(delta.reasons.len(), 1);
        assert!(delta.reasons[0].contains("1 matched record(s) hold redacted values"));
    }

    #[test]
    fn lists_are_capped_but_counts_are_complete() {
        let capture = complete();
        let before = Value::Array((0..30).map(|id| json!({"id": id})).collect());
        let after = Value::Array((100..130).map(|id| json!({"id": id})).collect());
        let delta = compare_records(
            side(&before, &capture),
            side(&after, &capture),
            &RecordDeltaOptions {
                list_limit: 5,
                ..RecordDeltaOptions::default()
            },
        )
        .expect("collection");
        assert_eq!(delta.counts.added, 30);
        assert_eq!(delta.counts.removed, 30);
        assert_eq!(delta.added.len(), 5);
        assert!(delta.truncated);
    }

    #[test]
    fn changed_leaves_cover_nested_added_removed_and_resized_fields() {
        let capture = complete();
        let long = "z".repeat(200);
        let before = json!([{"id": 1, "user": {"login": "a"}, "labels": [1], "gone": true, "body": long}, {"id": 2}]);
        let after = json!([{"id": 1, "user": {"login": "b"}, "labels": [1, 2], "new": 5, "body": "short"}, {"id": 2}]);
        let delta = compare_records(
            side(&before, &capture),
            side(&after, &capture),
            &RecordDeltaOptions::default(),
        )
        .expect("collection");
        let fields = &delta.changed[0].fields;
        let names = fields
            .iter()
            .map(|field| field.field.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["/user/login", "/labels", "/gone", "/body", "/new"]);
        assert_eq!(fields[1].before, Some(json!("«array: 1 items»")));
        assert_eq!(fields[2].after, None);
        assert_eq!(fields[4].before, None);
        assert!(
            fields[3]
                .before
                .as_ref()
                .unwrap()
                .as_str()
                .unwrap()
                .ends_with('…')
        );
    }
}

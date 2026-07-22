//! Rust libtest JSON test events (`cargo test -- -Z unstable-options
//! --format=json`, or an equivalent libtest-json bridge) provider.

use std::path::Path;

use serde_json::{Map, Value, json};

use super::common::{MAX_PROVIDER_ITEMS, line_range, parse_ndjson, point_span};
use crate::{
    Extra, LineRange,
    findings::{Candidate, ProviderFindingSpec},
    pointer,
};

pub(super) fn detect(value: &Value) -> Option<f64> {
    match value {
        Value::Array(items) => detect_items(items.iter()),
        Value::String(text) => {
            let lines = parse_ndjson(text);
            if lines.is_empty() {
                None
            } else {
                detect_items(lines.iter())
            }
        }
        _ => None,
    }
}

fn detect_items<'a>(items: impl Iterator<Item = &'a Value>) -> Option<f64> {
    let mut seen = 0usize;
    let mut test_events = 0usize;
    for item in items.take(50) {
        seen += 1;
        if is_test_event(item) {
            test_events += 1;
        }
    }
    if test_events == 0 {
        return None;
    }
    Some((0.78 + (test_events as f64 / seen.max(1) as f64) * 0.15).min(0.95))
}

fn is_test_event(item: &Value) -> bool {
    item.as_object().is_some_and(|object| {
        object.get("type").and_then(Value::as_str) == Some("test")
            && object.get("name").and_then(Value::as_str).is_some()
            && object.get("event").and_then(Value::as_str).is_some()
    })
}

pub(super) fn normalize(
    value: &Value,
    path: &str,
    workspace_root: Option<&Path>,
) -> Vec<Candidate> {
    match value {
        Value::Array(items) => normalize_indexed(items, path, workspace_root),
        Value::String(text) => normalize_text(&parse_ndjson(text), path, workspace_root),
        _ => Vec::new(),
    }
}

fn normalize_indexed(items: &[Value], path: &str, workspace_root: Option<&Path>) -> Vec<Candidate> {
    items
        .iter()
        .enumerate()
        .take(MAX_PROVIDER_ITEMS)
        .filter_map(|(index, item)| {
            failed_test(item).map(|(name, stdout)| {
                candidate_for(
                    name,
                    stdout,
                    pointer::push(path, &index.to_string()),
                    None,
                    workspace_root,
                    Extra::new(),
                )
            })
        })
        .collect()
}

/// As with the Cargo JSON diagnostics provider, a raw NDJSON blob has no
/// per-line JSON pointer of its own: surface the first failing test event at
/// the blob's real path with a line range into the captured text.
fn normalize_text(lines: &[Value], path: &str, workspace_root: Option<&Path>) -> Vec<Candidate> {
    let total = lines.len();
    let Some((line_number, (name, stdout))) = lines
        .iter()
        .enumerate()
        .find_map(|(index, item)| failed_test(item).map(|hit| (index as u64 + 1, hit)))
    else {
        return Vec::new();
    };
    let mut extra = Extra::new();
    extra.insert("provider_ndjson_lines_scanned".to_string(), json!(total));
    vec![candidate_for(
        name,
        stdout,
        path.to_string(),
        Some(line_range(line_number, line_number)),
        workspace_root,
        extra,
    )]
}

fn failed_test(item: &Value) -> Option<(&str, Option<&str>)> {
    let object = item.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("test") {
        return None;
    }
    if object.get("event").and_then(Value::as_str) != Some("failed") {
        return None;
    }
    let name = object.get("name").and_then(Value::as_str)?;
    Some((name, object.get("stdout").and_then(Value::as_str)))
}

fn candidate_for(
    name: &str,
    stdout: Option<&str>,
    path: String,
    line_range: Option<LineRange>,
    workspace_root: Option<&Path>,
    extra: Extra,
) -> Candidate {
    let message = stdout.unwrap_or("test failed").to_string();
    let primary_span = stdout
        .and_then(panic_location)
        .and_then(|(file, line, column)| {
            point_span(
                &file,
                line,
                Some(column),
                "structured.libtest_panic",
                workspace_root,
            )
        });
    let mut identity = Map::new();
    identity.insert("test_id".to_string(), Value::String(name.to_string()));
    identity.insert(
        "diagnostic_type".to_string(),
        Value::String("failed".to_string()),
    );
    identity.insert("message".to_string(), Value::String(message.clone()));
    Candidate::from_provider(ProviderFindingSpec {
        path,
        // Reuse the generic "test_failure" kind so existing goal-intent
        // scoring bonuses apply unchanged to provider-sourced findings.
        kind: "test_failure",
        confidence: 0.93,
        reason: format!("libtest JSON event: {name} failed"),
        title: Some(format!("test failed: {name}")),
        severity: Some("error"),
        source: "provider.cargo.libtest_json.v1",
        line_range,
        primary_span,
        related_spans: Vec::new(),
        identity_value: Value::Object(identity),
        extra,
    })
}

/// Parse `panicked at src/lib.rs:10:5:` (Rust 2021+ panic message format)
/// from libtest's captured stdout.
fn panic_location(text: &str) -> Option<(String, u64, u64)> {
    let marker = "panicked at ";
    let start = text.find(marker)? + marker.len();
    let location = text[start..].lines().next()?.trim_end_matches(':');
    let mut parts = location.rsplitn(3, ':');
    let column = parts.next()?.parse::<u64>().ok()?;
    let line = parts.next()?.parse::<u64>().ok()?;
    let file = parts.next()?;
    (!file.is_empty()).then(|| (file.to_string(), line, column))
}

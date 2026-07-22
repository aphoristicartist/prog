//! Cargo/rustc JSON diagnostics provider (`cargo build --message-format=json`,
//! `cargo test --message-format=json`, or a bare rustc `--error-format=json`
//! stream).
//!
//! Accepts either a real JSON array of diagnostic objects (each item gets its
//! own addressable JSON pointer, so multiple diagnostics all survive as
//! distinct findings) or a captured newline-delimited text blob (where only
//! one representative diagnostic can be attached to the blob's own real
//! path — see [`normalize_text`]).

use std::path::Path;

use serde_json::{Map, Value, json};

use super::common::{MAX_PROVIDER_ITEMS, line_range, parse_ndjson};
use crate::{
    Extra, LineRange, extract_source_spans_with_workspace_root,
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
        Value::Object(_) => diagnostic_message(value).map(|_| 0.9),
        _ => None,
    }
}

fn detect_items<'a>(items: impl Iterator<Item = &'a Value>) -> Option<f64> {
    let mut seen = 0usize;
    let mut matched = 0usize;
    for item in items.take(50) {
        seen += 1;
        if diagnostic_message(item).is_some() {
            matched += 1;
        }
    }
    if matched == 0 {
        return None;
    }
    Some((0.8 + (matched as f64 / seen.max(1) as f64) * 0.15).min(0.97))
}

/// Extract the diagnostic `message` object from either a
/// `cargo --message-format=json` wrapper
/// (`{"reason":"compiler-message","message":{...}}`) or a bare rustc
/// diagnostic (`{"message":...,"level":...,"spans":[...]}`), requiring a real
/// `level` in the known set and a `spans` array so unrelated JSON with a
/// coincidental `message`/`level` pair does not false-positive.
fn diagnostic_message(item: &Value) -> Option<&Map<String, Value>> {
    let object = item.as_object()?;
    let message = if object.get("reason").and_then(Value::as_str) == Some("compiler-message") {
        object.get("message")?.as_object()?
    } else {
        object
    };
    let level = message.get("level").and_then(Value::as_str)?;
    if !matches!(
        level,
        "error" | "warning" | "error: internal compiler error"
    ) {
        return None;
    }
    message.get("spans")?.as_array()?;
    message.get("message").and_then(Value::as_str)?;
    Some(message)
}

pub(super) fn normalize(
    value: &Value,
    path: &str,
    workspace_root: Option<&Path>,
) -> Vec<Candidate> {
    match value {
        Value::Array(items) => normalize_indexed(items, path, workspace_root),
        Value::String(text) => normalize_text(&parse_ndjson(text), path, workspace_root),
        Value::Object(_) => diagnostic_message(value)
            .map(|message| {
                vec![candidate_for(
                    message,
                    path.to_string(),
                    None,
                    workspace_root,
                    Extra::new(),
                )]
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn normalize_indexed(items: &[Value], path: &str, workspace_root: Option<&Path>) -> Vec<Candidate> {
    items
        .iter()
        .enumerate()
        .take(MAX_PROVIDER_ITEMS)
        .filter_map(|(index, item)| {
            diagnostic_message(item).map(|message| {
                candidate_for(
                    message,
                    pointer::push(path, &index.to_string()),
                    None,
                    workspace_root,
                    Extra::new(),
                )
            })
        })
        .collect()
}

/// A raw NDJSON text blob has no per-line JSON pointer of its own; surface
/// the first matching diagnostic at the blob's real path (with a line range
/// into the *captured* text) rather than fabricating an address that would
/// not resolve for `prog expand`/`prog evidence`. The full stream remains
/// inspectable at that same real path.
fn normalize_text(lines: &[Value], path: &str, workspace_root: Option<&Path>) -> Vec<Candidate> {
    let total = lines.len();
    let Some((line_number, message)) = lines.iter().enumerate().find_map(|(index, item)| {
        diagnostic_message(item).map(|message| (index as u64 + 1, message))
    }) else {
        return Vec::new();
    };
    let mut extra = Extra::new();
    extra.insert("provider_ndjson_lines_scanned".to_string(), json!(total));
    vec![candidate_for(
        message,
        path.to_string(),
        Some(line_range(line_number, line_number)),
        workspace_root,
        extra,
    )]
}

fn candidate_for(
    message: &Map<String, Value>,
    path: String,
    line_range: Option<LineRange>,
    workspace_root: Option<&Path>,
    extra: Extra,
) -> Candidate {
    let level = message
        .get("level")
        .and_then(Value::as_str)
        .unwrap_or("error");
    let text = message
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let code = message
        .get("code")
        .and_then(Value::as_object)
        .and_then(|code| code.get("code"))
        .and_then(Value::as_str);
    let (primary_span, related_spans) =
        extract_source_spans_with_workspace_root(&Value::Object(message.clone()), workspace_root);
    let mut identity = Map::new();
    if let Some(code) = code {
        identity.insert("code".to_string(), Value::String(code.to_string()));
    }
    identity.insert(
        "diagnostic_type".to_string(),
        Value::String(level.to_string()),
    );
    identity.insert("message".to_string(), Value::String(text.to_string()));
    // Reuse the generic kind taxonomy so existing goal-intent scoring bonuses
    // (root-cause / test-failure prioritize "rust_compile_error"; "warning"
    // is deprioritized) apply to provider-sourced findings unchanged.
    let kind = if level == "warning" {
        "warning"
    } else {
        "rust_compile_error"
    };
    Candidate::from_provider(ProviderFindingSpec {
        path,
        kind,
        confidence: 0.95,
        reason: format!(
            "cargo/rustc JSON diagnostic: {level}{}",
            code.map(|code| format!(" [{code}]")).unwrap_or_default()
        ),
        title: Some(match code {
            Some(code) => format!("{level}[{code}]"),
            None => level.to_string(),
        }),
        severity: Some(if level == "warning" {
            "warning"
        } else {
            "error"
        }),
        source: "provider.cargo.rustc_json_diagnostics.v1",
        line_range,
        primary_span,
        related_spans,
        identity_value: Value::Object(identity),
        extra,
    })
}

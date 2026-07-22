//! Bounded text fallback for standard pytest terminal output, used only when
//! neither `--json-report` nor `--junitxml` evidence is available.
//!
//! Only the compact `FAILED <nodeid> - <reason>` summary lines are trusted
//! for identity; prose inside `===== FAILURES =====` blocks is evidence to
//! inspect, not parsed for identity, since it is not stable across reruns the
//! way a node ID is.

use std::path::Path;

use serde_json::{Map, Value, json};

use super::common::MAX_PROVIDER_TEXT_LINES;
use crate::{
    Extra, LineRange,
    findings::{Candidate, ProviderFindingSpec},
};

pub(super) fn detect(value: &Value) -> Option<f64> {
    let text = value.as_str()?;
    let failed = failed_lines(text).count();
    let has_failures_header = text
        .lines()
        .take(MAX_PROVIDER_TEXT_LINES)
        .any(is_failures_header);
    if failed == 0 && !has_failures_header {
        return None;
    }
    Some(if failed > 0 { 0.8 } else { 0.6 })
}

pub(super) fn normalize(
    value: &Value,
    path: &str,
    _workspace_root: Option<&Path>,
) -> Vec<Candidate> {
    let Some(text) = value.as_str() else {
        return Vec::new();
    };
    let mut hits = failed_lines(text);
    let Some((line_number, nodeid, reason)) = hits.next() else {
        return Vec::new();
    };
    let total = 1 + hits.count();
    let exhaustive = is_exhaustive(text);
    let mut extra = Extra::new();
    extra.insert("provider_failed_lines_seen".to_string(), json!(total));
    if !exhaustive {
        extra.insert("provider_exhaustive".to_string(), json!(false));
    }
    let mut identity = Map::new();
    identity.insert("nodeid".to_string(), Value::String(nodeid.to_string()));
    identity.insert(
        "diagnostic_type".to_string(),
        Value::String("failed".to_string()),
    );
    identity.insert("message".to_string(), Value::String(reason.clone()));
    vec![Candidate::from_provider(ProviderFindingSpec {
        path: path.to_string(),
        kind: "test_failure",
        // Deliberately above the generic string-pattern detector's 0.8 for an
        // "AssertionError" substring on the same blob: a real extracted node
        // ID is stronger identity than a keyword match, and both can target
        // the same (path, kind) dedup key when they fire on the same text.
        confidence: 0.88,
        reason: format!("pytest text summary: FAILED {nodeid}"),
        title: Some(format!("pytest failed: {nodeid}")),
        severity: Some("error"),
        source: "provider.pytest.text.v1",
        line_range: Some(LineRange {
            start: line_number,
            end: line_number,
            extra: Extra::new(),
        }),
        primary_span: None,
        related_spans: Vec::new(),
        identity_value: Value::Object(identity),
        extra,
    })]
}

fn failed_lines(text: &str) -> impl Iterator<Item = (u64, &str, String)> {
    text.lines()
        .take(MAX_PROVIDER_TEXT_LINES)
        .enumerate()
        .filter_map(|(index, line)| {
            let rest = line.trim_start().strip_prefix("FAILED ")?;
            let (nodeid, reason) = rest.split_once(" - ").unwrap_or((rest, ""));
            let nodeid = nodeid.trim();
            (nodeid.contains("::") && !nodeid.contains(char::is_whitespace))
                .then(|| (index as u64 + 1, nodeid, reason.trim().to_string()))
        })
}

fn is_failures_header(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("=====") && trimmed.to_ascii_uppercase().contains("FAILURES")
}

/// A run is exhaustive only when no textual marker indicates deselection or
/// an early stop; text output cannot prove exhaustiveness the way a
/// structured `summary.deselected` count can, so this is intentionally more
/// conservative than [`super::pytest_json::normalize`]'s check.
fn is_exhaustive(text: &str) -> bool {
    !text.lines().take(MAX_PROVIDER_TEXT_LINES).any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("deselected")
            || lower.contains("stopped after")
            || lower.contains("no tests ran")
    })
}

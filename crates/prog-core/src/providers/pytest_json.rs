//! `pytest-json-report` (https://pypi.org/project/pytest-json-report/) provider.
//!
//! This is the preferred pytest evidence format: every test result carries a
//! stable `nodeid`, an explicit `outcome`, and (for failures) a `longrepr`
//! plus a `crash` location, so identity and source spans come from the tool
//! itself instead of being guessed from terminal prose.

use std::path::Path;

use serde_json::{Map, Value};

use super::common::{MAX_PROVIDER_ITEMS, point_span};
use crate::{
    Extra,
    findings::{Candidate, ProviderFindingSpec},
    pointer,
};

pub(super) fn detect(value: &Value) -> Option<f64> {
    let object = value.as_object()?;
    let tests = object.get("tests")?.as_array()?;
    if tests.is_empty() {
        return None;
    }
    let sample = tests.len().min(20);
    let matched = tests
        .iter()
        .take(sample)
        .filter(|test| {
            test.as_object().is_some_and(|test| {
                test.get("nodeid").and_then(Value::as_str).is_some()
                    && test.get("outcome").and_then(Value::as_str).is_some()
            })
        })
        .count();
    if matched == 0 {
        return None;
    }
    let ratio = matched as f64 / sample as f64;
    let has_summary = object.get("summary").and_then(Value::as_object).is_some();
    Some((0.75 + ratio * 0.15 + if has_summary { 0.08 } else { 0.0 }).min(0.97))
}

pub(super) fn normalize(
    value: &Value,
    path: &str,
    workspace_root: Option<&Path>,
) -> Vec<Candidate> {
    let Some(tests) = value.get("tests").and_then(Value::as_array) else {
        return Vec::new();
    };
    let exhaustive = is_exhaustive(value);
    let tests_path = pointer::push(path, "tests");
    tests
        .iter()
        .enumerate()
        .take(MAX_PROVIDER_ITEMS)
        .filter_map(|(index, test)| {
            let test_object = test.as_object()?;
            let nodeid = test_object.get("nodeid").and_then(Value::as_str)?;
            let outcome = test_object.get("outcome").and_then(Value::as_str)?;
            if !matches!(outcome, "failed" | "error") {
                return None;
            }
            Some(candidate_for(
                test_object,
                nodeid,
                outcome,
                pointer::push(&tests_path, &index.to_string()),
                exhaustive,
                workspace_root,
            ))
        })
        .collect()
}

fn candidate_for(
    test: &Map<String, Value>,
    nodeid: &str,
    outcome: &str,
    path: String,
    exhaustive: bool,
    workspace_root: Option<&Path>,
) -> Candidate {
    let message =
        test_message(test).unwrap_or_else(|| format!("pytest reported {outcome} for {nodeid}"));
    let (crash_path, crash_line) = crash_location(test);
    let primary_span = crash_path
        .as_deref()
        .zip(crash_line)
        .and_then(|(path, line)| {
            point_span(
                path,
                line,
                None,
                "structured.pytest_json_report",
                workspace_root,
            )
        });
    let mut identity = Map::new();
    identity.insert("nodeid".to_string(), Value::String(nodeid.to_string()));
    identity.insert(
        "diagnostic_type".to_string(),
        Value::String(outcome.to_string()),
    );
    identity.insert("message".to_string(), Value::String(message.clone()));
    let mut extra = Extra::new();
    if !exhaustive {
        extra.insert("provider_exhaustive".to_string(), Value::Bool(false));
    }
    Candidate::from_provider(ProviderFindingSpec {
        path,
        kind: if outcome == "error" {
            "test_error"
        } else {
            "test_failure"
        },
        confidence: 0.95,
        reason: format!("pytest-json-report: {nodeid} {outcome}"),
        title: Some(format!("pytest {outcome}: {nodeid}")),
        severity: Some("error"),
        source: "provider.pytest.json_report.v1",
        line_range: None,
        primary_span,
        related_spans: Vec::new(),
        identity_value: Value::Object(identity),
        extra,
    })
}

/// Prefer the `call` phase's failure text (the actual test body), falling
/// back to `setup`/`teardown` (collection/fixture errors) and finally a
/// top-level `longrepr` some report variants place directly on the test.
fn test_message(test: &Map<String, Value>) -> Option<String> {
    for phase in ["call", "setup", "teardown"] {
        let Some(longrepr) = test
            .get(phase)
            .and_then(Value::as_object)
            .and_then(|phase| phase.get("longrepr"))
        else {
            continue;
        };
        if let Some(text) = longrepr.as_str() {
            return Some(text.to_string());
        }
        if let Some(message) = longrepr
            .as_object()
            .and_then(|object| object.get("reprcrash"))
            .and_then(Value::as_object)
            .and_then(|reprcrash| reprcrash.get("message"))
            .and_then(Value::as_str)
        {
            return Some(message.to_string());
        }
    }
    test.get("longrepr")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn crash_location(test: &Map<String, Value>) -> (Option<String>, Option<u64>) {
    for phase in ["call", "setup"] {
        let Some(crash) = test
            .get(phase)
            .and_then(Value::as_object)
            .and_then(|phase| phase.get("crash"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let path = crash
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string);
        if path.is_some() {
            // pytest-json-report line numbers are 0-based; findings elsewhere
            // in this crate are always 1-based source lines.
            let line = crash
                .get("lineno")
                .and_then(Value::as_u64)
                .map(|line| line.saturating_add(1));
            return (path, line);
        }
    }
    (None, None)
}

/// A run is exhaustive only when the report's own summary confirms nothing
/// was deselected and the process reached a normal pass/fail exit code (`0`
/// or `1`); any other exit code (interrupted, internal error, no tests
/// collected) or a missing summary cannot prove the suite ran to completion.
fn is_exhaustive(value: &Value) -> bool {
    let Some(summary) = value.get("summary").and_then(Value::as_object) else {
        return false;
    };
    let deselected = summary
        .get("deselected")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let exitcode_ok = value
        .get("exitcode")
        .and_then(Value::as_i64)
        .is_none_or(|code| matches!(code, 0 | 1));
    deselected == 0 && exitcode_ok
}

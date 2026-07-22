//! Bounded, pure normalization providers for pytest and Cargo/rustc coding
//! tool output (issue #114).
//!
//! A provider inspects an already-redacted JSON value, declares a match
//! confidence, and — only when it clears [`MIN_PROVIDER_CONFIDENCE`] — emits
//! findings with stronger cross-run identity (real subject/diagnostic-code
//! identity, structured source spans) than the generic string-pattern
//! detectors in [`crate::findings`] can produce. [`collect_provider_signals`]
//! is called from `ranked_findings` alongside the generic collectors: a
//! provider match only ever *adds* candidates, and a provider that declines
//! or partially parses simply contributes nothing rather than erroring, so
//! generic findings remain the fallback and the captured observation is never
//! discarded.
//!
//! Providers are tried in priority order (explicit machine-readable formats
//! before bounded text fallbacks) against a small, bounded set of probe
//! locations: the payload's own scoped shape, plus — when the payload is a
//! `prog run` capture wrapper — its `stdout.text`/`stderr.text` fields, since
//! that is the only place a genuinely captured tool transcript lives when the
//! payload itself is not already a parsed JSON report.

mod cargo_json;
mod cargo_libtest;
mod common;
mod pytest_json;
mod pytest_text;

use std::path::Path;

use serde_json::Value;

use crate::{findings::Candidate, pointer};

/// A provider must clear this confidence before its normalized findings are
/// trusted over the generic detectors. Kept high: a provider asserts strong,
/// structured identity, so an ambiguous match should decline rather than
/// overclaim it.
const MIN_PROVIDER_CONFIDENCE: f64 = 0.75;

struct ProviderDef {
    id: &'static str,
    detect: fn(&Value) -> Option<f64>,
    normalize: fn(&Value, &str, Option<&Path>) -> Vec<Candidate>,
}

const PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        id: "pytest.json_report.v1",
        detect: pytest_json::detect,
        normalize: pytest_json::normalize,
    },
    ProviderDef {
        id: "cargo.rustc_json_diagnostics.v1",
        detect: cargo_json::detect,
        normalize: cargo_json::normalize,
    },
    ProviderDef {
        id: "cargo.libtest_json.v1",
        detect: cargo_libtest::detect,
        normalize: cargo_libtest::normalize,
    },
    ProviderDef {
        id: "pytest.text.v1",
        detect: pytest_text::detect,
        normalize: pytest_text::normalize,
    },
];

struct Probe<'a> {
    path: String,
    value: &'a Value,
}

/// The bounded set of locations a coding provider may match: the payload's
/// own scoped shape, plus a `prog run` capture's `stdout.text`/`stderr.text`.
fn probes<'a>(payload: &'a Value, path: &str) -> Vec<Probe<'a>> {
    let mut out = vec![Probe {
        path: path.to_string(),
        value: payload,
    }];
    if let Some(map) = payload.as_object() {
        for stream in ["stdout", "stderr"] {
            if let Some(text) = map
                .get(stream)
                .and_then(Value::as_object)
                .and_then(|stream| stream.get("text"))
            {
                out.push(Probe {
                    path: pointer::push(&pointer::push(path, stream), "text"),
                    value: text,
                });
            }
        }
    }
    out
}

fn first_match(value: &Value) -> Option<(&'static ProviderDef, f64)> {
    PROVIDERS.iter().find_map(|provider| {
        (provider.detect)(value)
            .filter(|confidence| *confidence >= MIN_PROVIDER_CONFIDENCE)
            .map(|confidence| (provider, confidence))
    })
}

/// Best-match provider id for `payload`, checked against its own shape and
/// (when present) its captured `stdout`/`stderr` text. This is the identity
/// CLI callers attach to `ObservationRecord.parser` (see `run.rs`'s
/// `record_capture` call) independent of whether any finding-worthy evidence
/// was actually found.
pub fn detect_coding_provider(payload: &Value) -> Option<&'static str> {
    probes(payload, "")
        .into_iter()
        .find_map(|probe| first_match(probe.value).map(|(provider, _)| provider.id))
}

/// Run every bounded provider against `payload`'s scoped value and its known
/// text-capture sub-paths, appending any normalized candidates to `out`.
pub(crate) fn collect_provider_signals(
    payload: &Value,
    path: &str,
    workspace_root: Option<&Path>,
    out: &mut Vec<Candidate>,
) {
    for probe in probes(payload, path) {
        if let Some((provider, _confidence)) = first_match(probe.value) {
            out.extend((provider.normalize)(
                probe.value,
                &probe.path,
                workspace_root,
            ));
        }
    }
}

//! Property and fuzz tests for the pytest/Cargo/rustc coding providers
//! (issue #114): determinism and bounded work under adversarial JSON input.
//!
//! Correctness of the actual normalization is covered by
//! `crates/prog-core/tests/providers.rs`'s unit and golden tests; these
//! properties instead pin the contract unit tests cannot cover cheaply:
//! providers must never panic, must always terminate, must produce
//! byte-identical output when called repeatedly on the same input, and must
//! never let a pathologically large input turn normalization into unbounded
//! work — no matter how adversarial or malformed that input is.

use proptest::prelude::*;
use serde_json::{Map, Value, json};

use prog_core::{CommandHintConfig, FindingOptions, ranked_findings};

fn prop_options() -> FindingOptions {
    FindingOptions {
        cursor: Some("pc1_providers_prop".to_string()),
        hints: CommandHintConfig::NAV_ALL,
        limit: 20,
        ..FindingOptions::default()
    }
}

/// Strings biased toward the exact literals the providers' `detect`
/// functions key off (`"compiler-message"`, `"failed"`, a panic marker, a
/// `FAILED nodeid - reason` line, ...) mixed with generic detector signals
/// and plain noise, so the fuzzer exercises real parsing code paths instead
/// of only early-decline paths.
fn signal_string() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("compiler-message".to_string()),
        Just("build-finished".to_string()),
        Just("error".to_string()),
        Just("warning".to_string()),
        Just("failed".to_string()),
        Just("passed".to_string()),
        Just("ok".to_string()),
        Just("test".to_string()),
        Just("suite".to_string()),
        Just("E0308".to_string()),
        Just("mismatched types".to_string()),
        Just("tests/test_x.py::test_case".to_string()),
        Just("FAILED tests/test_x.py::test_case - AssertionError: boom".to_string()),
        Just("thread 'tests::bar' panicked at src/lib.rs:10:5:\nboom".to_string()),
        Just("===== FAILURES =====".to_string()),
        Just("".to_string()),
        "[a-zA-Z0-9_:./ -]{0,40}",
    ]
}

fn signal_key() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("tests".to_string()),
        Just("nodeid".to_string()),
        Just("outcome".to_string()),
        Just("summary".to_string()),
        Just("call".to_string()),
        Just("crash".to_string()),
        Just("longrepr".to_string()),
        Just("spans".to_string()),
        Just("level".to_string()),
        Just("message".to_string()),
        Just("code".to_string()),
        Just("reason".to_string()),
        Just("type".to_string()),
        Just("event".to_string()),
        Just("name".to_string()),
        Just("stdout".to_string()),
        Just("stderr".to_string()),
        Just("text".to_string()),
        Just("is_primary".to_string()),
        Just("file_name".to_string()),
        "[a-z]{1,6}",
    ]
}

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (0..=9_999i64).prop_map(Value::from),
        signal_string().prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 40, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            prop::collection::vec((signal_key(), inner.clone()), 0..6).prop_map(|pairs| {
                let mut map = Map::new();
                for (key, value) in pairs {
                    map.insert(key, value);
                }
                Value::Object(map)
            }),
        ]
    })
}

/// A payload shaped like a `prog run` capture wrapper, so the fuzzer also
/// exercises the `stdout.text`/`stderr.text` probe path, not just the root.
fn run_wrapper_payload() -> impl Strategy<Value = Value> {
    (json_value(), signal_string(), signal_string()).prop_map(|(root, stdout_text, stderr_text)| {
        let mut object = root.as_object().cloned().unwrap_or_default();
        object.insert("stdout".to_string(), json!({"text": stdout_text}));
        object.insert("stderr".to_string(), json!({"text": stderr_text}));
        Value::Object(object)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn provider_pipeline_never_panics_on_arbitrary_json(payload in json_value()) {
        let _ = ranked_findings(&payload, &prop_options());
    }

    #[test]
    fn provider_pipeline_never_panics_on_run_wrapper_shapes(payload in run_wrapper_payload()) {
        let _ = ranked_findings(&payload, &prop_options());
    }

    #[test]
    fn provider_pipeline_is_deterministic(payload in run_wrapper_payload()) {
        let mut last: Option<String> = None;
        for _ in 0..5 {
            let found = ranked_findings(&payload, &prop_options()).unwrap();
            let serialized = serde_json::to_string(&found).unwrap();
            if let Some(previous) = &last {
                prop_assert_eq!(&serialized, previous);
            }
            last = Some(serialized);
        }
    }

    #[test]
    fn provider_findings_have_stable_navigable_pointers(payload in run_wrapper_payload()) {
        // Every provider-sourced finding's path must resolve back into the
        // real payload (or, for text-blob-sourced findings, its containing
        // string) so `prog expand`/`prog evidence` never dead-ends on a
        // fabricated address.
        let found = ranked_findings(&payload, &prop_options()).unwrap();
        for finding in &found {
            let Some(source) = finding.source.as_deref() else { continue };
            if !source.starts_with("provider.") {
                continue;
            }
            prop_assert!(
                prog_core::pointer::get(&payload, &finding.path).unwrap().is_some(),
                "provider finding path {} does not resolve in its own payload",
                finding.path
            );
        }
    }
}

/// Explicit boundedness check: a pathologically large structured array must
/// still normalize in bounded work, capped well below "every item became a
/// finding." `limit` is set far above the cap so truncation by the ranking
/// engine cannot be mistaken for the provider's own internal bound.
#[test]
fn cargo_json_diagnostics_array_normalization_is_capped() {
    let diagnostics: Vec<Value> = (0..10_000)
        .map(|index| {
            json!({
                "message": format!("error number {index}"),
                "code": {"code": format!("E{index:04}")},
                "level": "error",
                "spans": [{
                    "file_name": "src/lib.rs",
                    "line_start": 1,
                    "line_end": 1,
                    "column_start": 1,
                    "column_end": 2,
                    "is_primary": true
                }],
                "children": []
            })
        })
        .collect();
    let payload = Value::Array(diagnostics);
    let options = FindingOptions {
        limit: 100_000,
        ..FindingOptions::default()
    };
    let found = ranked_findings(&payload, &options).unwrap();
    // Every array item also independently triggers the *generic*
    // object-level severity detector (each diagnostic carries a `"level":
    // "error"` field), which is unbounded-by-design at this artificially
    // raised `limit` — that is pre-existing, out-of-scope behavior. What
    // this test pins is the provider's own contribution.
    let provider_found = found
        .iter()
        .filter(|finding| {
            finding.source.as_deref() == Some("provider.cargo.rustc_json_diagnostics.v1")
        })
        .count();
    assert!(
        provider_found <= 500,
        "provider normalization must stay bounded regardless of input size, got {provider_found}"
    );
}

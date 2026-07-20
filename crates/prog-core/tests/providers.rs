//! Unit and golden-fixture tests for the pytest/Cargo/rustc coding providers
//! (issue #114): `crates/prog-core/src/providers/*`.
//!
//! Providers are `pub(crate)`, so — deliberately, matching this crate's own
//! module boundary — every test here goes through the same public entry
//! points a real caller uses: [`ranked_findings`] (which calls
//! `providers::collect_provider_signals` internally) and
//! [`detect_coding_provider`].
//!
//! Regenerate the golden snapshots with:
//!
//! ```text
//! PROG_PROVIDERS_UPDATE=1 cargo test -p prog-core --test providers
//! ```

use prog_core::{CommandHintConfig, FindingOptions, detect_coding_provider, ranked_findings};
use serde_json::{Value, json};

fn options() -> FindingOptions {
    FindingOptions {
        cursor: Some("pc1_providers".to_string()),
        hints: CommandHintConfig::NAV_ALL,
        limit: 20,
        ..FindingOptions::default()
    }
}

fn findings(payload: &Value) -> Vec<prog_core::Finding> {
    ranked_findings(payload, &options()).unwrap()
}

/// Findings from exactly one provider, by its `source` tag. The generic
/// detectors run unconditionally alongside providers and legitimately fire
/// on the same fixture payloads (a numeric `"failed": 0` field, an
/// `"AssertionError"` substring, ...); tests that care about one provider's
/// own output filter down to it explicitly rather than asserting on the
/// total finding count.
fn from_provider<'a>(found: &'a [prog_core::Finding], source: &str) -> Vec<&'a prog_core::Finding> {
    found
        .iter()
        .filter(|f| f.source.as_deref() == Some(source))
        .collect()
}

const PYTEST_JSON_REPORT: &str = "provider.pytest.json_report.v1";
const CARGO_JSON: &str = "provider.cargo.rustc_json_diagnostics.v1";
const CARGO_LIBTEST: &str = "provider.cargo.libtest_json.v1";
const PYTEST_TEXT: &str = "provider.pytest.text.v1";

// ---------------------------------------------------------------------------
// pytest-json-report
// ---------------------------------------------------------------------------

fn pytest_json_report_payload() -> Value {
    json!({
        "created": 1721000000.0,
        "duration": 1.23,
        "exitcode": 1,
        "root": "/repo",
        "summary": {"collected": 3, "passed": 1, "failed": 1, "error": 1, "total": 3},
        "tests": [
            {
                "nodeid": "tests/test_checkout.py::test_total",
                "outcome": "passed",
                "call": {"outcome": "passed"}
            },
            {
                "nodeid": "tests/test_checkout.py::test_discount",
                "outcome": "failed",
                "call": {
                    "outcome": "failed",
                    "longrepr": "AssertionError: expected 19 got 21",
                    "crash": {
                        "path": "tests/test_checkout.py",
                        "lineno": 41,
                        "message": "AssertionError: expected 19 got 21"
                    }
                }
            },
            {
                "nodeid": "tests/test_checkout.py::test_tax",
                "outcome": "error",
                "setup": {
                    "outcome": "failed",
                    "longrepr": "fixture 'db' not found"
                }
            }
        ]
    })
}

#[test]
fn pytest_json_report_emits_one_finding_per_failure_with_real_pointers() {
    let payload = pytest_json_report_payload();
    let found = findings(&payload);

    let discount = found
        .iter()
        .find(|finding| finding.path == "/tests/1")
        .expect("failed test at /tests/1");
    assert_eq!(discount.kind, "test_failure");
    assert!(discount.fingerprint.is_some());
    assert_eq!(discount.severity.as_deref(), Some("error"));
    assert_eq!(
        discount.primary_span.as_ref().map(|span| span.start_line),
        Some(42), // pytest's 0-based lineno 41 -> 1-based source line 42
    );
    assert_eq!(
        discount
            .primary_span
            .as_ref()
            .and_then(|span| span.path.as_deref()),
        Some("tests/test_checkout.py")
    );

    let tax = found
        .iter()
        .find(|finding| finding.path == "/tests/2")
        .expect("error test at /tests/2");
    assert_eq!(tax.kind, "test_error");
    assert!(tax.fingerprint.is_some());
    assert_ne!(
        discount.fingerprint, tax.fingerprint,
        "distinct failures must remain distinct"
    );

    assert!(
        found.iter().all(|finding| finding.path != "/tests/0"),
        "a passing test must never become a finding"
    );

    assert_eq!(
        discount.commands.expand.as_deref(),
        Some("prog expand pc1_providers --path /tests/1"),
        "provider findings must use real, navigable JSON pointers"
    );
}

#[test]
fn pytest_json_report_fingerprint_is_stable_across_line_shifts() {
    let mut shifted = pytest_json_report_payload();
    shifted["tests"][1]["call"]["crash"]["lineno"] = json!(9_999);

    let original = findings(&pytest_json_report_payload());
    let shifted = findings(&shifted);

    let original = original.iter().find(|f| f.path == "/tests/1").unwrap();
    let shifted = shifted.iter().find(|f| f.path == "/tests/1").unwrap();
    assert_eq!(
        original.fingerprint, shifted.fingerprint,
        "raw line number must not participate in cross-run identity"
    );
    assert_ne!(
        original.primary_span.as_ref().unwrap().start_line,
        shifted.primary_span.as_ref().unwrap().start_line,
        "the span itself should still reflect the new location"
    );
}

#[test]
fn pytest_json_report_narrowed_run_is_marked_non_exhaustive() {
    let mut payload = pytest_json_report_payload();
    payload["summary"]["deselected"] = json!(2);
    let found = findings(&payload);
    let discount = found.iter().find(|f| f.path == "/tests/1").unwrap();
    assert_eq!(
        discount.extra.get("provider_exhaustive"),
        Some(&Value::Bool(false)),
        "a deselected run cannot prove the broader suite passed"
    );
}

#[test]
fn pytest_json_report_missing_summary_is_conservatively_non_exhaustive() {
    let mut payload = pytest_json_report_payload();
    payload.as_object_mut().unwrap().remove("summary");
    let found = findings(&payload);
    let discount = found.iter().find(|f| f.path == "/tests/1").unwrap();
    assert_eq!(
        discount.extra.get("provider_exhaustive"),
        Some(&Value::Bool(false))
    );
}

#[test]
fn pytest_json_report_handles_unicode_identity_without_panicking() {
    let payload = json!({
        "summary": {"collected": 1, "passed": 0, "failed": 1, "total": 1},
        "exitcode": 1,
        "tests": [{
            "nodeid": "tests/test_ünïcödé.py::test_émoji_🎉",
            "outcome": "failed",
            "call": {
                "outcome": "failed",
                "longrepr": "AssertionError: 期待値と異なります 🚨"
            }
        }]
    });
    let found = findings(&payload);
    let failure = found.iter().find(|f| f.path == "/tests/0").unwrap();
    assert!(failure.fingerprint.is_some());
    assert!(failure.title.as_deref().unwrap().contains("test_émoji_🎉"));
}

#[test]
fn pytest_json_report_malformed_tests_decline_without_panicking() {
    // `nodeid`/`outcome` are the wrong JSON type on every test; the provider
    // must decline (never panic), and generic detection has nothing to key
    // off either, so this must resolve to an empty, not error, result.
    let payload = json!({
        "tests": [
            {"nodeid": 123, "outcome": true},
            {"nodeid": 456, "outcome": false}
        ]
    });
    let found = findings(&payload);
    assert!(found.is_empty());
}

#[test]
fn pytest_json_report_provider_decline_still_yields_generic_findings() {
    // Malformed provider-shaped data sits *alongside* genuinely
    // generic-detectable evidence. The provider declining must never remove
    // or shadow the generic fallback for unrelated evidence in the same
    // payload ("provider failure never discards the captured observation").
    let payload = json!({
        "tests": [{"nodeid": 123, "outcome": true}],
        "failure_sections": [{
            "kind": "python",
            "stream": "stderr",
            "line_start": 1,
            "line_end": 2,
            "priority": 90,
            "lines": ["Traceback (most recent call last):", "AssertionError: boom"],
            "reason": "Python traceback"
        }]
    });
    let found = findings(&payload);
    assert!(!found.is_empty());
    assert_eq!(found[0].path, "/failure_sections/0");
}

#[test]
fn no_tests_array_declines_pytest_json_provider() {
    let payload = json!({"unrelated": "value"});
    assert!(findings(&payload).is_empty());
    assert_eq!(detect_coding_provider(&payload), None);
}

#[test]
fn detect_coding_provider_reports_pytest_json_report() {
    assert_eq!(
        detect_coding_provider(&pytest_json_report_payload()),
        Some("pytest.json_report.v1")
    );
}

// ---------------------------------------------------------------------------
// Cargo/rustc JSON diagnostics
// ---------------------------------------------------------------------------

fn rustc_diagnostic(
    code: &str,
    level: &str,
    message: &str,
    primary_line: u64,
    related_line: u64,
) -> Value {
    json!({
        "message": message,
        "code": {"code": code, "explanation": Value::Null},
        "level": level,
        "spans": [
            {
                "file_name": "src/lib.rs",
                "line_start": primary_line,
                "line_end": primary_line,
                "column_start": 5,
                "column_end": 8,
                "is_primary": true,
                "label": "expected `i32`, found `&str`"
            },
            {
                "file_name": "src/lib.rs",
                "line_start": related_line,
                "line_end": related_line,
                "column_start": 1,
                "column_end": 2,
                "is_primary": false,
                "label": "expected due to this"
            }
        ],
        "children": []
    })
}

#[test]
fn cargo_json_array_emits_one_finding_per_diagnostic_with_spans() {
    let payload = json!([
        rustc_diagnostic("E0308", "error", "mismatched types", 10, 20),
        rustc_diagnostic(
            "unused_variables",
            "warning",
            "unused variable: `x`",
            30,
            30
        ),
    ]);
    let found = findings(&payload);
    let provider_found = from_provider(&found, CARGO_JSON);
    assert_eq!(
        provider_found.len(),
        2,
        "one provider finding per diagnostic"
    );

    let error = provider_found
        .iter()
        .find(|f| f.path == "/0")
        .expect("diagnostic 0");
    assert_eq!(error.kind, "rust_compile_error");
    assert_eq!(error.severity.as_deref(), Some("error"));
    let primary = error.primary_span.as_ref().expect("primary span");
    assert_eq!(primary.start_line, 10);
    assert_eq!(primary.role, "primary");
    assert_eq!(error.related_spans.len(), 1);
    assert_eq!(error.related_spans[0].start_line, 20);
    assert_eq!(error.related_spans[0].role, "related");

    let warning = provider_found
        .iter()
        .find(|f| f.path == "/1")
        .expect("diagnostic 1");
    assert_eq!(warning.kind, "warning");
    assert_eq!(warning.severity.as_deref(), Some("warning"));

    assert_ne!(
        error.fingerprint, warning.fingerprint,
        "distinct diagnostics must remain distinct"
    );
}

#[test]
fn cargo_json_reordering_preserves_the_same_fingerprint_set() {
    let forward = json!([
        rustc_diagnostic("E0308", "error", "mismatched types", 10, 20),
        rustc_diagnostic("E0502", "error", "cannot borrow as mutable", 40, 45),
    ]);
    let reversed = json!([
        rustc_diagnostic("E0502", "error", "cannot borrow as mutable", 40, 45),
        rustc_diagnostic("E0308", "error", "mismatched types", 10, 20),
    ]);

    let mut forward_fps: Vec<_> = findings(&forward)
        .into_iter()
        .filter_map(|f| f.fingerprint)
        .collect();
    let mut reversed_fps: Vec<_> = findings(&reversed)
        .into_iter()
        .filter_map(|f| f.fingerprint)
        .collect();
    forward_fps.sort();
    reversed_fps.sort();
    assert_eq!(forward_fps, reversed_fps);
}

#[test]
fn cargo_json_fingerprint_is_stable_across_line_shifts() {
    let original = json!([rustc_diagnostic(
        "E0308",
        "error",
        "mismatched types",
        10,
        20
    )]);
    let shifted = json!([rustc_diagnostic(
        "E0308",
        "error",
        "mismatched types",
        500,
        501
    )]);
    let original_fp = findings(&original)[0].fingerprint.clone();
    let shifted_fp = findings(&shifted)[0].fingerprint.clone();
    assert_eq!(original_fp, shifted_fp);
}

#[test]
fn cargo_json_ndjson_text_surfaces_one_representative_diagnostic_at_a_real_path() {
    let ndjson = format!(
        "{}\n{}\n{}\n",
        serde_json::to_string(&json!({"reason": "compiler-artifact", "target": {}})).unwrap(),
        serde_json::to_string(&rustc_diagnostic(
            "E0308",
            "error",
            "mismatched types",
            10,
            20
        ))
        .unwrap(),
        serde_json::to_string(&json!({"reason": "build-finished", "success": false})).unwrap(),
    );
    let payload = json!({"stdout": {"text": ndjson}});
    let found = findings(&payload);
    let finding = found
        .iter()
        .find(|f| f.path == "/stdout/text")
        .expect("representative diagnostic at the real stdout.text pointer");
    assert_eq!(finding.kind, "rust_compile_error");
    assert_eq!(finding.line_range.as_ref().unwrap().start, 2);
    assert_eq!(
        finding.extra.get("provider_ndjson_lines_scanned"),
        Some(&json!(3))
    );
}

#[test]
fn cargo_json_malformed_array_items_are_skipped_not_fatal() {
    let payload = json!([
        {"reason": "build-finished", "success": false},
        rustc_diagnostic("E0308", "error", "mismatched types", 10, 20),
        {"totally": "unrelated"},
    ]);
    let found = findings(&payload);
    let provider_found = from_provider(&found, CARGO_JSON);
    assert_eq!(
        provider_found.len(),
        1,
        "non-diagnostic items must be skipped, not fatal"
    );
    assert_eq!(provider_found[0].path, "/1");
}

#[test]
fn cargo_json_handles_unicode_message_without_panicking() {
    let payload = json!([rustc_diagnostic(
        "E0308",
        "error",
        "mismatched types: expected `héllo`, found `wörld` 🎉",
        10,
        20
    )]);
    let found = findings(&payload);
    let provider_found = from_provider(&found, CARGO_JSON);
    assert_eq!(provider_found.len(), 1);
    assert!(provider_found[0].fingerprint.is_some());
}

#[test]
fn unrelated_json_declines_cargo_provider() {
    let payload = json!({"foo": "bar"});
    assert!(findings(&payload).is_empty());
    assert_eq!(detect_coding_provider(&payload), None);
}

// ---------------------------------------------------------------------------
// Cargo libtest JSON events
// ---------------------------------------------------------------------------

fn libtest_events() -> Value {
    json!([
        {"type": "suite", "event": "started", "test_count": 2},
        {"type": "test", "event": "started", "name": "tests::foo"},
        {"type": "test", "name": "tests::foo", "event": "ok"},
        {
            "type": "test",
            "name": "tests::bar",
            "event": "failed",
            "stdout": "thread 'tests::bar' panicked at src/lib.rs:10:5:\nassertion failed: left == right\n"
        },
        {"type": "suite", "event": "failed", "passed": 1, "failed": 1}
    ])
}

#[test]
fn cargo_libtest_array_emits_finding_only_for_the_failed_test() {
    let found = findings(&libtest_events());
    let provider_found = from_provider(&found, CARGO_LIBTEST);
    assert_eq!(provider_found.len(), 1, "only the one failed test event");
    let failure = provider_found[0];
    assert_eq!(failure.path, "/3");
    assert_eq!(failure.kind, "test_failure");
    let span = failure.primary_span.as_ref().expect("panic location span");
    assert_eq!(span.path.as_deref(), Some("src/lib.rs"));
    assert_eq!(span.start_line, 10);
    assert_eq!(span.start_column, Some(5));
}

#[test]
fn cargo_libtest_ndjson_text_surfaces_one_representative_event() {
    let ndjson = libtest_events()
        .as_array()
        .unwrap()
        .iter()
        .map(|line| serde_json::to_string(line).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let payload = json!({"stderr": {"text": ndjson}});
    let found = findings(&payload);
    let finding = found.iter().find(|f| f.path == "/stderr/text").unwrap();
    assert_eq!(finding.kind, "test_failure");
    assert_eq!(finding.line_range.as_ref().unwrap().start, 4);
}

#[test]
fn cargo_libtest_no_failed_events_yields_no_findings() {
    // The numeric `"failed": 0` field on the terminal suite event is real,
    // legitimate territory for the *generic* field-name detector (it does
    // not special-case "zero means nothing failed") — that pre-existing
    // behavior is out of scope here. What this provider must guarantee is
    // that *it* stays silent when no test actually failed.
    let payload = json!([
        {"type": "suite", "event": "started", "test_count": 1},
        {"type": "test", "name": "tests::foo", "event": "ok"},
        {"type": "suite", "event": "ok", "passed": 1, "failed": 0}
    ]);
    assert!(from_provider(&findings(&payload), CARGO_LIBTEST).is_empty());
}

// ---------------------------------------------------------------------------
// pytest text fallback
// ---------------------------------------------------------------------------

fn pytest_text_output(extra_summary: &str) -> String {
    format!(
        "============================= FAILURES ==============================\n\
         tests/test_checkout.py:42: AssertionError\n\
         =========================== short test summary info ===========================\n\
         FAILED tests/test_checkout.py::test_total - AssertionError: expected 19 got 21\n\
         FAILED tests/test_checkout.py::test_discount - AssertionError: discount mismatch\n\
         ========================= 2 failed, 5 passed{extra_summary} in 1.23s =========================\n"
    )
}

#[test]
fn pytest_text_fallback_picks_first_failure_with_real_path_and_line() {
    let payload = json!({"stdout": {"text": pytest_text_output("")}});
    let found = findings(&payload);
    let finding = found.iter().find(|f| f.path == "/stdout/text").unwrap();
    assert_eq!(finding.kind, "test_failure");
    assert_eq!(finding.line_range.as_ref().unwrap().start, 4);
    assert_eq!(
        finding.extra.get("provider_failed_lines_seen"),
        Some(&json!(2))
    );
    assert!(finding.extra.get("provider_exhaustive").is_none());
}

#[test]
fn pytest_text_fallback_narrowed_run_is_marked_non_exhaustive() {
    let payload = json!({"stdout": {"text": pytest_text_output(", 3 deselected")}});
    let found = findings(&payload);
    let finding = found.iter().find(|f| f.path == "/stdout/text").unwrap();
    assert_eq!(
        finding.extra.get("provider_exhaustive"),
        Some(&json!(false))
    );
}

#[test]
fn pytest_text_fallback_handles_unicode_nodeid() {
    let text = "FAILED tests/test_ünïcödé.py::test_émoji_🎉 - AssertionError: 🚨\n";
    let payload = json!({"stderr": {"text": text}});
    let found = findings(&payload);
    let finding = found.iter().find(|f| f.path == "/stderr/text").unwrap();
    assert!(finding.title.as_deref().unwrap().contains("test_émoji_🎉"));
}

#[test]
fn plain_text_without_failure_markers_declines_pytest_text_provider() {
    let payload = json!({"stdout": {"text": "collected 3 items\n3 passed in 0.01s\n"}});
    assert!(findings(&payload).is_empty());
}

#[test]
fn pytest_json_report_and_text_fallback_agree_on_identity_fields_for_equivalent_evidence() {
    // Structured and text-fallback evidence intentionally keep distinct
    // `source`/classifier tags (so a caller can tell which parser produced a
    // finding), so their final fingerprints legitimately differ. What must
    // agree is the underlying identity *content* extracted for the same
    // logical failure: same subject, same outcome, same message.
    let nodeid = "tests/test_checkout.py::test_discount";
    let message = "AssertionError: expected 19 got 21";

    let json_report = json!({
        "summary": {"collected": 1, "passed": 0, "failed": 1, "total": 1},
        "exitcode": 1,
        "tests": [{
            "nodeid": nodeid,
            "outcome": "failed",
            "call": {"outcome": "failed", "longrepr": message}
        }]
    });
    let text = json!({"stdout": {"text": format!("FAILED {nodeid} - {message}\n")}});

    let json_found = findings(&json_report);
    let text_found = findings(&text);
    let from_json = from_provider(&json_found, PYTEST_JSON_REPORT)[0];
    let from_text = from_provider(&text_found, PYTEST_TEXT)[0];
    assert_eq!(from_json.kind, from_text.kind);
    assert_eq!(from_json.severity, from_text.severity);
    assert!(from_json.title.as_deref().unwrap().contains(nodeid));
    assert!(from_text.title.as_deref().unwrap().contains(nodeid));
}

// ---------------------------------------------------------------------------
// Golden snapshots
// ---------------------------------------------------------------------------

const GOLDEN_CASES: &[&str] = &[
    "pytest-json-report",
    "cargo-json-diagnostics",
    "cargo-libtest-json",
];

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("providers")
}

fn assert_golden(name: &str) {
    let dir = fixtures_dir();
    let payload: Value =
        serde_json::from_slice(&std::fs::read(dir.join(format!("{name}.json"))).unwrap()).unwrap();
    let found = findings(&payload);

    let mut actual = serde_json::to_string_pretty(&serde_json::to_value(&found).unwrap())
        .expect("findings serialize");
    actual.push('\n');

    let expected_path = dir.join(format!("{name}.expected.json"));
    if std::env::var_os("PROG_PROVIDERS_UPDATE").is_some() {
        std::fs::write(&expected_path, &actual).expect("write golden");
        eprintln!("updated {}", expected_path.display());
    } else {
        let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|error| {
            panic!(
                "missing golden {} ({error}). Regenerate with \
                 PROG_PROVIDERS_UPDATE=1 cargo test -p prog-core --test providers",
                expected_path.display()
            )
        });
        assert_eq!(actual, expected, "golden mismatch for {name}");
    }
}

#[test]
fn golden_provider_snapshots_are_stable() {
    for name in GOLDEN_CASES {
        assert_golden(name);
    }
}

#[test]
fn provider_findings_are_deterministic_across_repeated_calls() {
    for name in GOLDEN_CASES {
        let dir = fixtures_dir();
        let payload: Value =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{name}.json"))).unwrap())
                .unwrap();
        let first = serde_json::to_string(&findings(&payload)).unwrap();
        for _ in 0..4 {
            assert_eq!(first, serde_json::to_string(&findings(&payload)).unwrap());
        }
    }
}

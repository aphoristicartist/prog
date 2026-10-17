use std::collections::BTreeMap;

use prog_core::{finding_derivation_is_complete, normalize_coding_output};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    argv: Vec<String>,
    stdout: String,
    stderr: String,
    capture_complete: bool,
    exit_code: Option<i32>,
    expected: Option<Value>,
    #[serde(default)]
    equivalence_group: Option<String>,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(include_str!("fixtures/providers/cases.json")).unwrap()
}

fn golden_projection(result: &prog_core::CodingProviderResult) -> Value {
    json!({
        "provider": result.provider,
        "input_format": result.input_format,
        "complete": result.complete,
        "selection": result.selection,
        "normalized": result.normalized
    })
}

fn failure_identity(normalized: &Value) -> Option<Value> {
    for test in normalized.get("tests").and_then(Value::as_array)? {
        if matches!(test["status"].as_str(), Some("failed" | "error")) {
            return Some(json!({
                "kind": "test",
                "node_id": test["node_id"],
                "status": test["status"],
                "message": test.get("message")
            }));
        }
    }
    for diagnostic in normalized.get("diagnostics").and_then(Value::as_array)? {
        if diagnostic["severity"] == "error" {
            return Some(json!({
                "kind": "diagnostic",
                "severity": diagnostic["severity"],
                "diagnostic_code": diagnostic.get("diagnostic_code"),
                "message": diagnostic["message"]
            }));
        }
    }
    None
}

#[test]
fn fixture_matrix_matches_golden_provider_output() {
    let mut equivalence_groups = BTreeMap::<String, Value>::new();
    for case in cases() {
        let actual = normalize_coding_output(
            &case.argv,
            &case.stdout,
            &case.stderr,
            case.capture_complete,
            case.exit_code,
        );
        match (actual, case.expected) {
            (None, None) => {}
            (Some(actual), Some(expected)) => {
                assert_eq!(golden_projection(&actual), expected, "{}", case.name);
                if actual.input_format == "pytest_json_report" {
                    let report: Value = serde_json::from_str(&case.stdout).unwrap();
                    for test in actual.normalized["tests"].as_array().unwrap() {
                        assert!(
                            test.get("evidence_line").is_none(),
                            "JSON indices are not text lines"
                        );
                        let source = report
                            .pointer(test["report_pointer"].as_str().unwrap())
                            .unwrap();
                        assert_eq!(source["nodeid"], test["node_id"]);
                        assert_eq!(source["outcome"], test["status"]);
                    }
                }
                assert!(actual.limits.lines_examined <= actual.limits.max_lines);
                assert!(actual.limits.items_emitted <= actual.limits.max_items);
                assert!(actual.limits.output_bytes <= actual.limits.max_output_bytes);
                if let Some(group) = case.equivalence_group {
                    let identity = failure_identity(&actual.normalized).unwrap();
                    if let Some(prior) = equivalence_groups.insert(group.clone(), identity.clone())
                    {
                        assert_eq!(prior, identity, "{group}");
                    }
                }
            }
            (actual, expected) => panic!(
                "{} provider match mismatch: actual={actual:?} expected={expected:?}",
                case.name
            ),
        }
    }
}

#[test]
fn provider_normalization_and_selection_exhaustion_are_independent() {
    let targeted = normalize_coding_output(
        &["pytest".to_string(), "-x".to_string()],
        "FAILED tests/test_api.py::test_one - AssertionError\n1 failed in 0.01s\n",
        "",
        true,
        Some(1),
    )
    .unwrap();
    let payload = json!({
        "stdout": {"format": "text", "text": "raw retained evidence"},
        "provider": targeted
    });
    assert_eq!(payload["stdout"]["text"], "raw retained evidence");
    assert_eq!(payload["provider"]["complete"], true);
    assert_eq!(payload["provider"]["selection"]["exhaustive"], false);
    assert!(finding_derivation_is_complete(&payload));
}

#[test]
fn observed_exit_and_late_failures_constrain_completion() {
    let passing_json = json!({
        "exitcode": 0, "summary": {"passed": 1, "total": 1},
        "tests": [{"nodeid": "test_api.py::test_one", "outcome": "passed"}]
    })
    .to_string();
    for (name, argv, stdout, stderr, exit_code) in [
        (
            "interrupted pytest",
            vec!["pytest"],
            "1 passed in 0.01s",
            "",
            Some(2),
        ),
        (
            "missing process exit",
            vec!["pytest"],
            "1 passed in 0.01s",
            "",
            None,
        ),
        (
            "JSON process mismatch",
            vec!["pytest"],
            passing_json.as_str(),
            "",
            Some(1),
        ),
        (
            "other stream failure",
            vec!["pytest"],
            passing_json.as_str(),
            "FAILED test_api.py::test_late - AssertionError",
            Some(0),
        ),
        (
            "internal error",
            vec!["pytest"],
            "1 passed in 0.01s",
            "INTERNALERROR> plugin crashed",
            Some(0),
        ),
        (
            "malformed duration",
            vec!["pytest"],
            "1 passed in unknown",
            "",
            Some(0),
        ),
        (
            "late doctest failure",
            vec!["cargo", "test", "--no-fail-fast"],
            "test result: ok. 1 passed; 0 failed",
            "error: doctest failed, to rerun pass --doc",
            Some(101),
        ),
        (
            "unexplained Cargo failure",
            vec!["cargo", "test", "--no-fail-fast"],
            "test result: ok. 1 passed; 0 failed",
            "",
            Some(101),
        ),
        (
            "unknown structured record",
            vec!["cargo", "check"],
            "{\"reason\":\"future-diagnostic\",\"level\":\"error\"}\n{\"reason\":\"build-finished\",\"success\":true}",
            "",
            Some(0),
        ),
        (
            "missing compiler message",
            vec!["cargo", "check"],
            "{\"reason\":\"compiler-message\"}\n{\"reason\":\"build-finished\",\"success\":true}",
            "",
            Some(0),
        ),
        (
            "failed build",
            vec!["cargo", "check"],
            "{\"reason\":\"build-finished\",\"success\":false}",
            "",
            Some(0),
        ),
    ] {
        let argv = argv.into_iter().map(str::to_string).collect::<Vec<_>>();
        let normalized = normalize_coding_output(&argv, stdout, stderr, true, exit_code).unwrap();
        assert!(!normalized.selection.exhaustive, "{name}: {normalized:#?}");
        assert!(!normalized.warnings.is_empty(), "{name}");
    }
}

#[test]
fn cargo_failure_footer_preserves_exact_harness_exhaustion() {
    let stdout = "test tests::addition ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
    let footer = "error: test failed, to rerun pass `-p example --lib`\n";
    for (argv, stderr, exhaustive) in [
        (
            vec!["cargo", "test", "-p", "example", "--lib"],
            footer.to_string(),
            true,
        ),
        (
            vec!["cargo", "test", "--workspace"],
            footer.to_string(),
            false,
        ),
        (
            vec!["cargo", "test", "--workspace", "--no-fail-fast"],
            format!("{footer}error[E0308]: mismatched types\n"),
            false,
        ),
    ] {
        let argv = argv.into_iter().map(str::to_string).collect::<Vec<_>>();
        let result = normalize_coding_output(&argv, stdout, &stderr, true, Some(101)).unwrap();
        assert!(result.complete);
        assert_eq!(result.selection.exhaustive, exhaustive, "{result:#?}");
    }
}

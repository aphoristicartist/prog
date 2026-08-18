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
        );
        match (actual, case.expected) {
            (None, None) => {}
            (Some(actual), Some(expected)) => {
                assert_eq!(golden_projection(&actual), expected, "{}", case.name);
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

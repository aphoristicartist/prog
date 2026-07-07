use prog_core::{FindingOptions, RedactionPolicy, ranked_findings};
use serde_json::{Value, json};

#[test]
fn run_failure_sections_rank_first_with_commands_and_line_range() {
    let payload = json!({
        "format": "run",
        "command": {
            "success": false,
            "exit_code": 1,
            "timed_out": false,
            "spawn_error": null
        },
        "stderr": {
            "format": "text",
            "text": "Traceback (most recent call last):\nAssertionError: expected 19 got 21"
        },
        "failure_sections": [{
            "index": 0,
            "kind": "python",
            "stream": "stderr",
            "line_start": 1,
            "line_end": 4,
            "reason": "Python traceback",
            "priority": 90,
            "lines": [
                "Traceback (most recent call last):",
                "  File \"tests/test_checkout.py\", line 42, in test_total",
                "    assert total == 19",
                "AssertionError: expected 19 got 21"
            ]
        }]
    });
    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            goal: Some("find the root cause".to_string()),
            cursor: Some("pc1_demo".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();

    assert!(!findings.is_empty());
    let first = &findings[0];
    assert_eq!(first.rank, 1);
    assert_eq!(first.kind, "python_traceback");
    assert_eq!(first.path, "/failure_sections/0");
    assert!(first.confidence >= 0.95);
    assert_eq!(first.line_range.as_ref().unwrap().start, 1);
    assert_eq!(first.line_range.as_ref().unwrap().end, 4);
    assert_eq!(
        first.commands.expand.as_deref(),
        Some("prog expand pc1_demo --path /failure_sections/0")
    );
    assert_eq!(
        first.commands.evidence.as_deref(),
        Some("prog evidence pc1_demo --path /failure_sections/0")
    );
}

#[test]
fn command_timeout_beats_generic_nonzero_exit() {
    let payload = json!({
        "format": "run",
        "command": {
            "success": false,
            "exit_code": null,
            "timed_out": true,
            "spawn_error": null
        },
        "failure_sections": [{
            "kind": "timeout",
            "stream": "stderr",
            "line_start": 1,
            "line_end": 1,
            "reason": "command exceeded --timeout-ms",
            "priority": 95,
            "lines": ["command timed out"]
        }]
    });

    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            goal: Some("why did the command fail".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();

    assert_eq!(findings[0].kind, "command_timeout");
    assert_eq!(findings[0].path, "/failure_sections/0");
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == "nonzero_exit")
    );
}

#[test]
fn generic_error_fields_rank_without_run_payload() {
    let payload = json!({
        "ok": false,
        "data": {"items": [1, 2, 3]},
        "errors": [{
            "message": "Error: failed to connect to database",
            "code": "E_DB"
        }],
        "warnings": ["deprecated field"]
    });

    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            goal: Some("find the root cause".to_string()),
            cursor: Some("pc1_api".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();

    assert_eq!(findings[0].kind, "generic_error_field");
    assert_eq!(findings[0].path, "/errors");
    assert!(
        findings
            .iter()
            .any(|finding| finding.path == "/errors/0/message")
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == "warning" && finding.path == "/warnings")
    );
}

#[test]
fn stack_trace_strings_are_detected_inside_nested_payloads() {
    let payload = json!({
        "events": [{
            "level": "info",
            "message": "startup complete"
        }, {
            "level": "error",
            "message": "Traceback (most recent call last):\n  File \"app.py\", line 3\nValueError: nope"
        }]
    });

    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            goal: Some("inspect logs".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();

    assert_eq!(findings[0].path, "/events/1/message");
    assert_eq!(findings[0].kind, "stack_trace");
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == "diagnostic" && finding.path == "/events/1")
    );
}

#[test]
fn benign_payload_returns_no_findings() {
    let payload = json!({
        "items": [{"id": 1, "title": "Open issue"}],
        "summary": "all tests passed",
        "command": {"success": true, "exit_code": 0}
    });

    let findings = ranked_findings(&payload, &FindingOptions::default()).unwrap();
    assert!(findings.is_empty(), "{findings:#?}");
}

#[test]
fn scope_path_limits_ranking_to_selected_subtree() {
    let payload = json!({
        "left": {"errors": [{"message": "Error: left failed"}]},
        "right": {"errors": [{"message": "Error: right failed"}]}
    });

    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            scope_path: Some("/right".to_string()),
            cursor: Some("pc1_scope".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();

    assert!(!findings.is_empty());
    assert!(
        findings
            .iter()
            .all(|finding| finding.path.starts_with("/right"))
    );
    assert_eq!(findings[0].path, "/right/errors");
}

#[test]
fn missing_scope_returns_no_findings_and_invalid_pointer_errors() {
    let payload = json!({"errors": [{"message": "Error: failed"}]});

    let missing = ranked_findings(
        &payload,
        &FindingOptions {
            scope_path: Some("/missing".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();
    assert!(missing.is_empty());

    let invalid = ranked_findings(
        &payload,
        &FindingOptions {
            scope_path: Some("missing-leading-slash".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap_err();
    assert_eq!(invalid.kind(), "bad_pointer");
}

#[test]
fn findings_do_not_copy_secret_values_into_metadata() {
    let raw_secret = "SUPER-SECRET-TOKEN";
    let payload = json!({
        "error": format!("failed with token={raw_secret}"),
        "detail": "Authorization: Bearer ALSOSECRET"
    });
    let (redacted, _paths) = RedactionPolicy::default().apply_persistence(&payload);

    let findings = ranked_findings(
        &redacted,
        &FindingOptions {
            cursor: Some("pc1_redacted".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();

    let rendered = serde_json::to_string(&findings).unwrap();
    assert!(!rendered.contains(raw_secret));
    assert!(!rendered.contains("ALSOSECRET"));
    assert!(
        findings
            .iter()
            .all(|finding| !finding.reason.contains("failed with token"))
    );
}

#[test]
fn ranking_is_deterministic() {
    let payload = json!({
        "errors": [{"message": "Error: failed"}],
        "diagnostics": [{"severity": "error", "message": "panic"}],
        "warnings": [{"message": "warning: deprecated"}]
    });
    let options = FindingOptions {
        goal: Some("find root cause".to_string()),
        cursor: Some("pc1_deterministic".to_string()),
        ..FindingOptions::default()
    };

    let left = ranked_findings(&payload, &options).unwrap();
    let right = ranked_findings(&payload, &options).unwrap();
    assert_eq!(
        serde_json::to_value(left).unwrap(),
        serde_json::to_value(right).unwrap()
    );
}

#[test]
fn limit_zero_returns_no_findings() {
    let payload = json!({"errors": [{"message": "Error: failed"}]});
    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            limit: 0,
            ..FindingOptions::default()
        },
    )
    .unwrap();
    assert!(findings.is_empty());
}

#[test]
fn fixture_run_failure_has_failure_section_as_top_finding() {
    let fixture = std::fs::read_to_string("../../lenses/fixtures/run-failure.json").unwrap();
    let payload: Value = serde_json::from_str(&fixture).unwrap();
    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            goal: Some("why did tests fail".to_string()),
            cursor: Some("pc1_fixture".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();

    assert_eq!(findings[0].path, "/failure_sections/0");
    assert_eq!(findings[0].kind, "python_traceback");
    assert!(findings[0].redaction_state.is_none());
}

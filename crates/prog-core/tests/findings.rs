use prog_core::{
    CommandHintConfig, FindingOptions, RedactionPolicy, SourceSpanExactness, extract_source_spans,
    ranked_findings,
};
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
            // NAV_ALL pins the runnable format for every navigation command.
            hints: CommandHintConfig::NAV_ALL,
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
fn default_hints_remain_minimal_while_nav_all_emits_runnable_commands() {
    // Low-level callers keep a minimal default; CLI surfaces opt into NAV_ALL.
    let payload = json!({
        "errors": [{"message": "Error: boom"}]
    });
    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            cursor: Some("pc1_demo".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();

    let first = &findings[0];
    assert_eq!(
        first.commands.expand.as_deref(),
        Some("prog expand pc1_demo --path /errors")
    );
    assert_eq!(first.commands.evidence, None);
    assert_eq!(first.commands.inspect, None);
    assert_eq!(first.commands.search, None);

    // Opting into NAV_ALL surfaces every hint with identical cursor/path framing.
    let all = ranked_findings(
        &payload,
        &FindingOptions {
            cursor: Some("pc1_demo".to_string()),
            hints: CommandHintConfig::NAV_ALL,
            ..FindingOptions::default()
        },
    )
    .unwrap();
    let first = &all[0];
    assert_eq!(
        first.commands.expand.as_deref(),
        Some("prog expand pc1_demo --path /errors")
    );
    assert_eq!(
        first.commands.inspect.as_deref(),
        Some("prog inspect pc1_demo --goal 'investigate generic_error_field' --path /errors")
    );
    assert_eq!(
        first.commands.evidence.as_deref(),
        Some("prog evidence pc1_demo --path /errors")
    );
    assert_eq!(
        first.commands.search.as_deref(),
        Some("prog find pc1_demo --kind generic_error_field --path /errors")
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
fn structured_rustc_spans_attach_to_message_findings_without_parsing_prose() {
    let payload = json!({
        "message": "error[E0308]: mismatched types",
        "level": "error",
        "spans": [
            {
                "file_name": "src\\lib.rs",
                "line_start": 12,
                "line_end": 12,
                "column_start": 5,
                "column_end": 9,
                "is_primary": true
            },
            {
                "file_name": "src/types.rs",
                "line_start": 4,
                "line_end": 4,
                "column_start": 1,
                "column_end": 8,
                "is_primary": false
            }
        ]
    });

    let findings = ranked_findings(&payload, &FindingOptions::default()).unwrap();
    let message = findings
        .iter()
        .find(|finding| finding.path == "/message")
        .expect("message finding");
    let primary = message.primary_span.as_ref().expect("primary source span");
    assert_eq!(primary.path.as_deref(), Some("src/lib.rs"));
    assert_eq!(primary.start_line, 12);
    assert_eq!(primary.start_column, Some(5));
    assert_eq!(primary.end_column, Some(9));
    assert_eq!(primary.role, "primary");
    assert_eq!(primary.exactness, SourceSpanExactness::Exact);
    assert_eq!(message.related_spans.len(), 1);
    assert_eq!(
        message.related_spans[0].path.as_deref(),
        Some("src/types.rs")
    );
    assert_eq!(message.related_spans[0].role, "related");
}

#[test]
fn sarif_locations_keep_external_uris_and_bound_related_locations() {
    let payload = json!({
        "level": "error",
        "message": {"text": "unsafe operation"},
        "locations": [{
            "physicalLocation": {
                "artifactLocation": {"uri": "git://example/repo/src/lib.rs"},
                "region": {"startLine": 8, "endLine": 10}
            }
        }],
        "relatedLocations": [{
            "physicalLocation": {
                "artifactLocation": {"uri": "https://example.test/generated.rs"},
                "region": {"startLine": 2, "startColumn": 1}
            }
        }]
    });

    let (primary, related) = extract_source_spans(&payload);
    let primary = primary.expect("SARIF primary location");
    assert_eq!(
        primary.uri.as_deref(),
        Some("git://example/repo/src/lib.rs")
    );
    assert_eq!(primary.start_line, 8);
    assert_eq!(primary.exactness, SourceSpanExactness::Range);
    assert_eq!(related.len(), 1);
    assert_eq!(
        related[0].uri.as_deref(),
        Some("https://example.test/generated.rs")
    );
    assert_eq!(related[0].exactness, SourceSpanExactness::Range);
}

#[test]
fn source_span_rejects_unsafe_or_malformed_locators() {
    for payload in [
        json!({"file_name": "../../private.rs", "line_start": 1}),
        json!({"file_name": "/Users/alice/private.rs", "line_start": 1}),
        json!({"file_name": "C:\\Users\\alice\\private.rs", "line_start": 1}),
        json!({"uri": "file:///Users/alice/private.rs", "line_start": 1}),
        json!({"file_name": "src/lib.rs", "line_start": 0}),
        json!({"file_name": "src/lib.rs", "line_start": 4, "line_end": 3}),
        json!({"file_name": "[REDACTED:path]", "line_start": 1}),
    ] {
        assert_eq!(extract_source_spans(&payload), (None, Vec::new()));
    }
}

#[test]
fn source_span_keeps_one_locator_and_does_not_promote_generated_spans() {
    let (primary, related) = extract_source_spans(&json!({
        "file_name": "src/lib.rs",
        "uri": "https://example.test/ignored.rs",
        "line_start": 7
    }));
    let primary = primary.expect("relative path span");
    assert_eq!(primary.path.as_deref(), Some("src/lib.rs"));
    assert_eq!(primary.uri, None);
    assert!(related.is_empty());

    let (primary, related) = extract_source_spans(&json!({
        "spans": [{
            "file_name": "generated.rs",
            "line_start": 1,
            "is_generated": true
        }]
    }));
    assert!(primary.is_none());
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].role, "generated");
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
fn successful_test_summary_is_not_a_failure_finding() {
    let payload = json!({
        "stdout": "test tool_is_error_maps_to_structured_error ... ok\ntest result: ok. 17 passed; 0 failed; 0 ignored"
    });

    let findings = ranked_findings(&payload, &FindingOptions::default()).unwrap();
    assert!(findings.is_empty(), "unexpected findings: {findings:?}");
}

#[test]
fn explicit_log_error_in_scalar_text_is_inspectable() {
    let payload = json!(
        "2026-07-10T12:00:00Z WARN retrying request\n2026-07-10T12:00:01Z ERROR database pool exhausted\nroot cause: stale credentials"
    );

    let findings = ranked_findings(&payload, &FindingOptions::default()).unwrap();
    assert_eq!(findings[0].kind, "log_error");
    assert_eq!(findings[0].path, "");
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
fn failure_section_stream_and_kind_are_not_echoed_unless_safe() {
    // A secret placed in a structural field (failure_sections[].stream / .kind)
    // that the value-redactor does NOT classify as high-confidence survives
    // persistence verbatim (Low -> preserved-and-flagged by default, or None).
    // The findings engine must NOT then echo that raw value into the agent-facing
    // reason/extra — only known-safe stream/kind labels may appear.
    let secret_blob = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"; // 44 chars -> Low
    let secret_kind = "ghp_opaquetoken1234567890"; // <40 opaque -> None
    let payload = json!({
        "failure_sections": [{
            "kind": secret_kind,
            "stream": secret_blob,
            "text": "error[E0308]: mismatched types",
            "line_start": 1,
            "line_end": 2
        }]
    });
    let (redacted, _paths) = RedactionPolicy::default().apply_persistence(&payload);
    // Redaction preserves both (Low/None), so they survive into the payload...
    let persisted = serde_json::to_string(&redacted).unwrap();
    assert!(persisted.contains(secret_blob));
    assert!(persisted.contains(secret_kind));

    // ...but the findings engine must not echo them into reason/extra.
    let findings = ranked_findings(
        &redacted,
        &FindingOptions {
            cursor: Some("pc1_section".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();
    let rendered = serde_json::to_string(&findings).unwrap();
    assert!(
        !rendered.contains(secret_blob),
        "section stream leaked: {rendered}"
    );
    assert!(
        !rendered.contains(secret_kind),
        "section kind leaked: {rendered}"
    );
    assert!(!findings.is_empty());
    // A known-safe stream IS still surfaced (regression guard on the allowlist).
    let safe = json!({
        "failure_sections": [{
            "kind": "rust",
            "stream": "stderr",
            "text": "error[E0308]: bad",
            "line_start": 1,
            "line_end": 2
        }]
    });
    let safe_findings = ranked_findings(&safe, &FindingOptions::default()).unwrap();
    let safe_rendered = serde_json::to_string(&safe_findings).unwrap();
    assert!(safe_rendered.contains("stderr"));
    assert!(safe_rendered.contains("rust"));
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

#[test]
fn rust_compile_error_takes_precedence_over_generic_compile_error() {
    // A rustc diagnostic (`error[E0308]`) must classify as rust_compile_error,
    // never double-counted as the generic compile_error kind.
    let payload = json!({
        "failure_sections": [{
            "kind": "rust",
            "stream": "stderr",
            "line_start": 1,
            "line_end": 3,
            "priority": 80,
            "lines": [
                "error[E0308]: mismatched types",
                "  --> src/lib.rs:10:5",
                "expected `u32`, found `i32`"
            ]
        }],
        "note": "error: could not compile `crate` due to previous error"
    });

    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            goal: Some("why did the build fail".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();

    assert_eq!(findings[0].kind, "rust_compile_error");
    assert_eq!(findings[0].path, "/failure_sections/0");
    assert!(
        findings
            .iter()
            .all(|finding| finding.kind != "compile_error"
                || finding.path != "/failure_sections/0"),
        "rustc diagnostic must not also be tagged compile_error at the same path"
    );
    // The standalone `could not compile` string still classifies as compile_error.
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == "compile_error" && finding.path == "/note")
    );
}

#[test]
fn compile_error_detects_cargo_rustc_and_build_framing() {
    for (label, text) in [
        ("cargo", "cargo: error: could not compile `foo`"),
        ("rustc", "rustc: error: unresolved import `bar`"),
        (
            "generic",
            "error: could not compile `baz` due to 2 previous errors",
        ),
        ("gmake", "gmake: *** [Makefile:4: all] Error 2"),
        (
            "ninja",
            "ninja: build stopped: subcommand failed with error",
        ),
    ] {
        let payload = json!({ "failure_sections": [{
            "kind": "build",
            "line_start": 1,
            "line_end": 1,
            "priority": 70,
            "lines": [text]
        }]});
        let findings = ranked_findings(
            &payload,
            &FindingOptions {
                goal: Some("find the root cause".to_string()),
                ..FindingOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            findings[0].kind, "compile_error",
            "{label} framing should classify as compile_error"
        );
        assert_eq!(
            findings[0].source.as_deref(),
            Some("generic.run.failure_sections")
        );
        assert_eq!(findings[0].severity.as_deref(), Some("error"));
    }

    // String-signal path (no failure section) lands at the lower confidence.
    let payload = json!({ "message": "error: could not compile `foo`" });
    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            goal: Some("find the root cause".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();
    assert_eq!(findings[0].kind, "compile_error");
    assert_eq!(
        findings[0].source.as_deref(),
        Some("generic.string_pattern")
    );
    assert!((0.80..=0.85).contains(&findings[0].confidence));
}

#[test]
fn test_name_detects_pytest_cargo_gtest_and_mocha_summaries() {
    for (label, text) in [
        ("pytest", "tests/test_checkout.py::test_total FAILED"),
        ("cargo", "test foo::bar::tests::test_case ... FAILED"),
        (
            "gtest",
            "Note: Google Test filter = TestSuite.Case\n[  FAILED  ] Suite.Case",
        ),
        ("mocha", "  2 passing (3s)\n  1 failing"),
    ] {
        let payload = json!({ "failure_sections": [{
            "kind": "test",
            "line_start": 1,
            "line_end": 1,
            "priority": 70,
            "lines": [text]
        }]});
        let findings = ranked_findings(
            &payload,
            &FindingOptions {
                goal: Some("which test failed".to_string()),
                ..FindingOptions::default()
            },
        )
        .unwrap();
        let top = findings
            .iter()
            .find(|finding| finding.kind == "test_name")
            .unwrap_or_else(|| {
                panic!("{label} framing should classify as test_name: {findings:#?}")
            });
        assert_eq!(top.path, "/failure_sections/0");
        assert_eq!(top.severity.as_deref(), Some("error"));
    }

    // String-signal path: a bare pytest nodeid is enough to flag a failing test.
    let payload = json!({ "note": "tests/test_x.py::test_case" });
    let findings = ranked_findings(&payload, &FindingOptions::default()).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == "test_name" && finding.path == "/note")
    );
}

#[test]
fn diff_hunk_detected_from_string_and_diff_key() {
    let payload = json!({
        "review": {
            "diff": "diff --git a/foo b/foo\nindex 1..2 100644\n--- a/foo\n+++ b/foo\n@@ -1,3 +1,4 @@\n old\n+new\n"
        }
    });
    let findings = ranked_findings(
        &payload,
        &FindingOptions {
            goal: Some("review the diff".to_string()),
            cursor: Some("pc1_diff".to_string()),
            ..FindingOptions::default()
        },
    )
    .unwrap();

    let diff = findings
        .iter()
        .find(|finding| finding.kind == "diff_hunk")
        .expect("diff markers should classify as diff_hunk");
    assert_eq!(diff.path, "/review/diff");
    assert_eq!(diff.severity, None);
    assert!((0.55..=0.65).contains(&diff.confidence));
    // A diff surfaces first under a DiffReview intent.
    assert_eq!(findings[0].kind, "diff_hunk");
}

#[test]
fn build_inspect_response_assembles_ranked_response_and_round_trips_serde() {
    use prog_core::{InspectRequest, build_inspect_response};

    let payload = json!({
        "format": "run",
        "command": {"success": false, "exit_code": 1, "timed_out": false, "spawn_error": null},
        "failure_sections": [{
            "kind": "python",
            "stream": "stderr",
            "line_start": 1,
            "line_end": 2,
            "priority": 90,
            "lines": ["Traceback (most recent call last):", "ValueError: nope"]
        }]
    });

    let request = InspectRequest::builder("pc1_inspect")
        .goal("why did tests fail")
        .build();
    let response = build_inspect_response(&payload, &request).unwrap();

    assert_eq!(response.schema, "prog.inspect");
    assert_eq!(response.cursor, "pc1_inspect");
    assert_eq!(response.goal, "why did tests fail");
    assert_eq!(response.normalized_goal.as_deref(), Some("root_cause"));
    assert_eq!(response.findings[0].rank, 1);
    assert_eq!(response.findings[0].kind, "python_traceback");
    assert!(response.omitted.is_empty());
    assert!(response.cache.is_none());
    assert!(response.warnings.is_empty());

    // Default hints: only `expand` is populated.
    assert!(response.findings[0].commands.expand.is_some());
    assert_eq!(response.findings[0].commands.evidence, None);

    // Serde round-trip: deserialize(reserialize) is identical (contract stable).
    let serialized = serde_json::to_value(&response).unwrap();
    let round_tripped: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&serialized).unwrap()).unwrap();
    assert_eq!(serialized, round_tripped);
    let _: prog_core::InspectResponse = serde_json::from_value(serialized).unwrap();
}

#[test]
fn build_inspect_response_returns_empty_findings_for_limit_zero_and_missing_scope() {
    use prog_core::{InspectRequest, build_inspect_response};

    let payload = json!({"errors": [{"message": "Error: failed"}]});

    let limited =
        build_inspect_response(&payload, &InspectRequest::builder("pc1").limit(0).build()).unwrap();
    assert!(limited.findings.is_empty());

    let scoped = build_inspect_response(
        &payload,
        &InspectRequest::builder("pc1")
            .scope_path("/missing")
            .build(),
    )
    .unwrap();
    assert!(scoped.findings.is_empty());
}

#[test]
fn inspect_request_builder_and_default_cursor_required() {
    use prog_core::{CommandHintConfig, InspectRequest};

    let default = InspectRequest::default();
    assert_eq!(default.cursor, "");
    assert_eq!(default.hints, CommandHintConfig::NAV_EXPAND_ONLY);

    let built = InspectRequest::builder("pc1_build")
        .goal("summarize the issues")
        .scope_path("/errors")
        .limit(3)
        .hints(CommandHintConfig::NAV_ALL)
        .build();
    assert_eq!(built.cursor, "pc1_build");
    assert_eq!(built.goal.as_deref(), Some("summarize the issues"));
    assert_eq!(built.scope_path.as_deref(), Some("/errors"));
    assert_eq!(built.limit, 3);
    assert_eq!(built.hints, CommandHintConfig::NAV_ALL);
}

#[test]
fn generic_fingerprints_preserve_values_but_normalize_whitespace() {
    let options = FindingOptions::default();
    let first = ranked_findings(&json!({"message": "Error: expected value 12"}), &options)
        .unwrap()
        .remove(0);
    let whitespace = ranked_findings(
        &json!({"message": " Error:   expected\n value 12 "}),
        &options,
    )
    .unwrap()
    .remove(0);
    let different = ranked_findings(&json!({"message": "Error: expected value 13"}), &options)
        .unwrap()
        .remove(0);

    assert_eq!(first.fingerprint, whitespace.fingerprint);
    assert_ne!(first.fingerprint, different.fingerprint);
    assert_eq!(first.occurrence_id, whitespace.occurrence_id);
}

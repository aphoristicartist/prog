use prog_core::{
    FindingOptions, LensManifest, SearchOptions, evidence_block, ranked_findings_with_lens,
    search_payload, validate_lens_manifest,
};
use serde_json::json;

#[test]
fn cached_search_supports_text_regex_key_kind_and_scope() {
    let payload = json!({
        "safe": {"message": "all good"},
        "failures": [
            {"severity": "error", "message": "NullPointerException in checkout"},
            {"severity": "warning", "message": "retrying"}
        ]
    });
    let text = search_payload(
        &payload,
        "pc1_demo",
        &SearchOptions {
            query: Some("nullpointerexception".to_string()),
            ..SearchOptions::default()
        },
    )
    .unwrap();
    assert_eq!(text.hits[0].path, "/failures/0/message");
    assert_eq!(
        text.hits[0].commands.evidence.as_deref(),
        Some("prog evidence pc1_demo --path /failures/0/message")
    );

    let regex = search_payload(
        &payload,
        "pc1_demo",
        &SearchOptions {
            query: Some("Null.*checkout$".to_string()),
            regex: true,
            case_sensitive: true,
            ..SearchOptions::default()
        },
    )
    .unwrap();
    assert_eq!(regex.hits.len(), 1);

    let errors = search_payload(
        &payload,
        "pc1_demo",
        &SearchOptions {
            kind: Some("error".to_string()),
            scope_path: Some("/failures".to_string()),
            ..SearchOptions::default()
        },
    )
    .unwrap();
    assert!(errors.hits.iter().any(|hit| hit.path == "/failures/0"));
    assert!(
        errors
            .hits
            .iter()
            .all(|hit| hit.path.starts_with("/failures"))
    );
}

#[test]
fn search_and_evidence_are_bounded_and_preserve_redaction() {
    let payload = json!({
        "lines": (0..200)
            .map(|index| json!(format!("line {index} error token=[REDACTED:value] {}", "x".repeat(500))))
            .collect::<Vec<_>>()
    });
    let search = search_payload(
        &payload,
        "pc1_demo",
        &SearchOptions {
            query: Some("error".to_string()),
            limit: 3,
            max_nodes: 20,
            ..SearchOptions::default()
        },
    )
    .unwrap();
    assert_eq!(search.hits.len(), 3);
    assert!(!search.omitted.is_empty());
    assert!(serde_json::to_vec(&search).unwrap().len() < 16 * 1024);
    assert!(search.hits[0].redaction_state.as_ref().unwrap().redacted);

    let evidence = evidence_block(&payload, "pc1_demo", "/lines/0").unwrap();
    assert!(serde_json::to_vec(&evidence).unwrap().len() < 4 * 1024);
    assert!(evidence.redaction_state.as_ref().unwrap().redacted);
    assert_eq!(evidence.citations[0].path, "/lines/0");
}

#[test]
fn search_hits_preserve_structured_source_span_parity() {
    let payload = json!({
        "diagnostic": {
            "message": "Error: mismatched types",
            "file_name": "src/lib.rs",
            "line_start": 9,
            "column_start": 3,
            "line_end": 9,
            "column_end": 7,
            "label": "expected integer"
        }
    });
    let response = search_payload(
        &payload,
        "pc1_demo",
        &SearchOptions {
            query: Some("mismatched".to_string()),
            ..SearchOptions::default()
        },
    )
    .unwrap();
    let hit = response
        .hits
        .iter()
        .find(|hit| hit.path == "/diagnostic/message")
        .expect("matching diagnostic message hit");
    let span = hit
        .primary_span
        .as_ref()
        .expect("inherited structured span");
    assert_eq!(span.path.as_deref(), Some("src/lib.rs"));
    assert_eq!(span.label.as_deref(), Some("expected integer"));
    assert!(hit.related_spans.is_empty());
}

#[test]
fn lens_findings_resolve_existing_wildcards_and_reject_path_escape() {
    let lens: LensManifest = serde_json::from_value(json!({
        "schema": "prog.lens_manifest",
        "id": "test.failures",
        "view": {"root": "/items"},
        "findings": [{
            "kind": "test_failure",
            "path": "/items/*/status",
            "confidence": 0.95,
            "reason": "item failed Bearer abcdefghijklmnopqrstuvwxyz",
            "severity": "error",
            "contains_any": ["failed"],
            "api_token": "plain-lens-secret"
        }]
    }))
    .unwrap();
    let payload = json!({"items": [{"status": "passed"}, {"status": "failed"}]});
    let findings = ranked_findings_with_lens(
        &payload,
        &FindingOptions {
            cursor: Some("pc1_demo".to_string()),
            scope_path: Some("/items".to_string()),
            ..FindingOptions::default()
        },
        Some(&lens),
    )
    .unwrap();
    let lens_finding = findings
        .iter()
        .find(|finding| finding.lens_id.as_deref() == Some("test.failures"))
        .unwrap();
    assert_eq!(lens_finding.path, "/items/1/status");
    assert_eq!(
        findings
            .iter()
            .filter(|finding| finding.path == "/items/1/status")
            .count(),
        1,
        "the lens classification must supersede generic findings at the same path"
    );
    let encoded = serde_json::to_string(lens_finding).unwrap();
    assert!(!encoded.contains("abcdefghijklmnopqrstuvwxyz"));
    assert!(!encoded.contains("plain-lens-secret"));

    let escaping: LensManifest = serde_json::from_value(json!({
        "schema": "prog.lens_manifest",
        "id": "test.escape",
        "view": {"root": "/items"},
        "findings": [{
            "kind": "bad",
            "path": "/secret",
            "confidence": 1.0,
            "reason": "escape"
        }]
    }))
    .unwrap();
    assert!(prog_core::validate_lens_manifest(&escaping).is_err());
}

#[test]
fn lens_identity_selectors_are_declarative_and_ignore_line_movement() {
    let lens: LensManifest = serde_json::from_value(json!({
        "schema": "prog.lens_manifest",
        "id": "rust.diagnostics",
        "findings": [{
            "kind": "diagnostic",
            "path": "/diagnostics/*",
            "confidence": 0.9,
            "reason": "compiler diagnostic",
            "identity_selectors": {
                "subject": "/code",
                "message_template": "/message",
                "file": "/file"
            }
        }]
    }))
    .unwrap();
    let first = json!({"diagnostics": [{
        "code": "E0308", "message": "mismatched types", "file": "src/a.rs", "line": 3
    }]});
    let moved = json!({"diagnostics": [{
        "code": "E0308", "message": "mismatched types", "file": "src/a.rs", "line": 300
    }]});
    let first_finding = ranked_findings_with_lens(&first, &FindingOptions::default(), Some(&lens))
        .unwrap()
        .pop()
        .unwrap();
    let moved_finding = ranked_findings_with_lens(&moved, &FindingOptions::default(), Some(&lens))
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first_finding.fingerprint, moved_finding.fingerprint);
    assert!(first_finding.occurrence_id.unwrap().starts_with("fo_"));

    let unsafe_selector: LensManifest = serde_json::from_value(json!({
        "schema": "prog.lens_manifest",
        "id": "bad.identity",
        "findings": [{
            "kind": "diagnostic", "path": "/diagnostics/*", "confidence": 0.9,
            "reason": "bad", "identity_selectors": {"subject": "/items/*"}
        }]
    }))
    .unwrap();
    assert!(validate_lens_manifest(&unsafe_selector).is_err());
}

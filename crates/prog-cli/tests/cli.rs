use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use proptest::prelude::*;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{method, path, query_param},
};

mod support;

use support::*;

struct EtagResponder;

impl Respond for EtagResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if request.headers.get("if-none-match").is_some() {
            ResponseTemplate::new(304)
        } else {
            ResponseTemplate::new(200)
                .insert_header("etag", "\"record-7\"")
                .set_body_json(json!({"id": 7, "body": "original"}))
        }
    }
}

#[test]
fn help_shows_complete_command_tree() {
    let output = prog(&["--help"]);
    assert!(output.status.success());
    assert_eq!(stderr(&output), "");

    let help = stdout(&output);
    for expected in [
        "discover",
        "source",
        "hints",
        "call",
        "observe",
        "run",
        "init",
        "cost",
        "paths",
        "expand",
        "cache",
        "meta",
        "--dir",
        "--lens-dir",
        "--pretty",
    ] {
        assert!(help.contains(expected), "help should include {expected}");
    }
}

#[test]
fn missing_call_and_expand_inputs_return_structured_errors() {
    let dir = tempfile::tempdir().unwrap();

    let call = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "call",
        "local",
        "list",
        "--args",
        "{}",
    ]);
    assert!(!call.status.success());
    assert_eq!(stderr(&call), "");
    let value: Value = serde_json::from_slice(&call.stdout).expect("stdout must be JSON");
    assert_eq!(value["error"]["kind"], "unknown_source");

    let expand = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "expand",
        "pc1_missing",
        "--path",
        "/items/0",
    ]);
    assert!(!expand.status.success());
    assert_eq!(stderr(&expand), "");
    let value: Value = serde_json::from_slice(&expand.stdout).expect("stdout must be JSON");
    assert_eq!(value["error"]["kind"], "cursor_not_found");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cursor 'pc1_missing':")
    );
}

#[test]
fn observe_json_file_uses_envelope_cache_redaction_and_expand() {
    let dir = tempfile::tempdir().unwrap();
    let payload = json!({
        "items": (0..30)
            .map(|index| json!({
                "id": index,
                "title": format!("Item {index}"),
                "body": "x".repeat(800),
                "token": "super-secret-token"
            }))
            .collect::<Vec<_>>()
    });
    let file = dir.path().join("large.json");
    fs::write(&file, serde_json::to_vec(&payload).unwrap()).unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let observed = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--mime",
        "application/json",
        "--name",
        "json-fixture",
    ]);
    assert!(observed.status.success(), "{}", stdout(&observed));
    assert_eq!(stderr(&observed), "");
    let envelope: Value = serde_json::from_slice(&observed.stdout).unwrap();
    assert_eq!(envelope["source_id"], "observe");
    assert_eq!(envelope["operation"], "json-fixture");
    assert_eq!(envelope["cache"]["status"], "stored");
    assert_eq!(envelope["data_preview"]["items"][0]["token"], "«redacted»");
    assert!(!stdout(&observed).contains("super-secret-token"));
    assert!(
        envelope["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omitted| omitted["path"] == "/items")
    );
    let cursor = envelope["cursor"].as_str().unwrap();

    let paths = prog(&[
        "--dir", dir_arg, "paths", cursor, "--prefix", "/items", "--limit", "20",
    ]);
    assert!(paths.status.success(), "{}", stdout(&paths));
    assert_eq!(stderr(&paths), "");
    let path_listing: Value = serde_json::from_slice(&paths.stdout).unwrap();
    assert_eq!(path_listing["cache"]["status"], "hit");
    assert!(
        path_listing["paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["path"] == "/items/0/token" && entry["omitted_reason"] == "redacted"
            })
    );
    assert!(!stdout(&paths).contains("super-secret-token"));

    let expanded = prog(&[
        "--dir",
        dir_arg,
        "expand",
        cursor,
        "--path",
        "/items/0/body",
        "--depth",
        "1",
    ]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let expanded_value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(expanded_value["cache"]["status"], "hit");
    assert!(
        expanded_value["data_preview"]
            .as_str()
            .unwrap()
            .starts_with('x')
    );
    assert!(
        expanded_value["summary"]["envelope_bytes"]
            .as_u64()
            .unwrap()
            <= 16 * 1024
    );
}

#[test]
fn scalar_expansion_receipts_preserve_scope_and_freshness_truth() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let exact_boundary = "x".repeat(384);
    let short_scalar = "short scalar";
    let file = dir.path().join("scalars.json");
    fs::write(
        &file,
        serde_json::to_vec(&json!({
            "nested": {
                "exact": exact_boundary,
                "short": short_scalar,
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let observed = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--mime",
        "application/json",
        "--name",
        "scalars",
    ]);
    assert!(observed.status.success(), "{}", stdout(&observed));
    let observed: Value = serde_json::from_slice(&observed.stdout).unwrap();
    let cursor = observed["cursor"].as_str().unwrap();

    for (path, expected, limit) in [
        ("/nested/exact", "x".repeat(384), "384"),
        ("/nested/short", short_scalar.to_string(), "384"),
    ] {
        let expanded = prog(&[
            "--dir", dir_arg, "expand", cursor, "--path", path, "--limit", limit,
        ]);
        assert!(expanded.status.success(), "{}", stdout(&expanded));
        let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
        assert_eq!(value["data_preview"], expected);
        assert!(value["omitted"].as_array().unwrap().is_empty());
        assert_eq!(value["observation"]["completeness"]["root_path"], path);
        assert_eq!(
            value["observation"]["completeness"]["preview_complete"],
            true
        );
        assert_eq!(value["observation"]["freshness"]["stale"], false);
        assert_eq!(
            value["observation"]["freshness"]["refresh_recommended"],
            false
        );
        assert!(
            value["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|warning| !warning.as_str().unwrap().contains("refresh"))
        );
    }

    let export = dir.path().join("exact.json");
    let exported = prog(&[
        "--dir",
        dir_arg,
        "expand",
        cursor,
        "--path",
        "/nested/exact",
        "--out",
        export.to_str().unwrap(),
    ]);
    assert!(exported.status.success(), "{}", stdout(&exported));
    let receipt: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert!(receipt["omitted"].as_array().unwrap().is_empty());
    assert_eq!(
        receipt["observation"]["completeness"]["root_path"],
        "/nested/exact"
    );
    assert_eq!(
        receipt["observation"]["completeness"]["preview_complete"],
        true
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(export).unwrap()).unwrap(),
        json!("x".repeat(384))
    );

    let paths = prog(&[
        "--dir", dir_arg, "paths", cursor, "--prefix", "/nested", "--limit", "10",
    ]);
    assert!(paths.status.success(), "{}", stdout(&paths));
    let paths: Value = serde_json::from_slice(&paths.stdout).unwrap();
    assert_eq!(paths["cache"]["status"], "hit");
    assert!(
        paths["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| !warning.as_str().unwrap().contains("refresh"))
    );
}

#[test]
fn observe_csv_file_yields_table_envelope_with_expandable_rows() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let csv = dir.path().join("roster.csv");
    fs::write(&csv, "name,role\nAda,engineer\nLin,manager\n").unwrap();

    let observed = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        csv.to_str().unwrap(),
        "--mime",
        "text/csv",
        "--name",
        "roster",
    ]);
    assert!(observed.status.success(), "{}", stdout(&observed));
    let envelope: Value = serde_json::from_slice(&observed.stdout).unwrap();
    assert_eq!(envelope["source_id"], "observe");
    // The table parser produced a tabular payload, not a text fallback.
    assert_eq!(envelope["data_preview"]["format"], "csv");
    assert_eq!(envelope["data_preview"]["columns"][0], "name");
    assert_eq!(envelope["data_preview"]["rows"][0][0], "Ada");

    // /rows is expandable from the cached payload without re-reading the file.
    let cursor = envelope["cursor"].as_str().unwrap();
    let expanded = prog(&[
        "--dir", dir_arg, "expand", cursor, "--path", "/rows", "--limit", "10",
    ]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let rows: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(rows["data_preview"].as_array().unwrap().len(), 2);
    assert_eq!(rows["data_preview"][1][0], "Lin");
}

#[test]
fn observe_can_apply_first_party_text_log_lens_and_reject_counterexample() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let lens_dir = first_party_lens_dir();
    let lens_dir_arg = lens_dir.to_str().unwrap();
    let log = dir.path().join("service.log");
    fs::write(
        &log,
        "2026-07-06T12:00:00Z INFO start\n2026-07-06T12:00:01Z ERROR token=plain-secret\n2026-07-06T12:00:02Z INFO stop\n",
    )
    .unwrap();

    let output = prog(&[
        "--dir",
        dir_arg,
        "--lens-dir",
        lens_dir_arg,
        "observe",
        "--file",
        log.to_str().unwrap(),
        "--mime",
        "text/plain",
        "--lens",
        "observe.text.logs",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(!stdout(&output).contains("plain-secret"));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["lens"]["id"], "observe.text.logs");
    assert_eq!(envelope["data_preview"]["line_count"], 3);
    assert!(
        envelope["data_preview"]
            .as_object()
            .unwrap()
            .get("lines")
            .is_none()
    );
    assert!(
        envelope["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omitted| omitted["path"] == "/lines" && omitted["expandable"] == true)
    );
    let cursor = envelope["cursor"].as_str().unwrap();
    let expanded = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/lines/1"]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    assert!(!stdout(&expanded).contains("plain-secret"));
    let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(value["data_preview"]["number"], 2);
    assert!(
        value["data_preview"]["text"]
            .as_str()
            .unwrap()
            .contains("[REDACTED:observed_text_secret]")
    );

    let json_file = dir.path().join("items.json");
    fs::write(&json_file, r#"{"items":[{"id":1,"title":"tiny"}]}"#).unwrap();
    let rejected = prog(&[
        "--dir",
        dir_arg,
        "--lens-dir",
        lens_dir_arg,
        "observe",
        "--file",
        json_file.to_str().unwrap(),
        "--mime",
        "application/json",
        "--lens",
        "observe.text.logs",
    ]);
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("artifact_kind 'text', not 'json'")
    );
}

#[test]
fn evidence_refs_are_stable_refresh_sensitive_and_redaction_safe() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let file = dir.path().join("evidence.json");
    fs::write(
        &file,
        serde_json::to_vec(&json!({
            "answer": "alpha",
            "token": "plain-secret",
            "nested": {"value": "alpha"}
        }))
        .unwrap(),
    )
    .unwrap();

    let observed = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--name",
        "evidence",
    ]);
    assert!(observed.status.success(), "{}", stdout(&observed));
    let observed_value: Value = serde_json::from_slice(&observed.stdout).unwrap();
    let cursor = observed_value["cursor"].as_str().unwrap();

    let expanded = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/answer"]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let expanded_value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    let evidence = &expanded_value["evidence_ref"];
    assert_eq!(evidence["schema"], "prog.evidence_ref");
    assert_eq!(evidence["source_id"], "observe");
    assert_eq!(evidence["operation"], "evidence");
    assert_eq!(evidence["cursor"], cursor);
    assert_eq!(evidence["path"], "/answer");
    assert_eq!(evidence["uri"], format!("prog://{cursor}#/answer"));
    assert_eq!(evidence["cache_status"], "hit");
    assert_eq!(evidence["stale"], false);
    assert_eq!(evidence["availability"], "redacted");
    assert_eq!(evidence["capture"]["stop_reason"], "redacted");
    assert_eq!(evidence["capture"]["can_prove_absence"], false);
    assert_eq!(evidence["redacted"], false);
    assert_eq!(evidence["lossy"], false);
    let first_hash = evidence["redacted_slice_sha256"].as_str().unwrap();

    let expanded_again = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/answer"]);
    assert!(
        expanded_again.status.success(),
        "{}",
        stdout(&expanded_again)
    );
    let expanded_again_value: Value = serde_json::from_slice(&expanded_again.stdout).unwrap();
    assert_eq!(
        expanded_again_value["evidence_ref"]["redacted_slice_sha256"],
        first_hash
    );
    assert_eq!(expanded_again_value["evidence_ref"]["uri"], evidence["uri"]);

    let paths = prog(&[
        "--dir", dir_arg, "paths", cursor, "--prefix", "/nested", "--limit", "10",
    ]);
    assert!(paths.status.success(), "{}", stdout(&paths));
    let paths_value: Value = serde_json::from_slice(&paths.stdout).unwrap();
    let nested = paths_value["paths"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "/nested")
        .unwrap();
    assert_eq!(nested["evidence_ref"]["path"], "/nested");
    assert_eq!(nested["evidence_ref"]["availability"], "redacted");
    assert_eq!(
        nested["evidence_ref"]["capture"]["can_prove_absence"],
        false
    );
    assert_eq!(
        nested["evidence_ref"]["uri"],
        format!("prog://{cursor}#/nested")
    );
    assert!(nested["evidence_ref"]["redacted_slice_sha256"].is_string());

    let redacted = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/token"]);
    assert!(redacted.status.success(), "{}", stdout(&redacted));
    assert!(!stdout(&redacted).contains("plain-secret"));
    let redacted_value: Value = serde_json::from_slice(&redacted.stdout).unwrap();
    assert_eq!(redacted_value["evidence_ref"]["redacted"], true);
    assert!(
        !redacted_value["evidence_ref"]
            .to_string()
            .contains("plain-secret")
    );

    let redacted_paths = prog(&[
        "--dir", dir_arg, "paths", cursor, "--prefix", "/token", "--limit", "10",
    ]);
    assert!(
        redacted_paths.status.success(),
        "{}",
        stdout(&redacted_paths)
    );
    assert!(!stdout(&redacted_paths).contains("plain-secret"));
    let redacted_paths_value: Value = serde_json::from_slice(&redacted_paths.stdout).unwrap();
    let redacted_entry = redacted_paths_value["paths"]
        .as_array()
        .unwrap()
        .first()
        .unwrap();
    assert_eq!(redacted_entry["path"], "/token");
    assert_eq!(redacted_entry["evidence_ref"]["redacted"], true);

    let out = dir.path().join("evidence-answer.json");
    let exported = prog(&[
        "--dir",
        dir_arg,
        "expand",
        cursor,
        "--path",
        "/answer",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(exported.status.success(), "{}", stdout(&exported));
    assert!(out.exists());
    let export_value: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(
        export_value["data_preview"]["evidence_ref"]["path"],
        "/answer"
    );
    assert_eq!(
        export_value["data_preview"]["evidence_ref"]["redacted_slice_sha256"],
        first_hash
    );

    fs::write(
        &file,
        serde_json::to_vec(&json!({
            "answer": "beta",
            "token": "plain-secret",
            "nested": {"value": "beta"}
        }))
        .unwrap(),
    )
    .unwrap();
    let refreshed = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--name",
        "evidence",
    ]);
    assert!(refreshed.status.success(), "{}", stdout(&refreshed));
    let refreshed_value: Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    let refreshed_cursor = refreshed_value["cursor"].as_str().unwrap();
    let changed = prog(&[
        "--dir",
        dir_arg,
        "expand",
        refreshed_cursor,
        "--path",
        "/answer",
    ]);
    assert!(changed.status.success(), "{}", stdout(&changed));
    let changed_value: Value = serde_json::from_slice(&changed.stdout).unwrap();
    assert_ne!(
        changed_value["evidence_ref"]["redacted_slice_sha256"],
        first_hash
    );
}

#[test]
fn observe_stdin_text_has_head_tail_line_paths_and_secret_redaction() {
    let dir = tempfile::tempdir().unwrap();
    let text = (0..25)
        .map(|index| {
            if index == 1 {
                "token=plain-secret".to_string()
            } else {
                format!("row-{index}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let output = prog_with_stdin(
        &[
            "--dir",
            dir.path().to_str().unwrap(),
            "observe",
            "--stdin",
            "--mime",
            "text/plain",
            "--name",
            "stdin-log",
        ],
        text.as_bytes(),
    );
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(!stdout(&output).contains("plain-secret"));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["data_preview"]["format"], "text");
    assert_eq!(envelope["data_preview"]["line_count"], 25);
    assert_eq!(envelope["data_preview"]["head"][0], "row-0");
    assert_eq!(
        envelope["data_preview"]["head"][1],
        "token=[REDACTED:observed_text_secret]"
    );
    assert!(
        envelope["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omitted| omitted["path"] == "/lines")
    );

    let cursor = envelope["cursor"].as_str().unwrap();
    let paths = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "paths",
        cursor,
        "--prefix",
        "/lines",
        "--limit",
        "12",
    ]);
    assert!(paths.status.success(), "{}", stdout(&paths));
    let path_listing: Value = serde_json::from_slice(&paths.stdout).unwrap();
    assert_eq!(path_listing["prefix"], "/lines");
    assert_eq!(path_listing["cache"]["status"], "hit");
    assert!(
        path_listing["paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "/lines/1/text")
    );

    let expanded = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "expand",
        cursor,
        "--path",
        "/lines/1/text",
    ]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(
        value["data_preview"],
        "token=[REDACTED:observed_text_secret]"
    );
}

#[test]
fn observe_ndjson_exposes_records_for_expansion() {
    let dir = tempfile::tempdir().unwrap();
    let input = br#"{"id":1,"value":"one"}
{"id":2,"value":"two"}
"#;
    let output = prog_with_stdin(
        &[
            "--dir",
            dir.path().to_str().unwrap(),
            "observe",
            "--stdin",
            "--mime",
            "application/x-ndjson",
            "--name",
            "events",
        ],
        input,
    );
    assert!(output.status.success(), "{}", stdout(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["data_preview"]["format"], "ndjson");
    assert_eq!(envelope["data_preview"]["record_count"], 2);
    let cursor = envelope["cursor"].as_str().unwrap();

    let paths = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "paths",
        cursor,
        "--prefix",
        "/records",
    ]);
    assert!(paths.status.success(), "{}", stdout(&paths));
    let path_listing: Value = serde_json::from_slice(&paths.stdout).unwrap();
    assert!(
        path_listing["paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "/records/1/value")
    );

    let expanded = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "expand",
        cursor,
        "--path",
        "/records/1/value",
    ]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(value["data_preview"], "two");
}

#[test]
fn observe_handles_empty_and_invalid_utf8_text_but_rejects_binary() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let empty = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "text/plain",
        ],
        b"",
    );
    assert!(empty.status.success(), "{}", stdout(&empty));
    let empty_value: Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(empty_value["data_preview"]["line_count"], 0);

    let invalid = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "text/plain",
            "--name",
            "invalid-utf8",
        ],
        &[b'o', 0xff, b'k'],
    );
    assert!(invalid.status.success(), "{}", stdout(&invalid));
    let invalid_value: Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert_eq!(invalid_value["data_preview"]["utf8_valid"], false);
    assert!(
        invalid_value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("UTF-8"))
    );

    let binary = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "text/plain",
            "--name",
            "binary",
        ],
        &[0, 1, 2, 3],
    );
    assert!(!binary.status.success());
    let error: Value = serde_json::from_slice(&binary.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("binary")
    );
}

#[test]
fn observe_huge_text_line_is_bounded_and_expandable() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("huge.log");
    fs::write(&file, format!("prefix-{}", "x".repeat(20_000))).unwrap();
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--mime",
        "text/plain",
        "--name",
        "huge-line",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(envelope["summary"]["envelope_bytes"].as_u64().unwrap() <= 16 * 1024);
    assert!(
        envelope["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |omitted| omitted["path"] == "/lines/0/text" && omitted["reason"] == "large_string"
            )
    );
    let cursor = envelope["cursor"].as_str().unwrap();

    let expanded = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "expand",
        cursor,
        "--path",
        "/lines/0/text",
        "--depth",
        "1",
    ]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert!(value["summary"]["envelope_bytes"].as_u64().unwrap() <= 16 * 1024);
    assert!(
        value["data_preview"]
            .as_str()
            .unwrap()
            .starts_with("prefix-")
    );
}

#[test]
fn observe_cursor_expiry_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let observed = prog_with_stdin(
        &[
            "--dir",
            dir.path().to_str().unwrap(),
            "observe",
            "--stdin",
            "--mime",
            "text/plain",
            "--name",
            "expired",
            "--ttl-seconds",
            "0",
        ],
        b"expired line",
    );
    assert!(observed.status.success(), "{}", stdout(&observed));
    let envelope: Value = serde_json::from_slice(&observed.stdout).unwrap();
    assert_eq!(envelope["cache"]["ttl_seconds"], 0);
    let cursor = envelope["cursor"].as_str().unwrap();

    let expanded = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "expand",
        cursor,
        "--path",
        "/lines/0/text",
    ]);
    assert!(!expanded.status.success());
    let error: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "cursor_expired");
}

#[test]
fn run_success_captures_streams_interleaving_out_file_and_expansion() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let out = dir.path().join("run-capture.json");
    let output = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--out",
        out.to_str().unwrap(),
        "--",
        "python3",
        "-c",
        "import sys\nfor i in range(3):\n print(f'out-{i}', flush=True)\n sys.stderr.write(f'err-{i}\\n'); sys.stderr.flush()",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert_eq!(stderr(&output), "");
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["source_id"], "run");
    assert_eq!(envelope["data_preview"]["format"], "run");
    assert_eq!(envelope["data_preview"]["command"]["success"], true);
    assert_eq!(envelope["data_preview"]["stdout"]["line_count"], 3);
    assert_eq!(envelope["data_preview"]["stderr"]["line_count"], 3);
    let combined = envelope["data_preview"]["combined"].as_array().unwrap();
    assert!(combined.iter().any(|chunk| chunk["stream"] == "stdout"));
    assert!(combined.iter().any(|chunk| chunk["stream"] == "stderr"));
    assert_eq!(envelope["observation"]["payload"]["cache_status"], "stored");
    assert_eq!(envelope["observation"]["payload"]["expandable"], true);
    assert!(out.exists());
    let out_value: Value = serde_json::from_slice(&fs::read(out).unwrap()).unwrap();
    assert_eq!(out_value["stdout"]["text"], "out-0\nout-1\nout-2");

    let cursor = envelope["cursor"].as_str().unwrap();
    let expanded = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/stderr/text"]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(value["data_preview"], "err-0\nerr-1\nerr-2");
}

#[test]
fn successful_test_output_does_not_fabricate_failure_sections_or_findings() {
    let dir = tempfile::tempdir().unwrap();
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "run",
        "--",
        "python3",
        "-c",
        "print('test tool_is_error_maps_to_structured_error ... ok')\nprint('test result: ok. 17 passed; 0 failed')",
    ]);

    assert!(output.status.success(), "{}", stdout(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["data_preview"]["command"]["success"], true);
    assert!(
        envelope["data_preview"]["failure_sections"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        envelope
            .get("findings")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    );
}

#[test]
fn cargo_recipe_centers_evidence_on_the_strongest_diagnostic_without_duplicate_findings() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let lens_dir = first_party_lens_dir();
    let cargo = dir.path().join("cargo");
    let script = r#"#!/usr/bin/env python3
import sys
print('running 1 test')
print('test tests::failing_case ... FAILED')
print('')
print('failures:')
print('')
print('---- tests::failing_case stdout ----')
print('')
print("thread 'tests::failing_case' panicked at src/lib.rs:3:5:")
print("assertion `left == right` failed")
print('  left: 1')
print(' right: 2')
print('note: run with RUST_BACKTRACE=1')
print('')
print('failures:')
print('    tests::failing_case')
print('')
print('test result: FAILED. 0 passed; 1 failed')
sys.exit(101)"#;
    fs::write(&cargo, script).unwrap();
    let mut permissions = fs::metadata(&cargo).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&cargo, permissions).unwrap();
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "--lens-dir",
        lens_dir.to_str().unwrap(),
        "recipe",
        "cargo-test",
        "--goal",
        "identify the exact failing assertion",
        cargo.to_str().unwrap(),
    ]);

    assert!(output.status.success(), "{}", stdout(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let findings = envelope["findings"].as_array().unwrap();
    let unique_paths = findings
        .iter()
        .map(|finding| finding["path"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(findings.len(), unique_paths.len());
    assert_eq!(findings[0]["kind"], "cargo_test_failure");
    assert_eq!(findings[0]["path"], "/failure_sections/0");

    let first_section = &envelope["data_preview"]["failures"][0];
    assert_eq!(first_section["kind"], "rust");
    assert!(
        first_section["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line
                .as_str()
                .unwrap()
                .contains("assertion `left == right` failed"))
    );

    let evidence = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "--lens-dir",
        lens_dir.to_str().unwrap(),
        "evidence",
        envelope["cursor"].as_str().unwrap(),
        "--path",
        "/failure_sections/0",
    ]);
    assert!(evidence.status.success(), "{}", stdout(&evidence));
    let evidence: Value = serde_json::from_slice(&evidence.stdout).unwrap();
    assert!(
        evidence["excerpt"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line
                .as_str()
                .unwrap()
                .contains("assertion `left == right` failed"))
    );
}

#[test]
fn run_repeated_identical_commands_create_distinct_cache_entries() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let args = [
        "--dir",
        dir_arg,
        "run",
        "--selection-scope",
        "full-suite",
        "--selection-exhaustive",
        "--",
        "python3",
        "-c",
        "print('same')",
    ];

    let first = prog(&args);
    assert!(first.status.success(), "{}", stdout(&first));
    let second = prog(&args);
    assert!(second.status.success(), "{}", stdout(&second));

    let first_envelope: Value = serde_json::from_slice(&first.stdout).unwrap();
    let second_envelope: Value = serde_json::from_slice(&second.stdout).unwrap();
    let first_capture = first_envelope["data_preview"]["command"]["capture_id"]
        .as_str()
        .unwrap();
    let second_capture = second_envelope["data_preview"]["command"]["capture_id"]
        .as_str()
        .unwrap();
    assert!(first_capture.starts_with("run_"));
    assert!(second_capture.starts_with("run_"));
    assert_ne!(first_capture, second_capture);
    assert_eq!(
        first_envelope["provenance"]["source_call_id"],
        first_capture
    );
    assert_eq!(
        second_envelope["provenance"]["source_call_id"],
        second_capture
    );
    assert_ne!(
        first_envelope["provenance"]["cache_key"],
        second_envelope["provenance"]["cache_key"]
    );

    for envelope in [first_envelope, second_envelope] {
        let cursor = envelope["cursor"].as_str().unwrap();
        let expanded = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/stdout/text"]);
        assert!(expanded.status.success(), "{}", stdout(&expanded));
        let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
        assert_eq!(value["data_preview"], "same");
    }
}

#[test]
fn run_failure_returns_envelope_and_can_preserve_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let script = "import sys\nsys.stderr.write('Traceback (most recent call last):\\n  File \"x.py\", line 1\\nValueError: bad\\n')\nsys.exit(7)";

    let output = prog(&["--dir", dir_arg, "run", "--", "python3", "-c", script]);
    assert!(output.status.success(), "{}", stdout(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["data_preview"]["command"]["success"], false);
    assert_eq!(envelope["data_preview"]["command"]["exit_code"], 7);
    assert_eq!(
        envelope["data_preview"]["failure_sections"][0]["kind"],
        "python"
    );
    assert!(
        envelope["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["path"] == "/failure_sections/0" && action["argv"][3] == "--path")
    );
    let cursor = envelope["cursor"].as_str().unwrap();
    let expanded = prog(&[
        "--dir",
        dir_arg,
        "expand",
        cursor,
        "--path",
        "/failure_sections/0",
    ]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(value["data_preview"]["kind"], "python");

    let preserved = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--preserve-exit-code",
        "--",
        "python3",
        "-c",
        script,
    ]);
    assert_eq!(preserved.status.code(), Some(7));
    let value: Value = serde_json::from_slice(&preserved.stdout).unwrap();
    assert_eq!(value["data_preview"]["command"]["exit_code"], 7);
}

#[cfg(unix)]
#[test]
fn pytest_node_id_hint_is_exact_argv_and_never_claims_broader_verification() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let pytest = dir.path().join("pytest");
    fs::write(
        &pytest,
        "#!/bin/sh\nprintf 'FAILED tests/test_math.py::test_negative_exponent[param-1] - AssertionError\\n'\nexit 1\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&pytest).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&pytest, permissions).unwrap();
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap()
    );
    let store = dir.path().join("store");
    let store_arg = store.to_str().unwrap();
    let session = prog(&["--dir", store_arg, "session", "start"]);
    assert!(session.status.success(), "{}", stdout(&session));
    for (id, scope) in [
        ("affected-check", "affected-suite"),
        ("full-check", "regression-suite"),
    ] {
        let obligation = prog(&[
            "--dir",
            store_arg,
            "session",
            "obligation-add",
            id,
            "--check",
            "rerun verification",
            "--scope",
            scope,
        ]);
        assert!(obligation.status.success(), "{}", stdout(&obligation));
    }
    let output = prog_with_env(
        &["--dir", store_arg, "run", "--", "pytest"],
        &[("PATH", &path)],
    );
    assert!(output.status.success(), "{}", stdout(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let hint = envelope["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["kind"] == "rerun")
        .expect("exact pytest node hint");
    assert_eq!(
        hint["argv"],
        json!([
            "pytest",
            "tests/test_math.py::test_negative_exponent[param-1]"
        ])
    );
    assert_eq!(hint["scope"], "target_test");
    assert_eq!(hint["exactness"], "exact");
    assert_eq!(hint["derived_from"], "pytest.failed_node_id");
    assert_eq!(
        hint["does_not_satisfy"],
        json!(["affected-check", "full-check"])
    );
    assert!(hint.get("command").is_none());
}

#[cfg(unix)]
#[test]
fn go_test_hint_is_an_escaped_exact_argv_recommendation() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let go = dir.path().join("go");
    fs::write(
        &go,
        "#!/bin/sh\nprintf '%s\\n' '--- FAIL: TestParser[unicode].case (0.00s)' 'FAIL\t./internal/parser\t0.003s'\nexit 1\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&go).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&go, permissions).unwrap();
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap()
    );
    let output = prog_with_env(
        &[
            "--dir",
            dir.path().join("store").to_str().unwrap(),
            "run",
            "--",
            "go",
            "test",
            "./internal/parser",
        ],
        &[("PATH", &path)],
    );
    assert!(output.status.success(), "{}", stdout(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let hint = envelope["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["derived_from"] == "go_test.failed_name_and_package")
        .expect("exact Go test hint");
    assert_eq!(
        hint["argv"],
        json!([
            "go",
            "test",
            "./internal/parser",
            "-run",
            "^TestParser\\[unicode\\]\\.case$"
        ])
    );
    assert_eq!(hint["exactness"], "exact");
}

#[test]
fn run_can_apply_first_party_failure_lens_and_expand_redacted_capture() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let lens_dir = first_party_lens_dir();
    let lens_dir_arg = lens_dir.to_str().unwrap();
    let script = "import sys\nsecret = 'plain' + '-secret'\nprint('token=' + secret)\nsys.stderr.write('Traceback (most recent call last):\\n  File \"x.py\", line 1\\nRuntimeError: token=' + secret + '\\n')\nsys.exit(2)";

    let output = prog(&[
        "--dir",
        dir_arg,
        "--lens-dir",
        lens_dir_arg,
        "run",
        "--lens",
        "run.failures",
        "--",
        "python3",
        "-c",
        script,
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(!stdout(&output).contains("plain-secret"));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["lens"]["id"], "run.failures");
    assert_eq!(envelope["data_preview"]["success"], false);
    assert_eq!(envelope["data_preview"]["exit_code"], 2);
    assert!(
        envelope["data_preview"]
            .as_object()
            .unwrap()
            .get("stderr")
            .is_none()
    );
    assert_eq!(
        envelope["data_preview"]["failure_sections"][0]["kind"],
        "python"
    );
    assert!(
        envelope["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omitted| omitted["path"] == "/stderr/text" && omitted["expandable"] == true)
    );
    assert!(
        envelope["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["path"] == "/failure_sections/0")
    );

    let cursor = envelope["cursor"].as_str().unwrap();
    let expanded = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/stderr/text"]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    assert!(!stdout(&expanded).contains("plain-secret"));
    let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert!(
        value["data_preview"]
            .as_str()
            .unwrap()
            .contains("[REDACTED:observed_text_secret]")
    );
}

#[test]
fn run_redacts_compound_secret_flags_in_recorded_argv() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let output = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--selection-scope",
        "full-suite",
        "--selection-exhaustive",
        "--",
        "python3",
        "-c",
        "import sys; print('ran')",
        "--access-token",
        "eyJLEAK_TOKEN",
        "--passwd",
        "leakpass",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    // The envelope itself must not leak the access-token value (the only
    // secret-bearing argv element that fits inside the bounded preview).
    assert!(!stdout(&output).contains("eyJLEAK_TOKEN"));

    // The cached payload reached via expand must be clean too. Before the fix,
    // compound flags like --access-token and the missing --passwd token were
    // not recognized, so their values were persisted raw in command.argv.
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let cursor = envelope["cursor"].as_str().unwrap();
    let expanded = prog(&[
        "--dir",
        dir_arg,
        "expand",
        cursor,
        "--path",
        "/command/argv",
        "--limit",
        "10",
    ]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let expanded_out = stdout(&expanded);
    assert!(
        !expanded_out.contains("eyJLEAK_TOKEN"),
        "access-token value reached the cache: {expanded_out}"
    );
    assert!(
        !expanded_out.contains("leakpass"),
        "passwd value reached the cache: {expanded_out}"
    );
    assert!(
        expanded_out.contains("[REDACTED"),
        "expected a redaction sentinel in recorded argv: {expanded_out}"
    );
}

#[test]
fn call_does_not_persist_raw_sensitive_args_in_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let script = dir.path().join("safe.py");
    fs::write(
        &script,
        "import json\nprint(json.dumps({'ok': True, 'items': [1, 2, 3]}))\n",
    )
    .unwrap();
    let seed_json = json!({
        "kind": "cli",
        "operations": [{
            "name": "fetch",
            "command": "python3",
            "args": [script.to_str().unwrap(), "{service_key}"],
            "input_schema": {
                "type": "object",
                "required": ["service_key"],
                "properties": {
                    "service_key": {"type": "string"}
                },
                "additionalProperties": false
            },
            "sensitive_args": ["service_key"],
            "effect": {
                "read_only": true,
                "mutating": false,
                "network": false,
                "shell": false,
                "sensitive": false,
                "cacheable": true,
                "requires_confirmation": false
            }
        }]
    });
    let seed = write_seed(dir.path(), "cli.json", &seed_json.to_string());
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "local",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));

    let learned_secret = "SK-LIVE-1234";
    let call = prog(&[
        "--dir",
        dir_arg,
        "call",
        "local",
        "fetch",
        "--args",
        &json!({"service_key": learned_secret}).to_string(),
    ]);
    assert!(call.status.success(), "{}", stdout(&call));
    let profile = fs::read_to_string(dir.path().join("profiles/local.json")).unwrap();
    assert!(!profile.contains(learned_secret));
    assert!(profile.contains("[REDACTED:declared_sensitive]"));

    let no_cache_secret = "SK-NOCACHE-5678";
    let no_cache = prog(&[
        "--dir",
        dir_arg,
        "call",
        "local",
        "fetch",
        "--args",
        &json!({"service_key": no_cache_secret}).to_string(),
        "--no-cache",
    ]);
    assert!(no_cache.status.success(), "{}", stdout(&no_cache));
    let envelope: Value = serde_json::from_slice(&no_cache.stdout).unwrap();
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("profile learning skipped"))
    );
    let profile = fs::read_to_string(dir.path().join("profiles/local.json")).unwrap();
    assert!(!profile.contains(no_cache_secret));
}

#[test]
fn run_large_streams_are_bounded_expandable_and_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let output = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--max-stdout-bytes",
        "256",
        "--max-stderr-bytes",
        "128",
        "--",
        "python3",
        "-c",
        "import sys\nsys.stdout.write('token=plain-secret\\n' + 'x' * 20000)\nsys.stderr.write('error[E0425]: bad\\n' + 'y' * 20000)",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(!stdout(&output).contains("plain-secret"));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(envelope["summary"]["envelope_bytes"].as_u64().unwrap() <= 16 * 1024);
    assert_eq!(envelope["data_preview"]["stdout"]["truncated"], true);
    assert_eq!(envelope["data_preview"]["stderr"]["truncated"], true);
    assert_eq!(envelope["observation"]["availability"], "capture_truncated");
    assert_eq!(
        envelope["observation"]["capture"]["can_prove_absence"],
        false
    );
    assert_eq!(
        envelope["observation"]["capture"]["affected"][0]["scope"],
        "stdout"
    );
    assert_eq!(
        envelope["observation"]["capture"]["affected"][1]["scope"],
        "stderr"
    );
    assert_eq!(
        envelope["data_preview"]["stdout"]["head"][0],
        "token=[REDACTED:observed_text_secret]"
    );
    assert_eq!(
        envelope["data_preview"]["failure_sections"][0]["kind"],
        "rust"
    );
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("stdout exceeded"))
    );
    let cursor = envelope["cursor"].as_str().unwrap();
    let expanded = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/stdout/text"]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    assert!(!stdout(&expanded).contains("plain-secret"));
    let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert!(
        value["data_preview"]
            .as_str()
            .unwrap()
            .contains("[REDACTED:observed_text_secret]")
    );
}

#[test]
fn run_timeout_and_missing_command_return_structured_envelopes() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let timeout = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--timeout-ms",
        "50",
        "--max-stdout-bytes",
        "123",
        "--max-stderr-bytes",
        "45",
        "--",
        "python3",
        "-c",
        "import time; time.sleep(5)",
    ]);
    assert!(timeout.status.success(), "{}", stdout(&timeout));
    let value: Value = serde_json::from_slice(&timeout.stdout).unwrap();
    assert_eq!(value["data_preview"]["command"]["timed_out"], true);
    assert_eq!(
        value["data_preview"]["failure_sections"][0]["kind"],
        "timeout"
    );
    assert_eq!(value["observation"]["capture"]["stop_reason"], "timeout");
    assert_eq!(value["observation"]["capture"]["can_prove_absence"], false);
    assert_eq!(
        value["observation"]["capture"]["budget"]["source"],
        "invocation"
    );
    assert_eq!(
        value["observation"]["capture"]["budget"]["limits"][0]["max_bytes"],
        123
    );
    assert_eq!(
        value["observation"]["capture"]["budget"]["limits"][0]["max_duration_ms"],
        50
    );
    assert_eq!(
        value["observation"]["capture"]["budget"]["limits"][1]["max_bytes"],
        45
    );
    assert_eq!(
        value["capture_budget"],
        value["observation"]["capture"]["budget"]
    );
    assert_eq!(value["storage_budget"]["source"], "default");

    let missing = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--",
        "prog-command-that-does-not-exist-43",
    ]);
    assert!(missing.status.success(), "{}", stdout(&missing));
    let value: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert!(value["data_preview"]["command"]["spawn_error"].is_string());
    assert_eq!(
        value["data_preview"]["failure_sections"][0]["kind"],
        "spawn_error"
    );

    let missing_preserved = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--preserve-exit-code",
        "--",
        "prog-command-that-does-not-exist-43",
    ]);
    assert_eq!(missing_preserved.status.code(), Some(127));
}

#[cfg(unix)]
#[test]
fn run_timeout_does_not_wait_for_detached_pipe_holders() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let started = Instant::now();

    let timeout = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--timeout-ms",
        "50",
        "--",
        "python3",
        "-c",
        r#"import os, time
pid = os.fork()
if pid == 0:
    os.setsid()
    time.sleep(2)
    os._exit(0)
time.sleep(5)
"#,
    ]);

    assert!(timeout.status.success(), "{}", stdout(&timeout));
    // The detached child retains inherited pipes for two seconds. Keep a
    // scheduling-tolerant margin below that lifetime while still proving the
    // runner aborts readers instead of waiting for the detached holder.
    assert!(started.elapsed() < Duration::from_millis(1500));
    let value: Value = serde_json::from_slice(&timeout.stdout).unwrap();
    assert_eq!(value["data_preview"]["command"]["timed_out"], true);
}

#[test]
fn envelopes_expose_observation_metadata_for_agent_safety() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let complete = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "application/json",
            "--name",
            "complete",
        ],
        br#"{"ok":true}"#,
    );
    assert!(complete.status.success(), "{}", stdout(&complete));
    let complete_value: Value = serde_json::from_slice(&complete.stdout).unwrap();
    assert_eq!(
        complete_value["observation"]["completeness"]["status"],
        "complete"
    );
    assert_eq!(
        complete_value["observation"]["completeness"]["preview_complete"],
        true
    );
    assert_eq!(
        complete_value["observation"]["payload"]["cache_status"],
        "stored"
    );
    assert_eq!(complete_value["observation"]["payload"]["cached"], true);
    assert_eq!(complete_value["observation"]["availability"], "recoverable");
    assert_eq!(
        complete_value["observation"]["capture"]["stop_reason"],
        "complete"
    );
    assert_eq!(
        complete_value["observation"]["capture"]["budget"]["source"],
        "default"
    );
    assert!(complete_value["evidence_ref"].is_object());
    assert_eq!(
        complete_value["observation"]["trust"]["profile_backed"],
        false
    );
    assert_eq!(
        complete_value["observation"]["trust"]["source_kind"],
        "artifact"
    );
    assert_eq!(
        complete_value["observation"]["safety"]["redacted_before_persistence"],
        false
    );

    let partial = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "application/json",
            "--name",
            "partial-redacted",
        ],
        serde_json::to_vec(&json!({
            "items": (0..12)
                .map(|index| json!({
                    "id": index,
                    "body": "x".repeat(500),
                    "token": "secret-value"
                }))
                .collect::<Vec<_>>()
        }))
        .unwrap()
        .as_slice(),
    );
    assert!(partial.status.success(), "{}", stdout(&partial));
    let partial_value: Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(
        partial_value["observation"]["completeness"]["status"],
        "truncated"
    );
    assert_eq!(
        partial_value["observation"]["completeness"]["preview_complete"],
        false
    );
    assert_eq!(
        partial_value["observation"]["completeness"]["redacted"],
        true
    );
    assert!(
        partial_value["observation"]["completeness"]["omitted_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        partial_value["observation"]["safety"]["redacted_paths"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(partial_value["observation"]["availability"], "redacted");
    assert_eq!(
        partial_value["observation"]["capture"]["can_prove_absence"],
        false
    );
    assert_eq!(partial_value["evidence_ref"]["availability"], "redacted");
    assert_eq!(
        partial_value["evidence_ref"]["capture"]["can_prove_absence"],
        false
    );

    std::thread::sleep(std::time::Duration::from_secs(1));
    let cursor = partial_value["cursor"].as_str().unwrap();
    let expanded = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/items"]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let expanded_value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(
        expanded_value["observation"]["payload"]["cache_status"],
        "hit"
    );
    assert_eq!(expanded_value["observation"]["freshness"]["stale"], false);
    assert_eq!(
        expanded_value["observation"]["freshness"]["refresh_recommended"],
        false
    );
    assert_eq!(
        expanded_value["observation"]["freshness"]["stale_after_seconds"],
        86_400
    );
    assert!(
        expanded_value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| !warning.as_str().unwrap().contains("--refresh"))
    );

    let script = dir.path().join("emit_metadata.py");
    fs::write(
        &script,
        "import json\nprint(json.dumps({'value': 1, 'token': 'secret-value'}))\n",
    )
    .unwrap();
    let safe_seed = write_seed(
        dir.path(),
        "safe-metadata.json",
        &format!(
            r#"{{
              "kind": "cli",
              "operations": [{{
                "name": "emit",
                "command": "python3",
                "args": ["{}"],
                "effect": {{
                  "read_only": true,
                  "mutating": false,
                  "network": false,
                  "shell": false,
                  "sensitive": false,
                  "cacheable": true,
                  "requires_confirmation": false
                }}
              }}]
            }}"#,
            script.to_str().unwrap()
        ),
    );
    let discover_safe = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "safe_meta",
        "--kind",
        "cli",
        "--seed",
        safe_seed.to_str().unwrap(),
    ]);
    assert!(discover_safe.status.success(), "{}", stdout(&discover_safe));
    let no_cache = prog(&[
        "--dir",
        dir_arg,
        "call",
        "safe_meta",
        "emit",
        "--args",
        "{}",
        "--no-cache",
    ]);
    assert!(no_cache.status.success(), "{}", stdout(&no_cache));
    let no_cache_value: Value = serde_json::from_slice(&no_cache.stdout).unwrap();
    assert_eq!(
        no_cache_value["observation"]["trust"]["profile_backed"],
        true
    );
    assert_eq!(no_cache_value["observation"]["trust"]["source_kind"], "cli");
    assert_eq!(
        no_cache_value["observation"]["payload"]["cache_status"],
        "skipped"
    );
    assert_eq!(
        no_cache_value["observation"]["payload"]["expandable"],
        false
    );
    assert!(
        no_cache_value["observation"]["safety"]["cache_disabled_reason"]
            .as_str()
            .unwrap()
            .contains("--no-cache")
    );

    let sensitive_seed = write_seed(
        dir.path(),
        "sensitive-metadata.json",
        &format!(
            r#"{{
              "kind": "cli",
              "operations": [{{
                "name": "emit",
                "command": "python3",
                "args": ["{}"],
                "effect": {{
                  "read_only": true,
                  "mutating": false,
                  "network": false,
                  "shell": false,
                  "sensitive": true,
                  "cacheable": false,
                  "requires_confirmation": false
                }}
              }}]
            }}"#,
            script.to_str().unwrap()
        ),
    );
    let discover_sensitive = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "sensitive_meta",
        "--kind",
        "cli",
        "--seed",
        sensitive_seed.to_str().unwrap(),
    ]);
    assert!(
        discover_sensitive.status.success(),
        "{}",
        stdout(&discover_sensitive)
    );
    let sensitive = prog(&[
        "--dir",
        dir_arg,
        "call",
        "sensitive_meta",
        "emit",
        "--args",
        "{}",
    ]);
    assert!(sensitive.status.success(), "{}", stdout(&sensitive));
    let sensitive_value: Value = serde_json::from_slice(&sensitive.stdout).unwrap();
    assert_eq!(
        sensitive_value["observation"]["safety"]["sensitive_cache_disabled"],
        true
    );
    assert_eq!(
        sensitive_value["observation"]["safety"]["effects"]["sensitive"],
        true
    );
}

#[test]
fn discover_http_seed_persists_profile_without_upstream_probe() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(
        dir.path(),
        "http.json",
        r#"{
          "kind": "http",
          "base_url": "http://127.0.0.1:9",
          "operations": [{
            "name": "list",
            "method": "GET",
            "path": "/items",
            "args": {"owner": "string"},
            "effect": {
              "read_only": true,
              "mutating": false,
              "network": true,
              "shell": false,
              "sensitive": false,
              "cacheable": true,
              "requires_confirmation": false
            }
          }]
        }"#,
    );

    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);

    assert!(output.status.success(), "{}", stdout(&output));
    assert_eq!(stderr(&output), "");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["operations_found"], 1);
    assert_eq!(report["operations_probed"], 0);

    let profile = read_profile(dir.path(), "api");
    assert_eq!(profile["kind"], "http");
    assert_eq!(profile["operations"][0]["id"], "list");
    assert_eq!(
        profile["adapter"]["http"]["max_response_bytes"],
        2 * 1024 * 1024
    );
}

#[test]
fn discover_imports_openapi_without_upstream_probe() {
    let dir = tempfile::tempdir().unwrap();
    let spec = write_seed(
        dir.path(),
        "openapi.json",
        r#"{
          "openapi": "3.1.0",
          "info": {"title": "Issues", "version": "2026-07"},
          "servers": [{"url": "http://127.0.0.1:9"}],
          "paths": {
            "/issues/{id}": {
              "get": {
                "operationId": "getIssue",
                "parameters": [{
                  "name": "id",
                  "in": "path",
                  "required": true,
                  "schema": {"type": "string"}
                }],
                "responses": {
                  "200": {
                    "content": {
                      "application/json": {
                        "schema": {
                          "type": "object",
                          "properties": {"id": {"type": "string"}}
                        }
                      }
                    }
                  }
                }
              }
            }
          }
        }"#,
    );

    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        spec.to_str().unwrap(),
        "--import",
        "openapi",
        "--probe",
    ]);

    assert!(output.status.success(), "{}", stdout(&output));
    assert_eq!(stderr(&output), "");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["import_format"], "openapi");
    assert_eq!(report["operations_found"], 1);
    assert_eq!(report["schemas_imported"], 1);
    assert_eq!(report["operations_probed"], 0);
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("never executes upstream"))
    );

    let profile = read_profile(dir.path(), "api");
    assert_eq!(profile["kind"], "http");
    assert_eq!(profile["import_source"], "openapi");
    assert_eq!(profile["adapter"]["http"]["base_url"], "http://127.0.0.1:9");
    let operation = &profile["operations"][0];
    assert_eq!(operation["id"], "getissue");
    assert!(operation["output_shape"].is_null());
    assert!(operation["declared_output_schema"].is_object());
    assert_eq!(operation["effects"]["read_only"], true);
    assert_eq!(operation["invocation"]["http"]["path"], "/issues/{id}");
}

#[tokio::test]
async fn call_openapi_get_records_auto_upgrade_audit_in_observation_trust() {
    // An imported OpenAPI GET is graded Proven and stored confirmation-gated.
    // Under default trust (auto_upgrade=true) it is callable WITHOUT --yes and
    // the auto-upgrade decision is surfaced as audit metadata in
    // observation.trust.extra["auto_upgrade"] (acceptance: auditable).
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/issues/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "1", "title": "ok"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let spec_contents = r#"{
          "openapi": "3.1.0",
          "info": {"title": "Issues", "version": "2026-07"},
          "servers": [{"url": "__BASE__"}],
          "paths": {
            "/issues/{id}": {
              "get": {
                "operationId": "getIssue",
                "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string"}}],
                "responses": {"200": {"content": {"application/json": {"schema": {"type": "object"}}}}}
              }
            }
          }
        }"#
    .replace("__BASE__", &server.uri());
    let spec = write_seed(dir.path(), "openapi.json", &spec_contents);
    let dir_arg = dir.path().to_str().unwrap();
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        spec.to_str().unwrap(),
        "--import",
        "openapi",
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));

    // Stored gated + Proven grade.
    let profile = read_profile(dir.path(), "api");
    assert_eq!(
        profile["operations"][0]["effects"]["requires_confirmation"],
        true
    );
    assert_eq!(
        profile["operations"][0]["effects"]["evidence_grade"],
        "proven"
    );

    let hints = prog(&["--dir", dir_arg, "hints", "api", "getissue"]);
    assert!(hints.status.success(), "{}", stdout(&hints));
    let hint_value: Value = serde_json::from_slice(&hints.stdout).unwrap();
    let hint = &hint_value["hints"]["operations"][0];
    assert_eq!(hint["effects"]["requires_confirmation"], false);
    assert_eq!(hint["effects"]["evidence_grade"], "proven");
    assert!(
        hint["effects"]["auto_upgrade"]
            .as_str()
            .unwrap()
            .contains("relaxed")
    );
    assert!(
        !hint["risk_notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("requires confirmation"))
    );

    // Callable without --yes (auto-upgrade relaxes confirmation) and the audit
    // is recorded.
    let call = prog(&[
        "--dir",
        dir_arg,
        "call",
        "api",
        "getissue",
        "--args",
        r#"{"id":"1"}"#,
    ]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    let audit = &envelope["observation"]["trust"]["auto_upgrade"];
    assert_eq!(audit["grade"], "proven");
    assert_eq!(audit["relaxed_requires_confirmation"], true);
    assert!(audit["reason"].as_str().unwrap().contains("proven"));
    // The EFFECTIVE (relaxed) effect set is the one recorded. EffectSet.extra
    // is flattened, so evidence_grade/auto_upgrade appear at the effects top
    // level, not under an "extra" key.
    assert_eq!(
        envelope["observation"]["safety"]["effects"]["requires_confirmation"],
        false
    );
    assert_eq!(
        envelope["observation"]["safety"]["effects"]["evidence_grade"],
        "proven"
    );
    assert!(
        envelope["observation"]["safety"]["effects"]["auto_upgrade"]
            .as_str()
            .unwrap()
            .contains("relaxed")
    );
}

#[test]
fn call_openapi_get_requires_yes_when_auto_upgrade_disabled_on_profile() {
    // The per-source escape hatch: flipping trust.auto_upgrade=false on the
    // committed profile re-gates the Proven read-only op, so the agent must
    // pass --yes again (strict V1 behavior).
    let dir = tempfile::tempdir().unwrap();
    let spec = write_seed(
        dir.path(),
        "openapi.json",
        r#"{
          "openapi": "3.1.0",
          "info": {"title": "Issues", "version": "2026-07"},
          "servers": [{"url": "http://127.0.0.1:9"}],
          "paths": {
            "/issues/{id}": {
              "get": {
                "operationId": "getIssue",
                "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string"}}],
                "responses": {"200": {"content": {"application/json": {"schema": {"type": "object"}}}}}
              }
            }
          }
        }"#,
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        spec.to_str().unwrap(),
        "--import",
        "openapi",
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));

    // Flip the live knob on the stored profile.
    let profile_path = dir.path().join("profiles/api.json");
    let mut profile: Value = serde_json::from_slice(&fs::read(&profile_path).unwrap()).unwrap();
    profile["trust"]["auto_upgrade"] = json!(false);
    fs::write(&profile_path, serde_json::to_vec(&profile).unwrap()).unwrap();

    let hints = prog(&["--dir", dir_arg, "hints", "api", "getissue"]);
    assert!(hints.status.success(), "{}", stdout(&hints));
    let hint_value: Value = serde_json::from_slice(&hints.stdout).unwrap();
    let hint = &hint_value["hints"]["operations"][0];
    assert_eq!(hint["effects"]["requires_confirmation"], true);
    assert!(hint["effects"].get("auto_upgrade").is_none());
    assert!(
        hint["risk_notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("requires confirmation"))
    );

    // Without --yes the call is refused (would require a live server anyway).
    let call = prog(&[
        "--dir",
        dir_arg,
        "call",
        "api",
        "getissue",
        "--args",
        r#"{"id":"1"}"#,
    ]);
    assert!(!call.status.success(), "call should require confirmation");
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(envelope["error"]["kind"], "requires_confirmation");
}

#[test]
fn discover_imports_cli_help_conservatively() {
    let dir = tempfile::tempdir().unwrap();
    let help = write_seed(
        dir.path(),
        "taskctl.help",
        "Usage: taskctl <COMMAND>\n\nCommands:\n  list      list tasks\n  delete    delete a task\n\nOptions:\n  -h, --help\n",
    );

    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "taskctl",
        "--kind",
        "cli",
        "--seed",
        help.to_str().unwrap(),
        "--import",
        "cli-help",
        "--command-base",
        "taskctl --profile prod",
    ]);

    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["import_format"], "cli-help");
    assert_eq!(report["operations_found"], 2);
    assert_eq!(report["operations_probed"], 0);
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("conservative"))
    );

    let profile = read_profile(dir.path(), "taskctl");
    assert_eq!(profile["kind"], "cli");
    assert_eq!(profile["trust"]["allow_shell"], false);
    for operation in profile["operations"].as_array().unwrap() {
        assert_eq!(operation["effects"]["read_only"], false);
        assert_eq!(operation["effects"]["mutating"], true);
        assert_eq!(operation["effects"]["requires_confirmation"], true);
        assert_eq!(operation["effects"]["shell"], false);
    }
    let list = profile["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["id"] == "taskctl_list")
        .unwrap();
    assert_eq!(list["invocation"]["cli"]["command"], "taskctl");
    assert_eq!(list["invocation"]["cli"]["args"][0], "--profile");
    assert_eq!(list["invocation"]["cli"]["args"][1], "prod");
    assert_eq!(list["invocation"]["cli"]["args"][2], "list");
}

#[test]
fn call_persists_cli_error_evidence_but_returns_non_zero() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fail.py");
    fs::write(
        &script,
        "import sys\nprint('useful stdout')\nsys.stderr.write('useful stderr\\n')\nsys.exit(7)\n",
    )
    .unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let added = prog(&[
        "--dir",
        dir_arg,
        "source",
        "add-cli",
        "failing",
        "--operation",
        "run",
        "--read-only",
        "--",
        "python3",
        script.to_str().unwrap(),
    ]);
    assert!(added.status.success(), "{}", stdout(&added));

    let output = prog(&["--dir", dir_arg, "call", "failing", "run", "--args", "{}"]);
    assert!(!output.status.success());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["received_error"], true);
    let cursor = envelope["cursor"].as_str().unwrap();
    assert!(stdout(&prog(&["--dir", dir_arg, "evidence", cursor])).contains("useful stderr"));
}

#[test]
fn discover_http_get_defaults_to_read_only_but_post_hardens_seed_claims() {
    let dir = tempfile::tempdir().unwrap();
    let get_seed = write_seed(
        dir.path(),
        "http-get.json",
        r#"{
          "kind": "http",
          "base_url": "http://127.0.0.1:9",
          "operations": [{
            "name": "list",
            "method": "GET",
            "path": "/items"
          }]
        }"#,
    );
    let post_seed = write_seed(
        dir.path(),
        "http-post.json",
        r#"{
          "kind": "http",
          "base_url": "http://127.0.0.1:9",
          "operations": [{
            "name": "create",
            "method": "POST",
            "path": "/items",
            "effect": {
              "read_only": true,
              "mutating": false,
              "network": false,
              "shell": false,
              "sensitive": false,
              "cacheable": true,
              "requires_confirmation": false
            }
          }]
        }"#,
    );

    let get = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "api_get",
        "--kind",
        "http",
        "--seed",
        get_seed.to_str().unwrap(),
    ]);
    assert!(get.status.success(), "{}", stdout(&get));
    let get_profile = read_profile(dir.path(), "api_get");
    let get_effects = &get_profile["operations"][0]["effects"];
    assert_eq!(get_effects["read_only"], true);
    assert_eq!(get_effects["mutating"], false);
    assert_eq!(get_effects["network"], true);
    assert_eq!(get_effects["cacheable"], true);

    let post = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "api_post",
        "--kind",
        "http",
        "--seed",
        post_seed.to_str().unwrap(),
    ]);
    assert!(post.status.success(), "{}", stdout(&post));
    let post_profile = read_profile(dir.path(), "api_post");
    let post_effects = &post_profile["operations"][0]["effects"];
    assert_eq!(post_effects["read_only"], false);
    assert_eq!(post_effects["mutating"], true);
    assert_eq!(post_effects["network"], true);
    assert_eq!(post_effects["requires_confirmation"], true);
}

#[test]
fn discover_probe_learns_shape_and_hints_expose_guidance() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(dir.path(), "cli.json", &cli_probe_seed(true));

    let discover = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "local",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
        "--probe",
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));
    let report: Value = serde_json::from_slice(&discover.stdout).unwrap();
    assert_eq!(report["operations_probed"], 1);
    assert_eq!(report["shapes_learned"], 1);

    let hints = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "hints",
        "local",
        "emit",
    ]);
    assert!(hints.status.success(), "{}", stdout(&hints));
    let value: Value = serde_json::from_slice(&hints.stdout).unwrap();
    let operation = &value["hints"]["operations"][0];
    assert_eq!(operation["id"], "emit");
    assert!(operation["inputs"].get("required").is_some());
    assert!(
        operation["output_fields"]["observed"]
            .as_str()
            .unwrap()
            .contains("object")
    );
    assert!(
        value["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omitted| omitted["path"] == "/operations/0/output_fields/observed")
    );
    assert!(value["cursor"].as_str().unwrap().starts_with("pc1_"));
    assert!(
        !operation["expandable_regions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(operation["effects"]["read_only"], true);
}

#[test]
fn operation_hints_filter_suggestions_and_show_effective_cache_policy() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(
        dir.path(),
        "cli.json",
        r#"{
          "kind": "cli",
          "operations": [
            {
              "name": "status",
              "command": "python3",
              "args": ["-c", "print('{}')"],
              "effect": {
                "read_only": true,
                "mutating": false,
                "network": false,
                "shell": false,
                "sensitive": false,
                "cacheable": true,
                "requires_confirmation": false
              }
            },
            {
              "name": "write",
              "command": "python3",
              "args": ["-c", "print('{}')"],
              "effect": {
                "read_only": false,
                "mutating": true,
                "network": false,
                "shell": false,
                "sensitive": false,
                "cacheable": false,
                "requires_confirmation": true
              }
            }
          ]
        }"#,
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "local",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));

    let hints = prog(&["--dir", dir_arg, "hints", "local", "status"]);
    assert!(hints.status.success(), "{}", stdout(&hints));
    let value: Value = serde_json::from_slice(&hints.stdout).unwrap();
    let operations = value["hints"]["operations"].as_array().unwrap();
    let suggestions = value["hints"]["suggested_next_calls"].as_array().unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0]["id"], "status");
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0]["operation"], "status");
    assert_eq!(operations[0]["cache"]["enabled"], true);
    assert_eq!(operations[0]["cache"]["ttl_seconds"], 86_400);
}

#[test]
fn rediscover_preserves_learned_shape() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(dir.path(), "cli.json", &cli_probe_seed(true));
    let dir_arg = dir.path().to_str().unwrap();
    let seed_arg = seed.to_str().unwrap();

    let first = prog(&[
        "--dir", dir_arg, "discover", "local", "--kind", "cli", "--seed", seed_arg, "--probe",
    ]);
    assert!(first.status.success(), "{}", stdout(&first));
    let second = prog(&[
        "--dir", dir_arg, "discover", "local", "--kind", "cli", "--seed", seed_arg,
    ]);
    assert!(second.status.success(), "{}", stdout(&second));

    let profile = read_profile(dir.path(), "local");
    assert!(profile["operations"][0]["output_shape"].is_object());
    assert_eq!(profile["revision"], 2);
}

#[test]
fn probe_skips_effectless_operations_with_i6_warning() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(dir.path(), "unsafe-cli.json", &cli_probe_seed(false));

    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "unsafe_local",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
        "--probe",
    ]);

    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["operations_probed"], 0);
    assert!(report["warnings"][0].as_str().unwrap().contains("I6"));
    let profile = read_profile(dir.path(), "unsafe_local");
    assert!(profile["operations"][0]["output_shape"].is_null());
}

#[test]
fn partial_effect_metadata_keeps_missing_flags_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let seed = write_seed(
        dir.path(),
        "partial-cli.json",
        r#"{
          "kind": "cli",
          "operations": [{
            "name": "emit",
            "command": "python3",
            "args": ["-c", "print('{name}')"],
            "input_schema": {
              "type": "object",
              "required": ["name"],
              "properties": {"name": {"type": "string", "default": "ok"}}
            },
            "effect": {"read_only": true, "shell": false, "network": false}
          }]
        }"#,
    );

    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "partial",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
        "--probe",
    ]);

    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["operations_probed"], 0);
    assert!(report["warnings"][0].as_str().unwrap().contains("mutating"));
    let profile = read_profile(dir.path(), "partial");
    let effects = &profile["operations"][0]["effects"];
    assert_eq!(effects["read_only"], true);
    assert_eq!(effects["mutating"], true);
    assert_eq!(effects["cacheable"], false);
    assert_eq!(effects["requires_confirmation"], true);
}

#[test]
fn discover_bad_seed_names_bad_field() {
    let dir = tempfile::tempdir().unwrap();
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "bad",
        "--kind",
        "http",
        "--seed",
        r#"{"kind":"http","base_url":1,"operations":[]}"#,
    ]);

    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "bad_args");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("base_url")
    );
}

#[test]
fn captures_surface_immutable_observation_identity_across_cursor_and_listing() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let file = dir.path().join("observation.json");
    fs::write(&file, br#"{"items":[{"id":1},{"id":2}]}"#).unwrap();

    let observed = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--name",
        "fixture",
    ]);
    assert!(observed.status.success(), "{}", stdout(&observed));
    let first: Value = serde_json::from_slice(&observed.stdout).unwrap();
    let observation_id = first["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(observation_id.starts_with("obs_"));
    let cursor = first["cursor"].as_str().unwrap();

    let expanded = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/items"]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let expanded: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(expanded["observation"]["observation_id"], observation_id);

    let listed = prog(&["--dir", dir_arg, "cache", "observations", "--limit", "1"]);
    assert!(listed.status.success(), "{}", stdout(&listed));
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["observations"].as_array().unwrap().len(), 1);
    assert_eq!(listed["observations"][0]["observation_id"], observation_id);
    assert_eq!(listed["observations"][0]["availability"], "recoverable");
}

#[test]
fn observation_records_persist_transport_provider_identity() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    // `run` always executes a subprocess argv directly: provider is the
    // fixed "cli" transport literal, with no parser/lens (there is no
    // format to interpret, only a captured process result).
    let run = prog(&["--dir", dir_arg, "run", "--", "true"]);
    assert!(run.status.success(), "{}", stdout(&run));
    let run_value: Value = serde_json::from_slice(&run.stdout).unwrap();
    let run_id = run_value["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    // `call` against a registered CLI-kind source: provider must come from
    // the profile's declared `SourceKind`, exercising `source_kind_provider`
    // rather than any hardcoded literal.
    let seed = write_seed(
        dir.path(),
        "cli.json",
        &json!({
            "kind": "cli",
            "operations": [{
                "name": "status",
                "command": "python3",
                "args": ["-c", "print('{}')"],
                "effect": {
                    "read_only": true,
                    "mutating": false,
                    "network": false,
                    "shell": false,
                    "sensitive": false,
                    "cacheable": true,
                    "requires_confirmation": false
                }
            }]
        })
        .to_string(),
    );
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "local",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));
    let call = prog(&["--dir", dir_arg, "call", "local", "status", "--args", "{}"]);
    assert!(call.status.success(), "{}", stdout(&call));
    let call_value: Value = serde_json::from_slice(&call.stdout).unwrap();
    let call_id = call_value["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    // `observe` ingests a local artifact directly: there is no transport,
    // so provider must be absent, while parser IS populated (it interprets
    // format, not transport).
    let file = dir.path().join("payload.json");
    fs::write(&file, br#"{"a":1}"#).unwrap();
    let observed = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--name",
        "fixture",
    ]);
    assert!(observed.status.success(), "{}", stdout(&observed));
    let observed_value: Value = serde_json::from_slice(&observed.stdout).unwrap();
    let observe_id = observed_value["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    let listed = prog(&["--dir", dir_arg, "cache", "observations", "--limit", "3"]);
    assert!(listed.status.success(), "{}", stdout(&listed));
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    let records = listed["observations"].as_array().unwrap();
    let record = |id: &str| {
        records
            .iter()
            .find(|record| record["observation_id"] == id)
            .unwrap_or_else(|| panic!("observation {id} missing from cache observations listing"))
    };

    let run_record = record(&run_id);
    assert_eq!(run_record["provider"], "cli");
    assert!(run_record["parser"].is_null());

    let call_record = record(&call_id);
    assert_eq!(call_record["provider"], "cli");

    let observe_record = record(&observe_id);
    assert!(observe_record["provider"].is_null());
    assert!(observe_record["parser"].is_string());
}

#[test]
fn automatic_delta_never_matches_similar_but_different_command_invocations() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let first = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--",
        "python3",
        "-c",
        "print('error one')",
    ]);
    assert!(first.status.success(), "{}", stdout(&first));
    let second = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--",
        "python3",
        "-c",
        "print('error two')",
    ]);
    assert!(second.status.success(), "{}", stdout(&second));
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert!(second.get("changes_since").is_none());
}

#[test]
fn automatic_delta_requires_the_same_comparison_family() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let state = dir.path().join("state.txt");
    let script = dir.path().join("emit.py");
    fs::write(
        &script,
        "from pathlib import Path\nprint(Path(__import__('sys').argv[1]).read_text())\n",
    )
    .unwrap();
    fs::write(&state, "error old failure\n").unwrap();
    let first = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--comparison-family",
        "targeted",
        "--selection-scope",
        "/failure_sections",
        "--selection-exhaustive",
        "--",
        "python3",
        script.to_str().unwrap(),
        state.to_str().unwrap(),
    ]);
    assert!(first.status.success(), "{}", stdout(&first));

    fs::write(&state, "error new failure\n").unwrap();
    let second = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--comparison-family",
        "full-suite",
        "--selection-scope",
        "/failure_sections",
        "--selection-exhaustive",
        "--",
        "python3",
        script.to_str().unwrap(),
        state.to_str().unwrap(),
    ]);
    assert!(second.status.success(), "{}", stdout(&second));
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert!(second.get("changes_since").is_none());

    let observations = prog(&["--dir", dir_arg, "cache", "observations"]);
    assert!(observations.status.success(), "{}", stdout(&observations));
    let observations: Value = serde_json::from_slice(&observations.stdout).unwrap();
    let families = observations["observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|observation| observation["comparison_family"].as_str())
        .collect::<Vec<_>>();
    assert!(families.contains(&"targeted"));
    assert!(families.contains(&"full-suite"));
    assert!(
        observations["observations"].as_array().unwrap().iter().all(
            |observation| observation["selection"]
                == json!({
                    "scopes": ["/failure_sections"],
                    "exhaustive": true,
                })
        )
    );
}

/// Builds a 30-line document where line index `error_index` (0-based) is an
/// error line and every other line is innocuous filler. `head` covers
/// indices `[0, 10)` and `tail` covers `[20, 30)`; index 15 falls in neither.
fn thirty_lines_with_error_at(error_index: usize) -> String {
    (0..30)
        .map(|index| {
            if index == error_index {
                "error alpha failure".to_string()
            } else {
                format!("line {index:02} ok")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}
#[test]
fn delta_never_reports_resolved_for_a_finding_that_moved_into_the_derivation_window() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let state = dir.path().join("state.txt");
    let script = dir.path().join("emit.py");
    fs::write(
        &script,
        "from pathlib import Path\nprint(Path(__import__('sys').argv[1]).read_text())\n",
    )
    .unwrap();

    // Baseline: the error line sits at index 5, inside `head` (indices 0..10).
    fs::write(&state, thirty_lines_with_error_at(5)).unwrap();
    let first = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--selection-scope",
        "suite",
        "--selection-exhaustive",
        "--",
        "python3",
        script.to_str().unwrap(),
        state.to_str().unwrap(),
    ]);
    assert!(first.status.success(), "{}", stdout(&first));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let first_id = first["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Subject: identical except the error line moved to index 15, which is
    // outside both `head` (0..10) and `tail` (20..30).
    fs::write(&state, thirty_lines_with_error_at(15)).unwrap();
    let second = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--selection-scope",
        "suite",
        "--selection-exhaustive",
        "--",
        "python3",
        script.to_str().unwrap(),
        state.to_str().unwrap(),
    ]);
    assert!(second.status.success(), "{}", stdout(&second));
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    let second_id = second["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    let delta = prog(&["--dir", dir_arg, "delta", &first_id, &second_id]);
    assert!(delta.status.success(), "{}", stdout(&delta));
    let delta: Value = serde_json::from_slice(&delta.stdout).unwrap();

    // Before the fix, capture completeness only tracked byte capture, so
    // `can_prove_absence` came back `true` even though only the head/tail
    // window was ever examined for findings.
    assert_eq!(
        delta["assessment"]["can_prove_absence"],
        json!(false),
        "{delta:#}"
    );
    assert!(
        delta["assessment"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason.as_str().unwrap().contains("derivation_windowed")),
        "expected a reason mentioning derivation_windowed: {delta:#}"
    );

    // Before the fix, the baseline's `/stdout/head/5` finding (moved out of
    // the subject's derivation window, not actually absent from the
    // subject's captured output) was falsely reported `resolved`.
    let findings = delta["findings"].as_array().unwrap();
    assert!(
        !findings
            .iter()
            .any(|finding| finding["status"] == "resolved"),
        "no finding should be resolved when absence cannot be proven: {delta:#}"
    );
    let moved_finding = findings
        .iter()
        .find(|finding| finding["baseline_path"] == "/stdout/head/5")
        .expect("baseline finding at /stdout/head/5 should still appear in the delta");
    assert_eq!(moved_finding["status"], json!("unknown"), "{delta:#}");
}
#[test]
fn verification_readiness_requires_every_declared_scope() {
    let workspace = test_git_repo();
    let root = workspace.path();
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let state = dir.path().join("state.txt");
    let script = dir.path().join("emit.py");
    fs::write(
        &script,
        "from pathlib import Path\nprint(Path(__import__('sys').argv[1]).read_text())\n",
    )
    .unwrap();
    fs::write(&state, "error old failure\n").unwrap();
    let first = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "run",
            "--selection-scope",
            "full-suite",
            "--selection-exhaustive",
            "--",
            "python3",
            script.to_str().unwrap(),
            state.to_str().unwrap(),
        ],
    );
    assert!(first.status.success(), "{}", stdout(&first));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let first_id = first["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    fs::write(&state, "error new failure\n").unwrap();
    let second = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "run",
            "--selection-scope",
            "full-suite",
            "--selection-exhaustive",
            "--",
            "python3",
            script.to_str().unwrap(),
            state.to_str().unwrap(),
        ],
    );
    assert!(second.status.success(), "{}", stdout(&second));
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    let second_id = second["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();
    let delta = prog_in_dir(root, &["--dir", dir_arg, "delta", &first_id, &second_id]);
    assert!(delta.status.success(), "{}", stdout(&delta));
    let delta: Value = serde_json::from_slice(&delta.stdout).unwrap();
    let resolved_fingerprint = delta["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["status"] == "resolved")
        .and_then(|finding| finding["fingerprint"].as_str())
        .unwrap()
        .to_string();

    let target = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "session",
            "obligation-add",
            "target-failure",
            "--check",
            "target failure is absent",
            "--scope",
            "target",
            "--origin-observation",
            &first_id,
            "--evidence-observation",
            &second_id,
            "--expected-absent-fingerprint",
            &resolved_fingerprint,
        ],
    );
    assert!(target.status.success(), "{}", stdout(&target));
    let affected = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "session",
            "obligation-add",
            "affected-suite",
            "--check",
            "affected test suite passes",
            "--scope",
            "affected-suite",
        ],
    );
    assert!(affected.status.success(), "{}", stdout(&affected));

    let readiness = prog_in_dir(root, &["--dir", dir_arg, "session", "show", "--readiness"]);
    assert!(readiness.status.success(), "{}", stdout(&readiness));
    let readiness: Value = serde_json::from_slice(&readiness.stdout).unwrap();
    assert_eq!(readiness["schema"], "prog.verification");
    assert_eq!(readiness["configured"], true);
    assert_eq!(readiness["ready"], false);
    let target = readiness["evaluations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|evaluation| evaluation["obligation"]["id"] == "target-failure")
        .unwrap();
    assert_eq!(target["status"], "new");
    assert!(
        target["new_regressions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|finding| finding["evidence_ref"].is_object())
    );
    let affected = readiness["evaluations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|evaluation| evaluation["obligation"]["id"] == "affected-suite")
        .unwrap();
    assert_eq!(affected["status"], "pending");
    assert!(
        readiness["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker.as_str().unwrap().contains("affected-suite"))
    );
}

#[test]
fn verification_without_obligations_is_explicitly_not_ready() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let started = prog(&[
        "--dir",
        dir_arg,
        "session",
        "start",
        "--goal",
        "verify a patch",
    ]);
    assert!(started.status.success(), "{}", stdout(&started));
    let readiness = prog(&["--dir", dir_arg, "session", "obligation-list"]);
    assert!(readiness.status.success(), "{}", stdout(&readiness));
    let readiness: Value = serde_json::from_slice(&readiness.stdout).unwrap();
    assert_eq!(readiness["configured"], false);
    assert_eq!(readiness["ready"], false);
    assert!(
        readiness["blockers"][0]
            .as_str()
            .unwrap()
            .contains("no verification obligations")
    );
}

#[test]
fn verification_accepts_a_complete_successful_command_as_evidence() {
    let workspace = test_git_repo();
    let root = workspace.path();
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let run = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "run",
            "--",
            "python3",
            "-c",
            "print('all clear')",
        ],
    );
    assert!(run.status.success(), "{}", stdout(&run));
    let run: Value = serde_json::from_slice(&run.stdout).unwrap();
    let observation_id = run["observation"]["observation_id"].as_str().unwrap();
    let add = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "session",
            "obligation-add",
            "target-command",
            "--check",
            "target command passes",
            "--scope",
            "target",
            "--evidence-observation",
            observation_id,
        ],
    );
    assert!(add.status.success(), "{}", stdout(&add));
    let readiness = prog_in_dir(root, &["--dir", dir_arg, "session", "show", "--readiness"]);
    assert!(readiness.status.success(), "{}", stdout(&readiness));
    let readiness: Value = serde_json::from_slice(&readiness.stdout).unwrap();
    assert_eq!(readiness["ready"], true);
    assert_eq!(readiness["evaluations"][0]["status"], "passed");
}

#[test]
fn verification_obligation_enforces_declared_operation_and_surfaces_advisory_hints() {
    let workspace = test_git_repo();
    let root = workspace.path();
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let run = prog_in_dir(root, &["--dir", dir_arg, "run", "--", "true"]);
    assert!(run.status.success(), "{}", stdout(&run));
    let run: Value = serde_json::from_slice(&run.stdout).unwrap();
    let observation_id = run["observation"]["observation_id"].as_str().unwrap();

    let declared = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "session",
            "obligation-add",
            "normalizer-advice",
            "--check",
            "normalizer suggestion",
            "--scope",
            "target",
            "--declared-by",
            "normalizer",
            "--evidence-observation",
            observation_id,
            "--advisory-argv",
            "true",
        ],
    );
    assert!(declared.status.success(), "{}", stdout(&declared));
    let declared: Value = serde_json::from_slice(&declared.stdout).unwrap();
    assert_eq!(declared["required"], false);
    assert_eq!(declared["declared_by"], "normalizer");
    assert_eq!(declared["advisory_actions"][0]["argv"], json!(["true"]));
    assert_eq!(
        declared["advisory_actions"][0]["does_not_satisfy"],
        json!(["normalizer-advice"])
    );

    let mismatched = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "session",
            "obligation-add",
            "wrong-operation",
            "--check",
            "the expected command passes",
            "--scope",
            "target",
            "--evidence-observation",
            observation_id,
            "--expected-argv",
            "false",
        ],
    );
    assert!(mismatched.status.success(), "{}", stdout(&mismatched));
    let readiness = prog_in_dir(root, &["--dir", dir_arg, "session", "show", "--readiness"]);
    assert!(readiness.status.success(), "{}", stdout(&readiness));
    let readiness: Value = serde_json::from_slice(&readiness.stdout).unwrap();
    let mismatch = readiness["evaluations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|evaluation| evaluation["obligation"]["id"] == "wrong-operation")
        .unwrap();
    assert_eq!(mismatch["status"], "stale");
    assert!(
        mismatch["reasons"][0]
            .as_str()
            .unwrap()
            .contains("declared operation")
    );
}

#[test]
fn recipe_declares_an_advisory_obligation_without_auto_execution() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let lens_dir = first_party_lens_dir();
    let log = dir.path().join("service.log");
    fs::write(&log, "INFO start\nERROR expected recipe evidence\n").unwrap();
    let recipe = prog(&[
        "--dir",
        dir_arg,
        "--lens-dir",
        lens_dir.to_str().unwrap(),
        "recipe",
        "logs-root-cause",
        "--file",
        log.to_str().unwrap(),
    ]);
    assert!(recipe.status.success(), "{}", stdout(&recipe));
    let obligations = prog(&["--dir", dir_arg, "session", "obligation-list"]);
    assert!(obligations.status.success(), "{}", stdout(&obligations));
    let obligations: Value = serde_json::from_slice(&obligations.stdout).unwrap();
    let recipe = obligations["evaluations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|evaluation| evaluation["obligation"]["declared_by"] == "recipe")
        .unwrap();
    assert_eq!(recipe["obligation"]["required"], false);
    assert!(recipe["obligation"]["evidence_observation_id"].is_string());
}

#[test]
fn verification_never_passes_truncated_or_unvalidated_source_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let truncated = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--max-stdout-bytes",
        "1",
        "--",
        "python3",
        "-c",
        "print('complete success output')",
    ]);
    assert!(truncated.status.success(), "{}", stdout(&truncated));
    let truncated: Value = serde_json::from_slice(&truncated.stdout).unwrap();
    let truncated_id = truncated["observation"]["observation_id"].as_str().unwrap();
    let add = prog(&[
        "--dir",
        dir_arg,
        "session",
        "obligation-add",
        "truncated",
        "--check",
        "truncated evidence cannot pass",
        "--scope",
        "target",
        "--evidence-observation",
        truncated_id,
    ]);
    assert!(add.status.success(), "{}", stdout(&add));

    let complete = prog(&["--dir", dir_arg, "run", "--", "true"]);
    assert!(complete.status.success(), "{}", stdout(&complete));
    let complete: Value = serde_json::from_slice(&complete.stdout).unwrap();
    let complete_id = complete["observation"]["observation_id"].as_str().unwrap();
    let add = prog(&[
        "--dir",
        dir_arg,
        "session",
        "obligation-add",
        "source-state",
        "--check",
        "source validator must confirm unchanged",
        "--scope",
        "target",
        "--evidence-observation",
        complete_id,
        "--required-state",
        "source-unchanged",
    ]);
    assert!(add.status.success(), "{}", stdout(&add));

    let readiness = prog(&["--dir", dir_arg, "session", "show", "--readiness"]);
    assert!(readiness.status.success(), "{}", stdout(&readiness));
    let readiness: Value = serde_json::from_slice(&readiness.stdout).unwrap();
    let status_for = |id: &str| {
        readiness["evaluations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|evaluation| evaluation["obligation"]["id"] == id)
            .unwrap()["status"]
            .as_str()
            .unwrap()
    };
    assert_eq!(status_for("truncated"), "unverifiable");
    assert_eq!(status_for("source-state"), "stale");
    assert!(!readiness["ready"].as_bool().unwrap());
}

#[test]
fn verification_treats_targeted_incomplete_reruns_as_not_observed() {
    let workspace = test_git_repo();
    let root = workspace.path();
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let state = dir.path().join("state.txt");
    let script = dir.path().join("emit.py");
    fs::write(
        &script,
        "from pathlib import Path\nprint(Path(__import__('sys').argv[1]).read_text())\n",
    )
    .unwrap();
    fs::write(&state, "error old failure\n").unwrap();
    let first = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "run",
            "--selection-scope",
            "full-suite",
            "--selection-exhaustive",
            "--",
            "python3",
            script.to_str().unwrap(),
            state.to_str().unwrap(),
        ],
    );
    assert!(first.status.success(), "{}", stdout(&first));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let first_id = first["observation"]["observation_id"].as_str().unwrap();
    fs::write(&state, "all clear\n").unwrap();
    let second = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "run",
            "--selection-scope",
            "targeted-case",
            "--",
            "python3",
            script.to_str().unwrap(),
            state.to_str().unwrap(),
        ],
    );
    assert!(second.status.success(), "{}", stdout(&second));
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    let second_id = second["observation"]["observation_id"].as_str().unwrap();
    let delta = prog_in_dir(root, &["--dir", dir_arg, "delta", first_id, second_id]);
    assert!(delta.status.success(), "{}", stdout(&delta));
    let delta: Value = serde_json::from_slice(&delta.stdout).unwrap();
    let fingerprint = delta["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["status"] == "not_observed")
        .and_then(|finding| finding["fingerprint"].as_str())
        .unwrap_or_else(|| panic!("expected not_observed delta: {delta}"));
    let add = prog_in_dir(
        root,
        &[
            "--dir",
            dir_arg,
            "session",
            "obligation-add",
            "targeted-rerun",
            "--check",
            "old failure is absent",
            "--scope",
            "full-suite",
            "--origin-observation",
            first_id,
            "--evidence-observation",
            second_id,
            "--expected-absent-fingerprint",
            fingerprint,
        ],
    );
    assert!(add.status.success(), "{}", stdout(&add));
    let readiness = prog_in_dir(root, &["--dir", dir_arg, "session", "show", "--readiness"]);
    assert!(readiness.status.success(), "{}", stdout(&readiness));
    let readiness: Value = serde_json::from_slice(&readiness.stdout).unwrap();
    assert_eq!(readiness["evaluations"][0]["status"], "not_observed");
    assert!(!readiness["ready"].as_bool().unwrap());
}

#[test]
fn verification_marks_passing_evidence_stale_after_workspace_edit() {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path();
    let state = root.join("tracked.txt");
    fs::write(&state, "before\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "prog@example.test"],
        vec!["config", "user.name", "prog test"],
        vec!["add", "tracked.txt"],
        vec!["commit", "-qm", "initial"],
    ] {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let store = root.join(".prog-state");
    let store_arg = store.to_str().unwrap();
    let run = prog_in_dir(root, &["--dir", store_arg, "run", "--", "true"]);
    assert!(run.status.success(), "{}", stdout(&run));
    let run: Value = serde_json::from_slice(&run.stdout).unwrap();
    let observation_id = run["observation"]["observation_id"].as_str().unwrap();
    let add = prog_in_dir(
        root,
        &[
            "--dir",
            store_arg,
            "session",
            "obligation-add",
            "workspace-check",
            "--check",
            "workspace remains unchanged",
            "--scope",
            "target",
            "--evidence-observation",
            observation_id,
            "--required-state",
            "workspace-unchanged",
        ],
    );
    assert!(add.status.success(), "{}", stdout(&add));
    fs::write(&state, "after\n").unwrap();
    let readiness = prog_in_dir(
        root,
        &["--dir", store_arg, "session", "show", "--readiness"],
    );
    assert!(readiness.status.success(), "{}", stdout(&readiness));
    let readiness: Value = serde_json::from_slice(&readiness.stdout).unwrap();
    assert_eq!(readiness["evaluations"][0]["status"], "stale");
    assert!(
        readiness["evaluations"][0]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason.as_str().unwrap().contains("workspace"))
    );
}

#[cfg(unix)]
#[test]
fn verification_treats_unreadable_untracked_workspace_evidence_as_stale() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path();
    fs::write(root.join("tracked.txt"), "initial\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "prog@example.test"],
        vec!["config", "user.name", "prog test"],
        vec!["add", "tracked.txt"],
        vec!["commit", "-qm", "initial"],
    ] {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }

    // This models a dirty worktree entry whose bytes cannot be safely
    // attributed to one snapshot. The product must make readiness stale,
    // rather than claiming workspace evidence is unchanged.
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("changing.txt"), "outside workspace\n").unwrap();
    symlink(
        outside.path().join("changing.txt"),
        root.join("unreadable-untracked-entry"),
    )
    .unwrap();

    let store = root.join(".prog-state");
    let store_arg = store.to_str().unwrap();
    let run = prog_in_dir(root, &["--dir", store_arg, "run", "--", "true"]);
    assert!(run.status.success(), "{}", stdout(&run));
    let run: Value = serde_json::from_slice(&run.stdout).unwrap();
    let observation_id = run["observation"]["observation_id"].as_str().unwrap();
    let add = prog_in_dir(
        root,
        &[
            "--dir",
            store_arg,
            "session",
            "obligation-add",
            "workspace-unreadable",
            "--check",
            "workspace remains unchanged",
            "--scope",
            "target",
            "--evidence-observation",
            observation_id,
            "--required-state",
            "workspace-unchanged",
        ],
    );
    assert!(add.status.success(), "{}", stdout(&add));

    let readiness = prog_in_dir(
        root,
        &["--dir", store_arg, "session", "show", "--readiness"],
    );
    assert!(readiness.status.success(), "{}", stdout(&readiness));
    let readiness: Value = serde_json::from_slice(&readiness.stdout).unwrap();
    assert_eq!(readiness["evaluations"][0]["status"], "stale");
    assert!(
        readiness["evaluations"][0]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap()
                .contains("unreadable or unstable dirty-file content"))
    );
    assert!(!readiness["ready"].as_bool().unwrap());
}

#[test]
fn disclosure_budget_flag_is_hard_and_retains_recovery_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("large.json");
    fs::write(
        &file,
        serde_json::to_vec(&json!({"items": [{"body": "x".repeat(16_000)}]})).unwrap(),
    )
    .unwrap();
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "--budget-bytes",
        "2048",
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--name",
        "large",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert!(
        output.stdout.len() <= 2048,
        "stdout was {} bytes: {}",
        output.stdout.len(),
        stdout(&output)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["disclosure_budget"]["source"], "flag");
    assert_eq!(value["disclosure_budget"]["effective_bytes"], 2048);
    assert_eq!(
        value["disclosure_budget"]["actual_bytes"].as_u64().unwrap() as usize,
        output.stdout.len()
    );
    assert!(value["cursor"].as_str().is_some());
    assert!(value["observation"]["observation_id"].as_str().is_some());
}

#[test]
fn disclosure_budget_precedence_and_token_estimate_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let from_environment = prog_with_env(
        &[
            "--dir",
            dir_arg,
            "--budget-tokens",
            "512",
            "session",
            "start",
        ],
        &[("PROG_BUDGET_BYTES", "4096")],
    );
    assert!(
        from_environment.status.success(),
        "{}",
        stdout(&from_environment)
    );
    let from_environment: Value = serde_json::from_slice(&from_environment.stdout).unwrap();
    assert_eq!(from_environment["disclosure_budget"]["source"], "flag");
    assert_eq!(
        from_environment["disclosure_budget"]["requested_tokens"],
        512
    );
    assert_eq!(
        from_environment["disclosure_budget"]["requested_bytes"],
        Value::Null
    );
    assert_eq!(
        from_environment["disclosure_budget"]["effective_bytes"],
        2048
    );
    assert_eq!(
        from_environment["disclosure_budget"]["token_estimator"],
        "bytes_div_4_approximate"
    );
    assert_eq!(from_environment["capture_budget"]["source"], "unavailable");
    assert_eq!(from_environment["storage_budget"]["source"], "default");

    let byte_flag_wins = prog_with_env(
        &[
            "--dir",
            dir_arg,
            "--budget-bytes",
            "3072",
            "session",
            "start",
        ],
        &[
            ("PROG_BUDGET_BYTES", "4096"),
            ("PROG_BUDGET_TOKENS", "8192"),
        ],
    );
    assert!(
        byte_flag_wins.status.success(),
        "{}",
        stdout(&byte_flag_wins)
    );
    let byte_flag_wins: Value = serde_json::from_slice(&byte_flag_wins.stdout).unwrap();
    assert_eq!(byte_flag_wins["disclosure_budget"]["effective_bytes"], 3072);
    assert_eq!(byte_flag_wins["disclosure_budget"]["requested_bytes"], 3072);

    let environment_only = prog_with_env(
        &["--dir", dir_arg, "session", "start"],
        &[("PROG_BUDGET_BYTES", "4096")],
    );
    assert!(
        environment_only.status.success(),
        "{}",
        stdout(&environment_only)
    );
    let environment_only: Value = serde_json::from_slice(&environment_only.stdout).unwrap();
    assert_eq!(
        environment_only["disclosure_budget"]["source"],
        "environment"
    );
    assert_eq!(
        environment_only["disclosure_budget"]["effective_bytes"],
        4096
    );

    let capped = prog_with_env(
        &[
            "--dir",
            dir_arg,
            "--budget-bytes",
            "1000000",
            "session",
            "start",
        ],
        &[],
    );
    assert!(capped.status.success(), "{}", stdout(&capped));
    let capped: Value = serde_json::from_slice(&capped.stdout).unwrap();
    assert_eq!(capped["disclosure_budget"]["effective_bytes"], 64 * 1024);
}

#[test]
fn source_profile_disclosure_budget_is_applied_and_lower_precedence_than_env_or_flag() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let script = dir.path().join("large-output.py");
    fs::write(
        &script,
        "import json\nprint(json.dumps({'items': [{'body': 'x' * 12000}]}))\n",
    )
    .unwrap();
    let seed = write_seed(
        dir.path(),
        "profile-budget.json",
        &format!(
            r#"{{
              "kind": "cli",
              "operations": [{{
                "name": "list",
                "command": "python3",
                "args": ["{}"],
                "effect": {{
                  "read_only": true,
                  "mutating": false,
                  "network": false,
                  "shell": false,
                  "sensitive": false,
                  "cacheable": true,
                  "requires_confirmation": false
                }}
              }}]
            }}"#,
            script.to_str().unwrap()
        ),
    );
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "budgeted",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let profile_path = dir.path().join("profiles/budgeted.json");
    let mut profile: Value = serde_json::from_slice(&fs::read(&profile_path).unwrap()).unwrap();
    profile["disclosure_budget"] = json!({"max_bytes": 3072});
    fs::write(&profile_path, serde_json::to_vec_pretty(&profile).unwrap()).unwrap();

    let profile_applied = prog(&["--dir", dir_arg, "call", "budgeted", "list", "--args", "{}"]);
    assert!(
        profile_applied.status.success(),
        "{}",
        stdout(&profile_applied)
    );
    assert!(
        profile_applied.stdout.len() <= 3072,
        "{} bytes: {}",
        profile_applied.stdout.len(),
        stdout(&profile_applied)
    );
    let profile_applied: Value = serde_json::from_slice(&profile_applied.stdout).unwrap();
    assert_eq!(profile_applied["disclosure_budget"]["source"], "profile");
    assert_eq!(
        profile_applied["disclosure_budget"]["effective_bytes"],
        3072
    );

    let env_applied = prog_with_env(
        &[
            "--dir",
            dir_arg,
            "call",
            "budgeted",
            "list",
            "--args",
            "{}",
            "--refresh",
        ],
        &[("PROG_BUDGET_BYTES", "4096")],
    );
    assert!(env_applied.status.success(), "{}", stdout(&env_applied));
    let env_applied: Value = serde_json::from_slice(&env_applied.stdout).unwrap();
    assert_eq!(env_applied["disclosure_budget"]["source"], "environment");
    assert_eq!(env_applied["disclosure_budget"]["effective_bytes"], 4096);

    let flag_applied = prog_with_env(
        &[
            "--dir",
            dir_arg,
            "--budget-bytes",
            "3584",
            "call",
            "budgeted",
            "list",
            "--args",
            "{}",
            "--refresh",
        ],
        &[("PROG_BUDGET_BYTES", "4096")],
    );
    assert!(flag_applied.status.success(), "{}", stdout(&flag_applied));
    let flag_applied: Value = serde_json::from_slice(&flag_applied.stdout).unwrap();
    assert_eq!(flag_applied["disclosure_budget"]["source"], "flag");
    assert_eq!(flag_applied["disclosure_budget"]["effective_bytes"], 3584);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]
    #[test]
    fn disclosure_budget_is_monotone_across_agent_visible_response_shapes(
        first in 3072u32..8193,
        second in 3072u32..8193,
    ) {
        let (smaller, larger) = (first.min(second), first.max(second));
        let prepare = || {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("monotone.json");
            fs::write(
                &file,
                serde_json::to_vec(&json!({
                    "items": [{"needle": "target", "body": "x".repeat(12_000)}]
                }))
                .unwrap(),
            )
            .unwrap();
            let captured = prog(&[
                "--dir", dir.path().to_str().unwrap(), "observe", "--file",
                file.to_str().unwrap(), "--name", "monotone",
            ]);
            assert!(captured.status.success(), "{}", stdout(&captured));
            let captured: Value = serde_json::from_slice(&captured.stdout).unwrap();
            (dir, file, captured["cursor"].as_str().unwrap().to_string())
        };
        let (lower_dir, lower_file, lower_cursor) = prepare();
        let (upper_dir, upper_file, upper_cursor) = prepare();
        let lower_dir = lower_dir.path().to_str().unwrap();
        let upper_dir = upper_dir.path().to_str().unwrap();
        let command_pairs = [
            (
                vec!["observe".to_string(), "--file".to_string(), lower_file.to_string_lossy().into_owned(), "--name".to_string(), "monotone-repeat".to_string()],
                vec!["observe".to_string(), "--file".to_string(), upper_file.to_string_lossy().into_owned(), "--name".to_string(), "monotone-repeat".to_string()],
            ),
            (
                vec!["inspect".to_string(), lower_cursor.clone(), "--goal".to_string(), "find target".to_string()],
                vec!["inspect".to_string(), upper_cursor.clone(), "--goal".to_string(), "find target".to_string()],
            ),
            (
                vec!["search".to_string(), lower_cursor.clone(), "target".to_string()],
                vec!["search".to_string(), upper_cursor.clone(), "target".to_string()],
            ),
            (
                vec!["evidence".to_string(), lower_cursor, "--path".to_string(), "/items/0/body".to_string()],
                vec!["evidence".to_string(), upper_cursor, "--path".to_string(), "/items/0/body".to_string()],
            ),
        ];

        for (lower_shape, upper_shape) in &command_pairs {
            let lower_args = lower_shape.iter().map(String::as_str).collect::<Vec<_>>();
            let upper_args = upper_shape.iter().map(String::as_str).collect::<Vec<_>>();
            let lower = prog_with_budget(lower_dir, smaller, &lower_args);
            let upper = prog_with_budget(upper_dir, larger, &upper_args);
            prop_assert!(lower.status.success(), "{}", stdout(&lower));
            prop_assert!(upper.status.success(), "{}", stdout(&upper));
            prop_assert!(
                lower.stdout.len() <= upper.stdout.len(),
                "lower {smaller} emitted {} bytes; upper {larger} emitted {} bytes for {lower_shape:?}",
                lower.stdout.len(),
                upper.stdout.len(),
            );
            prop_assert!(lower.stdout.len() <= smaller as usize);
            prop_assert!(upper.stdout.len() <= larger as usize);
        }
    }
}

#[test]
fn disclosure_budget_rejects_zero_and_reports_the_minimum() {
    let output = prog(&["--budget-bytes", "0", "session", "start"]);
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "bad_args");

    let output = prog(&["--budget-bytes", "128", "session", "start"]);
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "budget_too_small");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("at least 512 bytes")
    );
}

#[tokio::test]
async fn http_capture_persists_scoped_etag_source_state() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/records/7"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "W/\"record-7\"")
                .set_body_json(json!({"id": 7})),
        )
        .mount(&server)
        .await;
    let seed = write_seed(
        dir.path(),
        "source-state.json",
        &format!(
            r#"{{"kind":"http","base_url":"{}","operations":[{{"name":"get","method":"GET","path":"/records/{{id}}","input_schema":{{"type":"object","properties":{{"id":{{"type":"integer"}}}},"required":["id"]}},"effect":{{"read_only":true,"mutating":false,"network":true,"shell":false,"sensitive":false,"cacheable":true,"requires_confirmation":false}}}}]}}"#,
            server.uri()
        ),
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "state",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let called = prog(&[
        "--dir",
        dir_arg,
        "call",
        "state",
        "get",
        "--args",
        r#"{"id":7}"#,
    ]);
    assert!(called.status.success(), "{}", stdout(&called));
    let listed = prog(&["--dir", dir_arg, "cache", "observations"]);
    assert!(listed.status.success(), "{}", stdout(&listed));
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    let state = &listed["observations"][0]["source_state"];
    assert_eq!(state["kind"], "http_etag");
    assert_eq!(state["value"], "W/\"record-7\"");
    assert_eq!(state["source_id"], "state");
    assert_eq!(state["operation"], "get");
    assert!(
        state["subject_scope"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
}

#[tokio::test]
async fn refresh_304_revalidates_prior_observation_without_replacing_payload() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/records/7"))
        .respond_with(EtagResponder)
        .expect(2)
        .mount(&server)
        .await;
    let seed = write_seed(
        dir.path(),
        "refresh-state.json",
        &format!(
            r#"{{"kind":"http","base_url":"{}","operations":[{{"name":"get","method":"GET","path":"/records/{{id}}","input_schema":{{"type":"object","properties":{{"id":{{"type":"integer"}}}},"required":["id"]}},"effect":{{"read_only":true,"mutating":false,"network":true,"shell":false,"sensitive":false,"cacheable":true,"requires_confirmation":false}}}}]}}"#,
            server.uri()
        ),
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "refresh-state",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let first = prog(&[
        "--dir",
        dir_arg,
        "call",
        "refresh-state",
        "get",
        "--args",
        r#"{"id":7}"#,
    ]);
    assert!(first.status.success(), "{}", stdout(&first));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let first_observation = first["observation"]["observation_id"].as_str().unwrap();
    let refreshed = prog(&[
        "--dir",
        dir_arg,
        "call",
        "refresh-state",
        "get",
        "--args",
        r#"{"id":7}"#,
        "--refresh",
    ]);
    assert!(refreshed.status.success(), "{}", stdout(&refreshed));
    let refreshed: Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(refreshed["source_validity"], "confirmed_unchanged");
    assert_eq!(refreshed["data_preview"]["body"], "original");
    assert_eq!(refreshed["provenance"]["status"], "304");
    let second_observation = refreshed["observation"]["observation_id"].as_str().unwrap();
    assert_ne!(first_observation, second_observation);
    let observations = prog(&["--dir", dir_arg, "cache", "observations", "--limit", "2"]);
    assert!(observations.status.success(), "{}", stdout(&observations));
    let observations: Value = serde_json::from_slice(&observations.stdout).unwrap();
    let latest = observations["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["observation_id"] == second_observation)
        .unwrap();
    assert_eq!(latest["lineage"]["revalidates_id"], first_observation);
}

#[test]
fn cache_get_missing_uses_structured_cache_miss_error() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let output = prog(&["--dir", dir_arg, "cache", "get", "sha256:missing"]);

    assert!(!output.status.success());
    assert_eq!(stderr(&output), "");
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_eq!(value["error"]["kind"], "cache_miss");
}

#[tokio::test]
async fn call_http_then_expand_offline_and_write_out_file() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let items = (0..40)
        .map(|id| json!({"id": id, "body": "x".repeat(64)}))
        .collect::<Vec<_>>();
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": items,
            "next": "page-2"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let seed = write_seed(
        dir.path(),
        "http-call.json",
        &format!(
            r#"{{
              "kind": "http",
              "base_url": "{}",
              "operations": [{{
                "name": "list",
                "method": "GET",
                "path": "/items"
              }}]
            }}"#,
            server.uri()
        ),
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));

    let call = prog(&["--dir", dir_arg, "call", "api", "list", "--args", "{}"]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(envelope["cache"]["status"], "stored");
    assert_eq!(envelope["summary"]["kind"], "object");
    assert!(envelope["summary"]["envelope_bytes"].as_u64().unwrap() <= 16 * 1024);
    let cursor = envelope["cursor"].as_str().unwrap().to_string();
    assert!(cursor.starts_with("pc1_"));
    assert!(
        envelope["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omitted| omitted["path"] == "/items")
    );

    drop(server);

    let expand = prog(&[
        "--dir", dir_arg, "expand", &cursor, "--path", "/items", "--limit", "2",
    ]);
    assert!(expand.status.success(), "{}", stdout(&expand));
    let expanded: Value = serde_json::from_slice(&expand.stdout).unwrap();
    assert_eq!(expanded["cache"]["status"], "hit");
    assert_eq!(expanded["data_preview"].as_array().unwrap().len(), 2);
    assert!(expanded["summary"]["envelope_bytes"].as_u64().unwrap() <= 16 * 1024);

    let out = dir.path().join("items.json");
    let receipt = prog(&[
        "--dir",
        dir_arg,
        "expand",
        &cursor,
        "--path",
        "/items",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(receipt.status.success(), "{}", stdout(&receipt));
    let receipt_value: Value = serde_json::from_slice(&receipt.stdout).unwrap();
    assert_eq!(receipt_value["data_preview"]["path"], out.to_str().unwrap());
    assert_eq!(receipt_value["data_preview"]["json_pointer"], "/items");
    assert!(
        receipt_value["data_preview"]["sha256"]
            .as_str()
            .unwrap()
            .len()
            == 64
    );
    let file_value: Value = serde_json::from_slice(&fs::read(out).unwrap()).unwrap();
    assert_eq!(file_value.as_array().unwrap().len(), 40);
}

#[tokio::test]
async fn http_capture_truncation_persists_unknown_total_lifecycle_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/logs"))
        .respond_with(ResponseTemplate::new(200).set_body_string("line1\nline2\nline3"))
        .expect(1)
        .mount(&server)
        .await;
    let seed_json = json!({
        "kind": "http",
        "base_url": server.uri(),
        "operations": [{
            "name": "logs",
            "method": "GET",
            "path": "/logs",
            "effect": {
                "read_only": true,
                "mutating": false,
                "network": true,
                "shell": false,
                "sensitive": false,
                "cacheable": true,
                "requires_confirmation": false
            }
        }]
    });
    let seed_contents = seed_json.to_string();
    let seed = write_seed(dir.path(), "http-capture.json", &seed_contents);
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));

    let profile_path = dir.path().join("profiles/api.json");
    let mut profile: Value = serde_json::from_slice(&fs::read(&profile_path).unwrap()).unwrap();
    profile["adapter"]["http"]["max_response_bytes"] = json!(11);
    fs::write(&profile_path, serde_json::to_vec(&profile).unwrap()).unwrap();

    let called = prog(&["--dir", dir_arg, "call", "api", "logs", "--args", "{}"]);
    assert!(called.status.success(), "{}", stdout(&called));
    let envelope: Value = serde_json::from_slice(&called.stdout).unwrap();
    assert_eq!(envelope["observation"]["availability"], "capture_truncated");
    assert_eq!(
        envelope["observation"]["capture"]["total_bytes"],
        Value::Null
    );
    assert_eq!(envelope["observation"]["capture"]["captured_bytes"], 11);
    assert_eq!(
        envelope["observation"]["capture"]["stop_reason"],
        "byte_limit"
    );
    assert_eq!(
        envelope["observation"]["capture"]["affected"][0]["scope"],
        "body"
    );
    assert_eq!(
        envelope["observation"]["capture"]["affected"][0]["total_bytes"],
        Value::Null
    );
    assert_eq!(
        envelope["observation"]["capture"]["affected"][0]["captured_bytes"],
        11
    );
    assert_eq!(
        envelope["observation"]["capture"]["can_prove_absence"],
        false
    );

    let observation_id = envelope["observation"]["observation_id"].as_str().unwrap();
    let listed = prog(&["--dir", dir_arg, "cache", "observations", "--limit", "1"]);
    assert!(listed.status.success(), "{}", stdout(&listed));
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    let record = listed["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["observation_id"] == observation_id)
        .unwrap();
    assert_eq!(record["availability"], "capture_truncated");
    assert_eq!(record["capture"], envelope["observation"]["capture"]);
}

#[test]
fn mcp_capture_truncation_persists_known_pre_projection_total() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let script = dir.path().join("large_mcp.py");
    fs::write(
        &script,
        r#"import json
import sys

def reply(message_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": message_id, "result": result}), flush=True)

for line in sys.stdin:
    request = json.loads(line)
    message_id = request.get("id")
    if message_id is None:
        continue
    method = request.get("method")
    if method == "initialize":
        reply(message_id, {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}, "resources": {}, "prompts": {}}, "serverInfo": {"name": "large-mcp", "version": "1.0"}})
    elif method == "tools/list":
        reply(message_id, {"tools": [{"name": "large", "inputSchema": {"type": "object", "properties": {}}, "annotations": {"readOnlyHint": True}}]})
    elif method == "resources/list":
        reply(message_id, {"resources": []})
    elif method == "prompts/list":
        reply(message_id, {"prompts": []})
    elif method == "tools/call":
        reply(message_id, {"structuredContent": {"items": [{"id": index, "body": "x" * 128} for index in range(20)]}, "content": [], "isError": False})
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": message_id, "error": {"code": -32601, "message": "unknown method"}}), flush=True)
"#,
    )
    .unwrap();
    let seed_json = json!({
        "command": "python3",
        "args": [script],
        "timeout_ms": 2_000,
        "max_content_bytes": 256,
        "max_stderr_bytes": 64 * 1024,
        "max_schema_depth": 32
    });
    let seed_contents = seed_json.to_string();
    let seed = write_seed(dir.path(), "mcp-capture.json", &seed_contents);
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "fixture",
        "--kind",
        "mcp",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));

    let called = prog(&["--dir", dir_arg, "call", "fixture", "large", "--args", "{}"]);
    assert!(called.status.success(), "{}", stdout(&called));
    let envelope: Value = serde_json::from_slice(&called.stdout).unwrap();
    let response_bytes = envelope["provenance"]["adapter"]["response_bytes"]
        .as_u64()
        .unwrap();
    assert!(response_bytes > 256);
    assert_eq!(envelope["observation"]["availability"], "capture_truncated");
    assert_eq!(
        envelope["observation"]["capture"]["total_bytes"],
        response_bytes
    );
    assert_eq!(
        envelope["observation"]["capture"]["captured_bytes"],
        response_bytes
    );
    assert_eq!(
        envelope["observation"]["capture"]["stop_reason"],
        "storage_limit"
    );
    assert_eq!(
        envelope["observation"]["capture"]["affected"][0]["total_bytes"],
        response_bytes
    );
    assert_eq!(
        envelope["observation"]["capture"]["can_prove_absence"],
        false
    );

    let observation_id = envelope["observation"]["observation_id"].as_str().unwrap();
    let listed = prog(&["--dir", dir_arg, "cache", "observations", "--limit", "1"]);
    assert!(listed.status.success(), "{}", stdout(&listed));
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    let record = listed["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["observation_id"] == observation_id)
        .unwrap();
    assert_eq!(record["availability"], "capture_truncated");
    assert_eq!(record["capture"], envelope["observation"]["capture"]);
}

#[test]
fn call_validates_args_and_enforces_effect_policy() {
    let dir = tempfile::tempdir().unwrap();
    let cli_seed = write_seed(
        dir.path(),
        "safe-cli.json",
        r#"{
          "kind": "cli",
          "operations": [{
            "name": "hello",
            "command": "python3",
            "args": ["-c", "import json; print(json.dumps({'hello':'{name}'}))"],
            "input_schema": {
              "type": "object",
              "required": ["name"],
              "properties": {"name": {"type": "string"}}
            },
            "effect": {
              "read_only": true,
              "mutating": false,
              "network": false,
              "shell": false,
              "sensitive": false,
              "cacheable": true,
              "requires_confirmation": false
            }
          }]
        }"#,
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "local",
        "--kind",
        "cli",
        "--seed",
        cli_seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));

    let bad_args = prog(&[
        "--dir",
        dir_arg,
        "call",
        "local",
        "hello",
        "--args",
        r#"{"extra":true}"#,
    ]);
    assert!(!bad_args.status.success());
    let value: Value = serde_json::from_slice(&bad_args.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "bad_args");
    assert!(value["error"]["message"].as_str().unwrap().contains("name"));
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("extra")
    );

    let post_seed = write_seed(
        dir.path(),
        "mutating-http.json",
        r#"{
          "kind": "http",
          "base_url": "http://127.0.0.1:9",
          "operations": [{
            "name": "create",
            "method": "POST",
            "path": "/items"
          }]
        }"#,
    );
    let discover_post = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        post_seed.to_str().unwrap(),
    ]);
    assert!(discover_post.status.success(), "{}", stdout(&discover_post));
    let mutating = prog(&["--dir", dir_arg, "call", "api", "create", "--args", "{}"]);
    assert!(!mutating.status.success());
    let value: Value = serde_json::from_slice(&mutating.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "requires_confirmation");

    let shell_seed = write_seed(
        dir.path(),
        "shell-cli.json",
        r#"{
          "kind": "cli",
          "operations": [{
            "name": "shell",
            "command": "python3",
            "args": ["-c", "print('no trust')"],
            "shell": true,
            "effect": {
              "read_only": true,
              "mutating": false,
              "network": false,
              "shell": false,
              "sensitive": false,
              "cacheable": true,
              "requires_confirmation": false
            }
          }]
        }"#,
    );
    let discover_shell = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "shells",
        "--kind",
        "cli",
        "--seed",
        shell_seed.to_str().unwrap(),
    ]);
    assert!(
        discover_shell.status.success(),
        "{}",
        stdout(&discover_shell)
    );
    let shell = prog(&[
        "--dir", dir_arg, "call", "shells", "shell", "--args", "{}", "--yes",
    ]);
    assert!(!shell.status.success());
    let value: Value = serde_json::from_slice(&shell.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "shell_not_trusted");
}

#[test]
fn meta_lists_and_discloses_contract_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let list = prog(&["--dir", dir_arg, "meta"]);
    assert!(list.status.success(), "{}", stdout(&list));
    let value: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["source_id"], "prog");
    assert!(
        value["data_preview"]["contracts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|contract| contract == "SourceProfile")
    );
    let schema = prog(&["--dir", dir_arg, "--pretty", "meta", "SourceProfile"]);
    assert!(schema.status.success(), "{}", stdout(&schema));
    let text = stdout(&schema);
    let value: Value = serde_json::from_str(&text).unwrap();
    assert!(text.len() <= 16 * 1024);
    assert_eq!(value["disclosure_budget"]["effective_bytes"], 16 * 1024);
    assert_eq!(value["operation"], "SourceProfile");
    assert_eq!(value["summary"]["kind"], "object");
    assert!(value["data_preview"].is_object());

    let lens_schema = prog(&["--dir", dir_arg, "meta", "LensManifest"]);
    assert!(lens_schema.status.success(), "{}", stdout(&lens_schema));
    let value: Value = serde_json::from_slice(&lens_schema.stdout).unwrap();
    assert_eq!(value["operation"], "LensManifest");
    assert_eq!(value["data_preview"]["title"], "LensManifest");

    let envelope_schema = prog(&["--dir", dir_arg, "meta", "DisclosureEnvelope"]);
    assert!(
        envelope_schema.status.success(),
        "{}",
        stdout(&envelope_schema)
    );
    let value: Value = serde_json::from_slice(&envelope_schema.stdout).unwrap();
    assert!(
        value["data_preview"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("observation")
    );

    let observation_schema = prog(&["--dir", dir_arg, "meta", "ObservationMetadata"]);
    assert!(
        observation_schema.status.success(),
        "{}",
        stdout(&observation_schema)
    );
    let value: Value = serde_json::from_slice(&observation_schema.stdout).unwrap();
    assert_eq!(value["operation"], "ObservationMetadata");
    assert_eq!(value["data_preview"]["title"], "ObservationMetadata");

    let evidence_schema = prog(&["--dir", dir_arg, "meta", "EvidenceRef"]);
    assert!(
        evidence_schema.status.success(),
        "{}",
        stdout(&evidence_schema)
    );
    let value: Value = serde_json::from_slice(&evidence_schema.stdout).unwrap();
    assert_eq!(value["operation"], "EvidenceRef");
    assert_eq!(value["data_preview"]["title"], "EvidenceRef");
    assert!(
        value["data_preview"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("redacted_slice_sha256")
    );

    let inspect_schema = prog(&["--dir", dir_arg, "meta", "InspectResponse"]);
    assert!(
        inspect_schema.status.success(),
        "{}",
        stdout(&inspect_schema)
    );
    let value: Value = serde_json::from_slice(&inspect_schema.stdout).unwrap();
    assert_eq!(value["operation"], "InspectResponse");
    assert_eq!(value["data_preview"]["title"], "InspectResponse");
    assert!(
        value["data_preview"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("findings")
    );

    let evidence_block_schema = prog(&["--dir", dir_arg, "meta", "EvidenceBlock"]);
    assert!(
        evidence_block_schema.status.success(),
        "{}",
        stdout(&evidence_block_schema)
    );
    let value: Value = serde_json::from_slice(&evidence_block_schema.stdout).unwrap();
    assert_eq!(value["operation"], "EvidenceBlock");
    assert_eq!(value["data_preview"]["title"], "EvidenceBlock");

    let search_schema = prog(&["--dir", dir_arg, "meta", "SearchResponse"]);
    assert!(search_schema.status.success(), "{}", stdout(&search_schema));
    let value: Value = serde_json::from_slice(&search_schema.stdout).unwrap();
    assert_eq!(value["operation"], "SearchResponse");
    assert_eq!(value["data_preview"]["title"], "SearchResponse");
}

#[test]
fn call_can_apply_repo_local_lens_manifest_and_expand_original_payload() {
    let dir = tempfile::tempdir().unwrap();
    let lens_dir = dir.path().join("lenses");
    fs::create_dir(&lens_dir).unwrap();
    let script = dir.path().join("emit_items.py");
    fs::write(
        &script,
        r#"import json
items = [
    {
        "id": index,
        "state": "open" if index % 2 == 0 else "closed",
        "title": f"Lens item {index}",
        "body": "body " + ("x" * 700),
        "token": f"secret-token-{index}",
    }
    for index in range(4)
]
print(json.dumps({"items": items, "meta": {"count": len(items)}}))
"#,
    )
    .unwrap();
    let seed_json = json!({
        "kind": "cli",
        "operations": [{
            "name": "list",
            "command": "python3",
            "args": [script.to_str().unwrap()],
            "effect": {
                "read_only": true,
                "mutating": false,
                "network": false,
                "shell": false,
                "sensitive": false,
                "cacheable": true,
                "requires_confirmation": false
            }
        }]
    });
    let seed = write_seed(dir.path(), "cli.json", &seed_json.to_string());
    fs::write(
        lens_dir.join("cli-items.json"),
        r#"{
          "schema": "prog.lens_manifest",
          "id": "cli.items",
          "match": {
            "source_kind": "cli",
            "operation": "list"
          },
          "view": {
            "root": "/items",
            "limit": 2,
            "fields": {
              "id": "/id",
              "state": "/state",
              "title": "/title",
              "token": "/token"
            }
          },
          "omit": [{
            "path": "/items/*/body",
            "reason": "large_string",
            "detail": "body is expandable on demand",
            "expandable": true
          }],
          "next_actions": [{
            "kind": "expand",
            "path": "/items/{index}/body",
            "reason": "inspect body only when the row matters"
          }]
        }"#,
    )
    .unwrap();
    fs::write(
        lens_dir.join("unused.yaml"),
        r#"schema: prog.lens_manifest
id: unused.yaml
view:
  root: /items
"#,
    )
    .unwrap();

    let dir_arg = dir.path().to_str().unwrap();
    let lens_dir_arg = lens_dir.to_str().unwrap();
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "local",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));

    let call = prog(&[
        "--dir",
        dir_arg,
        "--lens-dir",
        lens_dir_arg,
        "call",
        "local",
        "list",
        "--args",
        "{}",
        "--lens",
        "cli.items",
    ]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(envelope["lens"]["id"], "cli.items");
    assert_eq!(envelope["data_preview"].as_array().unwrap().len(), 2);
    assert!(envelope["data_preview"][0].get("body").is_none());
    assert_eq!(envelope["data_preview"][0]["token"], "«redacted»");
    assert!(
        envelope["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omitted| omitted["path"] == "/items/*/body" && omitted["expandable"] == true)
    );
    assert!(
        envelope["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["path"] == "/items/{index}/body")
    );
    assert!(envelope["summary"]["envelope_bytes"].as_u64().unwrap() <= 16 * 1024);
    let cursor = envelope["cursor"].as_str().unwrap();

    let outside_paths = prog(&["--dir", dir_arg, "paths", cursor, "--prefix", "/meta"]);
    assert!(!outside_paths.status.success());
    let outside_error: Value = serde_json::from_slice(&outside_paths.stdout).unwrap();
    assert_eq!(outside_error["error"]["kind"], "path_outside_boundary");

    let expand = prog(&[
        "--dir",
        dir_arg,
        "expand",
        cursor,
        "--path",
        "/items/0/body",
        "--depth",
        "1",
    ]);
    assert!(expand.status.success(), "{}", stdout(&expand));
    let expanded: Value = serde_json::from_slice(&expand.stdout).unwrap();
    assert_eq!(expanded["cache"]["status"], "hit");
    assert!(
        expanded["data_preview"]
            .as_str()
            .unwrap()
            .starts_with("body ")
    );
}

#[test]
fn call_can_apply_first_party_github_issues_lens() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("issues.py");
    fs::write(
        &script,
        r#"import json
items = [
    {
        "number": 44,
        "title": "Add first-party lens packs",
        "state": "open",
        "user": {"login": "alice"},
        "labels": [{"name": "enhancement"}, {"name": "agent"}],
        "comments": 3,
        "updated_at": "2026-07-06T12:00:00Z",
        "html_url": "https://github.com/example/prog/issues/44",
        "body": "body " + ("x" * 900),
    },
    {
        "number": 45,
        "title": "Install hooks",
        "state": "closed",
        "user": {"login": "bob"},
        "labels": [{"name": "integration"}],
        "comments": 1,
        "updated_at": "2026-07-06T12:05:00Z",
        "html_url": "https://github.com/example/prog/issues/45",
        "pull_request": {"url": "https://api.github.com/repos/example/prog/pulls/45"},
        "body": "other " + ("y" * 900),
    },
]
print(json.dumps({"items": items, "total_count": len(items)}))
"#,
    )
    .unwrap();
    let seed_json = json!({
        "kind": "cli",
        "operations": [{
            "name": "list_issues",
            "command": "python3",
            "args": [script.to_str().unwrap()],
            "effect": {
                "read_only": true,
                "mutating": false,
                "network": false,
                "shell": false,
                "sensitive": false,
                "cacheable": true,
                "requires_confirmation": false
            }
        }]
    });
    let seed = write_seed(dir.path(), "cli.json", &seed_json.to_string());
    let dir_arg = dir.path().to_str().unwrap();
    let lens_dir = first_party_lens_dir();
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "githubish",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));

    let call = prog(&[
        "--dir",
        dir_arg,
        "--lens-dir",
        lens_dir.to_str().unwrap(),
        "call",
        "githubish",
        "list_issues",
        "--args",
        "{}",
        "--lens",
        "github.issues.triage",
    ]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(envelope["lens"]["id"], "github.issues.triage");
    assert_eq!(envelope["data_preview"].as_array().unwrap().len(), 2);
    assert_eq!(envelope["data_preview"][0]["number"], 44);
    assert_eq!(
        envelope["data_preview"][0]["labels"],
        json!(["enhancement", "agent"])
    );
    assert!(envelope["data_preview"][0].get("body").is_none());
    assert!(
        envelope["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omitted| omitted["path"] == "/items/*/body" && omitted["expandable"] == true)
    );
    let cursor = envelope["cursor"].as_str().unwrap();
    let expanded = prog(&[
        "--dir",
        dir_arg,
        "expand",
        cursor,
        "--path",
        "/items/0/body",
    ]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert!(value["data_preview"].as_str().unwrap().starts_with("body "));
}

#[test]
fn call_rejects_lens_manifest_that_escapes_its_root() {
    let dir = tempfile::tempdir().unwrap();
    let lens_dir = dir.path().join("lenses");
    fs::create_dir(&lens_dir).unwrap();
    let seed = write_seed(
        dir.path(),
        "cli.json",
        r#"{
          "kind": "cli",
          "operations": [{
            "name": "list",
            "command": "python3",
            "args": ["-c", "import json; print(json.dumps({'items':[{'id':1}], 'meta': {'count': 1}}))"],
            "effect": {
              "read_only": true,
              "mutating": false,
              "network": false,
              "shell": false,
              "sensitive": false,
              "cacheable": true,
              "requires_confirmation": false
            }
          }]
        }"#,
    );
    fs::write(
        lens_dir.join("bad.json"),
        r#"{
          "schema": "prog.lens_manifest",
          "id": "bad",
          "view": {"root": "/items"},
          "omit": [{"path": "/meta", "reason": "deep_object"}]
        }"#,
    )
    .unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "local",
        "--kind",
        "cli",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stdout(&discover));

    let call = prog(&[
        "--dir",
        dir_arg,
        "--lens-dir",
        lens_dir.to_str().unwrap(),
        "call",
        "local",
        "list",
        "--args",
        "{}",
        "--lens",
        "bad",
    ]);
    assert!(!call.status.success());
    let error: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside view.root")
    );
}

fn write_seed(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).unwrap();
    path
}

// --- Auto-pagination gap-closure (issue #69 / invariant I10) ---

const READ_ONLY_EFFECT: &str = r#""effect":{"read_only":true,"mutating":false,"network":true,"shell":false,"sensitive":false,"cacheable":true,"requires_confirmation":false}"#;
const MUTATING_EFFECT: &str = r#""effect":{"read_only":false,"mutating":true,"network":true,"shell":false,"sensitive":false,"cacheable":true,"requires_confirmation":false}"#;

/// A cursor-paginated chain: page_token=start -> tok_2 -> tok_3 (end).
async fn mount_cursor_chain(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("page_token", "tok_2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": 2, "label": "b"}],
            "next_cursor": "tok_3"
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("page_token", "tok_3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": 3, "extra": {"k": 1}}],
            "has_more": false
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("page_token", "start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": 1, "name": "a"}],
            "next_cursor": "tok_2"
        })))
        .mount(server)
        .await;
}

fn cursor_chain_seed(server_uri: &str) -> String {
    format!(
        r#"{{
          "kind": "http",
          "base_url": "{server_uri}",
          "operations": [{{
            "name": "list",
            "method": "GET",
            "path": "/items",
            "query": {{"page_token": "{{page_token}}"}},
            "args": {{"page_token": "string"}},
            {READ_ONLY_EFFECT}
          }}]
        }}"#
    )
}

#[tokio::test]
async fn pagination_fixtures_round_trip_across_conventions() {
    // Golden fixtures covering the three pagination conventions prog must
    // detect: RFC 5988 Link rel="next", opaque cursor tokens, and offset/limit.
    // Each fixture pins both extract_pagination_hints and next_args_from_hints
    // against a frozen canonical expectation.
    let fixtures_dir = repo_root().join("crates/prog-cli/tests/fixtures/pagination");
    let cases = ["link-header", "cursor-token", "offset-limit"];
    for name in cases {
        let path = fixtures_dir.join(format!("{name}.json"));
        let fixture: Value =
            serde_json::from_slice(&fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}")))
                .unwrap();
        let body = &fixture["body"];
        let link = fixture["link_header"].as_str();
        let args = &fixture["args"];
        let expect = &fixture["expect"];
        let hints = prog_core::extract_pagination_hints(body, link)
            .unwrap_or_else(|| panic!("{name}: hints"));
        match expect["next_target"].as_str().unwrap_or("args") {
            "url" => {
                let target =
                    prog_core::next_args_from_hints(&hints, args).expect("{name}: url target");
                match target {
                    prog_core::PageTarget::Url(url) => {
                        assert!(
                            url.contains(expect["next_url_contains"].as_str().unwrap()),
                            "{name}: url {url}"
                        );
                    }
                    other => panic!("{name}: expected Url target, got {other:?}"),
                }
            }
            "args" => {
                let target =
                    prog_core::next_args_from_hints(&hints, args).expect("{name}: args target");
                match target {
                    prog_core::PageTarget::Args(out) => {
                        if let Some(param) = expect["written_param"].as_str() {
                            assert_eq!(
                                out[param],
                                *expect.get("next_cursor").unwrap_or(&json!(null)),
                                "{name}: written param {param}"
                            );
                        }
                        if let Some(offset) = expect["next_offset"].as_u64() {
                            assert_eq!(out["offset"].as_u64(), Some(offset), "{name}: offset");
                            assert_eq!(out["limit"].as_u64(), expect["preserved_limit"].as_u64());
                        }
                    }
                    other => panic!("{name}: expected Args target, got {other:?}"),
                }
            }
            other => panic!("{name}: unknown expect.next_target {other}"),
        }
        // Convention-specific canonical-field checks.
        if let Some(want) = expect["next_cursor"].as_str() {
            assert_eq!(hints["next_cursor"], json!(want), "{name}: next_cursor");
        }
        if let Some(want) = expect["cursor_param"].as_str() {
            assert_eq!(hints["cursor_param"], json!(want), "{name}: cursor_param");
        }
        if let Some(want) = expect["page_strategy"].as_str() {
            assert_eq!(hints["page_strategy"], json!(want), "{name}: page_strategy");
        }
        if let Some(needle) = expect["link_rel_next_contains"].as_str() {
            assert!(
                hints["link_rel_next"].as_str().unwrap().contains(needle),
                "{name}: link_rel_next"
            );
        }
    }
}

#[tokio::test]
async fn prog_call_pages_projects_each_page_and_merges_shapes() {
    let server = MockServer::start().await;
    mount_cursor_chain(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let seed = write_seed(dir.path(), "http.json", &cursor_chain_seed(&server.uri()));
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stderr(&discover));

    let call = prog(&[
        "--dir",
        dir_arg,
        "call",
        "api",
        "list",
        "--args",
        r#"{"page_token":"start"}"#,
        "--pages",
        "3",
    ]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    let pagination = &envelope["pagination"];
    assert_eq!(pagination["pages_fetched"], json!(3));
    assert_eq!(pagination["stop_reason"], json!("no_more"));
    assert_eq!(pagination["max_pages"], json!(3));
    // Per-page summaries, each with its own pc1_ cursor.
    let pages = pagination["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 3);
    for page in pages {
        assert!(page["cache_key"].as_str().unwrap().starts_with("sha256:"));
        if page["page"].as_u64() >= Some(2) {
            assert!(page["cursor"].as_str().unwrap().starts_with("pc1_"));
        }
    }
    // The merged shape is the monotone union of all three pages (I5): the
    // items element object gained `name`, then `label`, then `extra`.
    assert!(pagination["merged_shape"].is_object());
}

#[tokio::test]
async fn prog_call_surfaces_next_actions_resume_with_page_args() {
    let server = MockServer::start().await;
    mount_cursor_chain(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let seed = write_seed(dir.path(), "http.json", &cursor_chain_seed(&server.uri()));
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stderr(&discover));

    // --pages 2 stops at the page cap while tok_3 is still available: a resume
    // NextAction must be surfaced with the page-3 args.
    let call = prog(&[
        "--dir",
        dir_arg,
        "call",
        "api",
        "list",
        "--args",
        r#"{"page_token":"start"}"#,
        "--pages",
        "2",
    ]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(envelope["pagination"]["stop_reason"], json!("page_cap"));
    let resume = envelope["next_actions"]
        .as_array()
        .expect("next_actions present")
        .iter()
        .find(|action| action["kind"] == "call")
        .expect("a call resume action");
    assert_eq!(resume["operation"], json!("list"));
    assert_eq!(resume["args"]["page_token"], json!("tok_3"));
    assert_eq!(resume["source_id"], json!("api"));
}

#[tokio::test]
async fn prog_call_pages_skipped_for_mutating_operation_emits_warning() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [1]})))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    // Mutating (not read-only) operation: auto-pagination must refuse and fetch
    // a single page (effect policy I6/I7, I10).
    let seed = write_seed(
        dir.path(),
        "http.json",
        &format!(
            r#"{{
              "kind": "http",
              "base_url": "{base}",
              "operations": [{{
                "name": "list",
                "method": "GET",
                "path": "/items",
                {MUTATING_EFFECT}
              }}]
            }}"#,
            base = server.uri()
        ),
    );
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stderr(&discover));

    let call = prog(&[
        "--dir", dir_arg, "call", "api", "list", "--args", "{}", "--pages", "5", "--yes",
    ]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("not auto-pagination-safe"))
    );
    // No pagination block was added: a single page was fetched.
    assert!(envelope.get("pagination").is_none());
}

#[tokio::test]
async fn prog_call_url_continuation_end_to_end_via_link_header() {
    let server = MockServer::start().await;
    let page2 = format!("{}/items?page=2", server.uri());
    // Page 2 (specific matcher first).
    Mock::given(method("GET"))
        .and(path("/items"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [2]})))
        .expect(1)
        .mount(&server)
        .await;
    // Page 1 advertises Link rel="next" to the same host.
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("link", format!("<{page2}>; rel=\"next\""))
                .set_body_json(json!({"items": [1]})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let seed = write_seed(
        dir.path(),
        "http.json",
        &format!(
            r#"{{
              "kind": "http",
              "base_url": "{base}",
              "operations": [{{
                "name": "list",
                "method": "GET",
                "path": "/items",
                {READ_ONLY_EFFECT}
              }}]
            }}"#,
            base = server.uri()
        ),
    );
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stderr(&discover));

    let call = prog(&[
        "--dir", dir_arg, "call", "api", "list", "--args", "{}", "--pages", "3",
    ]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    let pagination = &envelope["pagination"];
    assert_eq!(pagination["pages_fetched"], json!(2));
}

#[tokio::test]
async fn pagination_follows_readonly_and_stops_at_caps() {
    // I10 (envelope-budget half): a read-only operation follows the chain up
    // to the --pages cap, and the final serialized envelope stays within the
    // 16 KiB disclosure budget even after multiple prefetched pages.
    let server = MockServer::start().await;
    mount_cursor_chain(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let seed = write_seed(dir.path(), "http.json", &cursor_chain_seed(&server.uri()));
    let discover = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "api",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discover.status.success(), "{}", stderr(&discover));

    let call = prog(&[
        "--dir",
        dir_arg,
        "call",
        "api",
        "list",
        "--args",
        r#"{"page_token":"start"}"#,
        "--pages",
        "2",
    ]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope_bytes = call.stdout.len();
    assert!(
        envelope_bytes <= 16 * 1024 + 16,
        "envelope must stay within the 16 KiB budget after prefetch, got {envelope_bytes}"
    );
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(envelope["pagination"]["stop_reason"], json!("page_cap"));
    assert_eq!(envelope["pagination"]["pages_fetched"], json!(2));

    let cached = prog(&[
        "--dir",
        dir_arg,
        "call",
        "api",
        "list",
        "--args",
        r#"{"page_token":"start"}"#,
        "--pages",
        "2",
    ]);
    assert!(cached.status.success(), "{}", stdout(&cached));
    let cached_envelope: Value = serde_json::from_slice(&cached.stdout).unwrap();
    assert_eq!(cached_envelope["cache"]["status"], "hit");
    assert_eq!(
        cached_envelope["pagination"]["stop_reason"],
        json!("page_cap")
    );
    assert_eq!(cached_envelope["pagination"]["pages_fetched"], json!(2));
    assert!(
        cached_envelope["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["kind"] == "call")
    );
}

fn cli_probe_seed(with_effect: bool) -> String {
    let effect = if with_effect {
        r#","effect":{"read_only":true,"mutating":false,"network":false,"shell":false,"sensitive":false,"cacheable":true,"requires_confirmation":false}"#
    } else {
        ""
    };
    format!(
        r#"{{
          "kind": "cli",
          "operations": [{{
            "name": "emit",
            "command": "python3",
            "args": ["-c", "import json; print(json.dumps({{'items':[{{'id':1,'state':'open','body':'{}'}}]}}))"]
            {effect}
          }}]
        }}"#,
        "x".repeat(1000)
    )
}

#[test]
fn pretty_errors_are_still_machine_readable() {
    let dir = tempfile::tempdir().unwrap();
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "--pretty",
        "call",
        "missing",
        "list",
        "--args",
        "{}",
    ]);
    assert!(!output.status.success());
    assert_eq!(stderr(&output), "");

    let text = stdout(&output);
    assert!(text.starts_with("{\n"));
    let value: Value = serde_json::from_str(&text).expect("pretty stdout must still be JSON");
    assert_eq!(value["error"]["kind"], "unknown_source");
}

#[test]
fn parser_errors_use_the_same_json_error_contract() {
    let output = prog(&["unknown"]);
    assert!(!output.status.success());
    assert_eq!(stderr(&output), "");

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_eq!(value["error"]["kind"], "cli_usage");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown")
    );
    assert!(
        value["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("prog --help")
    );
}

#[test]
fn log_recipe_composes_observe_lens_findings_and_recommended_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("service.log");
    fs::write(
        &file,
        format!(
            "INFO start\n{}\nERROR checkout failed status=500\nINFO stop\n",
            "noise".repeat(500)
        ),
    )
    .unwrap();
    let lens_dir = first_party_lens_dir();
    let recipe = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "--lens-dir",
        lens_dir.to_str().unwrap(),
        "recipe",
        "logs-root-cause",
        "--file",
        file.to_str().unwrap(),
    ]);
    assert!(recipe.status.success(), "{}", stdout(&recipe));
    let value: Value = serde_json::from_slice(&recipe.stdout).unwrap();
    assert_eq!(value["recipe"]["id"], "logs-root-cause");
    assert_eq!(value["findings"][0]["kind"], "log_error");
    assert!(
        value["recipe"]["recommended_next"]
            .as_str()
            .unwrap()
            .contains("prog evidence")
    );
    assert!(recipe.stdout.len() <= 16 * 1024);
}

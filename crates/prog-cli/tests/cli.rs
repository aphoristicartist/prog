use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

fn prog(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_prog"))
        .args(args)
        .output()
        .expect("prog binary should run")
}

fn prog_with_stdin(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_prog"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("prog binary should spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin)
        .expect("stdin should write");
    child.wait_with_output().expect("prog binary should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be utf8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be utf8")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should canonicalize")
}

fn first_party_lens_dir() -> PathBuf {
    repo_root().join("lenses")
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
    assert_eq!(evidence["schema_version"], "prog.evidence_ref.v1");
    assert_eq!(evidence["source_id"], "observe");
    assert_eq!(evidence["operation"], "evidence");
    assert_eq!(evidence["cursor"], cursor);
    assert_eq!(evidence["path"], "/answer");
    assert_eq!(evidence["uri"], format!("prog://{cursor}#/answer"));
    assert_eq!(evidence["cache_status"], "hit");
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
fn observe_parser_metadata_covers_structured_and_fallback_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let diff = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "text/x-diff",
            "--name",
            "diff",
        ],
        b"diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new-target\n",
    );
    assert!(diff.status.success(), "{}", stdout(&diff));
    let diff_value: Value = serde_json::from_slice(&diff.stdout).unwrap();
    assert_eq!(diff_value["data_preview"]["format"], "unified_diff");
    assert_eq!(diff_value["observation"]["parser"]["id"], "unified_diff");
    assert_eq!(diff_value["observation"]["parser"]["lossy"], false);
    assert!(
        diff_value["observation"]["parser"]["confidence"]
            .as_f64()
            .unwrap()
            > 0.8
    );
    let diff_cursor = diff_value["cursor"].as_str().unwrap();
    let diff_expand = prog(&[
        "--dir",
        dir_arg,
        "expand",
        diff_cursor,
        "--path",
        "/lines/5/text",
    ]);
    assert!(diff_expand.status.success(), "{}", stdout(&diff_expand));
    let diff_expanded: Value = serde_json::from_slice(&diff_expand.stdout).unwrap();
    assert_eq!(diff_expanded["data_preview"], "+new-target");

    let sarif = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "application/sarif+json",
            "--name",
            "sarif",
        ],
        br#"{"version":"2.1.0","runs":[{"results":[{"message":{"text":"sarif-target"}}]}]}"#,
    );
    assert!(sarif.status.success(), "{}", stdout(&sarif));
    let sarif_value: Value = serde_json::from_slice(&sarif.stdout).unwrap();
    assert_eq!(sarif_value["observation"]["parser"]["id"], "sarif");
    let sarif_cursor = sarif_value["cursor"].as_str().unwrap();
    let sarif_expand = prog(&[
        "--dir",
        dir_arg,
        "expand",
        sarif_cursor,
        "--path",
        "/runs/0/results/0/message/text",
    ]);
    assert!(sarif_expand.status.success(), "{}", stdout(&sarif_expand));
    let sarif_expanded: Value = serde_json::from_slice(&sarif_expand.stdout).unwrap();
    assert_eq!(sarif_expanded["data_preview"], "sarif-target");

    let junit = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "application/junit+xml",
            "--name",
            "junit",
        ],
        br#"<testsuite><testcase classname="suite" name="case_a" time="0.1"><failure>junit-target</failure></testcase></testsuite>"#,
    );
    assert!(junit.status.success(), "{}", stdout(&junit));
    let junit_value: Value = serde_json::from_slice(&junit.stdout).unwrap();
    assert_eq!(junit_value["observation"]["parser"]["id"], "junit_xml");
    assert_eq!(junit_value["observation"]["parser"]["lossy"], true);
    assert_eq!(
        junit_value["data_preview"]["testcases"][0]["name"],
        "case_a"
    );

    let html = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "text/html",
            "--name",
            "html",
        ],
        br#"<!doctype html><html><head><title>Doc Target</title></head><body><h1>Heading Target</h1><a href="/next">Next</a></body></html>"#,
    );
    assert!(html.status.success(), "{}", stdout(&html));
    let html_value: Value = serde_json::from_slice(&html.stdout).unwrap();
    assert_eq!(html_value["observation"]["parser"]["id"], "html_basic");
    assert_eq!(html_value["data_preview"]["title"], "Doc Target");
    assert_eq!(html_value["data_preview"]["headings"][0], "Heading Target");
    assert_eq!(html_value["data_preview"]["links"][0]["href"], "/next");

    let fallback = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "application/x-unknown",
            "--name",
            "fallback",
        ],
        b"plain fallback target",
    );
    assert!(fallback.status.success(), "{}", stdout(&fallback));
    let fallback_value: Value = serde_json::from_slice(&fallback.stdout).unwrap();
    assert_eq!(
        fallback_value["observation"]["parser"]["id"],
        "text_fallback"
    );
    assert_eq!(fallback_value["observation"]["parser"]["fallback"], true);
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
fn observe_parser_edge_cases_cover_malformed_json_and_stack_traces() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let malformed = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "application/json",
            "--name",
            "malformed-json",
        ],
        b"{not valid json but still useful text}",
    );
    assert!(malformed.status.success(), "{}", stdout(&malformed));
    let malformed_value: Value = serde_json::from_slice(&malformed.stdout).unwrap();
    assert_eq!(
        malformed_value["observation"]["parser"]["id"],
        "text_fallback"
    );
    assert!(
        malformed_value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("json:"))
    );

    let repeated_stack = "Error: boom\n    at service (app.js:10)\n    at main (app.js:20)\nError: boom\n    at service (app.js:10)\n    at main (app.js:20)\n";
    let stack = prog_with_stdin(
        &[
            "--dir",
            dir_arg,
            "observe",
            "--stdin",
            "--mime",
            "text/plain",
            "--name",
            "stack",
        ],
        repeated_stack.as_bytes(),
    );
    assert!(stack.status.success(), "{}", stdout(&stack));
    let stack_value: Value = serde_json::from_slice(&stack.stdout).unwrap();
    assert_eq!(stack_value["data_preview"]["repeated_stack_traces"], 2);
    assert_eq!(
        stack_value["observation"]["parser"]["range_semantics"],
        "line ranges from text"
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
fn run_repeated_identical_commands_create_distinct_cache_entries() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let args = [
        "--dir",
        dir_arg,
        "run",
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
    assert!(started.elapsed() < Duration::from_secs(1));
    let value: Value = serde_json::from_slice(&timeout.stdout).unwrap();
    assert_eq!(value["data_preview"]["command"]["timed_out"], true);
}

#[test]
fn init_codex_project_dry_run_reports_reviewable_files_without_writing() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let output = prog(&[
        "init",
        "--agent",
        "codex",
        "--project",
        "--dry-run",
        "--root",
        root,
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert_eq!(stderr(&output), "");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], "prog.init.v1");
    assert_eq!(report["agent"], "codex");
    assert_eq!(report["scope"], "project");
    assert_eq!(report["dry_run"], true);
    let files = report["files"].as_array().unwrap();
    assert_eq!(files.len(), 5);
    assert!(files.iter().all(|file| file["action"] == "would_create"));
    assert!(files.iter().any(|file| {
        file["path"] == ".codex/skills/prog/SKILL.md" && file["executable"] == false
    }));
    assert!(
        files
            .iter()
            .any(|file| file["path"] == ".codex/prog-hooks/prog-run.sh"
                && file["executable"] == true)
    );
    assert!(!project.path().join(".codex").exists());
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("dry-run"))
    );
    assert!(
        report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap().contains("prog inspect"))
    );
}

#[test]
fn init_codex_project_creates_hook_skill_manifest_and_preserves_existing_files() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let output = prog(&["init", "--agent", "codex", "--project", "--root", root]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["action"] == "created")
    );

    let skill = project.path().join(".codex/skills/prog/SKILL.md");
    let hook = project.path().join(".codex/prog-hooks/prog-run.sh");
    let manifest = project.path().join(".codex/prog-hooks/manifest.json");
    let uninstall = project.path().join(".codex/prog-hooks/uninstall.sh");
    assert!(skill.exists());
    assert!(hook.exists());
    assert!(manifest.exists());
    assert!(uninstall.exists());

    let skill_text = fs::read_to_string(&skill).unwrap();
    for expected in [
        "prog run",
        "prog observe",
        "prog inspect",
        "prog evidence",
        "EvidenceRef",
        "MCP is optional",
    ] {
        assert!(
            skill_text.contains(expected),
            "skill should contain {expected}"
        );
    }
    let manifest_value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(manifest_value["schema_version"], "prog.integration.v1");
    assert_eq!(manifest_value["agent"], "codex");
    assert_eq!(manifest_value["mcp"]["status"], "optional");
    assert!(
        manifest_value["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file.as_str() == Some(".codex/prog-hooks/uninstall.sh"))
    );

    let prog_bin = Path::new(env!("CARGO_BIN_EXE_prog"));
    let prog_dir = prog_bin.parent().unwrap();
    let path = format!(
        "{}:{}",
        prog_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let hook_output = Command::new("sh")
        .arg(&hook)
        .args(["python3", "-c", "print('hooked')"])
        .current_dir(project.path())
        .env("PATH", path)
        .output()
        .expect("hook should run");
    assert!(hook_output.status.success(), "{}", stdout(&hook_output));
    let envelope: Value = serde_json::from_slice(&hook_output.stdout).unwrap();
    assert_eq!(envelope["source_id"], "run");
    assert_eq!(envelope["data_preview"]["stdout"]["text"], "hooked");
    assert!(envelope["cursor"].as_str().unwrap().starts_with("pc1_"));

    fs::write(&skill, "custom skill").unwrap();
    let rerun = prog(&["init", "--agent", "codex", "--project", "--root", root]);
    assert!(rerun.status.success(), "{}", stdout(&rerun));
    let rerun_report: Value = serde_json::from_slice(&rerun.stdout).unwrap();
    assert!(
        rerun_report["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["action"] == "exists")
    );
    assert_eq!(fs::read_to_string(&skill).unwrap(), "custom skill");
    assert!(
        rerun_report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("left unchanged"))
    );
}

#[test]
fn init_requires_project_scope_and_supports_each_documented_agent() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let missing_scope = prog(&["init", "--agent", "codex", "--root", root]);
    assert!(!missing_scope.status.success());
    assert_eq!(stderr(&missing_scope), "");
    let error: Value = serde_json::from_slice(&missing_scope.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--project")
    );

    for (agent, expected_skill) in [
        ("claude-code", ".claude/skills/prog/SKILL.md"),
        ("cursor", ".cursor/rules/prog.mdc"),
        ("gemini-cli", ".gemini/skills/prog/SKILL.md"),
    ] {
        let output = prog(&[
            "init",
            "--agent",
            agent,
            "--project",
            "--dry-run",
            "--root",
            root,
        ]);
        assert!(output.status.success(), "{}", stdout(&output));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["agent"], agent);
        assert!(
            report["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|file| file["path"] == expected_skill)
        );
    }
    assert!(!project.path().join(".claude").exists());
    assert!(!project.path().join(".cursor").exists());
    assert!(!project.path().join(".gemini").exists());
}

#[test]
fn non_codex_integrations_create_valid_agent_files_and_uninstall_cleanly() {
    for (agent, skill, hook_dir) in [
        (
            "claude-code",
            ".claude/skills/prog/SKILL.md",
            ".claude/prog-hooks",
        ),
        ("cursor", ".cursor/rules/prog.mdc", ".cursor/prog-hooks"),
        (
            "gemini-cli",
            ".gemini/skills/prog/SKILL.md",
            ".gemini/prog-hooks",
        ),
    ] {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_str().unwrap();
        let output = prog(&["init", "--agent", agent, "--project", "--root", root]);
        assert!(output.status.success(), "{}", stdout(&output));
        let skill_path = project.path().join(skill);
        assert!(skill_path.exists());
        let skill_text = fs::read_to_string(&skill_path).unwrap();
        assert!(skill_text.starts_with("---\n"));
        assert!(skill_text.contains("prog inspect"));

        let manifest_path = project.path().join(hook_dir).join("manifest.json");
        let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["agent"], agent);
        assert_eq!(
            manifest["commands"]["inspect"],
            "prog inspect <cursor> --goal <goal>"
        );

        let uninstall = project.path().join(hook_dir).join("uninstall.sh");
        let result = Command::new("sh")
            .arg(&uninstall)
            .current_dir(project.path())
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(!skill_path.exists());
        assert!(!project.path().join(hook_dir).exists());
    }
}

#[test]
fn cost_planner_reports_profile_driven_savings_and_repeated_cache_hits() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("model.json");
    fs::write(
        &profile,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "prog.model_profile.v1",
            "model": "fable-class-test",
            "input_price_per_million_tokens": 10.0,
            "output_price_per_million_tokens": 50.0,
            "context_window_tokens": 1000000,
            "cache_read_price_per_million_tokens": 1.0,
            "cache_write_price_per_million_tokens": 10.0,
            "pricing_source": "test profile",
            "priced_at": "2026-07-06"
        }))
        .unwrap(),
    )
    .unwrap();
    let raw = dir.path().join("payload.json");
    let payload = json!({
        "items": (0..80)
            .map(|index| json!({
                "id": index,
                "title": format!("Item {index}"),
                "body": "x".repeat(512)
            }))
            .collect::<Vec<_>>()
    });
    fs::write(&raw, serde_json::to_vec(&payload).unwrap()).unwrap();
    let output = prog(&[
        "cost",
        "--model-profile",
        profile.to_str().unwrap(),
        "--raw-file",
        raw.to_str().unwrap(),
        "--expand-path",
        "/items/3/body",
        "--estimated-output-tokens",
        "100",
        "--repeated-inspections",
        "4",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert_eq!(stderr(&output), "");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], "prog.cost_report.v1");
    assert_eq!(report["model"]["model"], "fable-class-test");
    assert_eq!(report["input"]["expand_paths"][0], "/items/3/body");
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("profile-driven"))
    );
    let scenarios = report["scenarios"].as_array().unwrap();
    let raw_scenario = scenarios
        .iter()
        .find(|scenario| scenario["name"] == "raw_payload")
        .unwrap();
    let raw_tokens = report["input"]["raw_tokens"].as_u64().unwrap();
    assert_eq!(raw_scenario["input_tokens"], raw_tokens);
    let expected_raw_cost =
        (((raw_tokens as f64 * 10.0 / 1_000_000.0) + (100.0 * 50.0 / 1_000_000.0)) * 1_000_000.0)
            .round()
            / 1_000_000.0;
    assert_eq!(
        raw_scenario["total_estimated_cost_usd"].as_f64().unwrap(),
        expected_raw_cost
    );
    let targeted = scenarios
        .iter()
        .find(|scenario| scenario["name"] == "prog_observe_paths_expand")
        .unwrap();
    assert!(targeted["input_tokens"].as_u64().unwrap() < raw_tokens);
    assert!(targeted["savings_ratio"].as_f64().unwrap() > 1.0);
    assert_eq!(targeted["lossless"], true);
    let repeated = scenarios
        .iter()
        .find(|scenario| scenario["name"] == "repeated_cache_hits")
        .unwrap();
    assert_eq!(repeated["baseline_input_tokens"], raw_tokens * 4);
    assert!(repeated["input_tokens"].as_u64().unwrap() < raw_tokens * 4);
}

#[test]
fn cost_planner_validates_prices_and_reports_tiny_payload_counterexample() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("tiny.json");
    fs::write(&raw, "{}").unwrap();

    let missing_price = dir.path().join("missing-price.json");
    fs::write(
        &missing_price,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "prog.model_profile.v1",
            "model": "bad",
            "output_price_per_million_tokens": 1.0,
            "context_window_tokens": 1000
        }))
        .unwrap(),
    )
    .unwrap();
    let error = prog(&[
        "cost",
        "--model-profile",
        missing_price.to_str().unwrap(),
        "--raw-file",
        raw.to_str().unwrap(),
    ]);
    assert!(!error.status.success());
    let value: Value = serde_json::from_slice(&error.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "bad_args");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("input_price_per_million_tokens")
    );

    let profile = dir.path().join("model.json");
    fs::write(
        &profile,
        serde_json::to_vec_pretty(&json!({
            "schema_version": "prog.model_profile.v1",
            "model": "tiny-test",
            "input_price_per_million_tokens": 0.25,
            "output_price_per_million_tokens": 1.0,
            "context_window_tokens": 1000,
            "pricing_source": "test profile",
            "priced_at": "2026-07-06"
        }))
        .unwrap(),
    )
    .unwrap();
    let output = prog(&[
        "cost",
        "--model-profile",
        profile.to_str().unwrap(),
        "--raw-file",
        raw.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("tiny payload"))
    );
    let observe_only = report["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scenario| scenario["name"] == "prog_observe_only")
        .unwrap();
    assert!(observe_only["savings_ratio"].as_f64().unwrap() <= 1.0);
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

    std::thread::sleep(std::time::Duration::from_secs(1));
    let cursor = partial_value["cursor"].as_str().unwrap();
    let expanded = prog(&["--dir", dir_arg, "expand", cursor, "--path", "/items"]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let expanded_value: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(
        expanded_value["observation"]["payload"]["cache_status"],
        "hit"
    );
    assert_eq!(expanded_value["observation"]["freshness"]["stale"], true);
    assert_eq!(
        expanded_value["observation"]["freshness"]["refresh_recommended"],
        true
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
fn paths_filters_and_planner_actions_cover_omission_reasons() {
    let dir = tempfile::tempdir().unwrap();
    let wide = (0..30)
        .map(|index| (format!("field_{index:02}"), json!(index)))
        .collect::<serde_json::Map<_, _>>();
    let payload = json!({
        "deep": {"a": {"b": {"c": {"d": {"leaf": "value"}}}}},
        "items": (0..12)
            .map(|index| json!({
                "id": index,
                "body": "body ".to_string() + &"x".repeat(500),
                "token": format!("secret-token-{index}")
            }))
            .collect::<Vec<_>>(),
        "large": "z".repeat(600),
        "wide": Value::Object(wide)
    });
    let file = dir.path().join("planner.json");
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
        "planner",
    ]);
    assert!(observed.status.success(), "{}", stdout(&observed));
    let envelope: Value = serde_json::from_slice(&observed.stdout).unwrap();
    let cursor = envelope["cursor"].as_str().unwrap();
    let first_action = &envelope["next_actions"][0];
    assert_eq!(first_action["kind"], "expand");
    assert_eq!(first_action["priority"], 90);
    assert_eq!(first_action["omitted_reason"], "large_string");
    assert_eq!(first_action["argv"][0], "prog");
    assert_eq!(first_action["argv"][1], "expand");
    assert_eq!(first_action["argv"][2], cursor);
    assert_eq!(
        first_action["offline"],
        "uses cached redacted payload; does not contact upstream"
    );

    for (reason, expected_path) in [
        ("large_string", "/large"),
        ("long_array", "/items"),
        ("many_fields", "/wide"),
        ("deep_object", "/deep/a/b/c"),
        ("redacted", "/items/0/token"),
    ] {
        let mut args = vec![
            "--dir",
            dir_arg,
            "paths",
            cursor,
            "--reason",
            reason,
            "--omitted-only",
        ];
        if reason == "large_string" {
            args.extend(["--field", "large"]);
        }
        let paths = prog(&args);
        assert!(paths.status.success(), "{}", stdout(&paths));
        let listing: Value = serde_json::from_slice(&paths.stdout).unwrap();
        assert!(
            listing["paths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["path"] == expected_path && entry["omitted_reason"] == reason),
            "paths for {reason} should include {expected_path}: {}",
            stdout(&paths)
        );
        assert!(
            listing["next_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["path"] == expected_path
                    && action["omitted_reason"] == reason
                    && action["argv"][3] == "--path"),
            "next actions for {reason} should include {expected_path}: {}",
            stdout(&paths)
        );
    }

    let token_paths = prog(&[
        "--dir",
        dir_arg,
        "paths",
        cursor,
        "--field",
        "token",
        "--expandable-only",
    ]);
    assert!(token_paths.status.success(), "{}", stdout(&token_paths));
    let filtered: Value = serde_json::from_slice(&token_paths.stdout).unwrap();
    assert!(
        filtered["paths"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["path"].as_str().unwrap().contains("token"))
    );
    assert!(!stdout(&token_paths).contains("secret-token"));

    let bad_reason = prog(&[
        "--dir",
        dir_arg,
        "paths",
        cursor,
        "--reason",
        "semantic_table",
    ]);
    assert!(!bad_reason.status.success());
    let error: Value = serde_json::from_slice(&bad_reason.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown omission reason")
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

#[tokio::test]
async fn source_add_http_creates_working_profile_from_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{"id": 1, "state": "open", "body": "x".repeat(500)}]
        })))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let url = format!("{}/items", server.uri());

    let output = prog(&[
        "--dir",
        dir_arg,
        "source",
        "add-http",
        "api",
        "--operation",
        "list",
        "--url",
        &url,
        "--probe",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["source_id"], "api");
    assert_eq!(report["kind"], "http");
    assert_eq!(report["generated_seed"]["kind"], "http");
    assert_eq!(report["generated_seed"]["base_url"], server.uri());
    assert_eq!(report["generated_seed"]["operations"][0]["path"], "/items");
    assert_eq!(
        report["generated_seed"]["operations"][0]["effect"]["read_only"],
        true
    );
    assert_eq!(
        report["generated_seed"]["operations"][0]["effect"]["network"],
        true
    );
    assert_eq!(report["discovery"]["operations_found"], 1);
    assert_eq!(report["discovery"]["operations_probed"], 1);
    assert_eq!(report["discovery"]["shapes_learned"], 1);
    assert!(
        report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap()
                == format!("prog --dir {dir_arg} call api list --args '{{}}'"))
    );

    let profile = read_profile(dir.path(), "api");
    let effects = &profile["operations"][0]["effects"];
    assert_eq!(effects["read_only"], true);
    assert_eq!(effects["mutating"], false);
    assert_eq!(effects["network"], true);
    assert_eq!(effects["cacheable"], true);
    assert!(profile["operations"][0]["output_shape"].is_object());

    let call = prog(&["--dir", dir_arg, "call", "api", "list", "--args", "{}"]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(envelope["source_id"], "api");
    assert_eq!(envelope["operation"], "list");
}

#[test]
fn source_add_cli_creates_working_profile_from_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("emit.py");
    fs::write(
        &script,
        "import json\nprint(json.dumps({'items':[{'id': 1, 'state': 'open', 'body': 'x' * 500}]}))\n",
    )
    .unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let output = prog(&[
        "--dir",
        dir_arg,
        "source",
        "add-cli",
        "local",
        "--operation",
        "list",
        "--read-only",
        "--",
        "python3",
        script.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["source_id"], "local");
    assert_eq!(report["kind"], "cli");
    assert_eq!(
        report["generated_seed"]["operations"][0]["command"],
        "python3"
    );
    assert_eq!(
        report["generated_seed"]["operations"][0]["args"][0],
        script.to_str().unwrap()
    );
    assert_eq!(
        report["generated_seed"]["operations"][0]["effect"]["read_only"],
        true
    );
    assert_eq!(
        report["generated_seed"]["operations"][0]["effect"]["network"],
        false
    );
    assert!(
        report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap()
                == format!("prog --dir {dir_arg} call local list --args '{{}}'"))
    );
    assert_eq!(
        report["discovery"]["effects_assumed"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let profile = read_profile(dir.path(), "local");
    let effects = &profile["operations"][0]["effects"];
    assert_eq!(effects["read_only"], true);
    assert_eq!(effects["mutating"], false);
    assert_eq!(effects["network"], false);
    assert_eq!(effects["shell"], false);
    assert_eq!(effects["cacheable"], true);

    let call = prog(&["--dir", dir_arg, "call", "local", "list", "--args", "{}"]);
    assert!(call.status.success(), "{}", stdout(&call));
    let envelope: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(envelope["data_preview"]["items"][0]["state"], "open");
}

#[test]
fn source_add_preserves_fail_closed_defaults_and_reports_invalid_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let invalid = prog(&[
        "--dir",
        dir_arg,
        "source",
        "add-http",
        "api",
        "--operation",
        "list",
        "--url",
        "ftp://example.test/items",
    ]);
    assert!(!invalid.status.success());
    let error: Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("scheme must be http or https")
    );

    let output = prog(&[
        "--dir",
        dir_arg,
        "source",
        "add-cli",
        "danger",
        "--operation",
        "show",
        "--",
        "python3",
        "-c",
        "import json; print(json.dumps({'ok': True}))",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("confirmation-gated"))
    );
    assert!(
        report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap()
                == format!("prog --dir {dir_arg} call danger show --args '{{}}' --yes"))
    );
    let profile = read_profile(dir.path(), "danger");
    let effects = &profile["operations"][0]["effects"];
    assert_eq!(effects["read_only"], false);
    assert_eq!(effects["mutating"], true);
    assert_eq!(effects["requires_confirmation"], true);
    assert_eq!(effects["sensitive"], true);
    assert_eq!(effects["cacheable"], false);

    let call = prog(&["--dir", dir_arg, "call", "danger", "show", "--args", "{}"]);
    assert!(!call.status.success());
    let error: Value = serde_json::from_slice(&call.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "requires_confirmation");
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
    assert_eq!(profile["version"], 2);
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
fn cache_list_and_purge_are_real_json_commands() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let list = prog(&["--dir", dir_arg, "cache", "list"]);
    assert!(list.status.success());
    assert_eq!(stderr(&list), "");
    let value: Value = serde_json::from_slice(&list.stdout).expect("stdout must be JSON");
    assert_eq!(value["entries"], json_array());

    let purge = prog(&["--dir", dir_arg, "cache", "purge", "--all"]);
    assert!(purge.status.success());
    assert_eq!(stderr(&purge), "");
    let value: Value = serde_json::from_slice(&purge.stdout).expect("stdout must be JSON");
    assert_eq!(value["purged_entries"], 0);
    assert_eq!(value["purged_payloads"], 0);
    assert_eq!(value["purged_cursors"], 0);
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
    assert!(text.starts_with("{\n"));
    let value: Value = serde_json::from_str(&text).unwrap();
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
          "schema_version": "prog.lens_manifest.v1",
          "id": "cli.items",
          "version": 1,
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
        r#"schema_version: prog.lens_manifest.v1
id: unused.yaml
version: 1
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
          "schema_version": "prog.lens_manifest.v1",
          "id": "bad",
          "version": 1,
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

fn json_array() -> Value {
    Value::Array(Vec::new())
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
}

fn read_profile(dir: &Path, id: &str) -> Value {
    serde_json::from_slice(&fs::read(dir.join("profiles").join(format!("{id}.json"))).unwrap())
        .unwrap()
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
fn evidence_navigation_workflow_is_offline_scoped_bounded_and_session_backed() {
    let dir = tempfile::tempdir().unwrap();
    let run = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "run",
        "--",
        "sh",
        "-c",
        "echo 'error[E0308]: mismatched types' >&2; exit 1",
    ]);
    assert!(run.status.success(), "{}", stdout(&run));
    let envelope: Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(envelope["findings"][0]["path"], "/failure_sections/0");
    assert!(run.stdout.len() <= 16 * 1024);
    let cursor = envelope["cursor"].as_str().unwrap();

    let inspect = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "inspect",
        cursor,
        "--goal",
        "find the root cause",
        "--kind",
        "error",
    ]);
    assert!(inspect.status.success(), "{}", stdout(&inspect));
    let inspect_value: Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(inspect_value["normalized_goal"], "root_cause");
    assert_eq!(inspect_value["findings"][0]["path"], "/failure_sections/0");
    assert!(inspect.stdout.len() <= 16 * 1024);

    let evidence = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "evidence",
        cursor,
        "--path",
        "/failure_sections/0",
    ]);
    assert!(evidence.status.success(), "{}", stdout(&evidence));
    let evidence_value: Value = serde_json::from_slice(&evidence.stdout).unwrap();
    assert_eq!(evidence_value["schema_version"], "prog.evidence.v1");
    assert_eq!(evidence_value["line_range"]["start"], 1);
    assert!(
        evidence_value["source_command"]
            .as_str()
            .unwrap()
            .contains("sh")
    );

    let search = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "search",
        cursor,
        "mismatched",
        "--path",
        "/failure_sections",
    ]);
    assert!(search.status.success(), "{}", stdout(&search));
    let search_value: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert!(search_value["hits"].as_array().unwrap().iter().all(|hit| {
        hit["path"]
            .as_str()
            .unwrap()
            .starts_with("/failure_sections")
    }));

    let escape = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "find",
        cursor,
        "--kind",
        "error",
        "--path",
        "/outside",
    ]);
    assert!(!escape.status.success());
    let escape_value: Value = serde_json::from_slice(&escape.stdout).unwrap();
    assert!(matches!(
        escape_value["error"]["kind"].as_str(),
        Some("path_not_found" | "path_outside_boundary")
    ));

    let session = prog(&["--dir", dir.path().to_str().unwrap(), "session", "show"]);
    assert!(session.status.success(), "{}", stdout(&session));
    let session_value: Value = serde_json::from_slice(&session.stdout).unwrap();
    let kinds = session_value["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"run"));
    assert!(kinds.contains(&"inspect"));
    assert!(kinds.contains(&"evidence"));
    assert!(kinds.contains(&"search"));
}

#[test]
fn source_add_cli_detects_and_optionally_applies_structured_output() {
    let dir = tempfile::tempdir().unwrap();
    let suggested = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "source",
        "add-cli",
        "pods",
        "--operation",
        "list",
        "--read-only",
        "--",
        "kubectl",
        "get",
        "pods",
    ]);
    assert!(suggested.status.success(), "{}", stdout(&suggested));
    let suggested_value: Value = serde_json::from_slice(&suggested.stdout).unwrap();
    assert_eq!(
        suggested_value["structured_output"][0]["status"],
        "suggested"
    );
    assert_eq!(
        suggested_value["structured_output"][0]["flag"],
        json!(["-o", "json"])
    );

    let applied = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "source",
        "add-cli",
        "pods_json",
        "--operation",
        "list",
        "--read-only",
        "--prefer-json",
        "--",
        "kubectl",
        "get",
        "pods",
    ]);
    assert!(applied.status.success(), "{}", stdout(&applied));
    let applied_value: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(
        applied_value["generated_seed"]["operations"][0]["args"],
        json!(["get", "pods", "-o", "json"])
    );
    assert_eq!(applied_value["structured_output"][0]["status"], "detected");
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

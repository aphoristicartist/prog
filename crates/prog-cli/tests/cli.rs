use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
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

#[test]
fn help_shows_complete_command_tree() {
    let output = prog(&["--help"]);
    assert!(output.status.success());
    assert_eq!(stderr(&output), "");

    let help = stdout(&output);
    for expected in [
        "discover",
        "hints",
        "call",
        "observe",
        "run",
        "init",
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
            .any(|step| step.as_str().unwrap().contains("prog paths"))
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
        "prog paths",
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
fn init_requires_project_scope_and_rejects_unimplemented_agents() {
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

    let unsupported = prog(&["init", "--agent", "cursor", "--project", "--root", root]);
    assert!(!unsupported.status.success());
    assert_eq!(stderr(&unsupported), "");
    let error: Value = serde_json::from_slice(&unsupported.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not implemented yet")
    );
    assert!(!project.path().join(".codex").exists());
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

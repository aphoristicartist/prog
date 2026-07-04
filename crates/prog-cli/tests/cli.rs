use std::process::{Command, Output};

use serde_json::Value;

fn prog(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_prog"))
        .args(args)
        .output()
        .expect("prog binary should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be utf8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be utf8")
}

fn assert_placeholder(args: &[&str], command: &str) {
    let output = prog(args);
    assert!(
        !output.status.success(),
        "{args:?} should fail until implemented"
    );
    assert_eq!(
        stderr(&output),
        "",
        "{args:?} must not write diagnostics to stderr"
    );

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout must be JSON");
    assert_eq!(value["error"]["kind"], "not_implemented");
    assert!(
        value["error"]["message"]
            .as_str()
            .expect("message should be a string")
            .contains(command),
        "message should name {command}"
    );
    assert!(
        value["error"]["hint"]
            .as_str()
            .expect("hint should be a string")
            .contains("issue #1"),
        "hint should point to the scaffold state"
    );
}

#[test]
fn help_shows_complete_command_tree() {
    let output = prog(&["--help"]);
    assert!(output.status.success());
    assert_eq!(stderr(&output), "");

    let help = stdout(&output);
    for expected in [
        "discover", "hints", "call", "expand", "cache", "meta", "--dir", "--pretty",
    ] {
        assert!(help.contains(expected), "help should include {expected}");
    }
}

#[test]
fn every_placeholder_command_returns_structured_json_on_stdout() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["discover", "local", "--kind", "cli", "--seed", "{}"],
            "discover",
        ),
        (&["hints", "local"], "hints"),
        (&["call", "local", "list", "--args", "{}"], "call"),
        (&["expand", "pc1_test", "--path", "/items/0"], "expand"),
        (&["meta"], "meta"),
    ];

    for (args, command) in cases {
        assert_placeholder(args, command);
    }
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

fn json_array() -> Value {
    Value::Array(Vec::new())
}

#[test]
fn pretty_errors_are_still_machine_readable() {
    let output = prog(&["--pretty", "meta"]);
    assert!(!output.status.success());
    assert_eq!(stderr(&output), "");

    let text = stdout(&output);
    assert!(text.starts_with("{\n"));
    let value: Value = serde_json::from_str(&text).expect("pretty stdout must still be JSON");
    assert_eq!(value["error"]["kind"], "not_implemented");
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

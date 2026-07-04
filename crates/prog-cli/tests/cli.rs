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
        (&["cache", "list"], "cache list"),
        (&["cache", "get", "sha256:abc"], "cache get"),
        (&["cache", "purge", "--expired"], "cache purge"),
        (&["meta"], "meta"),
    ];

    for (args, command) in cases {
        assert_placeholder(args, command);
    }
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

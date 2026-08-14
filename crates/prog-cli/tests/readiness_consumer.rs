//! Contract test for the documented example harness readiness consumer.

use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

use serde_json::Value;

mod support;

use support::repo_root;

fn consume(fixture: &str) -> std::process::Output {
    let root = repo_root();
    let input = fs::read(root.join("fixtures/harness").join(fixture)).unwrap();
    let mut child = Command::new("python3")
        .arg(root.join("fixtures/harness/readiness_consumer.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn documented_readiness_consumer_passes_only_ready_configured_reports() {
    let ready = consume("readiness-ready.json");
    assert!(ready.status.success());
    let ready: Value = serde_json::from_slice(&ready.stdout).unwrap();
    assert_eq!(ready["decision"], "pass");
    assert_eq!(ready["blockers"], serde_json::json!([]));

    let blocked = consume("readiness-blocked.json");
    assert_eq!(blocked.status.code(), Some(1));
    let blocked: Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(blocked["decision"], "block");
    assert_eq!(blocked["reason"], "required_obligations_unsatisfied");
    assert_eq!(blocked["blockers"].as_array().unwrap().len(), 1);
}

#[test]
fn documented_readiness_consumer_fails_closed_on_contract_drift() {
    let root = repo_root();
    let mut child = Command::new("python3")
        .arg(root.join("fixtures/harness/readiness_consumer.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"ready":true,"blockers":[]}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["decision"], "block");
    assert_eq!(report["reason"], "unexpected_schema");
}

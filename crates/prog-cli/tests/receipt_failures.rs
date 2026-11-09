//! Receipts for runs that fail without a failing test (#313).
//!
//! Every fixture under `tests/fixtures/receipts/` is the real stdout, stderr,
//! and exit code of one run, captured with cargo 1.98, pytest 9.1, and Go
//! 1.27 (paths rewritten to `/repo`). `jest-out-of-memory` reproduces Node's
//! V8 heap-limit abort, which happens before Jest prints any summary.
//!
//! A replaying fake tool runs each fixture through `prog run`, and the test
//! checks the receipt the agent would read: the failure is never reported as
//! `0 failed`, and the first-failure line names the real error, never a build
//! progress line, a report header, or a passing test.

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use serde_json::Value;

mod support;

use support::{prog, stdout};

/// Replays `fixture/{stdout,stderr,exit_code}` from the directory named in
/// the `fixture` file beside it.
const REPLAY: &str = r#"#!/usr/bin/env python3
import sys
from pathlib import Path
fixture = Path(Path(__file__).with_name("fixture").read_text().strip())
sys.stdout.write((fixture / "stdout").read_text())
sys.stdout.flush()
sys.stderr.write((fixture / "stderr").read_text())
sys.exit(int((fixture / "exit_code").read_text().strip()))
"#;

const PROGRESS_PREFIXES: &[&str] = &[
    "Compiling ",
    "Checking ",
    "Running ",
    "Finished ",
    "Doc-tests ",
    "running ",
    "rootdir:",
    "cachedir:",
    "platform ",
    "collecting ",
    "test result: ok",
];

/// Run one fixture through `prog run` with a fake `program`; return the
/// receipt for that run.
fn receipt_for(program: &str, args: &[&str], fixture: &str) -> Value {
    let tool_dir = tempfile::tempdir().unwrap();
    let tool = tool_dir.path().join(program);
    fs::write(&tool, REPLAY).unwrap();
    let mut permissions = fs::metadata(&tool).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&tool, permissions).unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/receipts")
        .join(fixture);
    fs::write(
        tool_dir.path().join("fixture"),
        fixture_dir.to_str().unwrap(),
    )
    .unwrap();

    let store = tempfile::tempdir().unwrap();
    let dir = store.path().to_str().unwrap();
    let mut argv = vec!["--dir", dir, "run", "--", tool.to_str().unwrap()];
    argv.extend_from_slice(args);
    let output = prog(&argv);
    assert!(output.status.success(), "{}", stdout(&output));
    let output = prog(&["--dir", dir, "receipt"]);
    assert!(output.status.success(), "{}", stdout(&output));
    let list: Value = serde_json::from_slice(&output.stdout).unwrap();
    list["receipts"][0].clone()
}

fn first_failure_line(text: &str) -> &str {
    text.lines()
        .find_map(|line| {
            line.strip_prefix("first failure: ")
                .or_else(|| line.strip_prefix("first error: "))
        })
        .unwrap_or_else(|| panic!("no first-failure line: {text}"))
}

/// Cases: (program, args, fixture, headline fragment, first-failure prefix).
const CASES: &[(&str, &[&str], &str, &str, &str)] = &[
    (
        "cargo",
        &["test"],
        "cargo-abort",
        "unittests src/lib.rs did not finish",
        "`--lib` crashed (signal: 6, SIGABRT: process abort signal)",
    ),
    (
        "cargo",
        &["test", "--no-fail-fast"],
        "cargo-no-fail-fast-abort",
        "2 passed, failure not attributed to a test (tests/a.rs did not finish)",
        "`--test a` crashed (signal: 6, SIGABRT: process abort signal)",
    ),
    (
        "cargo",
        &["test"],
        "cargo-compile-error",
        "exit 101",
        "src/lib.rs:1:41: error: expected expression, found `}`",
    ),
    (
        "cargo",
        &["clippy", "--", "-D", "warnings"],
        "cargo-clippy",
        "exit 101",
        "src/lib.rs:1:20: error: writing `&Vec` instead of `&[_]`",
    ),
    (
        "cargo",
        &["test"],
        "cargo-doctest",
        "0 passed, 1 failed: src/lib.rs - add (line 1)",
        "src/lib.rs - add (line 1): assertion `left == right` failed",
    ),
    (
        "pytest",
        &["-v"],
        "pytest-internalerror",
        "0 passed, failure not attributed to a test",
        "RuntimeError: plugin boom",
    ),
    (
        "pytest",
        &["-v"],
        "pytest-collection-error",
        "0 passed, 1 failed",
        "ModuleNotFoundError: No module named 'nope'",
    ),
    (
        "go",
        &["test", "-v", "./..."],
        "go-init-panic",
        "0 passed, failure not attributed to a test",
        "panic: init boom",
    ),
    (
        "jest",
        &[],
        "jest-out-of-memory",
        "exit 134",
        "FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory",
    ),
];

/// #313: each receipt names the real failure.
#[test]
fn receipts_name_the_real_failure_of_runs_without_a_failing_test() {
    for (program, args, fixture, headline, first) in CASES {
        let receipt = receipt_for(program, args, fixture);
        assert_eq!(receipt["verdict"], "failing", "{fixture}: {receipt:#}");
        let text = receipt["text"].as_str().unwrap();
        let first_line = text.lines().next().unwrap();
        assert!(first_line.contains(headline), "{fixture}: {text}");
        assert!(
            first_failure_line(text).starts_with(first),
            "{fixture}: {text}"
        );
        assert!(
            text.len() <= prog_core::MAX_RECEIPT_BYTES,
            "{fixture}: {} bytes",
            text.len()
        );
    }
}

/// I16: a run that exited unsuccessfully never reads as zero failures, and
/// its first-failure line is never build progress or a passing result.
#[test]
fn unsuccessful_runs_never_read_as_zero_failures() {
    for (program, args, fixture, _, _) in CASES {
        let text = receipt_for(program, args, fixture)["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            !text.contains(" 0 failed"),
            "{fixture}: a failed run reads as 0 failed: {text}"
        );
        let first = first_failure_line(&text);
        assert!(
            !PROGRESS_PREFIXES
                .iter()
                .any(|prefix| first.starts_with(prefix))
                && !first.ends_with(" ... ok"),
            "{fixture}: progress line chosen as the failure: {text}"
        );
    }
}

/// Build and lint failures are described as errors, not test failures.
#[test]
fn build_and_lint_failures_are_errors_not_test_failures() {
    let clippy = receipt_for("cargo", &["clippy", "--", "-D", "warnings"], "cargo-clippy");
    let text = clippy["text"].as_str().unwrap();
    assert!(text.contains("\nfirst error: "), "{text}");
    let test = receipt_for("cargo", &["test"], "cargo-compile-error");
    let text = test["text"].as_str().unwrap();
    assert!(text.contains("\nfirst failure: "), "{text}");
}

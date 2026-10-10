//! Seeded false-completion scenarios (#284, #291).
//!
//! Each scenario reruns the *same* argv after an "edit" that makes the test
//! output clean. A real fix must still be provable. Deleting, skipping, or
//! ignoring the failing test produces equally clean output, so it must never
//! be reported as `resolved`, a `passed` obligation, or `ready`.

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use serde_json::Value;

mod support;

use support::{prog, prog_in_dir, stdout, test_git_repo};

/// A fake `cargo` whose libtest output is chosen by a state file. The file
/// name must stay `cargo` so the Cargo/Rust coding provider recognizes it.
const FAKE_CARGO: &str = r#"#!/usr/bin/env python3
import sys
from pathlib import Path
state = Path(__file__).with_name("state").read_text().strip()
quiet = "-q" in sys.argv
panic = """
failures:

---- tests::adds stdout ----

thread 'tests::adds' panicked at src/lib.rs:6:25:
assertion `left == right` failed
  left: 5
 right: 4

failures:
    tests::adds
"""
def harness(lines, summary, failed):
    print("running %d tests" % len(lines))
    for name, status in lines:
        if not quiet:
            print("test %s ... %s" % (name, status))
    if failed:
        print(panic)
    print("test result: %s" % summary)
if state == "baseline":
    harness([("tests::adds", "FAILED"), ("tests::muls", "ok")],
            "FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s", True)
    print("error: 1 target failed:", file=sys.stderr)
    sys.exit(101)
if state == "fixed":
    harness([("tests::adds", "ok"), ("tests::muls", "ok")],
            "ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s", False)
elif state == "deleted":
    harness([("tests::muls", "ok")],
            "ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s", False)
elif state == "ignored":
    harness([("tests::adds", "ignored"), ("tests::muls", "ok")],
            "ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s", False)
else:
    raise SystemExit("unknown state " + state)
"#;

/// A fake quiet `pytest -q`: passing tests are never named, only counted.
const FAKE_PYTEST: &str = r#"#!/usr/bin/env python3
import sys
from pathlib import Path
state = Path(__file__).with_name("state").read_text().strip()
if state == "baseline":
    print("F.                                                    [100%]")
    print("FAILED tests/test_math.py::test_add - AssertionError: assert 5 == 4")
    print("1 failed, 1 passed in 0.01s")
    sys.exit(1)
if state == "fixed":
    print("..                                                    [100%]")
    print("2 passed in 0.01s")
elif state == "deleted":
    print(".                                                     [100%]")
    print("1 passed in 0.01s")
elif state == "ignored":
    print("s.                                                    [100%]")
    print("1 passed, 1 skipped in 0.01s")
else:
    raise SystemExit("unknown state " + state)
"#;

struct Tool {
    _dir: tempfile::TempDir,
    executable: String,
}

impl Tool {
    fn new(name: &str, script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        fs::write(&path, script).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).unwrap();
        Self {
            executable: path.to_str().unwrap().to_string(),
            _dir: dir,
        }
    }

    fn set_state(&self, state: &str) {
        fs::write(Path::new(&self.executable).with_file_name("state"), state).unwrap();
    }
}

fn run(dir: &str, argv: &[&str]) -> Value {
    let mut args = vec!["--dir", dir, "run", "--"];
    args.extend_from_slice(argv);
    let output = prog(&args);
    assert!(output.status.success(), "{}", stdout(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

fn observation_id(envelope: &Value) -> String {
    envelope["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn delta(dir: &str, baseline: &str, subject: &str) -> Value {
    let output = prog(&["--dir", dir, "delta", baseline, subject]);
    assert!(output.status.success(), "{}", stdout(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

fn count(delta: &Value, status: &str) -> u64 {
    delta["counts"][status].as_u64().unwrap_or(0)
}

fn reasons(delta: &Value) -> String {
    delta["assessment"]["reasons"].to_string()
}

/// Run baseline then the edited state with identical argv; return the delta.
fn scenario(tool: &Tool, argv: &[&str], edited_state: &str) -> (tempfile::TempDir, Value) {
    let store = tempfile::tempdir().unwrap();
    let dir = store.path().to_str().unwrap().to_string();
    tool.set_state("baseline");
    let baseline = run(&dir, argv);
    tool.set_state(edited_state);
    let subject = run(&dir, argv);
    let delta = delta(&dir, &observation_id(&baseline), &observation_id(&subject));
    (store, delta)
}

fn assert_real_fix_is_provable(delta: &Value) {
    assert_eq!(delta["assessment"]["can_prove_absence"], true, "{delta:#}");
    assert!(count(delta, "resolved") > 0, "{delta:#}");
    assert_eq!(count(delta, "unknown"), 0, "{delta:#}");
}

fn assert_fake_fix_is_not_provable(delta: &Value, expected_reason: &str) {
    assert_eq!(delta["assessment"]["can_prove_absence"], false, "{delta:#}");
    assert_eq!(count(delta, "resolved"), 0, "{delta:#}");
    assert!(
        reasons(delta).contains(expected_reason),
        "missing reason {expected_reason:?}: {}",
        reasons(delta)
    );
}

#[test]
fn cargo_real_fix_is_resolved() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test", "--no-fail-fast"];
    let (_store, delta) = scenario(&cargo, &argv, "fixed");
    assert_real_fix_is_provable(&delta);
}

#[test]
fn cargo_deleted_failing_test_is_not_resolved() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test", "--no-fail-fast"];
    let (_store, delta) = scenario(&cargo, &argv, "deleted");
    assert_fake_fix_is_not_provable(&delta, "test population shrank");
    assert_fake_fix_is_not_provable(&delta, "tests::adds (missing)");
}

#[test]
fn cargo_ignored_failing_test_is_not_resolved() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test", "--no-fail-fast"];
    let (_store, delta) = scenario(&cargo, &argv, "ignored");
    assert_fake_fix_is_not_provable(&delta, "tests::adds (skipped)");
}

#[test]
fn cargo_quiet_deleted_test_is_caught_by_executed_counts() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test", "-q", "--no-fail-fast"];
    let (_store, delta) = scenario(&cargo, &argv, "deleted");
    assert_fake_fix_is_not_provable(&delta, "executed 1 tests but the baseline executed 2");
    let (_store, fixed) = scenario(&cargo, &argv, "fixed");
    assert_real_fix_is_provable(&fixed);
}

#[test]
fn pytest_quiet_real_fix_is_resolved() {
    let pytest = Tool::new("pytest", FAKE_PYTEST);
    let argv = [pytest.executable.as_str(), "-q"];
    let (_store, delta) = scenario(&pytest, &argv, "fixed");
    assert_real_fix_is_provable(&delta);
}

#[test]
fn pytest_deleted_or_skipped_failing_test_is_not_resolved() {
    let pytest = Tool::new("pytest", FAKE_PYTEST);
    let argv = [pytest.executable.as_str(), "-q"];
    for state in ["deleted", "ignored"] {
        let (_store, delta) = scenario(&pytest, &argv, state);
        assert_fake_fix_is_not_provable(&delta, "test population shrank");
    }
}

/// The readiness gate inherits the comparison: an obligation declared against
/// the baseline failure must not pass when the failing test was deleted.
/// Readiness needs a Git workspace to prove nothing changed after the run, so
/// commands run inside a fixture repository with the store kept outside it.
#[test]
fn readiness_is_not_ready_when_the_failing_test_was_deleted() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test", "--no-fail-fast"];
    for (state, expect_ready) in [("deleted", false), ("fixed", true)] {
        let workspace = test_git_repo();
        let root = workspace.path();
        let store = tempfile::tempdir().unwrap();
        let dir = store.path().to_str().unwrap();
        let in_repo = |args: &[&str]| {
            let mut full = vec!["--dir", dir];
            full.extend_from_slice(args);
            let output = prog_in_dir(root, &full);
            assert!(output.status.success(), "{}", stdout(&output));
            serde_json::from_slice::<Value>(&output.stdout).unwrap()
        };
        let run_in_repo = |argv: &[&str]| {
            let mut args = vec!["run", "--"];
            args.extend_from_slice(argv);
            in_repo(&args)
        };
        in_repo(&["session", "start", "--goal", "fix add"]);
        cargo.set_state("baseline");
        let baseline = run_in_repo(&argv);
        let fingerprint = baseline["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|finding| finding["kind"] == "rust_panic")
            .and_then(|finding| finding["fingerprint"].as_str())
            .unwrap_or_else(|| panic!("no rust_panic finding: {baseline:#}"))
            .to_string();
        cargo.set_state(state);
        let subject = run_in_repo(&argv);
        let baseline_id = observation_id(&baseline);
        let subject_id = observation_id(&subject);
        in_repo(&[
            "session",
            "obligation-add",
            "add-fixed",
            "--check",
            "add panic gone",
            "--scope",
            "tests",
            "--origin-observation",
            &baseline_id,
            "--expected-absent-fingerprint",
            &fingerprint,
            "--evidence-observation",
            &subject_id,
        ]);
        let readiness = in_repo(&["session", "show", "--readiness"]);
        assert_eq!(readiness["ready"], expect_ready, "{state}: {readiness:#}");
        let status = &readiness["evaluations"][0]["status"];
        if expect_ready {
            assert_eq!(status, "passed", "{readiness:#}");
        } else {
            assert_ne!(status, "passed", "{readiness:#}");
        }
    }
}

/// #287: a default fail-fast baseline cannot prove absence, and the delta now
/// says how to fix that instead of only "selection is not exhaustive". Scopes
/// that completed normally are no longer reported as stops.
#[test]
fn fail_fast_baseline_explains_how_to_get_a_provable_comparison() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test"];
    let (_store, delta) = scenario(&cargo, &argv, "fixed");
    assert_eq!(delta["assessment"]["can_prove_absence"], false, "{delta:#}");
    assert_eq!(count(&delta, "resolved"), 0, "{delta:#}");
    let reasons = reasons(&delta);
    assert!(reasons.contains("--no-fail-fast"), "{reasons}");
    assert!(!reasons.contains("stopped: complete"), "{reasons}");
}

fn receipt(dir: &str, observation: Option<&str>) -> Value {
    let mut args = vec!["--dir", dir, "receipt"];
    args.extend(observation);
    let output = prog(&args);
    assert!(output.status.success(), "{}", stdout(&output));
    let list: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(list["schema"], "prog.receipts");
    list["receipts"][0].clone()
}

/// #288: the receipt is a few hundred bytes, names the failing test and its
/// location, and only says "fixed" when the delta can prove it.
#[test]
fn receipts_are_small_and_never_claim_an_unproven_fix() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test", "--no-fail-fast"];
    for (state, verdict, expected) in [
        ("fixed", "fixed", "vs previous run: fixed: tests::adds"),
        ("deleted", "not_proven", "NOT PROVEN"),
    ] {
        let store = tempfile::tempdir().unwrap();
        let dir = store.path().to_str().unwrap();
        cargo.set_state("baseline");
        let baseline = run(dir, &argv);
        let first = receipt(dir, Some(&observation_id(&baseline)));
        assert_eq!(first["verdict"], "failing", "{first:#}");
        let text = first["text"].as_str().unwrap();
        assert!(text.contains("1 passed, 1 failed: tests::adds"), "{text}");
        assert!(
            text.contains("first failure: tests::adds panicked at src/lib.rs:6:25"),
            "{text}"
        );
        assert!(text.contains("--path /failure_sections/0"), "{text}");
        assert!(text.len() <= 300, "{} bytes: {text}", text.len());

        cargo.set_state(state);
        let subject = run(dir, &argv);
        let latest = receipt(dir, None);
        assert_eq!(latest["observation_id"], observation_id(&subject).as_str());
        assert_eq!(latest["compared_with"], observation_id(&baseline).as_str());
        assert_eq!(latest["verdict"], verdict, "{latest:#}");
        let text = latest["text"].as_str().unwrap();
        assert!(text.contains(expected), "{state}: {text}");
        assert!(text.len() <= 300, "{} bytes: {text}", text.len());
        // The evidence cursor in the receipt resolves to the summarized run.
        let cursor = latest["cursor"].as_str().unwrap();
        let evidence = prog(&["--dir", dir, "evidence", cursor, "--path", "/command"]);
        assert!(evidence.status.success(), "{}", stdout(&evidence));
    }
}

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
elif state == "swapped":
    harness([("tests::muls", "ok"), ("tests::subs", "ok")],
            "ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s", False)
# A suite with a permanently ignored test (#297).
elif state == "skip-baseline":
    harness([("tests::adds", "FAILED"), ("tests::muls", "ok"), ("tests::slow", "ignored, needs network")],
            "FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s", True)
    print("error: 1 target failed:", file=sys.stderr)
    sys.exit(101)
elif state == "skip-fixed":
    harness([("tests::adds", "ok"), ("tests::muls", "ok"), ("tests::slow", "ignored, needs network")],
            "ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s", False)
elif state == "skip-hidden":
    harness([("tests::adds", "ignored"), ("tests::muls", "ok"), ("tests::slow", "ignored, needs network")],
            "ok. 1 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.00s", False)
elif state == "skip-swapped":
    harness([("tests::adds", "ok"), ("tests::muls", "ignored"), ("tests::slow", "ok")],
            "ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s", False)
else:
    raise SystemExit("unknown state " + state)
"#;

/// A fake `pytest`. With `-q` passing tests are never named, only counted;
/// with `-v` every test is named with its outcome.
const FAKE_PYTEST: &str = r#"#!/usr/bin/env python3
import sys
from pathlib import Path
state = Path(__file__).with_name("state").read_text().strip()
verbose = "-v" in sys.argv
def report(tests, summary):
    if verbose:
        for name, outcome in tests:
            print("tests/test_math.py::%s %s" % (name, outcome))
    else:
        print("".join({"PASSED": ".", "FAILED": "F", "SKIPPED": "s"}[o] for _, o in tests).ljust(53) + "[100%]")
    for name, outcome in tests:
        if outcome == "FAILED":
            print("FAILED tests/test_math.py::%s - AssertionError: assert 5 == 4" % name)
    print(summary)
if state == "baseline":
    report([("test_add", "FAILED"), ("test_mul", "PASSED")], "1 failed, 1 passed in 0.01s")
    sys.exit(1)
if state == "fixed":
    report([("test_add", "PASSED"), ("test_mul", "PASSED")], "2 passed in 0.01s")
elif state == "deleted":
    report([("test_mul", "PASSED")], "1 passed in 0.01s")
elif state == "ignored":
    report([("test_add", "SKIPPED"), ("test_mul", "PASSED")], "1 passed, 1 skipped in 0.01s")
elif state == "swapped":
    report([("test_mul", "PASSED"), ("test_sub", "PASSED")], "2 passed in 0.01s")
elif state == "skip-baseline":
    report([("test_add", "FAILED"), ("test_mul", "PASSED"), ("test_linux", "SKIPPED")],
           "1 failed, 1 passed, 1 skipped in 0.01s")
    sys.exit(1)
elif state == "skip-fixed":
    report([("test_add", "PASSED"), ("test_mul", "PASSED"), ("test_linux", "SKIPPED")],
           "2 passed, 1 skipped in 0.01s")
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
    scenario_from(tool, argv, "baseline", edited_state)
}

fn scenario_from(
    tool: &Tool,
    argv: &[&str],
    baseline_state: &str,
    edited_state: &str,
) -> (tempfile::TempDir, Value) {
    let store = tempfile::tempdir().unwrap();
    let dir = store.path().to_str().unwrap().to_string();
    tool.set_state(baseline_state);
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
}

/// Quiet output names only failures, so "the failing test passed" and "the
/// failing test was deleted and another took its place" print the same
/// thing. Neither is a proven fix; the reason says how to get one.
#[test]
fn quiet_runs_cannot_prove_a_fix_and_say_how_to_get_one() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test", "-q", "--no-fail-fast"];
    for state in ["fixed", "swapped"] {
        let (_store, delta) = scenario(&cargo, &argv, state);
        assert_fake_fix_is_not_provable(&delta, "quiet output does not name passing tests");
        assert_fake_fix_is_not_provable(&delta, "rerun verbosely");
    }
    let pytest = Tool::new("pytest", FAKE_PYTEST);
    let argv = [pytest.executable.as_str(), "-q"];
    for state in ["fixed", "swapped"] {
        let (_store, delta) = scenario(&pytest, &argv, state);
        assert_fake_fix_is_not_provable(&delta, "tests/test_math.py::test_add cannot be shown");
    }
}

#[test]
fn verbose_runs_prove_a_real_fix_and_catch_a_swapped_test() {
    let pytest = Tool::new("pytest", FAKE_PYTEST);
    let argv = [pytest.executable.as_str(), "-v"];
    let (_store, delta) = scenario(&pytest, &argv, "fixed");
    assert_real_fix_is_provable(&delta);
    let (_store, delta) = scenario(&pytest, &argv, "swapped");
    assert_fake_fix_is_not_provable(&delta, "tests/test_math.py::test_add (missing)");

    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test", "--no-fail-fast"];
    let (_store, delta) = scenario(&cargo, &argv, "swapped");
    assert_fake_fix_is_not_provable(&delta, "tests::adds (missing)");
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

/// #297: a suite with a permanently ignored or skipped test used to make
/// every real fix unprovable. When both runs skip exactly the same named
/// tests, the skipped tests ran in neither run and the fix is provable.
#[test]
fn a_fix_is_provable_when_both_runs_skip_the_same_named_tests() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test", "--no-fail-fast"];
    let (_store, delta) = scenario_from(&cargo, &argv, "skip-baseline", "skip-fixed");
    assert_real_fix_is_provable(&delta);
    assert!(
        reasons(&delta).contains("both runs skipped the same 1 named tests"),
        "{}",
        reasons(&delta)
    );

    let pytest = Tool::new("pytest", FAKE_PYTEST);
    let argv = [pytest.executable.as_str(), "-v"];
    let (_store, delta) = scenario_from(&pytest, &argv, "skip-baseline", "skip-fixed");
    assert_real_fix_is_provable(&delta);
}

/// Changing what is skipped is still a narrowed population, and a skip the
/// output does not name cannot be matched, so none of these prove a fix.
#[test]
fn changing_or_unnamed_skips_never_prove_a_fix() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test", "--no-fail-fast"];
    let (_store, delta) = scenario_from(&cargo, &argv, "skip-baseline", "skip-hidden");
    assert_fake_fix_is_not_provable(&delta, "tests::adds (skipped)");
    let (_store, delta) = scenario_from(&cargo, &argv, "skip-baseline", "skip-swapped");
    assert_fake_fix_is_not_provable(&delta, "--include-ignored");
    let (_store, delta) = scenario(&cargo, &argv, "skip-fixed");
    assert_fake_fix_is_not_provable(&delta, "selection coverage is unknown or not exhaustive");

    let pytest = Tool::new("pytest", FAKE_PYTEST);
    let argv = [pytest.executable.as_str(), "-q"];
    let (_store, delta) = scenario_from(&pytest, &argv, "skip-baseline", "skip-fixed");
    assert_fake_fix_is_not_provable(&delta, "pytest -v");
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

/// #299: a failing default (fail-fast) `cargo test` stops early, but it still
/// reports the failures it saw, so a green rerun of the same command proves
/// the fix: the agent loop `cargo test`, fix, `cargo test`. That includes a
/// suite with an ignored test (#297), the exact case from the first live
/// Claude Code session. Deleting the failing test is still caught.
#[test]
fn a_fail_fast_failure_is_proven_fixed_by_a_green_rerun_of_the_same_command() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test"];
    let (_store, delta) = scenario(&cargo, &argv, "fixed");
    assert_real_fix_is_provable(&delta);
    assert!(
        reasons(&delta).contains("baseline selection is not exhaustive"),
        "{}",
        reasons(&delta)
    );
    let (_store, delta) = scenario_from(&cargo, &argv, "skip-baseline", "skip-fixed");
    assert_real_fix_is_provable(&delta);
    let (_store, delta) = scenario(&cargo, &argv, "deleted");
    assert_fake_fix_is_not_provable(&delta, "tests::adds (missing)");
}

/// #287: when the rerun itself fails fast, it may have skipped later
/// harnesses, so the delta cannot prove absence and says how to fix that
/// instead of only "selection is not exhaustive". Scopes that completed
/// normally are not reported as stops.
#[test]
fn a_fail_fast_rerun_that_still_fails_explains_how_to_get_a_provable_comparison() {
    let cargo = Tool::new("cargo", FAKE_CARGO);
    let argv = [cargo.executable.as_str(), "test"];
    let (store, delta) = scenario(&cargo, &argv, "baseline");
    assert_eq!(delta["assessment"]["can_prove_absence"], false, "{delta:#}");
    assert_eq!(count(&delta, "resolved"), 0, "{delta:#}");
    let reasons = reasons(&delta);
    assert!(reasons.contains("subject provider:"), "{reasons}");
    assert!(reasons.contains("--no-fail-fast"), "{reasons}");
    assert!(!reasons.contains("stopped: complete"), "{reasons}");
    // The bounded receipt keeps the advice, not a truncated explanation.
    let text = receipt(store.path().to_str().unwrap(), None)["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.contains("NOT PROVEN (use one exact package+harness target or --no-fail-fast"),
        "{text}"
    );
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

/// A fake `python3` for `python3 -m unittest` (#302). Output goes to stderr,
/// as the real runner's does; `-v` names every test.
const FAKE_UNITTEST: &str = r#"#!/usr/bin/env python3
import sys
from pathlib import Path
state = Path(__file__).with_name("state").read_text().strip()
verbose = "-v" in sys.argv
SEP1, SEP2 = "=" * 70, "-" * 70
def report(tests, summary, code, ran=None):
    for name, status in tests:
        if verbose:
            print("%s (test_calc.TestCalc.%s) ... %s" % (name, name, status), file=sys.stderr)
        else:
            print({"ok": ".", "FAIL": "F", "ERROR": "E"}.get(status, "s"), end="", file=sys.stderr)
    print(file=sys.stderr)
    for name, status in tests:
        if status in ("FAIL", "ERROR"):
            print("%s\n%s: %s (test_calc.TestCalc.%s)\n%s\nAssertionError: 0 != 4\n" % (SEP1, status, name, name, SEP2), file=sys.stderr)
    if summary is not None:
        print("%s\nRan %d tests in 0.001s\n\n%s" % (SEP2, len(tests) if ran is None else ran, summary), file=sys.stderr)
    sys.exit(code)
states = {
    "baseline": ([("test_add", "FAIL"), ("test_zero", "ok")], "FAILED (failures=1)", 1),
    "fixed": ([("test_add", "ok"), ("test_zero", "ok")], "OK", 0),
    "deleted": ([("test_zero", "ok")], "OK", 0),
    "ignored": ([("test_add", "skipped 'later'"), ("test_zero", "ok")], "OK (skipped=1)", 0),
    "swapped": ([("test_other", "ok"), ("test_zero", "ok")], "OK", 0),
    "lying": ([("test_add", "ok"), ("test_zero", "ok")], "FAILED (failures=1)", 0),
    "cut": ([("test_add", "ok"), ("test_zero", "ok")], None, 0),
    "failfast-other": ([("test_aaa", "FAIL")], "FAILED (failures=1)", 1),
}
if state == "loadfail":
    print("test_calc (unittest.loader._FailedTest.test_calc) ... ERROR\ntest_zero (test_calc.TestCalc.test_zero) ... ok\n\n%s\nRan 2 tests in 0.001s\n\nFAILED (errors=1)" % SEP2, file=sys.stderr)
    sys.exit(1)
report(*states[state])
"#;

/// A fake `go` for `go test` (#302): `-v` text, or `-json` events.
const FAKE_GO: &str = r##"#!/usr/bin/env python3
import json, sys
from pathlib import Path
state = Path(__file__).with_name("state").read_text().strip()
as_json = "-json" in sys.argv
verbose = "-v" in sys.argv or as_json
PKG = "example.com/m/calc"
def run(tests, package, code):
    for name, status in tests:
        if as_json:
            print(json.dumps({"Action": "run", "Package": PKG, "Test": name}))
            if status == "FAIL":
                print(json.dumps({"Action": "output", "Package": PKG, "Test": name, "Output": "    calc_test.go:7: Add(2, 2) = 0, want 4\n"}))
            print(json.dumps({"Action": {"PASS": "pass", "FAIL": "fail", "SKIP": "skip"}[status], "Package": PKG, "Test": name}))
        else:
            if verbose:
                print("=== RUN   %s" % name)
            if verbose or status == "FAIL":
                print("--- %s: %s (0.00s)" % (status, name))
            if status == "FAIL":
                print("    calc_test.go:7: Add(2, 2) = 0, want 4")
    if package is not None:
        if as_json:
            print(json.dumps({"Action": "pass" if package == "ok" else "fail", "Package": PKG}))
        elif package == "ok":
            print("PASS\nok  \t%s\t0.01s" % PKG)
        else:
            print("FAIL\nFAIL\t%s\t0.01s\nFAIL" % PKG)
    sys.exit(code)
states = {
    "baseline": ([("TestAdd", "FAIL"), ("TestZero", "PASS")], "FAIL", 1),
    "fixed": ([("TestAdd", "PASS"), ("TestZero", "PASS")], "ok", 0),
    "deleted": ([("TestZero", "PASS")], "ok", 0),
    "ignored": ([("TestAdd", "SKIP"), ("TestZero", "PASS")], "ok", 0),
    "swapped": ([("TestOther", "PASS"), ("TestZero", "PASS")], "ok", 0),
    "lying": ([("TestAdd", "PASS"), ("TestZero", "PASS")], "FAIL", 0),
    "cut": ([("TestAdd", "PASS"), ("TestZero", "PASS")], None, 0),
    "failfast-other": ([("TestAaa", "FAIL")], "FAIL", 1),
}
if state == "buildfail" and as_json:
    # Go 1.24+ reports build failures as events (captured from Go 1.27).
    target = "%s [%s.test]" % (PKG, PKG)
    for event in [{"ImportPath": target, "Action": "build-output", "Output": "calc/calc.go:3:37: syntax error\n"},
                  {"ImportPath": target, "Action": "build-fail"},
                  {"Action": "start", "Package": PKG},
                  {"Action": "output", "Package": PKG, "Output": "FAIL\t%s [build failed]\n" % PKG},
                  {"Action": "fail", "Package": PKG, "FailedBuild": target}]:
        print(json.dumps(event))
    sys.exit(1)
if state == "buildfail":
    print("# %s\ncalc/calc.go:3:37: syntax error\nFAIL\t%s [build failed]\nFAIL" % (PKG, PKG))
    sys.exit(1)
run(*states[state])
"##;

/// A fake `jest` (#302): `--json` prints Jest 30's report; otherwise the
/// failures-only text Jest prints in an agent session.
const FAKE_JEST: &str = r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path
state = Path(__file__).with_name("state").read_text().strip()
FILE = "/repo/src/calc.test.js"
def report(tests, success, code, extra_files=(), prefix=""):
    results = [{"ancestorTitles": ["calc"], "title": t, "fullName": "calc " + t, "status": s,
                "failureMessages": ["Error: expect(received).toBe(expected)"] if s == "failed" else []} for t, s in tests]
    count = lambda wanted: sum(1 for _, s in tests if s == wanted)
    files = [{"name": FILE, "status": "failed" if count("failed") else "passed", "message": "", "assertionResults": results}]
    files += list(extra_files)
    if "--json" in sys.argv:
        print(prefix + json.dumps({"numFailedTests": count("failed"), "numPassedTests": count("passed"),
            "numPendingTests": count("pending"), "numTodoTests": 0, "numTotalTests": len(tests),
            "numRuntimeErrorTestSuites": len(extra_files), "success": success, "wasInterrupted": False,
            "testResults": files}))
    else:
        for t, s in tests:
            if s == "failed":
                print("FAIL src/calc.test.js\n  ● calc › %s\n" % t, file=sys.stderr)
        print("Tests:       %d failed, %d passed, %d total" % (count("failed"), count("passed"), len(tests)), file=sys.stderr)
    sys.exit(code)
broken = {"name": "/repo/src/broken.test.js", "status": "failed", "assertionResults": [],
          "message": "  ● Test suite failed to run\n\n    Cannot find module './nope'"}
states = {
    "baseline": ([("adds", "failed"), ("zero", "passed")], False, 1),
    "fixed": ([("adds", "passed"), ("zero", "passed")], True, 0),
    "deleted": ([("zero", "passed")], True, 0),
    "ignored": ([("adds", "pending"), ("zero", "passed")], True, 0),
    "swapped": ([("other", "passed"), ("zero", "passed")], True, 0),
    "lying": ([("adds", "failed"), ("zero", "passed")], True, 0),
    "loadfail": ([("adds", "passed"), ("zero", "passed")], False, 1, [broken]),
    "embedded": ([("adds", "passed"), ("zero", "passed")], True, 0, (), "console.log from a test\n"),
    "bail-other": ([("aaa", "failed")], False, 1),
}
if state == "cut":
    print('{"numFailedTests": 0, "numPassedTests": 2, "testResults": [{"name": "/repo/src/calc.te')
    sys.exit(0)
report(*states[state])
"#;

/// #302: each new runner proves a real fix by name, and every way of making
/// the output clean without fixing the code stays unproven.
fn runner_matrix(tool: &Tool, argv: &[&str], fixed_name: &str, refusals: &[(&str, &str)]) {
    let (_store, delta) = scenario(tool, argv, "fixed");
    assert_real_fix_is_provable(&delta);
    let store = tempfile::tempdir().unwrap();
    let dir = store.path().to_str().unwrap();
    tool.set_state("baseline");
    run(dir, argv);
    tool.set_state("fixed");
    run(dir, argv);
    let latest = receipt(dir, None);
    assert_eq!(latest["verdict"], "fixed", "{latest:#}");
    let text = latest["text"].as_str().unwrap();
    assert!(text.contains(&format!("fixed: {fixed_name}")), "{text}");
    for (state, reason) in refusals {
        let (_store, delta) = scenario(tool, argv, state);
        assert_eq!(
            delta["assessment"]["can_prove_absence"], false,
            "{state}: {delta:#}"
        );
        assert_eq!(count(&delta, "resolved"), 0, "{state}: {delta:#}");
        assert!(
            reasons(&delta).contains(reason),
            "{state}: missing reason {reason:?}: {}",
            reasons(&delta)
        );
    }
}

#[test]
fn unittest_proves_a_real_fix_and_refuses_every_false_one() {
    let python = Tool::new("python3", FAKE_UNITTEST);
    let argv = [python.executable.as_str(), "-m", "unittest", "-v"];
    runner_matrix(
        &python,
        &argv,
        "test_calc.TestCalc.test_add",
        &[
            ("deleted", "test population shrank"),
            ("ignored", "test_calc.TestCalc.test_add (skipped)"),
            ("swapped", "test_calc.TestCalc.test_add (missing)"),
            ("lying", "disagrees with the observed process exit"),
            ("cut", "one unambiguous completion summary"),
            ("loadfail", "could not import or set up some tests"),
            ("failfast-other", "test population shrank"),
        ],
    );
    // Quiet output and a filtered run never prove a fix.
    let quiet = [python.executable.as_str(), "-m", "unittest"];
    let (_store, delta) = scenario(&python, &quiet, "fixed");
    assert_fake_fix_is_not_provable(&delta, "python -m unittest -v");
    let filtered = [
        python.executable.as_str(),
        "-m",
        "unittest",
        "-v",
        "-k",
        "test_add",
    ];
    let (_store, delta) = scenario(&python, &filtered, "fixed");
    assert_fake_fix_is_not_provable(&delta, "-k runs a subset");
}

#[test]
fn go_test_proves_a_real_fix_and_refuses_every_false_one() {
    let go = Tool::new("go", FAKE_GO);
    let refusals = [
        ("deleted", "test population shrank"),
        ("ignored", "example.com/m/calc.TestAdd (skipped)"),
        ("swapped", "example.com/m/calc.TestAdd (missing)"),
        ("lying", "despite a successful process exit"),
        ("cut", "ended before every package"),
        ("buildfail", "could not build or set up"),
    ];
    for argv in [
        [go.executable.as_str(), "test", "-v", "./..."],
        [go.executable.as_str(), "test", "-json", "./..."],
    ] {
        runner_matrix(&go, &argv, "example.com/m/calc.TestAdd", &refusals);
    }
    let quiet = [go.executable.as_str(), "test", "./..."];
    let (_store, delta) = scenario(&go, &quiet, "fixed");
    assert_fake_fix_is_not_provable(&delta, "rerun with -v");
    let filtered = [
        go.executable.as_str(),
        "test",
        "-v",
        "-run",
        "TestAdd",
        "./...",
    ];
    let (_store, delta) = scenario(&go, &filtered, "fixed");
    assert_fake_fix_is_not_provable(&delta, "-run does not run every selected test");
    // A fail-fast rerun that stopped at another failure never ran TestAdd.
    let failfast = [go.executable.as_str(), "test", "-v", "-failfast", "./..."];
    let (_store, delta) = scenario(&go, &failfast, "failfast-other");
    assert_fake_fix_is_not_provable(&delta, "-failfast stops at the first failure");
}

#[test]
fn jest_json_proves_a_real_fix_and_refuses_every_false_one() {
    let jest = Tool::new("jest", FAKE_JEST);
    let argv = [jest.executable.as_str(), "--json"];
    runner_matrix(
        &jest,
        &argv,
        "/repo/src/calc.test.js > calc > adds",
        &[
            ("deleted", "test population shrank"),
            ("ignored", "/repo/src/calc.test.js > calc > adds (skipped)"),
            ("swapped", "/repo/src/calc.test.js > calc > adds (missing)"),
            ("lying", "reports success despite failing tests"),
            ("cut", "summary was not observed"),
            ("loadfail", "failed to load or run"),
            ("embedded", "shares stdout with other output"),
        ],
    );
    // Jest's agent-mode text names only failures.
    let text = [jest.executable.as_str()];
    let (_store, delta) = scenario(&jest, &text, "fixed");
    assert_fake_fix_is_not_provable(&delta, "rerun with `--json`");
    let filtered = [jest.executable.as_str(), "--json", "-t", "adds"];
    let (_store, delta) = scenario(&jest, &filtered, "fixed");
    assert_fake_fix_is_not_provable(&delta, "-t runs a subset");
    // A `--bail` rerun that stopped at another failing file.
    let bail = [jest.executable.as_str(), "--json", "--bail"];
    let (_store, delta) = scenario(&jest, &bail, "bail-other");
    assert_fake_fix_is_not_provable(&delta, "--bail stops after the first failing test file");
}

/// #299: `pytest -x` stops only at a failure, so a green `-x` rerun covered
/// every test and proves the fix; a swapped test still never does.
#[test]
fn pytest_exitfirst_failure_is_proven_fixed_by_a_green_rerun() {
    let pytest = Tool::new("pytest", FAKE_PYTEST);
    let argv = [pytest.executable.as_str(), "-x", "-v"];
    let (_store, delta) = scenario(&pytest, &argv, "fixed");
    assert_real_fix_is_provable(&delta);
    let (_store, delta) = scenario(&pytest, &argv, "swapped");
    assert_fake_fix_is_not_provable(&delta, "tests/test_math.py::test_add (missing)");
    let argv = [pytest.executable.as_str(), "--lf", "-v"];
    let (_store, delta) = scenario(&pytest, &argv, "fixed");
    assert_fake_fix_is_not_provable(&delta, "selection coverage is unknown or not exhaustive");
}

/// Replays `tests/fixtures/unreached_skips/<state>/{stdout,stderr,exit_code}`,
/// where `<state>` is the content of the `state` file beside the tool and the
/// fixture root is in the `root` file. Each
/// fixture is the real output of one run (cargo 1.98, Go 1.27, Python 3.15,
/// pytest 9.1), with paths rewritten to `/repo`.
const REPLAY: &str = r#"#!/usr/bin/env python3
import sys
from pathlib import Path
state = Path(__file__).with_name("state").read_text().strip()
fixture = Path(Path(__file__).with_name("root").read_text().strip()) / state
sys.stdout.write((fixture / "stdout").read_text())
sys.stdout.flush()
sys.stderr.write((fixture / "stderr").read_text())
sys.exit(int((fixture / "exit_code").read_text().strip()))
"#;

fn unreached_skip_fixtures() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/unreached_skips")
        .to_str()
        .unwrap()
        .to_string()
}

/// Run `baseline` then `subject` through a replaying `program` with the same
/// argv; return the delta and the subject's receipt.
fn replayed(program: &str, args: &[&str], baseline: &str, subject: &str) -> (Value, Value) {
    let tool = Tool::new(program, REPLAY);
    fs::write(
        Path::new(&tool.executable).with_file_name("root"),
        unreached_skip_fixtures(),
    )
    .unwrap();
    let mut argv = vec![tool.executable.as_str()];
    argv.extend_from_slice(args);
    let store = tempfile::tempdir().unwrap();
    let dir = store.path().to_str().unwrap();
    tool.set_state(baseline);
    let first = run(dir, &argv);
    tool.set_state(subject);
    let second = run(dir, &argv);
    let delta = delta(dir, &observation_id(&first), &observation_id(&second));
    let receipt = receipt(dir, Some(&observation_id(&second)));
    (delta, receipt)
}

/// #308: a failing run that stopped early (fail-fast) or never built names
/// every test it ran. A test the rerun skips that the baseline never reached
/// cannot hide a baseline failure, so a real fix is proven.
#[test]
fn a_fix_is_provable_when_the_rerun_skips_a_test_the_baseline_never_reached() {
    for (program, args, scenario) in [
        // Cargo stops after the failing lib harness; tests/integration.rs
        // with its #[ignore] test never started.
        ("cargo", &["test"][..], "cargo-failfast"),
        // go test -failfast stops after TestAdd; TestNet (t.Skip) never ran.
        (
            "go",
            &["test", "-v", "-failfast", "./..."][..],
            "go-failfast",
        ),
        (
            "go",
            &["test", "-json", "-failfast", "./..."][..],
            "go-json-failfast",
        ),
        // Go says the package never built, so none of its tests ran.
        ("go", &["test", "-v", "./..."][..], "go-buildfail"),
        (
            "python3",
            &["-m", "unittest", "-v", "-f"][..],
            "unittest-failfast",
        ),
        (
            "pytest",
            &["-v", "-x", "-p", "no:cacheprovider"][..],
            "pytest-x",
        ),
    ] {
        let (delta, receipt) = replayed(
            program,
            args,
            &format!("{scenario}-baseline"),
            &format!("{scenario}-fixed"),
        );
        assert_real_fix_is_provable(&delta);
        assert!(
            reasons(&delta).contains("the baseline never reached"),
            "{scenario}: {}",
            reasons(&delta)
        );
        assert_eq!(receipt["verdict"], "fixed", "{scenario}: {receipt:#}");
    }
}

/// #308 counterexamples: every case where a skipped test could hide a
/// baseline failure still refuses.
#[test]
fn skips_the_baseline_may_have_run_never_prove_a_fix() {
    for (program, args, baseline, subject, why) in [
        // Quiet output names no passing test, so an unnamed test may have run.
        (
            "cargo",
            &["test", "-q"][..],
            "cargo-failfast-quiet-baseline",
            "cargo-failfast-quiet-fixed",
            "quiet baseline",
        ),
        // slow_network ran (and passed) in the baseline, then was ignored.
        (
            "cargo",
            &["test", "--no-fail-fast"][..],
            "cargo-ran-baseline",
            "cargo-ran-fixed",
            "test ran in the baseline",
        ),
        // The test binary aborted before printing any test: the crashing test
        // is unnamed, and ignoring it must not count as a fix.
        (
            "cargo",
            &["test"][..],
            "cargo-crash-baseline",
            "cargo-crash-ignored",
            "crash before output",
        ),
        // Under --no-fail-fast tests/a.rs aborted silently while the other
        // harnesses finished; its crashing test is then ignored.
        (
            "cargo",
            &["test", "--no-fail-fast"][..],
            "cargo-nff-crash-baseline",
            "cargo-nff-crash-ignored",
            "silently crashed harness",
        ),
    ] {
        let (delta, receipt) = replayed(program, args, baseline, subject);
        assert_eq!(
            delta["assessment"]["can_prove_absence"], false,
            "{why}: {delta:#}"
        );
        assert_eq!(count(&delta, "resolved"), 0, "{why}: {delta:#}");
        assert_eq!(receipt["verdict"], "not_proven", "{why}: {receipt:#}");
    }
}


/// #312: `uv run pytest` is judged as the pytest run it is. uv's own
/// environment lines on stderr change nothing, every false fix still
/// refuses, and an option whose arity prog does not know is never guessed.
#[test]
fn uv_run_pytest_is_judged_as_pytest_and_refuses_every_false_fix() {
    let script = FAKE_PYTEST.replacen(
        "import sys\n",
        "import sys\nprint('Installed 5 packages in 41ms', file=sys.stderr)\n",
        1,
    );
    let uv = Tool::new("uv", &script);
    let uv_path = uv.executable.as_str();
    for argv in [
        &[uv_path, "run", "pytest", "-v"][..],
        &[
            uv_path,
            "run",
            "--frozen",
            "--with",
            "pytest-xdist",
            "pytest",
            "-v",
        ],
        &[uv_path, "run", "-m", "pytest", "-v"],
    ] {
        let (_store, delta) = scenario(&uv, argv, "fixed");
        assert_real_fix_is_provable(&delta);
        for (state, reason) in [
            ("deleted", "test population shrank"),
            ("ignored", "tests/test_math.py::test_add (skipped)"),
            ("swapped", "tests/test_math.py::test_add (missing)"),
        ] {
            let (_store, delta) = scenario(&uv, argv, state);
            assert_fake_fix_is_not_provable(&delta, reason);
        }
    }
    // Quiet pytest under uv names no passing test, exactly as without uv.
    let (_store, delta) = scenario(&uv, &[uv_path, "run", "pytest", "-q"], "fixed");
    assert_fake_fix_is_not_provable(&delta, "tests/test_math.py::test_add cannot be shown");
    // An unknown option might take a value: the run stays unjudged.
    let unknown = [uv_path, "run", "--future-option", "pytest", "-v"];
    let store = tempfile::tempdir().unwrap();
    let dir = store.path().to_str().unwrap();
    uv.set_state("baseline");
    let envelope = run(dir, &unknown);
    assert!(
        envelope.pointer("/data_preview/provider").is_none()
            || envelope
                .pointer("/data_preview/provider")
                .unwrap()
                .is_null(),
        "{envelope:#}"
    );
    let (_store, delta) = scenario(&uv, &unknown, "fixed");
    assert_eq!(count(&delta, "resolved"), 0, "{delta:#}");
}

/// A fake `playwright` (#312). `--reporter=json` prints the JSON report shape
/// of `@playwright/test` 1.65.0; anything else prints the default `list`
/// reporter's summary, which never names every passing test.
const FAKE_PLAYWRIGHT: &str = r##"#!/usr/bin/env python3
import json, sys
from pathlib import Path
state = Path(__file__).with_name("state").read_text().strip()
as_json = "--reporter=json" in sys.argv
def report(tests, code, errors=()):
    stats = {"expected": 0, "unexpected": 0, "flaky": 0, "skipped": 0}
    specs = []
    for title, status in tests:
        expected_status, results = "passed", [{"status": "passed"}]
        if status == "failed":
            raw, results = "unexpected", [{"status": "failed", "errors": [{"message": "Error: expect(received).toBe(expected)"}]}]
        elif status == "skipped":
            raw, expected_status, results = "skipped", "skipped", [{"status": "skipped"}]
        elif status == "xfail":
            raw, expected_status, results = "expected", "failed", [{"status": "failed"}]
        elif status == "flaky":
            raw, results = "flaky", [{"status": "failed"}, {"status": "passed"}]
        elif status == "notrun":
            raw, results = "skipped", []
        else:
            raw = "expected"
        stats[raw] += 1
        specs.append({"title": title, "file": "calc.spec.js", "line": 4, "column": 3,
                      "tests": [{"projectName": "", "expectedStatus": expected_status, "status": raw, "results": results}]})
    if as_json:
        print(json.dumps({"config": {}, "suites": [{"title": "calc.spec.js", "file": "calc.spec.js", "specs": [],
                          "suites": [{"title": "calc", "file": "calc.spec.js", "specs": specs, "suites": []}]}],
                          "errors": list(errors), "stats": stats}, indent=2))
    else:
        print("\nRunning %d tests using 1 worker\n" % len(tests))
        failed = [t for t, s in tests if s == "failed"]
        for i, t in enumerate(failed):
            print("  %d) e2e/calc.spec.js:4:3 › calc › %s ───\n\n    Error: expect(received).toBe(expected)\n" % (i + 1, t))
        if failed:
            print("  %d failed" % len(failed))
            for t in failed:
                print("    e2e/calc.spec.js:4:3 › calc › %s ───" % t)
        skipped = sum(1 for _, s in tests if s in ("skipped", "notrun"))
        if skipped:
            print("  %d skipped" % skipped)
        print("  %d passed (0.5s)" % sum(1 for _, s in tests if s in ("passed", "xfail", "flaky")))
    sys.exit(code)
states = {
    "baseline": ([("adds", "failed"), ("muls", "passed")], 1),
    "fixed": ([("adds", "passed"), ("muls", "passed")], 0),
    "deleted": ([("muls", "passed")], 0),
    "ignored": ([("adds", "skipped"), ("muls", "passed")], 0),
    "expected-fail": ([("adds", "xfail"), ("muls", "passed")], 0),
    "swapped": ([("other", "passed"), ("muls", "passed")], 0),
    "lying": ([("adds", "failed"), ("muls", "passed")], 0),
    "flaky": ([("adds", "flaky"), ("muls", "passed")], 0),
    "stopped": ([("adds", "passed"), ("muls", "notrun")], 0),
    "loadfail": ([("adds", "passed"), ("muls", "passed")], 1,
                 [{"message": "SyntaxError: broken.spec.js: Missing semicolon. (1:6)", "location": {"file": "/repo/e2e/broken.spec.js"}}]),
}
if state == "cut":
    print('{"config": {}, "suites": [{"title": "calc.spec.js", "specs": [{"title": "adds", "tests": [{"stat')
    sys.exit(0)
report(*states[state])
"##;

#[test]
fn playwright_json_proves_a_real_fix_and_refuses_every_false_one() {
    let playwright = Tool::new("playwright", FAKE_PLAYWRIGHT);
    let argv = [playwright.executable.as_str(), "test", "--reporter=json"];
    runner_matrix(
        &playwright,
        &argv,
        "calc.spec.js > calc > adds",
        &[
            ("deleted", "test population shrank"),
            ("ignored", "calc.spec.js > calc > adds (skipped)"),
            // `test.fail()` that fails as expected never ran the assertion.
            ("expected-fail", "calc.spec.js > calc > adds (skipped)"),
            ("swapped", "calc.spec.js > calc > adds (missing)"),
            ("lying", "disagrees with the observed process exit"),
            ("flaky", "passed only on retry"),
            ("stopped", "some tests did not run"),
            ("loadfail", "failed outside any test"),
            ("cut", "summary was not observed"),
        ],
    );
    // The text reporters never name every passing test.
    let text = [playwright.executable.as_str(), "test"];
    let (_store, delta) = scenario(&playwright, &text, "fixed");
    assert_fake_fix_is_not_provable(&delta, "rerun with `--reporter=json`");
    for (filter, reason) in [
        (&["--grep", "adds"][..], "--grep runs a subset"),
        (&["--project=chromium"], "--project runs a subset"),
        (&["e2e/calc.spec.js:4"], "file:line runs a subset"),
        (&["--last-failed"], "--last-failed runs a subset"),
    ] {
        let mut argv = vec![playwright.executable.as_str(), "test", "--reporter=json"];
        argv.extend_from_slice(filter);
        let (_store, delta) = scenario(&playwright, &argv, "fixed");
        assert_fake_fix_is_not_provable(&delta, reason);
    }
}

/// A fake `bun` for `bun test` (#312): Bun 1.3's output in an agent session,
/// which names failing tests only.
const FAKE_BUN: &str = r##"#!/usr/bin/env python3
import sys
from pathlib import Path
state = Path(__file__).with_name("state").read_text().strip()
def report(tests, code, summary=True):
    print("bun test v1.3.14 (0d9b296a)")
    print("\ntests/calc.test.js:", file=sys.stderr)
    for t, s in tests:
        if s == "fail":
            print("error: expect(received).toBe(expected)\n\nExpected: 5\nReceived: -1\n", file=sys.stderr)
            print("(fail) calc > %s [0.16ms]" % t, file=sys.stderr)
    if summary:
        print("", file=sys.stderr)
        for label in ("pass", "skip", "fail"):
            print(" %d %s" % (sum(1 for _, s in tests if s == label), label), file=sys.stderr)
    print("Ran %d tests across 1 file. [6.00ms]" % len(tests), file=sys.stderr)
    sys.exit(code)
states = {
    "baseline": ([("adds", "fail"), ("muls", "pass")], 1),
    "fixed": ([("adds", "pass"), ("muls", "pass")], 0),
    "deleted": ([("muls", "pass")], 0),
    "lying": ([("adds", "fail"), ("muls", "pass")], 0),
    "bailed": ([("adds", "fail")], 1, False),
}
report(*states[state])
"##;

#[test]
fn bun_test_names_failures_but_never_proves_a_fix() {
    let bun = Tool::new("bun", FAKE_BUN);
    let argv = [bun.executable.as_str(), "test"];
    let store = tempfile::tempdir().unwrap();
    let dir = store.path().to_str().unwrap();
    bun.set_state("baseline");
    run(dir, &argv);
    let failing = receipt(dir, None);
    assert_eq!(failing["verdict"], "failing", "{failing:#}");
    let text = failing["text"].as_str().unwrap();
    assert!(
        text.contains("1 passed, 1 failed: tests/calc.test.js > calc > adds"),
        "{text}"
    );
    assert!(
        text.contains("first failure: tests/calc.test.js > calc > adds: expect(received)"),
        "{text}"
    );
    for (state, reason) in [
        ("fixed", "cannot prove a fix"),
        ("deleted", "test population shrank"),
        ("lying", "disagrees with the observed process exit"),
    ] {
        let (_store, delta) = scenario(&bun, &argv, state);
        assert_fake_fix_is_not_provable(&delta, reason);
    }
}

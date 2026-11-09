//! The stop gate's decisions on the sequences that made it block without
//! cause in a replay of real Claude Code and Codex sessions (#311), driven the
//! way Claude Code drives the hooks: hook-input JSON on stdin, Bash commands
//! that source the session's `CLAUDE_ENV_FILE`, and real PATH shims in front
//! of a scripted `cargo`.

use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

mod support;

use support::test_git_repo;

/// A scripted `cargo`: `test` prints libtest output for the scenario in the
/// `state` file; `build`, `check`, and `clippy` print a success or a compile
/// error per the `build` file.
const FAKE_CARGO: &str = r#"#!/usr/bin/env python3
import sys
from pathlib import Path
here = Path(__file__).parent
sub = next((arg for arg in sys.argv[1:] if not arg.startswith("+")), "")
if sub in ("build", "check", "clippy"):
    if (here / "build").read_text().strip() == "error":
        print("    Checking a v0.1.0 (/tmp/a)", file=sys.stderr)
        print("error: expected expression, found `}`", file=sys.stderr)
        print(" --> src/lib.rs:3:1", file=sys.stderr)
        print("\nerror: could not compile `a` (lib) due to 1 previous error", file=sys.stderr)
        sys.exit(101)
    print("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.01s", file=sys.stderr)
    sys.exit(0)
state = (here / "state").read_text().strip()
tests = {
    "baseline": [("t::adds", "FAILED"), ("t::muls", "ok")],
    "fixed": [("t::adds", "ok"), ("t::muls", "ok")],
    "deleted": [("t::muls", "ok")],
}[state]
print("running %d tests" % len(tests))
for name, status in tests:
    print("test %s ... %s" % (name, status))
failed = sum(1 for _, status in tests if status == "FAILED")
if failed:
    print("\nthread 't::adds' panicked at src/lib.rs:6:25:\nassertion failed\n")
print("test result: %s. %d passed; %d failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
      % ("FAILED" if failed else "ok", len(tests) - failed, failed))
sys.exit(101 if failed else 0)
"#;

struct Session {
    _root: tempfile::TempDir,
    root: PathBuf,
    project: tempfile::TempDir,
    tools: PathBuf,
    env_file: PathBuf,
}

impl Session {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_path_buf();
        let tools = root_path.join("tools");
        fs::create_dir_all(&tools).unwrap();
        let cargo = tools.join("cargo");
        fs::write(&cargo, FAKE_CARGO).unwrap();
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(tools.join("build"), "ok").unwrap();
        let project = test_git_repo();
        fs::write(project.path().join("lib.rs"), "fn main() {}\n").unwrap();
        fs::write(project.path().join("README.md"), "# a\n").unwrap();
        git(project.path(), &["add", "lib.rs", "README.md"]);
        commit(project.path(), "init");
        let session = Self {
            env_file: root_path.join("claude-env.sh"),
            _root: root,
            root: root_path,
            project,
            tools,
        };
        let output = session.hook(
            json!({"hook_event_name": "SessionStart", "session_id": "s1"}),
            false,
        );
        assert_eq!(output, "", "SessionStart prints nothing");
        session
    }

    fn hook(&self, mut input: Value, stop_gate: bool) -> String {
        input["cwd"] = json!(self.project.path());
        let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
        command
            .args(["harness", "hook", "claude-code"])
            .current_dir(self.project.path())
            .env_remove("PROG_DIR")
            .env_remove("PROG_FORMAT")
            .env_remove("PROG_STOP_GATE")
            .env("CLAUDE_PROJECT_DIR", self.project.path())
            .env("CLAUDE_ENV_FILE", &self.env_file)
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if stop_gate {
            command.env("CLAUDE_PLUGIN_OPTION_STOP_GATE", "true");
        }
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "hooks always exit 0");
        String::from_utf8(output.stdout).unwrap()
    }

    /// What Claude's Bash tool does: run a shell command in the project with
    /// the session exports applied. `state` picks the scripted test results.
    fn bash(&self, script: &str, state: &str) {
        fs::write(self.tools.join("state"), state).unwrap();
        let output = Command::new("bash")
            .arg("-c")
            .arg(format!(". '{}'; {script}", self.env_file.display()))
            .current_dir(self.project.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.tools.display()))
            .env_remove("PROG_DIR")
            .env("XDG_STATE_HOME", self.root.join("state"))
            .output()
            .unwrap();
        assert!(
            output.status.code().is_some(),
            "{script} was killed: {output:?}"
        );
    }

    fn set_build(&self, result: &str) {
        fs::write(self.tools.join("build"), result).unwrap();
    }

    /// The PostToolUse receipt for the last command, if any is pending.
    fn receipt(&self) -> Option<String> {
        let output = self.hook(
            json!({
                "hook_event_name": "PostToolUse",
                "session_id": "s1",
                "tool_name": "Bash",
                "tool_input": {"command": "cargo test"}
            }),
            false,
        );
        (!output.trim().is_empty()).then(|| {
            let value: Value = serde_json::from_str(&output).unwrap();
            value["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .to_string()
        })
    }

    fn stop_raw(&self, message: Option<&str>, stop_hook_active: bool) -> Option<Value> {
        let mut input = json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
            "stop_hook_active": stop_hook_active
        });
        if let Some(message) = message {
            input["last_assistant_message"] = json!(message);
        }
        let output = self.hook(input, true);
        (!output.trim().is_empty()).then(|| serde_json::from_str(&output).unwrap())
    }

    fn stop_with(&self, message: Option<&str>) -> Option<String> {
        self.stop_raw(message, false).map(|value| {
            assert_eq!(value["decision"], "block", "{value:#}");
            value["reason"].as_str().unwrap().to_string()
        })
    }

    /// The PostToolUse output for an arbitrary command (no shim involved).
    fn post_tool_use(&self, command: &str) -> Option<String> {
        let output = self.hook(
            json!({
                "hook_event_name": "PostToolUse",
                "session_id": "s1",
                "tool_name": "Bash",
                "tool_input": {"command": command}
            }),
            false,
        );
        (!output.trim().is_empty()).then(|| {
            let value: Value = serde_json::from_str(&output).unwrap();
            value["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .to_string()
        })
    }

    fn stop(&self) -> Option<String> {
        self.stop_with(None)
    }

    fn write(&self, path: &str, contents: &str) {
        fs::write(self.project.path().join(path), contents).unwrap();
    }

    fn git(&self, args: &[&str]) {
        git(self.project.path(), args);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit(dir: &Path, message: &str) {
    git(
        dir,
        &[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            message,
        ],
    );
}

/// A failing narrow run (`cargo test -p a adds`) followed by a passing
/// broader run (`cargo test`) is two invocation chains. Only the newest test
/// run says where things stand, so the narrow failure must not block every
/// later turn end.
#[test]
fn a_later_broader_pass_supersedes_an_older_narrow_failure() {
    let session = Session::start();
    session.bash("cargo test -p a adds 2>&1 | tail -1", "baseline");
    let blocked = session.stop().unwrap();
    assert!(blocked.contains("latest test run is failing"), "{blocked}");

    session.bash("cargo test 2>&1 | tail -1", "fixed");
    assert_eq!(session.stop(), None);
    assert_eq!(session.stop(), None, "and at every later turn end");
}

/// "Files changed but no test run was captured" misfired whenever tests ran
/// outside the shims, and made agents claim a project had no tests.
#[test]
fn missing_evidence_never_blocks() {
    let session = Session::start();
    session.write("README.md", "# a, with the typo fixed\n");
    session.write("notes.txt", "a new untracked file\n");
    session.write("lib.rs", "fn main() { 1; }\n");
    assert_eq!(session.stop(), None);
    assert_eq!(session.stop(), None);
}

/// Staging and committing what was tested change no file's content.
#[test]
fn staging_and_committing_after_a_pass_do_not_block() {
    let session = Session::start();
    session.write("lib.rs", "fn main() { 2; }\n");
    session.bash("cargo test", "fixed");
    assert_eq!(session.stop(), None);

    session.git(&["add", "lib.rs"]);
    assert_eq!(session.stop(), None, "git add");
    commit(session.project.path(), "tested change");
    assert_eq!(session.stop(), None, "git commit");
    git(
        session.project.path(),
        &[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-qm",
            "empty",
        ],
    );
    assert_eq!(session.stop(), None, "git commit --allow-empty");
    session.git(&["checkout", "-qb", "topic"]);
    assert_eq!(session.stop(), None, "git checkout -b");

    // A real content change after the pass still blocks.
    session.write("lib.rs", "fn main() { 3; }\n");
    let stale = session.stop().unwrap();
    assert!(
        stale.contains("files changed after the last passing test run"),
        "{stale}"
    );
}

/// Content that changed after the tested state blocks however it changed:
/// a stash that drops the tested edit, or a checkout of other content.
#[test]
fn content_changes_through_git_still_block() {
    let session = Session::start();
    session.write("lib.rs", "fn main() { 4; }\n");
    session.bash("cargo test", "fixed");
    assert_eq!(session.stop(), None);
    session.git(&["stash", "-q"]);
    let stale = session.stop().unwrap();
    assert!(
        stale.contains("files changed after the last passing test run"),
        "{stale}"
    );

    let session = Session::start();
    session.git(&["checkout", "-qb", "other"]);
    session.write("lib.rs", "fn main() { 5; }\n");
    session.git(&["add", "lib.rs"]);
    commit(session.project.path(), "other content");
    session.bash("cargo test", "fixed");
    assert_eq!(session.stop(), None);
    session.git(&["checkout", "-q", "-"]);
    let stale = session.stop().unwrap();
    assert!(
        stale.contains("files changed after the last passing test run"),
        "{stale}"
    );
}

/// A passing `cargo check` is no evidence that tests pass.
#[test]
fn a_build_after_an_edit_is_not_test_evidence() {
    let session = Session::start();
    session.bash("cargo test", "fixed");
    session.write("lib.rs", "fn main() { 6; }\n");
    session.bash("cargo check", "fixed");
    let stale = session.stop().unwrap();
    assert!(
        stale.contains("files changed after the last passing test run"),
        "{stale}"
    );
}

/// A failing lint run blocks with its own wording until a newer lint run
/// passes; a test run does not supersede it.
#[test]
fn a_failing_lint_run_blocks_as_a_lint_failure() {
    let session = Session::start();
    session.set_build("error");
    session.bash("cargo clippy -- -D warnings", "fixed");
    let blocked = session.stop().unwrap();
    assert!(blocked.contains("latest lint run is failing"), "{blocked}");
    assert!(!blocked.contains("test run"), "{blocked}");

    session.bash("cargo test", "fixed");
    let blocked = session.stop().unwrap();
    assert!(blocked.contains("latest lint run is failing"), "{blocked}");

    session.set_build("ok");
    session.bash("cargo clippy -- -D warnings", "fixed");
    assert_eq!(session.stop(), None);
}

/// A failing build is superseded by a newer passing test run, which compiles
/// the same code.
#[test]
fn a_failing_build_blocks_until_a_newer_build_or_test_run_passes() {
    let session = Session::start();
    session.set_build("error");
    session.bash("cargo build", "fixed");
    let blocked = session.stop().unwrap();
    assert!(blocked.contains("latest build run is failing"), "{blocked}");
    session.set_build("ok");
    session.bash("cargo test", "fixed");
    assert_eq!(session.stop(), None);
}

/// The user has been told: the gate stands aside. A claim of success over a
/// failing run is still blocked.
#[test]
fn an_honest_final_message_is_not_blocked() {
    let session = Session::start();
    session.bash("cargo test", "baseline");
    for honest in [
        "The tests are still failing: add() returns a - b. I have not fixed it because you asked only for an explanation.",
        "Tests are still failing (1 failed: t::adds); not fixed per your request.",
        "I didn't run the tests after the last edit.",
        "prog says the fix is NOT PROVEN, because a test was skipped.",
    ] {
        assert_eq!(session.stop_with(Some(honest)), None, "{honest}");
    }
    for claim in [
        "All tests pass.",
        "The tests failed before the fix and now pass.",
        "No tests are failing.",
        "I did not run into any problems; everything is green.",
    ] {
        let blocked = session
            .stop_with(Some(claim))
            .unwrap_or_else(|| panic!("{claim:?} must not count as a disclosure"));
        assert!(blocked.contains("latest test run is failing"), "{blocked}");
    }
}

/// Rerunning the same command after `NOT PROVEN` must not forget the earlier
/// failure: the rerun is compared with that failure, not with the unproven
/// run just before it. A rerun that proves the fix clears it.
#[test]
fn not_proven_survives_an_identical_rerun_until_a_run_proves_the_fix() {
    let session = Session::start();
    session.bash("cargo test", "baseline");
    session.receipt();
    session.bash("cargo test", "deleted");
    let receipt = session.receipt().unwrap();
    assert!(receipt.contains("vs previous run: NOT PROVEN"), "{receipt}");
    let blocked = session.stop().unwrap();
    assert!(blocked.contains("does not prove"), "{blocked}");

    session.bash("cargo test", "deleted");
    let receipt = session.receipt().unwrap();
    assert!(
        receipt.contains("vs earlier failing run: NOT PROVEN"),
        "{receipt}"
    );
    let blocked = session.stop().unwrap();
    assert!(blocked.contains("does not prove"), "{blocked}");

    session.bash("cargo test", "fixed");
    let receipt = session.receipt().unwrap();
    assert!(receipt.contains("fixed"), "{receipt}");
    assert_eq!(session.stop(), None);

    // Once proven, a further identical rerun is simply clean.
    session.bash("cargo test", "fixed");
    let receipt = session.receipt().unwrap();
    assert!(!receipt.contains("NOT PROVEN"), "{receipt}");
    assert_eq!(session.stop(), None);
}

/// #305: a claim of success that the newest evidence contradicts is quoted in
/// the block; if the agent still claims it at the next stop, the user is
/// warned instead of the agent being blocked again.
#[test]
fn a_contradicted_claim_is_quoted_and_then_shown_to_the_user() {
    let session = Session::start();
    session.bash("cargo test", "baseline");
    let blocked = session.stop_with(Some("Done. All tests pass.")).unwrap();
    assert!(
        blocked.starts_with(
            "prog stop gate: you said \"All tests pass\", but the latest test run is failing"
        ),
        "{blocked}"
    );
    let warning = session
        .stop_raw(Some("All tests pass."), true)
        .expect("a second stop that repeats the claim warns the user");
    assert!(warning.get("decision").is_none(), "{warning:#}");
    let text = warning["systemMessage"].as_str().unwrap();
    assert!(
        text.starts_with("prog: Claude said \"All tests pass\", but the latest test run is failing (test_failing)."),
        "{text}"
    );
    assert!(text.contains("1 failed: t::adds"), "{text}");

    // No claim, or an honest message, at the second stop: no warning.
    assert_eq!(session.stop_raw(Some("I could not fix it."), true), None);
    assert_eq!(session.stop_raw(None, true), None);
}

/// NOT PROVEN contradicts only a claim that the failure is fixed: "N passed"
/// is true of a run that could not prove the earlier failure gone.
#[test]
fn not_proven_contradicts_fixed_claims_but_not_pass_counts() {
    let session = Session::start();
    session.bash("cargo test", "baseline");
    session.bash("cargo test", "deleted");
    let blocked = session.stop_with(Some("1 passed, 0 failed.")).unwrap();
    assert!(
        blocked.starts_with("prog stop gate: the latest test run does not prove"),
        "{blocked}"
    );
    assert_eq!(session.stop_raw(Some("1 passed, 0 failed."), true), None);

    let blocked = session
        .stop_with(Some("The earlier failure is now proven fixed."))
        .unwrap();
    assert!(
        blocked.starts_with("prog stop gate: you said \"The earlier failure is now proven fixed\", but the latest test run does not prove"),
        "{blocked}"
    );
}

/// The false-alarm classes found in the #305 replay never quote a claim.
#[test]
fn replay_false_alarm_classes_are_not_contradictions() {
    let session = Session::start();
    session.bash("cargo test", "baseline");
    for message in [
        // CI claims: no local evidence.
        "PR #293 is open and all 8 CI checks pass.",
        // Tallies of other things, or with failures.
        "The 100-intent provider benchmark is still running: 28/34 passed, with six timeouts.",
        "Lint/build pass; full suite hit reported provider fixture failures.",
        // A deliberate red-on-base run, disclosed.
        "The new regression test fails on the original code, as expected; 309 tests passed elsewhere.",
        // Hedged.
        "Say go and I'll do the rewrite as a PR with the docs tests green.",
    ] {
        if let Some(reason) = session.stop_with(Some(message)) {
            assert!(!reason.contains("you said"), "{message} -> {reason}");
        }
        assert_eq!(session.stop_raw(Some(message), true), None, "{message}");
    }
}

/// A test command that ran outside the shims after the newest captured run
/// may have changed what stands: the gate stands aside, and the receipt slot
/// says that nothing was captured.
#[test]
fn an_uncaptured_test_run_supersedes_the_newest_captured_one() {
    let session = Session::start();
    session.bash("cargo test", "baseline");
    session.receipt();
    assert!(session.stop().is_some());
    // Agents run the venv's interpreter by path, past every PATH shim.
    let notice = session
        .post_tool_use(".venv/bin/python -m pytest -q 2>&1 | tail -3")
        .unwrap();
    assert_eq!(
        notice,
        "prog: no capture for this test command (`python -m pytest`): it ran outside prog's shims, or the run could not be recorded, so there is no verdict"
    );
    assert_eq!(session.stop(), None);
    assert_eq!(session.stop_raw(Some("All tests pass."), true), None);

    // A newer captured run decides again.
    std::thread::sleep(std::time::Duration::from_millis(20));
    session.bash("cargo test", "baseline");
    assert!(
        session
            .stop()
            .unwrap()
            .contains("latest test run is failing")
    );
}

#[test]
fn only_test_commands_get_the_uncaptured_notice() {
    let session = Session::start();
    for command in [
        "ls -la",
        "cat pytest.ini",
        "grep -rn vitest package.json",
        "cargo build",
        "git commit -m 'run the tests'",
        "npm install",
    ] {
        assert_eq!(session.post_tool_use(command), None, "{command}");
    }
    for (command, route) in [
        ("pytest -q", "pytest"),
        ("cd web && npx vitest run src", "npx vitest"),
        ("pnpm exec playwright test", "pnpm exec playwright"),
        ("uv run pytest tests/", "pytest"),
        ("PATH=/opt/node/bin:$PATH npm test -- a.test.ts", "npm test"),
        ("bun test", "bun test"),
        ("node --test test/", "node --test"),
        ("python3 -m unittest discover -v", "python3 -m unittest"),
        ("go test ./...", "go test"),
    ] {
        let notice = session
            .post_tool_use(command)
            .unwrap_or_else(|| panic!("{command}"));
        assert!(
            notice.contains(&format!("(`{route}`)")),
            "{command}: {notice}"
        );
    }
}

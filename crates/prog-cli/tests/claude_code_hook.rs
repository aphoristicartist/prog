//! Claude Code plugin hooks (#290), driven exactly as Claude Code drives them:
//! hook-input JSON on stdin, `CLAUDE_ENV_FILE` exports sourced by later Bash
//! commands, and hook-output JSON on stdout.

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

/// State-file driven libtest output for `cargo test --no-fail-fast`.
const FAKE_CARGO: &str = r#"#!/usr/bin/env python3
import sys
from pathlib import Path
state = Path(__file__).with_name("state").read_text().strip()
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
        let project = test_git_repo();
        fs::write(project.path().join("lib.rs"), "fn main() {}\n").unwrap();
        git(project.path(), &["add", "lib.rs"]);
        git(
            project.path(),
            &[
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "init",
            ],
        );
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

    fn hook_command(&self, stop_gate: bool) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
        command
            .args(["harness", "hook", "claude-code"])
            .current_dir(self.project.path())
            .env_remove("PROG_DIR")
            .env("CLAUDE_PROJECT_DIR", self.project.path())
            .env("CLAUDE_ENV_FILE", &self.env_file)
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if stop_gate {
            command.env("CLAUDE_PLUGIN_OPTION_STOP_GATE", "true");
        }
        command
    }

    fn hook(&self, mut input: Value, stop_gate: bool) -> String {
        input["cwd"] = json!(self.project.path());
        self.raw_hook(&input.to_string(), stop_gate)
    }

    fn raw_hook(&self, input: &str, stop_gate: bool) -> String {
        let mut child = self.hook_command(stop_gate).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "hooks always exit 0");
        String::from_utf8(output.stdout).unwrap()
    }

    /// What Claude's Bash tool does: run a shell command in the project with
    /// the session exports applied.
    fn bash(&self, script: &str, state: &str) -> String {
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
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn post_tool_use(&self, event: &str) -> Option<Value> {
        let output = self.hook(
            json!({
                "hook_event_name": event,
                "session_id": "s1",
                "tool_name": "Bash",
                "tool_input": {"command": "cargo test --no-fail-fast 2>&1 | tail -1"}
            }),
            false,
        );
        (!output.trim().is_empty()).then(|| serde_json::from_str(&output).unwrap())
    }

    fn stop(&self, stop_hook_active: bool) -> Option<Value> {
        let output = self.hook(
            json!({
                "hook_event_name": "Stop",
                "session_id": "s1",
                "stop_hook_active": stop_hook_active
            }),
            true,
        );
        (!output.trim().is_empty()).then(|| serde_json::from_str(&output).unwrap())
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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn session_start_activates_shims_with_a_store_outside_the_project() {
    let session = Session::start();
    let exports = fs::read_to_string(&session.env_file).unwrap();
    let shims = session.root.join("data/prog/shims");
    assert!(exports.contains(&shims.display().to_string()), "{exports}");
    assert!(exports.contains("export PROG_DIR="), "{exports}");
    assert!(shims.join("cargo").is_file());
    assert!(
        !exports.contains(&format!("PROG_DIR={}", session.project.path().display())),
        "the store must not live inside the project: {exports}"
    );
}

#[test]
fn receipts_reach_claude_after_a_tailed_failing_run_and_only_once() {
    let session = Session::start();
    let seen = session.bash("cargo test --no-fail-fast 2>&1 | tail -1", "baseline");
    assert!(seen.starts_with("test result: FAILED"), "{seen}");
    let output = session.post_tool_use("PostToolUseFailure").unwrap();
    assert_eq!(
        output["hookSpecificOutput"]["hookEventName"],
        "PostToolUseFailure"
    );
    let context = output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("1 passed, 1 failed: t::adds"), "{context}");
    assert!(
        context.contains("t::adds panicked at src/lib.rs:6:25"),
        "{context}"
    );
    assert!(context.len() <= 300, "{} bytes: {context}", context.len());
    assert_eq!(session.post_tool_use("PostToolUseFailure"), None);
}

#[test]
fn stop_gate_blocks_until_the_fix_is_proven_and_the_workspace_is_unchanged() {
    let session = Session::start();
    // No test run yet this session: nothing to gate.
    assert_eq!(session.stop(false), None);

    session.bash("cargo test --no-fail-fast 2>&1 | tail -1", "baseline");
    let blocked = session.stop(false).unwrap();
    assert_eq!(blocked["decision"], "block");
    assert!(
        blocked["reason"]
            .as_str()
            .unwrap()
            .contains("latest test run is failing"),
        "{blocked:#}"
    );
    // Blocks at most once in a row, so the agent can always explain and stop.
    assert_eq!(session.stop(true), None);

    session.bash("cargo test --no-fail-fast 2>&1 | tail -1", "fixed");
    assert_eq!(session.stop(false), None, "a proven fix may finish");

    fs::write(
        session.project.path().join("lib.rs"),
        "fn main() { todo!() }\n",
    )
    .unwrap();
    let stale = session.stop(false).unwrap();
    assert!(
        stale["reason"]
            .as_str()
            .unwrap()
            .contains("files changed after the last passing test run"),
        "{stale:#}"
    );
}

#[test]
fn stop_gate_does_not_accept_a_deleted_failing_test() {
    let session = Session::start();
    session.bash("cargo test --no-fail-fast 2>&1 | tail -1", "baseline");
    session.bash("cargo test --no-fail-fast 2>&1 | tail -1", "deleted");
    let blocked = session.stop(false).unwrap();
    let reason = blocked["reason"].as_str().unwrap();
    assert!(reason.contains("does not prove"), "{reason}");
    assert!(reason.contains("test population shrank"), "{reason}");
}

#[test]
fn hooks_fail_open() {
    let session = Session::start();
    assert_eq!(session.raw_hook("not json", true), "");
    assert_eq!(session.raw_hook("{}", true), "");
    // The gate is opt-in.
    session.bash("cargo test --no-fail-fast 2>&1 | tail -1", "baseline");
    let output = session.hook(
        json!({"hook_event_name": "Stop", "session_id": "s1", "stop_hook_active": false}),
        false,
    );
    assert_eq!(output, "");
    let unknown_host = Command::new(env!("CARGO_BIN_EXE_prog"))
        .args(["harness", "hook", "other-host"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(unknown_host.status.success());
    assert!(unknown_host.stdout.is_empty());
}

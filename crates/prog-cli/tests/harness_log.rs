//! The local hook decision log (#314): one line per Stop and PostToolUse
//! decision, no message or command text, fail-open, and a summary command.

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

const FAKE_CARGO: &str = r#"#!/bin/sh
state=$(cat "$(dirname "$0")/state")
echo "running 1 tests"
if [ "$state" = fail ]; then
  echo "test t::adds ... FAILED"
  echo "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
  exit 101
fi
echo "test t::adds ... ok"
echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
"#;

struct Session {
    _root: tempfile::TempDir,
    root: PathBuf,
    project: tempfile::TempDir,
    tools: PathBuf,
    env_file: PathBuf,
    store: PathBuf,
}

impl Session {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_path_buf();
        let tools = root_path.join("tools");
        fs::create_dir_all(&tools).unwrap();
        fs::write(tools.join("cargo"), FAKE_CARGO).unwrap();
        fs::set_permissions(tools.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
        let project = test_git_repo();
        let session = Self {
            env_file: root_path.join("claude-env.sh"),
            store: root_path.join("store"),
            _root: root,
            root: root_path,
            project,
            tools,
        };
        session.hook(
            json!({"hook_event_name": "SessionStart", "session_id": "s-secret-1"}),
            false,
            &[],
        );
        session
    }

    fn command(&self, stop_gate: bool, env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
        command
            .current_dir(self.project.path())
            .env("PROG_DIR", &self.store)
            .env_remove("PROG_FORMAT")
            .env_remove("PROG_STOP_GATE")
            .env_remove("PROG_HARNESS_LOG")
            .env("CLAUDE_PROJECT_DIR", self.project.path())
            .env("CLAUDE_ENV_FILE", &self.env_file)
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"));
        if stop_gate {
            command.env("CLAUDE_PLUGIN_OPTION_STOP_GATE", "true");
        } else {
            command.env_remove("CLAUDE_PLUGIN_OPTION_STOP_GATE");
        }
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }

    fn hook(&self, mut input: Value, stop_gate: bool, env: &[(&str, &str)]) -> String {
        input["cwd"] = json!(self.project.path());
        let mut child = self
            .command(stop_gate, env)
            .args(["harness", "hook", "claude-code"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
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

    fn bash(&self, script: &str, state: &str) {
        fs::write(self.tools.join("state"), state).unwrap();
        let status = Command::new("bash")
            .arg("-c")
            .arg(format!(". '{}'; {script}", self.env_file.display()))
            .current_dir(self.project.path())
            .env("PATH", format!("{}:/usr/bin:/bin", self.tools.display()))
            .env("PROG_DIR", &self.store)
            .env("XDG_STATE_HOME", self.root.join("state"))
            .output()
            .unwrap();
        assert!(status.status.code().is_some(), "{status:?}");
    }

    fn post_tool_use(&self, command: &str) -> String {
        self.hook(
            json!({
                "hook_event_name": "PostToolUse",
                "session_id": "s-secret-1",
                "tool_name": "Bash",
                "tool_input": {"command": command}
            }),
            false,
            &[],
        )
    }

    fn stop(&self, message: &str, active: bool, gate: bool) -> String {
        self.hook(
            json!({
                "hook_event_name": "Stop",
                "session_id": "s-secret-1",
                "stop_hook_active": active,
                "last_assistant_message": message
            }),
            gate,
            &[],
        )
    }

    fn log_lines(&self) -> Vec<Value> {
        log_lines(&self.store)
    }

    fn prog(&self, args: &[&str]) -> Value {
        let output = self.command(false, &[]).args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn log_lines(store: &Path) -> Vec<Value> {
    let Ok(files) = fs::read_dir(store.join("harness-log")) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for file in files.flatten() {
        let text = fs::read_to_string(file.path()).unwrap();
        lines.extend(
            text.lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap()),
        );
    }
    lines
}

#[test]
fn each_decision_writes_exactly_one_line() {
    let session = Session::start();
    session.bash("cargo test", "fail");
    assert!(session.post_tool_use("cargo test").contains("1 failed"));
    assert!(
        session
            .stop("All tests pass.", false, true)
            .contains("\"block\"")
    );
    session.stop("All tests pass.", true, true);
    session.stop("Tests are still failing.", false, true);
    session.stop("All tests pass.", false, false);
    session.post_tool_use("python3 -m pytest -q");
    session.post_tool_use("ls");

    let lines = session.log_lines();
    let decisions = lines
        .iter()
        .map(|line| {
            format!(
                "{}:{}",
                line["event"].as_str().unwrap(),
                line["decision"].as_str().unwrap_or("-")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        decisions,
        [
            "post_tool_use:-",
            "stop:block",
            "stop:skip_stop_hook_active",
            "stop:stand_aside",
            "stop:would_block",
            "post_tool_use:-",
        ]
    );
    for line in &lines {
        assert_eq!(line["schema"], "prog.harness_log.v1");
        assert_eq!(line["host"], "claude-code");
    }
    assert_eq!(lines[0]["captured"][0]["verdict"], "failing");
    assert_eq!(lines[1]["rule"], "test_failing");
    assert_eq!(lines[1]["contradiction"], true);
    assert_eq!(lines[1]["message"]["claims_success"], true);
    assert_eq!(lines[3]["message"]["discloses_gap"], true);
    assert_eq!(lines[4]["gate_enabled"], false);
    assert_eq!(lines[5]["uncaptured_runner"], "python3 -m pytest");
    // `ls` is not a test command: nothing to log.
    assert_eq!(lines.len(), 6);
}

/// No message text, command text, session id, or secret ever reaches the log.
#[test]
fn the_log_holds_no_message_command_or_secret_text() {
    let session = Session::start();
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    session.bash(&format!("GITHUB_TOKEN={secret} cargo test"), "fail");
    session.post_tool_use(&format!(
        "GITHUB_TOKEN={secret} cargo test --quiet-marker-xyz"
    ));
    session.post_tool_use(&format!("TOKEN={secret} pytest -k marker-abc"));
    session.stop(
        &format!("All tests pass. My token is {secret}; see marker-def."),
        false,
        true,
    );
    let raw = fs::read_dir(session.store.join("harness-log"))
        .unwrap()
        .flatten()
        .map(|file| fs::read_to_string(file.path()).unwrap())
        .collect::<String>();
    for needle in [
        secret,
        "marker-xyz",
        "marker-abc",
        "marker-def",
        "All tests pass",
        "s-secret-1",
    ] {
        assert!(!raw.contains(needle), "{needle} leaked into the log: {raw}");
    }
}

#[test]
fn logging_can_be_turned_off_and_never_breaks_a_hook() {
    let session = Session::start();
    session.bash("cargo test", "fail");
    session.hook(
        json!({"hook_event_name": "PostToolUse", "session_id": "s-secret-1", "tool_name": "Bash", "tool_input": {"command": "cargo test"}}),
        false,
        &[("PROG_HARNESS_LOG", "0")],
    );
    assert!(session.log_lines().is_empty());

    // A log path that cannot be written: the hook answers exactly as before.
    fs::create_dir_all(&session.store).unwrap();
    fs::write(session.store.join("harness-log"), "not a directory").unwrap();
    let blocked = session.stop("All tests pass.", false, true);
    let blocked: Value = serde_json::from_str(&blocked).unwrap();
    assert_eq!(blocked["decision"], "block");
}

#[test]
fn summary_reports_blocks_rules_and_what_followed() {
    let session = Session::start();
    session.bash("cargo test", "fail");
    session.post_tool_use("cargo test");
    session.stop("Working on it.", false, true);
    session.stop("Working on it.", true, true);
    session.bash("cargo test", "pass");
    session.post_tool_use("cargo test");
    session.stop("All tests pass.", false, true);

    let summary = session.prog(&["harness", "log", "--summary"]);
    assert_eq!(summary["schema"], "prog.harness_log_summary.v1");
    assert_eq!(summary["turn_ends"], 3);
    assert_eq!(summary["blocks"], 1);
    assert_eq!(summary["by_rule"]["test_failing"], 1);
    assert_eq!(summary["second_stops_let_through"], 1);
    assert_eq!(summary["after_block"]["same_invocation"], 1);
    assert_eq!(summary["test_commands"]["captured"], 2);
    assert_eq!(summary["test_commands"]["captured_share"], 1.0);

    let listing = session.prog(&["harness", "log", "--since", "1d"]);
    assert_eq!(listing["total"], 5);
    assert_eq!(listing["entries"].as_array().unwrap().len(), 5);
}

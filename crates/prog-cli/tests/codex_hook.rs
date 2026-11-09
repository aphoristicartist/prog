//! Codex hooks (#304), driven with stdin payloads shaped by Codex 0.167's
//! pinned hook schemas (`codex-rs/hooks/schema/generated/*.command.input.schema.json`:
//! every required field present), shell commands run with the environment the
//! `prog harness codex` launcher sets, and hook-output JSON read from stdout.

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
echo "running 2 tests"
if [ "$state" = fail ]; then
  echo "test t::adds ... FAILED"
  echo "test t::muls ... ok"
  echo "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
  exit 101
fi
echo "test t::adds ... ok"
echo "test t::muls ... ok"
echo "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
"#;

struct Session {
    _root: tempfile::TempDir,
    root: PathBuf,
    project: tempfile::TempDir,
    tools: PathBuf,
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
        let session = Self {
            store: root_path.join("store"),
            _root: root,
            root: root_path,
            project: test_git_repo(),
            tools,
        };
        let output = session.hook(
            json!({
                "hook_event_name": "SessionStart",
                "source": "startup",
            }),
            &[],
        );
        assert_eq!(output, "", "SessionStart prints nothing");
        session
    }

    fn shims(&self) -> PathBuf {
        self.root.join("data/prog/shims")
    }

    /// A hook payload with every field Codex 0.167 marks required.
    fn payload(&self, mut fields: Value) -> Value {
        let mut payload = json!({
            "session_id": "019a-codex-session",
            "transcript_path": self.root.join("rollout.jsonl"),
            "cwd": self.project.path(),
            "model": "gpt-test",
            "permission_mode": "default",
            "turn_id": "turn-1",
        });
        payload
            .as_object_mut()
            .unwrap()
            .append(fields.as_object_mut().unwrap());
        payload
    }

    fn hook(&self, fields: Value, env: &[(&str, &str)]) -> String {
        let input = self.payload(fields);
        self.raw_hook(&input.to_string(), env)
    }

    fn raw_hook(&self, input: &str, env: &[(&str, &str)]) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
        command
            .args(["harness", "hook", "codex"])
            .current_dir(self.project.path())
            .env("PROG_DIR", &self.store)
            .env_remove("PROG_FORMAT")
            .env_remove("PROG_STOP_GATE")
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("CLAUDE_ENV_FILE")
            .env_remove("CLAUDE_PLUGIN_OPTION_STOP_GATE")
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
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

    /// A shell command with the launcher's environment: shims first on PATH,
    /// PROG_DIR set. Runs through `zsh -c` like Codex with
    /// `allow_login_shell=false` (here `sh -c`).
    fn bash(&self, script: &str, state: &str, store: &Path) {
        fs::write(self.tools.join("state"), state).unwrap();
        let output = Command::new("sh")
            .arg("-c")
            .arg(script)
            .current_dir(self.project.path())
            .env(
                "PATH",
                format!(
                    "{}:{}:/usr/bin:/bin",
                    self.shims().display(),
                    self.tools.display()
                ),
            )
            .env("PROG_DIR", store)
            .env("XDG_STATE_HOME", self.root.join("state"))
            .output()
            .unwrap();
        assert!(output.status.code().is_some(), "{output:?}");
    }

    fn post_tool_use(&self, command: &str, id: &str) -> Option<Value> {
        let output = self.hook(
            json!({
                "hook_event_name": "PostToolUse",
                "tool_name": "Bash",
                "tool_use_id": id,
                "tool_input": {"command": command},
                // Codex passes only the model-facing output, no exit code.
                "tool_response": "test result: FAILED. 1 passed; 1 failed",
            }),
            &[],
        );
        (!output.trim().is_empty()).then(|| serde_json::from_str(&output).unwrap())
    }

    fn stop(&self, message: Option<&str>, active: bool, gate: bool) -> Option<Value> {
        let output = self.hook(
            json!({
                "hook_event_name": "Stop",
                "stop_hook_active": active,
                "last_assistant_message": message,
            }),
            if gate {
                &[("PROG_STOP_GATE", "1")]
            } else {
                &[]
            },
        );
        (!output.trim().is_empty()).then(|| serde_json::from_str(&output).unwrap())
    }
}

#[test]
fn session_start_installs_shims_without_an_env_file() {
    let session = Session::start();
    assert!(session.shims().join("cargo").is_file());
    assert!(
        session
            .store
            .join("claude-sessions/019a-codex-session")
            .is_file()
    );
}

#[test]
fn a_failing_run_reaches_codex_as_a_receipt_once() {
    let session = Session::start();
    session.bash("cargo test 2>&1 | tail -1", "fail", &session.store);
    let output = session
        .post_tool_use("cargo test 2>&1 | tail -1", "call-1")
        .unwrap();
    assert_eq!(output["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    let context = output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("1 passed, 1 failed: t::adds"), "{context}");
    // The exit code came from the shim's capture, not from Codex.
    assert!(context.contains("exit 101"), "{context}");
    assert_eq!(
        session.post_tool_use("cargo test 2>&1 | tail -1", "call-1"),
        None
    );
}

#[test]
fn the_stop_gate_needs_prog_stop_gate_and_blocks_once() {
    let session = Session::start();
    session.bash("cargo test", "fail", &session.store);
    session.post_tool_use("cargo test", "call-1");
    assert_eq!(session.stop(Some("Done."), false, false), None, "gate off");

    let blocked = session.stop(Some("All tests pass."), false, true).unwrap();
    assert_eq!(blocked["decision"], "block");
    let reason = blocked["reason"].as_str().unwrap();
    assert!(
        reason.starts_with(
            "prog stop gate: you said \"All tests pass\", but the latest test run is failing"
        ),
        "{reason}"
    );
    // The repeated stop is let through; the user is told instead.
    let warning = session.stop(Some("All tests pass."), true, true).unwrap();
    assert!(warning.get("decision").is_none(), "{warning:#}");
    assert!(
        warning["systemMessage"]
            .as_str()
            .unwrap()
            .starts_with("prog: Codex said \"All tests pass\""),
        "{warning:#}"
    );
    assert_eq!(
        session.stop(Some("Still failing; not fixed."), true, true),
        None
    );

    // An honest report that names the failing test is not blocked; the same
    // name inside a success claim is.
    assert_eq!(
        session.stop(
            Some("cargo test exited 101: t::adds expected 4, got 0. I did not change anything."),
            false,
            true
        ),
        None
    );
    assert!(
        session
            .stop(Some("t::adds now passes."), false, true)
            .is_some()
    );

    session.bash("cargo test", "pass", &session.store);
    session.post_tool_use("cargo test", "call-2");
    assert_eq!(session.stop(Some("Fixed."), false, true), None);
}

/// Under Codex's sandbox the store may not be writable: the shim's output is
/// unchanged, and the receipt slot says no capture happened instead of
/// staying silent.
#[test]
fn an_unwritable_store_is_reported_never_silent() {
    let session = Session::start();
    let locked = session.root.join("locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
    session.bash("cargo test", "fail", &locked.join("store"));
    let output = session.post_tool_use("cargo test", "call-1").unwrap();
    let context = output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        context.starts_with("prog: no capture for this test command (`cargo test`)"),
        "{context}"
    );
    assert!(context.contains("could not be recorded"), "{context}");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn malformed_or_foreign_input_fails_open() {
    let session = Session::start();
    for input in [
        "",
        "not json",
        "{}",
        r#"{"hook_event_name":"Stop"}"#,
        "[1,2]",
    ] {
        assert_eq!(
            session.raw_hook(input, &[("PROG_STOP_GATE", "1")]),
            "",
            "{input}"
        );
    }
    assert_eq!(
        session.hook(
            json!({"hook_event_name": "PostToolUse", "tool_name": "apply_patch", "tool_use_id": "x", "tool_input": {}, "tool_response": ""}),
            &[]
        ),
        ""
    );
}

fn fake_codex(dir: &Path) -> PathBuf {
    let codex = dir.join("codex");
    fs::write(
        &codex,
        "#!/bin/sh\nout=\"$(dirname \"$0\")/launched\"\nfor arg in \"$@\"; do printf '%s\\n' \"$arg\"; done > \"$out.args\"\nenv > \"$out.env\"\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
    codex
}

fn launch(
    root: &Path,
    project: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
    command
        .args(["harness", "codex"])
        .args(args)
        .current_dir(project)
        .env_remove("PROG_DIR")
        .env_remove("PROG_FORMAT")
        .env_remove("PROG_STOP_GATE")
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("PATH", "/usr/bin:/bin");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().unwrap()
}

#[test]
fn the_launcher_puts_shims_first_and_makes_the_store_writable() {
    let root = tempfile::tempdir().unwrap();
    let project = test_git_repo();
    let codex = fake_codex(root.path());
    let output = launch(
        root.path(),
        project.path(),
        &["--stop-gate", "exec", "--json", "fix the failing test"],
        &[("PROG_CODEX_BIN", codex.to_str().unwrap())],
    );
    assert!(output.status.success(), "{output:?}");
    let args = fs::read_to_string(root.path().join("launched.args")).unwrap();
    let args = args.lines().collect::<Vec<_>>();
    let projects = root.path().join("state/prog/projects");
    assert_eq!(
        args,
        [
            "-c".to_string(),
            "allow_login_shell=false".to_string(),
            "-c".to_string(),
            format!(
                "sandbox_workspace_write.writable_roots=[\"{}\"]",
                projects.display()
            ),
            "exec".to_string(),
            "--json".to_string(),
            "fix the failing test".to_string(),
        ]
    );
    let env = fs::read_to_string(root.path().join("launched.env")).unwrap();
    let shims = root.path().join("data/prog/shims");
    assert!(
        env.lines()
            .any(|line| line == format!("PATH={}:/usr/bin:/bin", shims.display())),
        "{env}"
    );
    assert!(shims.join("cargo").is_file());
    let store = env
        .lines()
        .find_map(|line| line.strip_prefix("PROG_DIR="))
        .unwrap();
    assert!(Path::new(store).starts_with(&projects), "{store}");
    assert!(
        Path::new(store).is_dir(),
        "the store exists before the sandbox starts"
    );
    assert!(env.lines().any(|line| line == "PROG_STOP_GATE=1"), "{env}");
    assert!(
        env.lines().any(|line| line == "PROG_FORMAT=compact"),
        "{env}"
    );

    let output = launch(
        root.path(),
        project.path(),
        &["--keep-codex-config", "exec", "hi"],
        &[("PROG_CODEX_BIN", codex.to_str().unwrap())],
    );
    assert!(output.status.success(), "{output:?}");
    let args = fs::read_to_string(root.path().join("launched.args")).unwrap();
    assert_eq!(args.lines().collect::<Vec<_>>(), ["exec", "hi"]);
    let env = fs::read_to_string(root.path().join("launched.env")).unwrap();
    assert!(!env.contains("PROG_STOP_GATE"), "{env}");
}

#[test]
fn the_launcher_reports_a_missing_codex_as_json() {
    let root = tempfile::tempdir().unwrap();
    let project = test_git_repo();
    let output = launch(root.path(), project.path(), &["exec", "hi"], &[]);
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        error.to_string().contains("codex was not found on PATH"),
        "{error:#}"
    );
}

fn doctor_codex_check(codex_home: &Path) -> Value {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_prog"))
        .args(["harness", "doctor", "--root"])
        .arg(root.path())
        .env("CODEX_HOME", codex_home)
        .env("HOME", root.path())
        .env("XDG_DATA_HOME", root.path().join("data"))
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "codex_hooks")
        .cloned()
        .unwrap_or(Value::Null)
}

#[test]
fn doctor_reports_untrusted_codex_hooks() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(doctor_codex_check(home.path())["status"], "absent");

    fs::write(
        home.path().join("config.toml"),
        "model = \"x\"\n\n[plugins.\"prog@personal\"]\nenabled = true\n",
    )
    .unwrap();
    let check = doctor_codex_check(home.path());
    assert_eq!(check["status"], "untrusted", "{check:#}");
    assert!(check["detail"].as_str().unwrap().contains("/hooks"));

    fs::write(
        home.path().join("config.toml"),
        "[plugins.\"prog@personal\"]\nenabled = true\n\n[hooks.state.\"plugin:prog@personal:hooks/hooks.json:Stop:0:0\"]\ntrusted_hash = \"abc\"\n",
    )
    .unwrap();
    assert_eq!(doctor_codex_check(home.path())["status"], "ok");

    fs::write(
        home.path().join("config.toml"),
        "[plugins.\"prog@personal\"]\nenabled = false\n",
    )
    .unwrap();
    assert_eq!(doctor_codex_check(home.path())["status"], "absent");
}

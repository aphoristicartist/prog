//! PATH shim equivalence (#289): with the shim directory on PATH, every
//! agent-shaped pipeline produces byte-identical output and identical
//! pipeline statuses, while progressive commands are recorded in full.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

mod support;

use support::{prog, stdout};

/// Deterministic stand-in for `cargo`: interleaves stdout and stderr, prints a
/// libtest summary, and exits 101 for `test`. `--many` floods stdout so an
/// early `head` closes the pipe. SIGPIPE keeps its default disposition, as for
/// native tools.
const FAKE_CARGO: &str = r#"#!/usr/bin/env python3
import os, signal, sys, time
signal.signal(signal.SIGPIPE, signal.SIG_DFL)
args = sys.argv[1:]
if args[:1] == ["--version"]:
    print("cargo 9.9.9 (fake)")
    sys.exit(0)
if "--sleep" in args:
    time.sleep(30)
    sys.exit(0)
lines = 200000 if "--many" in args else 1
for index in range(lines):
    sys.stdout.write("test t::case%d ... ok\n" % index)
sys.stdout.flush()
sys.stderr.write("   Compiling fake v0.1.0\n")
sys.stderr.flush()
sys.stdout.write("test t::adds ... FAILED\n")
sys.stdout.write("\nthread 't::adds' panicked at src/lib.rs:6:25:\nassertion failed\n")
sys.stdout.write("test result: FAILED. %d passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n" % lines)
sys.stdout.flush()
sys.stderr.write("error: test failed, to rerun pass `--lib`\n")
sys.exit(101)
"#;

struct Fixture {
    _root: tempfile::TempDir,
    tools: PathBuf,
    shims: PathBuf,
    store: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let tools = root.path().join("tools");
        let shims = root.path().join("shims");
        let store = root.path().join("store");
        fs::create_dir_all(&tools).unwrap();
        let cargo = tools.join("cargo");
        fs::write(&cargo, FAKE_CARGO).unwrap();
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
        let installed = prog(&[
            "shim",
            "install",
            "--dir",
            shims.to_str().unwrap(),
            "--tool",
            "cargo",
        ]);
        assert!(installed.status.success(), "{}", stdout(&installed));
        Self {
            _root: root,
            tools,
            shims,
            store,
        }
    }

    fn path(&self, with_shims: bool) -> String {
        let base = format!("{}:/usr/bin:/bin", self.tools.display());
        if with_shims {
            format!("{}:{base}", self.shims.display())
        } else {
            base
        }
    }

    /// Run `script` under bash and return everything it wrote (both streams,
    /// in order) followed by its pipeline statuses.
    fn bash(&self, script: &str, with_shims: bool, store: &Path) -> String {
        let output = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "{{ {script}; }} 2>&1; echo \"PIPESTATUS=${{PIPESTATUS[*]}} status=$?\""
            ))
            // Run inside the fixture, never the contributor checkout: a
            // capture records the workspace it ran in.
            .current_dir(self._root.path())
            .env("PATH", self.path(with_shims))
            .env("PROG_DIR", store)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Shim captures recorded so far: each writes one pending-receipt marker.
    fn captures(&self) -> usize {
        fs::read_dir(self.store.join("receipts-pending")).map_or(0, |entries| entries.count())
    }

    fn latest_observation(&self) -> Value {
        let listed = prog(&[
            "--dir",
            self.store.to_str().unwrap(),
            "cache",
            "observations",
            "--limit",
            "1",
        ]);
        assert!(listed.status.success(), "{}", stdout(&listed));
        let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
        listed["observations"][0].clone()
    }
}

const PIPELINES: &[&str] = &[
    "cargo test 2>&1 | tail -5",
    "cargo test 2>&1 | grep -E 'test result|FAILED|panicked'",
    "cargo test --no-fail-fast 2>&1 | tail -3",
    "cd . && cargo test -q 2>&1 | tail -4",
    "for i in 1 2; do cargo test 2>&1 | tail -1; done",
    "cargo test",
    "cargo test 2>/dev/null | tail -2",
    "cargo test > /dev/null 2>&1; echo status=$?",
    "(cargo test 2>&1) | tail -2",
    "cargo test 2>&1 | tee /dev/null | tail -2",
    "set -o pipefail; cargo test 2>&1 | tail -1",
    "nice cargo test 2>&1 | head -3",
    "cargo test --many 2>/dev/null | head -3",
    "cargo --version",
];

#[test]
fn shimmed_pipelines_are_byte_identical_and_capture_the_full_output() {
    let fixture = Fixture::new();
    let unused_store = fixture.store.with_file_name("unused-store");
    for script in PIPELINES {
        let before = fixture.captures();
        let raw = fixture.bash(script, false, &unused_store);
        let shimmed = fixture.bash(script, true, &fixture.store);
        assert_eq!(raw, shimmed, "output differs for: {script}");
        let recorded = fixture.captures() - before;
        let expected = match *script {
            "cargo --version" => 0,
            script if script.starts_with("for i in 1 2") => 2,
            _ => 1,
        };
        assert_eq!(recorded, expected, "captures for: {script}");
    }
    assert!(
        !unused_store.exists(),
        "the raw runs must not touch any store"
    );
}

#[test]
fn shim_capture_is_a_complete_run_observation_even_when_the_agent_tails() {
    let fixture = Fixture::new();
    let seen = fixture.bash("cargo test 2>&1 | tail -1", true, &fixture.store);
    assert!(seen.starts_with("error: test failed"), "{seen}");
    let observation = fixture.latest_observation();
    assert_eq!(observation["source_id"], "run");
    assert_eq!(
        observation["capture"]["can_prove_absence"], true,
        "{observation:#}"
    );
    // The receipt for the pending capture names the failure the tail hid.
    let receipt = prog(&[
        "--dir",
        fixture.store.to_str().unwrap(),
        "receipt",
        "--pending",
    ]);
    assert!(receipt.status.success(), "{}", stdout(&receipt));
    let receipt: Value = serde_json::from_slice(&receipt.stdout).unwrap();
    let text = receipt["text"].as_str().unwrap();
    assert!(text.contains("prog: `cargo test` · exit 101"), "{text}");
    assert!(text.contains("1 passed, 1 failed: t::adds"), "{text}");
    assert!(
        text.contains("t::adds panicked at src/lib.rs:6:25"),
        "{text}"
    );
    // Pending markers are consumed once.
    let again = prog(&[
        "--dir",
        fixture.store.to_str().unwrap(),
        "receipt",
        "--pending",
    ]);
    let again: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(again["receipts"].as_array().unwrap().len(), 0);
}

#[test]
fn pending_receipts_beyond_the_newest_five_are_dropped_not_replayed_later() {
    let fixture = Fixture::new();
    fixture.bash(
        "for run in 1 2 3 4 5 6 7; do cargo test >/dev/null 2>&1; done",
        true,
        &fixture.store,
    );
    assert_eq!(fixture.captures(), 7);
    let pending = || {
        let receipt = prog(&[
            "--dir",
            fixture.store.to_str().unwrap(),
            "receipt",
            "--pending",
        ]);
        assert!(receipt.status.success(), "{}", stdout(&receipt));
        let receipt: Value = serde_json::from_slice(&receipt.stdout).unwrap();
        receipt["receipts"].as_array().unwrap().len()
    };
    assert_eq!(pending(), 5);
    // The two oldest must not surface after some later, unrelated command.
    assert_eq!(pending(), 0);
}

#[test]
fn a_second_shim_directory_on_path_is_skipped_rather_than_looped() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let fixture = Fixture::new();
    let second = fixture._root.path().join("more-shims");
    let installed = prog(&[
        "shim",
        "install",
        "--dir",
        second.to_str().unwrap(),
        "--tool",
        "cargo",
    ]);
    assert!(installed.status.success(), "{}", stdout(&installed));
    for (first, other) in [(&fixture.shims, &second), (&second, &fixture.shims)] {
        for (args, expected) in [
            (&["--version"][..], "cargo 9.9.9 (fake)"),
            (&["test"][..], "test result: FAILED"),
        ] {
            let mut child = Command::new(first.join("cargo"))
                .args(args)
                .current_dir(fixture._root.path())
                .env(
                    "PATH",
                    format!(
                        "{}:{}:{}",
                        first.display(),
                        other.display(),
                        fixture.path(false)
                    ),
                )
                .env("PROG_DIR", &fixture.store)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while child.try_wait().unwrap().is_none() {
                if std::time::Instant::now() > deadline {
                    // SAFETY: killing the process group this test created.
                    unsafe { libc::killpg(child.id() as i32, libc::SIGKILL) };
                    let _ = child.wait();
                    panic!("`cargo {args:?}` looped between two shim directories");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let output = child.wait_with_output().unwrap();
            let seen = String::from_utf8_lossy(&output.stdout);
            assert!(seen.contains(expected), "cargo {args:?}: {seen}");
        }
    }
}

#[test]
fn an_unusable_store_never_changes_the_command() {
    let fixture = Fixture::new();
    let broken = PathBuf::from("/dev/null/not-a-directory");
    let raw = fixture.bash("cargo test 2>&1 | tail -2", false, &broken);
    let shimmed = fixture.bash("cargo test 2>&1 | tail -2", true, &broken);
    assert_eq!(raw, shimmed);
}

#[test]
fn termination_is_forwarded_and_reported_like_the_child() {
    let fixture = Fixture::new();
    let mut shim = Command::new(fixture.shims.join("cargo"))
        .args(["test", "--sleep"])
        .env("PATH", fixture.path(true))
        .env("PROG_DIR", &fixture.store)
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    // SAFETY: signalling a child process this test spawned.
    assert_eq!(unsafe { libc::kill(shim.id() as i32, libc::SIGTERM) }, 0);
    let status = shim.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
}

#[test]
fn install_never_overwrites_foreign_files_and_uninstall_removes_only_shims() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("shims");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("go"), "#!/bin/sh\necho mine\n").unwrap();
    let installed = prog(&[
        "shim",
        "install",
        "--dir",
        dir.to_str().unwrap(),
        "--tool",
        "go",
        "--tool",
        "cargo",
    ]);
    assert!(installed.status.success(), "{}", stdout(&installed));
    let installed: Value = serde_json::from_slice(&installed.stdout).unwrap();
    let actions = installed["shims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|shim| shim["action"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(actions, ["exists", "created"]);
    assert_eq!(
        fs::read_to_string(dir.join("go")).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
    let again = prog(&[
        "shim",
        "install",
        "--dir",
        dir.to_str().unwrap(),
        "--tool",
        "cargo",
    ]);
    let again: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(again["shims"][0]["action"], "unchanged");

    let removed = prog(&["shim", "uninstall", "--dir", dir.to_str().unwrap()]);
    assert!(removed.status.success(), "{}", stdout(&removed));
    assert!(dir.join("go").exists());
    assert!(!dir.join("cargo").exists());
}

#[test]
fn shim_list_reports_generated_shims_their_activity_and_real_tools() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("shims");
    let tools = root.path().join("tools");
    fs::create_dir_all(&tools).unwrap();
    let real = tools.join("cargo");
    fs::write(&real, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
    let installed = prog(&[
        "shim",
        "install",
        "--dir",
        dir.to_str().unwrap(),
        "--tool",
        "cargo",
    ]);
    assert!(installed.status.success(), "{}", stdout(&installed));
    fs::write(dir.join("notes.txt"), "not a shim").unwrap();

    let list = |path: &str| -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_prog"))
            .args(["shim", "list", "--dir", dir.to_str().unwrap()])
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stdout(&output));
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let active = list(&format!(
        "{}:{}:/usr/bin:/bin",
        dir.display(),
        tools.display()
    ));
    assert_eq!(active["installed"], true);
    assert_eq!(active["on_path"], true);
    assert_eq!(active["shims"].as_array().unwrap().len(), 1, "{active:#}");
    assert_eq!(active["shims"][0]["tool"], "cargo");
    assert_eq!(active["shims"][0]["active"], true);
    assert_eq!(active["shims"][0]["wraps"], real.to_str().unwrap());

    let inactive = list(&format!("{}:/usr/bin:/bin", tools.display()));
    assert_eq!(inactive["on_path"], false);
    assert_eq!(inactive["shims"][0]["active"], false);
    assert!(
        inactive["warnings"].to_string().contains("not on PATH"),
        "{inactive:#}"
    );
}

#[test]
fn shim_env_is_idempotent_and_keeps_the_store_outside_the_project() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_prog"))
        .args([
            "shim",
            "env",
            "--dir",
            "/opt/prog \"shims\" $HOME",
            "--project",
            project.to_str().unwrap(),
            "--shell",
        ])
        .env("XDG_STATE_HOME", root.path().join("state"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let exports = String::from_utf8(output.stdout).unwrap();
    assert!(exports.contains("export PROG_DIR="), "{exports}");
    assert!(!exports.contains(&format!("PROG_DIR={}", project.display())));
    // Sourcing twice prepends the directory once, verbatim, even when its
    // name holds quotes or `$`.
    let path = Command::new("sh")
        .arg("-c")
        .arg(format!("{exports}{exports}printf %s \"$PATH\""))
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(path.stdout).unwrap(),
        "/opt/prog \"shims\" $HOME:/usr/bin:/bin"
    );
}

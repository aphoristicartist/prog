//! Shared integration-test process and fixture helpers.
//!
//! Each file in `tests/` is a separate Rust crate. A helper that is used by
//! one integration-test crate can therefore appear unused while another crate
//! is being compiled, so this allowance is intentionally limited to this
//! shared support module.
#![allow(dead_code)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

/// Returns a directory created by the test, never the contributor checkout.
///
/// Commands that share a `--dir` store also share that store's fixture
/// directory as their workspace. This lets an observation and a later
/// readiness/delta query compare the same controlled workspace. Commands
/// without a store receive a fresh temporary directory instead. Tests that
/// assert workspace validity use [`prog_in_dir`] with a fixture repository.
pub fn isolated_working_dir(args: &[&str]) -> (Option<tempfile::TempDir>, PathBuf) {
    if let Some(window) = args.windows(2).find(|window| window[0] == "--dir") {
        let path = PathBuf::from(window[1]);
        if path.is_dir() {
            return (None, path);
        }
    }
    let directory = tempfile::tempdir().expect("isolated test working directory should exist");
    let path = directory.path().to_path_buf();
    (Some(directory), path)
}

pub fn test_git_repo() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("isolated Git repository should be creatable");
    let output = Command::new("git")
        .args(["init", "-q"])
        .current_dir(directory.path())
        .output()
        .expect("git should initialize the isolated test workspace");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    directory
}

pub fn prog(args: &[&str]) -> Output {
    let (_temporary_cwd, cwd) = isolated_working_dir(args);
    Command::new(env!("CARGO_BIN_EXE_prog"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("prog binary should run")
}

pub fn prog_in_dir(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_prog"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("prog binary should run")
}

pub fn prog_with_env(args: &[&str], env: &[(&str, &str)]) -> Output {
    let (_temporary_cwd, cwd) = isolated_working_dir(args);
    let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
    command.current_dir(cwd);
    command.args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("prog binary should run")
}

pub fn prog_with_budget(dir: &str, budget: u32, command: &[&str]) -> Output {
    let (_temporary_cwd, cwd) = isolated_working_dir(&["--dir", dir]);
    Command::new(env!("CARGO_BIN_EXE_prog"))
        .current_dir(cwd)
        .arg("--dir")
        .arg(dir)
        .arg("--budget-bytes")
        .arg(budget.to_string())
        .args(command)
        .output()
        .expect("prog binary should run")
}

pub fn prog_with_stdin(args: &[&str], stdin: &[u8]) -> Output {
    let (_temporary_cwd, cwd) = isolated_working_dir(args);
    let mut child = Command::new(env!("CARGO_BIN_EXE_prog"))
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("prog binary should spawn");
    child
        .stdin
        .take()
        .expect("piped stdin should be present")
        .write_all(stdin)
        .expect("stdin should write");
    child.wait_with_output().expect("prog binary should run")
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be utf8")
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be utf8")
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should canonicalize")
}

pub fn first_party_lens_dir() -> PathBuf {
    repo_root().join("lenses")
}

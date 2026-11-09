//! `prog codex`: launch Codex so that prog's PATH shims, receipts, and stop
//! gate work in it (#304).
//!
//! Codex has no `CLAUDE_ENV_FILE`, and a live check of Codex 0.167 found two
//! things that silently disable the shims:
//!
//! - its default login shell (`zsh -lc`) re-runs the user's profile, which
//!   puts Homebrew and `~/.local/bin` ahead of an inherited shim directory, so
//!   `go`, `npm`, `npx`, `python3`, and `pytest` resolved past the shims;
//! - its `workspace-write` sandbox denies writes outside the workspace, so a
//!   shim could not record into prog's store under `~/.local/state`.
//!
//! The launcher prepends the shims, points `PROG_DIR` at the project's store,
//! and passes `-c allow_login_shell=false` and the store's directory as a
//! sandbox `writable_roots` entry, then replaces itself with `codex`. It never
//! rewrites a command Codex runs.

use std::{
    ffi::OsString,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
};

use crate::commands::shim::{default_shim_dir, project_store_dir, shim_install};
use crate::*;

/// Replace this process with `codex`. Returns only on failure.
pub(crate) fn codex_launch(args: &CodexArgs) -> CoreError {
    match prepare(args) {
        Ok(mut command) => {
            let error = command.exec();
            CoreError::BadArgs {
                operation: "codex".to_string(),
                reason: format!("could not start codex: {error}"),
            }
        }
        Err(error) => error,
    }
}

fn prepare(args: &CodexArgs) -> Result<std::process::Command> {
    let shims = default_shim_dir();
    shim_install(&ShimInstallArgs {
        dir: Some(shims.clone()),
        tools: Vec::new(),
    })?;
    let explicit_store = std::env::var_os("PROG_DIR").filter(|dir| !dir.is_empty());
    let store = match &explicit_store {
        Some(dir) => PathBuf::from(dir),
        None => project_store_dir(&std::env::current_dir()?),
    };
    std::fs::create_dir_all(&store)?;
    // Per-project stores share one parent; allowing it once covers every
    // project Codex works in during the session.
    let writable = match &explicit_store {
        Some(_) => store.clone(),
        None => store
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| store.clone()),
    };
    let codex = codex_binary(&shims).ok_or_else(|| CoreError::BadArgs {
        operation: "codex".to_string(),
        reason: "codex was not found on PATH; install it or set PROG_CODEX_BIN".to_string(),
    })?;
    let mut command = std::process::Command::new(codex);
    command.args(codex_overrides(args, &writable));
    command.args(&args.args);
    command.env("PATH", shimmed_path(&shims));
    command.env("PROG_DIR", &store);
    if std::env::var_os("PROG_FORMAT").is_none() {
        command.env("PROG_FORMAT", "compact");
    }
    if args.stop_gate {
        command.env("PROG_STOP_GATE", "1");
    }
    Ok(command)
}

/// The `-c` overrides that keep the shims reachable and the store writable.
pub(crate) fn codex_overrides(args: &CodexArgs, writable: &Path) -> Vec<String> {
    if args.keep_codex_config {
        return Vec::new();
    }
    // A JSON string is a valid TOML basic string.
    let root = serde_json::to_string(&writable.to_string_lossy()).unwrap_or_default();
    vec![
        "-c".to_string(),
        "allow_login_shell=false".to_string(),
        "-c".to_string(),
        format!("sandbox_workspace_write.writable_roots=[{root}]"),
    ]
}

fn shimmed_path(shims: &Path) -> OsString {
    let mut entries = vec![shims.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&path).filter(|entry| entry != shims));
    }
    std::env::join_paths(entries).unwrap_or_default()
}

/// `PROG_CODEX_BIN`, else the first executable `codex` on `PATH` outside the
/// shim directory.
fn codex_binary(shims: &Path) -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("PROG_CODEX_BIN").filter(|bin| !bin.is_empty()) {
        return Some(PathBuf::from(bin));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir != shims)
        .map(|dir| dir.join("codex"))
        .find(|candidate| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

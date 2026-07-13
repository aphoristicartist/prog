use std::{fs, path::Path, process::Command};

use prog_core::{
    WorkspaceValidity, capture_workspace, capture_workspace_with_cap, compare_workspace,
};

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "prog@example.test"]);
    git(dir.path(), &["config", "user.name", "prog test"]);
    fs::write(dir.path().join("tracked.txt"), "initial\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "initial"]);
    dir
}

#[test]
fn non_git_workspace_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let state = capture_workspace(dir.path());
    assert!(state.unavailable_reason.is_some());
    assert_eq!(
        compare_workspace(&state, &state).validity,
        WorkspaceValidity::Unknown
    );
}

#[test]
fn dirty_content_and_cap_are_conservative() {
    let repo = repo();
    let clean = capture_workspace(repo.path());
    fs::write(repo.path().join("tracked.txt"), "changed\n").unwrap();
    let changed = capture_workspace(repo.path());
    assert_eq!(
        compare_workspace(&clean, &changed).validity,
        WorkspaceValidity::Changed
    );
    assert!(changed.dirty[0].sha256.as_deref().is_some());

    fs::write(repo.path().join("another.txt"), "new\n").unwrap();
    let capped = capture_workspace_with_cap(repo.path(), 1);
    assert!(capped.dirty_truncated);
    assert_eq!(
        compare_workspace(&changed, &capped).validity,
        WorkspaceValidity::Unknown
    );
}

#[test]
fn linked_worktree_and_sparse_checkout_never_compare_unchanged() {
    let repo = repo();
    let worktree = tempfile::tempdir().unwrap();
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "--detach",
            worktree.path().to_str().unwrap(),
            "HEAD",
        ],
    );
    let primary = capture_workspace(repo.path());
    let linked = capture_workspace(worktree.path());
    assert_eq!(
        compare_workspace(&primary, &linked).validity,
        WorkspaceValidity::Changed
    );

    git(repo.path(), &["sparse-checkout", "init", "--no-cone"]);
    let sparse = capture_workspace(repo.path());
    assert!(sparse.sparse_checkout);
    assert_eq!(
        compare_workspace(&primary, &sparse).validity,
        WorkspaceValidity::Changed
    );
}

#[test]
fn submodule_pointer_or_dirty_state_changes_workspace() {
    let child = repo();
    let parent = repo();
    git(
        parent.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            child.path().to_str().unwrap(),
            "child",
        ],
    );
    git(parent.path(), &["commit", "-qm", "add submodule"]);
    let recorded = capture_workspace(parent.path());
    fs::write(parent.path().join("child/tracked.txt"), "dirty submodule\n").unwrap();
    let dirty = capture_workspace(parent.path());
    assert_eq!(
        compare_workspace(&recorded, &dirty).validity,
        WorkspaceValidity::Changed
    );
    assert_ne!(recorded.submodules, dirty.submodules);
}

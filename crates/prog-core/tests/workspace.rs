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
fn non_git_workspace_is_not_applicable() {
    let dir = tempfile::tempdir().unwrap();
    let state = capture_workspace(dir.path());
    assert!(state.unavailable_reason.is_some());
    assert_eq!(
        compare_workspace(&state, &state).validity,
        WorkspaceValidity::NotApplicable
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
fn git_status_records_staged_rename_deletion_and_untracked_paths() {
    let repo = repo();
    fs::rename(
        repo.path().join("tracked.txt"),
        repo.path().join("renamed.txt"),
    )
    .unwrap();
    git(repo.path(), &["add", "-A"]);
    fs::write(repo.path().join("untracked.txt"), "new\n").unwrap();

    let renamed = capture_workspace(repo.path());
    let rename = renamed
        .dirty
        .iter()
        .find(|entry| entry.status.starts_with('R'))
        .expect("staged rename should be represented");
    assert_eq!(rename.path, "renamed.txt");
    assert_eq!(rename.extra["rename_from"], "tracked.txt");
    assert!(
        renamed
            .dirty
            .iter()
            .any(|entry| entry.path == "untracked.txt")
    );

    git(repo.path(), &["commit", "-qm", "rename"]);
    fs::remove_file(repo.path().join("renamed.txt")).unwrap();
    let deleted = capture_workspace(repo.path());
    let deletion = deleted
        .dirty
        .iter()
        .find(|entry| entry.path == "renamed.txt")
        .expect("deletion should be represented");
    assert!(deletion.status.contains('D'));
    assert!(!deletion.unreadable);
    assert_eq!(deletion.extra["hash_omitted_reason"], "deleted");
    assert_eq!(
        compare_workspace(&deleted, &deleted).validity,
        WorkspaceValidity::Unchanged
    );
}

#[test]
fn incomplete_dirty_file_capture_cannot_prove_workspace_unchanged() {
    let repo = repo();
    fs::write(repo.path().join("too-large.bin"), vec![7u8; 1_048_577]).unwrap();
    let oversized = capture_workspace(repo.path());
    let entry = oversized
        .dirty
        .iter()
        .find(|entry| entry.path == "too-large.bin")
        .unwrap();
    assert!(entry.unreadable);
    assert_eq!(entry.extra["hash_omitted_reason"], "file_exceeds_hash_cap");
    assert_eq!(
        compare_workspace(&oversized, &oversized).validity,
        WorkspaceValidity::Unknown
    );
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_retained_as_an_unknown_dirty_entry_without_following_target() {
    use std::os::unix::fs::symlink;

    let repo = repo();
    let external = tempfile::tempdir().unwrap();
    fs::write(external.path().join("secret.txt"), "outside\n").unwrap();
    symlink(
        external.path().join("secret.txt"),
        repo.path().join("escape"),
    )
    .unwrap();

    let state = capture_workspace(repo.path());
    let entry = state
        .dirty
        .iter()
        .find(|entry| entry.path == "escape")
        .unwrap();
    assert!(entry.unreadable);
    assert_eq!(entry.extra["hash_omitted_reason"], "symlink_not_followed");
    assert_eq!(
        compare_workspace(&state, &state).validity,
        WorkspaceValidity::Unknown
    );
}

#[test]
fn detached_unborn_and_subdirectory_snapshots_are_explicit() {
    let repo = repo();
    let subdirectory = repo.path().join("nested");
    fs::create_dir(&subdirectory).unwrap();
    let from_subdirectory = capture_workspace(&subdirectory);
    assert_eq!(
        from_subdirectory.root.as_deref(),
        repo.path().canonicalize().unwrap().to_str()
    );

    git(repo.path(), &["checkout", "--detach", "-q"]);
    let detached = capture_workspace(repo.path());
    assert!(detached.head.is_some());
    assert!(!detached.unborn_head);

    let unborn = tempfile::tempdir().unwrap();
    git(unborn.path(), &["init", "-q"]);
    let unborn_state = capture_workspace(unborn.path());
    assert!(unborn_state.applicable);
    assert!(unborn_state.unborn_head);
    assert!(unborn_state.head.is_none());
}

#[test]
fn capture_time_is_not_a_workspace_change() {
    let repo = repo();
    let captured = capture_workspace(repo.path());
    let mut later = captured.clone();
    later.captured_at = "2030-01-01T00:00:00Z".to_string();
    assert_eq!(
        compare_workspace(&captured, &later).validity,
        WorkspaceValidity::Unchanged
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

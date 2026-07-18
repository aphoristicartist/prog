use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Extra;

const DEFAULT_DIRTY_FILE_CAP: usize = 128;
const MAX_HASH_BYTES: u64 = 1_048_576;
const WORKSPACE_HASH_ALGORITHM: &str = "sha256-v1-max-1048576-no-symlink";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceState {
    pub captured_at: String,
    pub algorithm: String,
    #[serde(default)]
    pub applicable: bool,
    pub root: Option<String>,
    pub git_dir: Option<String>,
    pub head: Option<String>,
    #[serde(default)]
    pub unborn_head: bool,
    pub sparse_checkout: bool,
    pub dirty: Vec<WorkspacePathState>,
    /// True when Git reported a dirty entry that could not be represented
    /// safely. This is distinct from `dirty_truncated`: either condition
    /// means the workspace snapshot cannot prove unchanged state.
    #[serde(default)]
    pub dirty_incomplete: bool,
    pub dirty_truncated: bool,
    pub submodules: Vec<SubmoduleState>,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct WorkspacePathState {
    pub path: String,
    pub status: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub unreadable: bool,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SubmoduleState {
    pub path: String,
    pub commit: String,
    pub dirty: bool,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceValidity {
    Unchanged,
    Changed,
    NotApplicable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceComparison {
    pub validity: WorkspaceValidity,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

pub fn capture_workspace(path: impl AsRef<Path>) -> WorkspaceState {
    capture_workspace_with_cap(path.as_ref(), DEFAULT_DIRTY_FILE_CAP)
}

pub fn capture_workspace_with_cap(path: &Path, dirty_file_cap: usize) -> WorkspaceState {
    let root = match git(path, ["rev-parse", "--show-toplevel"]) {
        Ok(root) => PathBuf::from(root),
        Err(reason) => return unavailable(reason),
    };
    let git_dir = git(&root, ["rev-parse", "--git-dir"]).ok().map(|value| {
        let value = PathBuf::from(value);
        if value.is_absolute() {
            value
        } else {
            root.join(value)
        }
        .to_string_lossy()
        .into_owned()
    });
    let head = git(&root, ["rev-parse", "HEAD"]).ok();
    let unborn_head = head.is_none() && git(&root, ["symbolic-ref", "--quiet", "HEAD"]).is_ok();
    let sparse_checkout =
        git(&root, ["config", "--bool", "core.sparseCheckout"]).is_ok_and(|value| value == "true");
    let mut submodules: Vec<SubmoduleState> = git(&root, ["submodule", "status", "--recursive"])
        .ok()
        .map(|output| output.lines().filter_map(parse_submodule).collect())
        .unwrap_or_default();
    let status = match git_bytes(
        &root,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    ) {
        Ok(status) => status,
        Err(reason) => return unavailable(reason),
    };
    let mut dirty = Vec::new();
    let mut dirty_incomplete = false;
    let mut dirty_truncated = false;
    let mut records = status
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if record.len() < 4 {
            dirty_incomplete = true;
            continue;
        }
        if dirty.len() >= dirty_file_cap {
            dirty_truncated = true;
            break;
        }
        let status = String::from_utf8_lossy(&record[..2]).into_owned();
        let relative = String::from_utf8_lossy(&record[3..]).into_owned();
        let rename_from = if status.contains('R') || status.contains('C') {
            match records.next() {
                Some(previous) => Some(String::from_utf8_lossy(previous).into_owned()),
                None => {
                    dirty_incomplete = true;
                    None
                }
            }
        } else {
            None
        };
        if !workspace_relative_path_is_safe(&relative)
            || rename_from
                .as_deref()
                .is_some_and(|path| !workspace_relative_path_is_safe(path))
        {
            dirty_incomplete = true;
            continue;
        }
        let file = root.join(&relative);
        let (sha256, unreadable, reason) = if status.contains('D') {
            // Git's index/worktree status is the evidence for a deletion; no
            // file content exists to hash, and treating that known state as
            // unreadable would incorrectly make two deleted snapshots
            // incomparable.
            (None, false, Some("deleted"))
        } else if submodules
            .iter()
            .any(|submodule| submodule.path == relative)
        {
            // `git submodule status` is the authoritative, bounded state for
            // a submodule directory. It carries pointer and dirty facts, so
            // never follow the directory or classify it as an unreadable file.
            (None, false, Some("submodule_state"))
        } else {
            hash_workspace_file(&file)
        };
        let mut extra = Extra::new();
        if let Some(reason) = reason {
            extra.insert("hash_omitted_reason".to_string(), reason.into());
        }
        if let Some(rename_from) = rename_from {
            extra.insert("rename_from".to_string(), rename_from.into());
        }
        dirty.push(WorkspacePathState {
            path: relative,
            status,
            sha256,
            unreadable,
            extra,
        });
    }
    for submodule in &mut submodules {
        submodule.dirty |= dirty.iter().any(|entry| {
            entry.path == submodule.path || entry.path.starts_with(&format!("{}/", submodule.path))
        });
    }
    WorkspaceState {
        captured_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        algorithm: WORKSPACE_HASH_ALGORITHM.to_string(),
        applicable: true,
        root: Some(root.to_string_lossy().into_owned()),
        git_dir,
        head,
        unborn_head,
        sparse_checkout,
        dirty,
        dirty_incomplete,
        dirty_truncated,
        submodules,
        unavailable_reason: None,
        extra: Extra::new(),
    }
}

pub fn compare_workspace(
    captured: &WorkspaceState,
    current: &WorkspaceState,
) -> WorkspaceComparison {
    let mut reasons = Vec::new();
    if !captured.applicable && !current.applicable {
        return WorkspaceComparison {
            validity: WorkspaceValidity::NotApplicable,
            reasons: vec!["workspace capture is not applicable outside a Git worktree".to_string()],
            extra: Extra::new(),
        };
    }
    if !captured.applicable || !current.applicable {
        reasons.push("workspace applicability changed".to_string());
    }
    if captured.unavailable_reason.is_some() || current.unavailable_reason.is_some() {
        reasons.push("workspace capture unavailable".to_string());
    }
    if captured.dirty_truncated || current.dirty_truncated {
        reasons.push("dirty-file capture reached its cap".to_string());
    }
    if captured.dirty_incomplete || current.dirty_incomplete {
        reasons.push("workspace capture omitted an unsafe Git status entry".to_string());
    }
    if captured.dirty.iter().any(|entry| entry.unreadable)
        || current.dirty.iter().any(|entry| entry.unreadable)
    {
        reasons.push(
            "workspace capture omitted unreadable or unstable dirty-file content".to_string(),
        );
    }
    if reasons.is_empty() && workspace_equivalent(captured, current) {
        return WorkspaceComparison {
            validity: WorkspaceValidity::Unchanged,
            reasons,
            extra: Extra::new(),
        };
    }
    if !reasons.is_empty() {
        return WorkspaceComparison {
            validity: WorkspaceValidity::Unknown,
            reasons,
            extra: Extra::new(),
        };
    }
    for (label, before, after) in [
        ("worktree root", &captured.root, &current.root),
        ("Git directory", &captured.git_dir, &current.git_dir),
        ("HEAD", &captured.head, &current.head),
    ] {
        if before != after {
            reasons.push(format!("{label} changed"));
        }
    }
    if captured.sparse_checkout != current.sparse_checkout {
        reasons.push("sparse-checkout mode changed".to_string());
    }
    if captured.dirty != current.dirty {
        reasons.push("dirty workspace paths changed".to_string());
    }
    if captured.submodules != current.submodules {
        reasons.push("submodule state changed".to_string());
    }
    WorkspaceComparison {
        validity: WorkspaceValidity::Changed,
        reasons,
        extra: Extra::new(),
    }
}

fn workspace_equivalent(captured: &WorkspaceState, current: &WorkspaceState) -> bool {
    captured.applicable == current.applicable
        && captured.algorithm == current.algorithm
        && captured.root == current.root
        && captured.git_dir == current.git_dir
        && captured.head == current.head
        && captured.unborn_head == current.unborn_head
        && captured.sparse_checkout == current.sparse_checkout
        && captured.dirty == current.dirty
        && captured.dirty_incomplete == current.dirty_incomplete
        && captured.dirty_truncated == current.dirty_truncated
        && captured.submodules == current.submodules
        && captured.unavailable_reason == current.unavailable_reason
}

fn unavailable(reason: String) -> WorkspaceState {
    WorkspaceState {
        captured_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        algorithm: WORKSPACE_HASH_ALGORITHM.to_string(),
        applicable: false,
        root: None,
        git_dir: None,
        head: None,
        unborn_head: false,
        sparse_checkout: false,
        dirty: Vec::new(),
        dirty_incomplete: false,
        dirty_truncated: false,
        submodules: Vec::new(),
        unavailable_reason: Some(reason),
        extra: Extra::new(),
    }
}

fn hash_workspace_file(path: &Path) -> (Option<String>, bool, Option<&'static str>) {
    hash_workspace_file_with_after_read(path, || {})
}

fn hash_workspace_file_with_after_read(
    path: &Path,
    after_read: impl FnOnce(),
) -> (Option<String>, bool, Option<&'static str>) {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return (None, true, Some("unreadable")),
    };
    if metadata.file_type().is_symlink() {
        return (None, true, Some("symlink_not_followed"));
    }
    if !metadata.is_file() {
        return (None, true, Some("not_regular_file"));
    }
    if metadata.len() > MAX_HASH_BYTES {
        return (None, true, Some("file_exceeds_hash_cap"));
    }
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return (None, true, Some("unreadable")),
    };
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if file
        .by_ref()
        .take(MAX_HASH_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return (None, true, Some("unreadable"));
    }
    if bytes.len() as u64 > MAX_HASH_BYTES {
        return (None, true, Some("file_changed_during_hash"));
    }
    after_read();
    let after = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return (None, true, Some("file_changed_during_hash")),
    };
    // Metadata is only an instability detector; equality is never used as a
    // substitute for the content digest. A changed size, type, or observable
    // modification time means the digest cannot be assigned to one stable
    // workspace state.
    if after.file_type() != metadata.file_type()
        || after.len() != metadata.len()
        || (metadata.modified().ok().zip(after.modified().ok()))
            .is_some_and(|(before, after)| before != after)
    {
        return (None, true, Some("file_changed_during_hash"));
    }
    (
        Some(format!("sha256:{:x}", Sha256::digest(bytes))),
        false,
        None,
    )
}

fn workspace_relative_path_is_safe(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

fn git<const N: usize>(path: &Path, args: [&str; N]) -> std::result::Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_bytes<const N: usize>(path: &Path, args: [&str; N]) -> std::result::Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(output.stdout)
}

fn parse_submodule(line: &str) -> Option<SubmoduleState> {
    let marker = line.chars().next()?;
    let mut fields = line[1..].split_whitespace();
    let commit = fields.next()?.to_string();
    let path = fields.next()?.to_string();
    Some(SubmoduleState {
        path,
        commit,
        // `git` output is trimmed by the command helper, so a clean leading
        // space may be absent. Status markers are non-hex (`+`, `-`, `U`).
        dirty: !marker.is_ascii_hexdigit() && marker != ' ',
        extra: Extra::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_read_mutation_is_explicitly_unstable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("racy.txt");
        fs::write(&path, "before").unwrap();

        let result = hash_workspace_file_with_after_read(&path, || {
            fs::write(&path, "after with a different length").unwrap();
        });

        assert_eq!(result, (None, true, Some("file_changed_during_hash")));
    }

    #[test]
    fn unsafe_relative_paths_are_rejected_before_filesystem_access() {
        for path in ["../outside", "/absolute", "", "nested/../../outside"] {
            assert!(!workspace_relative_path_is_safe(path), "{path}");
        }
        for path in ["src/lib.rs", "nested/./file", "unicodé/ファイル.rs"] {
            assert!(workspace_relative_path_is_safe(path), "{path}");
        }
    }
}

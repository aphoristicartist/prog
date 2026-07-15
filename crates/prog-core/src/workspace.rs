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
    let status = match git_bytes(
        &root,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    ) {
        Ok(status) => status,
        Err(reason) => return unavailable(reason),
    };
    let mut dirty = Vec::new();
    let mut dirty_truncated = false;
    for record in status
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        if record.len() < 4 {
            continue;
        }
        if dirty.len() >= dirty_file_cap {
            dirty_truncated = true;
            break;
        }
        let status = String::from_utf8_lossy(&record[..2]).into_owned();
        let relative = String::from_utf8_lossy(&record[3..]).into_owned();
        let file = root.join(&relative);
        let (sha256, unreadable, reason) = hash_workspace_file(&file);
        let mut extra = Extra::new();
        if let Some(reason) = reason {
            extra.insert("hash_omitted_reason".to_string(), reason.into());
        }
        dirty.push(WorkspacePathState {
            path: relative,
            status,
            sha256,
            unreadable,
            extra,
        });
    }
    let mut submodules: Vec<SubmoduleState> = git(&root, ["submodule", "status", "--recursive"])
        .ok()
        .map(|output| output.lines().filter_map(parse_submodule).collect())
        .unwrap_or_default();
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
        dirty_truncated: false,
        submodules: Vec::new(),
        unavailable_reason: Some(reason),
        extra: Extra::new(),
    }
}

fn hash_workspace_file(path: &Path) -> (Option<String>, bool, Option<&'static str>) {
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
    (
        Some(format!("sha256:{:x}", Sha256::digest(bytes))),
        false,
        None,
    )
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

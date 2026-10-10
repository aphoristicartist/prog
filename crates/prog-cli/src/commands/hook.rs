//! Claude Code hook adapter (#290): one tested entry point for the plugin's
//! `SessionStart`, `PostToolUse`, `PostToolUseFailure`, and `Stop` hooks.
//!
//! It never modifies a tool's output. It activates PATH shims, injects the
//! compact receipt of new captures as `additionalContext`, and, only when the
//! user enables it, blocks the end of a turn once while this session's latest
//! test run is failing, unproven, or older than the workspace.
//!
//! Every unexpected condition fails open: no output, exit 0.

use std::{fs, io::Read};

use crate::commands::{
    receipt::receipt_report,
    shim::{PENDING_RECEIPTS_DIR, default_shim_dir, project_store_dir, shim_env, shim_install},
};
use crate::*;
use prog_core::{ReceiptBaseline, ReceiptSubject, ReceiptVerdict, TestPopulation, build_receipt};

const MAX_HOOK_INPUT_BYTES: u64 = 1024 * 1024;
const SESSIONS_DIR: &str = "claude-sessions";

/// Run one Claude Code hook event read from stdin. Returns the JSON to print,
/// if any.
pub(crate) fn claude_code_hook(input: &str) -> Option<Value> {
    let input: Value = serde_json::from_str(input).ok()?;
    let event = input.get("hook_event_name")?.as_str()?;
    let project = std::env::var_os("CLAUDE_PROJECT_DIR")
        .map(PathBuf::from)
        .or_else(|| input.get("cwd").and_then(Value::as_str).map(PathBuf::from))?;
    // An explicit PROG_DIR wins; otherwise each project gets a store outside
    // its tree so captures never count as workspace changes.
    let explicit_store = std::env::var_os("PROG_DIR").filter(|dir| !dir.is_empty());
    let store_dir = explicit_store
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| project_store_dir(&project));
    match event {
        "SessionStart" => {
            session_start(&input, &project, &store_dir, explicit_store.is_some());
            None
        }
        "PostToolUse" | "PostToolUseFailure" if input["tool_name"] == "Bash" => {
            pending_receipts(&store_dir).map(|text| {
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": event,
                        "additionalContext": text,
                    }
                })
            })
        }
        "Stop" if stop_gate_enabled() => stop_gate(&input, &store_dir)
            .map(|reason| json!({"decision": "block", "reason": reason})),
        _ => None,
    }
}

pub(crate) fn read_hook_input() -> String {
    let mut input = String::new();
    let _ = std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES)
        .read_to_string(&mut input);
    input
}

fn session_start(input: &Value, project: &Path, store_dir: &Path, explicit_store: bool) {
    let shims = default_shim_dir();
    if shim_install(&ShimInstallArgs {
        dir: Some(shims.clone()),
        tools: Vec::new(),
    })
    .is_err()
    {
        return;
    }
    if let Some(env_file) = std::env::var_os("CLAUDE_ENV_FILE") {
        let env = shim_env(&ShimEnvArgs {
            dir: Some(shims),
            // Keep a user's explicit PROG_DIR; otherwise point shims at the
            // per-project store.
            project: (!explicit_store).then(|| project.to_path_buf()),
            shell: true,
        });
        if let Some(exports) = env["exports"].as_str()
            && let Ok(mut file) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(env_file)
        {
            use std::io::Write;
            let _ = file.write_all(exports.as_bytes());
        }
    }
    if let Some(session) = input.get("session_id").and_then(Value::as_str)
        && is_safe_name(session)
    {
        let sessions = store_dir.join(SESSIONS_DIR);
        if fs::create_dir_all(&sessions).is_ok() {
            let _ = fs::write(
                sessions.join(session),
                Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            );
        }
    }
}

fn pending_receipts(store_dir: &Path) -> Option<String> {
    // Do not create a store just to find out nothing is pending.
    let has_pending = fs::read_dir(store_dir.join(PENDING_RECEIPTS_DIR))
        .ok()?
        .next()
        .is_some();
    if !has_pending {
        return None;
    }
    let store = Store::open_silently(store_dir).ok()?;
    let report = receipt_report(
        &store,
        &ReceiptArgs {
            observation: None,
            since: None,
            pending: true,
        },
    )
    .ok()?;
    (!report.text.is_empty()).then_some(report.text)
}

fn stop_gate_enabled() -> bool {
    ["PROG_STOP_GATE", "CLAUDE_PLUGIN_OPTION_STOP_GATE"]
        .iter()
        .filter_map(std::env::var_os)
        .any(|value| matches!(value.to_str(), Some("1" | "true")))
}

/// The reason to keep working, or `None` to let the turn end. Considers only
/// test runs captured during this session, and blocks at most once in a row
/// (`stop_hook_active`), so the agent can always explain and finish.
fn stop_gate(input: &Value, store_dir: &Path) -> Option<String> {
    if input["stop_hook_active"].as_bool() == Some(true) {
        return None;
    }
    let session = input.get("session_id")?.as_str()?;
    if !is_safe_name(session) {
        return None;
    }
    let started = fs::read_to_string(store_dir.join(SESSIONS_DIR).join(session)).ok()?;
    let started = chrono::DateTime::parse_from_rfc3339(started.trim()).ok()?;
    let store = Store::open_silently(store_dir).ok()?;
    // Test runs captured this session; each command's reruns form a lineage
    // chain, and only the head of every chain says where things stand now.
    let runs = store
        .list_observations(100)
        .ok()?
        .observations
        .into_iter()
        .filter(|observation| {
            chrono::DateTime::parse_from_rfc3339(&observation.captured_at)
                .is_ok_and(|captured| captured >= started)
        })
        .filter_map(|observation| {
            let payload = store.get_payload(&observation.payload_hash).ok()??;
            TestPopulation::from_payload(payload.as_value())?;
            Some((observation, payload))
        })
        .collect::<Vec<_>>();
    let superseded = runs
        .iter()
        .filter_map(|(observation, _)| observation.lineage.supersedes_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let heads = runs
        .iter()
        .filter(|(observation, _)| !superseded.contains(&observation.observation_id))
        .collect::<Vec<_>>();
    for (observation, payload) in &heads {
        let receipt = receipt_for_head(&store, observation, payload.as_value());
        let lead = match receipt.verdict {
            ReceiptVerdict::Failing => "the latest test run is failing",
            ReceiptVerdict::NotProven => {
                "the latest test run does not prove the earlier failure fixed"
            }
            ReceiptVerdict::Fixed | ReceiptVerdict::Clean => continue,
        };
        return Some(format!(
            "prog stop gate: {lead}. Fix it, or tell the user plainly why it is still open.\n{}",
            receipt.text
        ));
    }
    // Every chain is green: make sure nothing changed after the newest run.
    let (newest, _) = heads.first()?;
    let captured_workspace = newest.workspace_state.as_ref()?;
    if !captured_workspace.applicable {
        return None;
    }
    let current = captured_workspace
        .root
        .as_deref()
        .map(prog_core::capture_workspace)?;
    let comparison = prog_core::compare_workspace(captured_workspace, &current);
    (comparison.validity == prog_core::WorkspaceValidity::Changed).then(|| {
        "prog stop gate: files changed after the last passing test run; rerun the tests (or tell the user they were not rerun) before finishing.".to_string()
    })
}

fn receipt_for_head(
    store: &Store,
    observation: &prog_core::ObservationRecord,
    payload: &Value,
) -> prog_core::Receipt {
    let previous = observation
        .lineage
        .supersedes_id
        .as_deref()
        .and_then(|id| store.get_observation(id).ok().flatten())
        .and_then(|previous| {
            let previous_payload = store.get_payload(&previous.payload_hash).ok()??;
            let delta = compare_observation_ids(
                store,
                &previous.observation_id,
                &observation.observation_id,
            )
            .ok()?;
            Some((previous, previous_payload, delta))
        });
    build_receipt(
        ReceiptSubject {
            observation,
            payload,
            cursor: None,
        },
        previous
            .as_ref()
            .map(|(previous, previous_payload, delta)| ReceiptBaseline {
                observation: previous,
                payload: previous_payload.as_value(),
                delta,
            }),
    )
}

fn is_safe_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

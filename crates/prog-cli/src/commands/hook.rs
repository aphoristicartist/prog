//! Claude Code hook adapter (#290): one tested entry point for the plugin's
//! `SessionStart`, `PostToolUse`, `PostToolUseFailure`, and `Stop` hooks.
//!
//! It never modifies a tool's output. It activates PATH shims, injects the
//! compact receipt of new captures as `additionalContext`, and, only when the
//! user enables it, blocks the end of a turn once while this session's newest
//! test run is failing, unproven, or older than the files it tested, or its
//! newest build or lint run is failing. It never blocks on missing evidence,
//! and stands aside when the final message already says verification is open.
//!
//! Every unexpected condition fails open: no output, exit 0.

use std::{
    fs,
    io::{Read, Write},
};

use crate::commands::{
    receipt::{Baseline, comparison_baseline, receipt_report},
    shim::{PENDING_RECEIPTS_DIR, default_shim_dir, project_store_dir, shim_env, shim_install},
};
use crate::*;
use prog_core::{ReceiptSubject, ReceiptVerdict, TestPopulation, build_receipt};

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
            let _ = file.write_all(exports.as_bytes());
            // Claude reads prog's responses; scripts that need the full
            // contract can still pass `--format full`. A user's choice wins.
            if std::env::var_os("PROG_FORMAT").is_none() {
                let _ = file.write_all(b"export PROG_FORMAT=compact\n");
            }
        }
    }
    // Resume and compaction start the same session again. Keep the first
    // start, or test runs from before a compaction would escape the gate.
    if let Some(session) = input.get("session_id").and_then(Value::as_str)
        && is_safe_name(session)
    {
        let sessions = store_dir.join(SESSIONS_DIR);
        if fs::create_dir_all(&sessions).is_ok()
            && let Ok(mut marker) = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(sessions.join(session))
        {
            // The workspace as the session found it lets the stop gate tell
            // "nothing to test" from "changed files, never tested".
            let workspace = prog_core::capture_workspace(project);
            let marker_value = json!({
                "started_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                "workspace": workspace.applicable.then_some(workspace),
            });
            let _ = marker.write_all(marker_value.to_string().as_bytes());
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

/// The reason to keep working, or `None` to let the turn end.
///
/// Only test, build, and lint runs captured during this session count, and
/// only the newest of them decides: an older run of a different invocation
/// (`cargo test -p a adds`) is superseded by a later broader one (`cargo
/// test`) even though the store keeps them in separate chains. The gate never
/// blocks on missing evidence (no run captured): that misfires whenever tests
/// ran outside prog's shims. A failing build or lint run blocks while it is
/// the newest of its kind. It blocks at most once in a row
/// (`stop_hook_active`), and not at all when the final message already tells
/// the user the tests are failing, unproven, or were not run.
fn stop_gate(input: &Value, store_dir: &Path) -> Option<String> {
    if input["stop_hook_active"].as_bool() == Some(true) {
        return None;
    }
    if input
        .get("last_assistant_message")
        .and_then(Value::as_str)
        .is_some_and(discloses_open_verification)
    {
        return None;
    }
    let session = input.get("session_id")?.as_str()?;
    if !is_safe_name(session) {
        return None;
    }
    let marker = fs::read_to_string(store_dir.join(SESSIONS_DIR).join(session)).ok()?;
    let (started, _) = parse_session_marker(&marker)?;
    let store = Store::open_silently(store_dir).ok()?;
    let mut runs = store
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
            let kind = RunKind::of(payload.as_value());
            Some((observation, payload, kind))
        })
        .collect::<Vec<_>>();
    // `captured_at` has one-second resolution; the run's own end time orders
    // runs that finished within the same second.
    runs.sort_by(|(left, left_payload, _), (right, right_payload, _)| {
        run_end(right, right_payload.as_value()).cmp(&run_end(left, left_payload.as_value()))
    });

    // The newest run of each kind is what stands now. A lint run is
    // superseded only by a newer lint run; a build run also by a newer test
    // run, which compiles the same code.
    let newest = |kind: RunKind| runs.iter().position(|(_, _, run_kind)| *run_kind == kind);
    let newest_test = newest(RunKind::Test);
    for (kind, label) in [(RunKind::Lint, "lint"), (RunKind::Build, "build")] {
        let Some(index) = newest(kind) else {
            continue;
        };
        if kind == RunKind::Build && newest_test.is_some_and(|test| test < index) {
            continue;
        }
        let (observation, payload, _) = &runs[index];
        let receipt = receipt_for(&store, observation, payload.as_value());
        if receipt.verdict == ReceiptVerdict::Failing {
            return Some(format!(
                "prog stop gate: the latest {label} run is failing. Fix it, or tell the user plainly why it is still open.\n{}",
                receipt.text
            ));
        }
    }
    let (observation, payload, _) = &runs[newest_test?];
    let receipt = receipt_for(&store, observation, payload.as_value());
    let lead = match receipt.verdict {
        ReceiptVerdict::Failing => Some("the latest test run is failing"),
        ReceiptVerdict::NotProven => {
            Some("the latest test run does not prove the earlier failure fixed")
        }
        ReceiptVerdict::Fixed | ReceiptVerdict::Clean => None,
    };
    if let Some(lead) = lead {
        return Some(format!(
            "prog stop gate: {lead}. Fix it, or tell the user plainly why it is still open.\n{}",
            receipt.text
        ));
    }
    // The newest test run passed: block only if a file's content changed
    // after it. Staging or committing what was tested changes no content.
    let captured_workspace = observation.workspace_state.as_ref()?;
    if !captured_workspace.applicable {
        return None;
    }
    let current = captured_workspace
        .root
        .as_deref()
        .map(prog_core::capture_workspace)?;
    let comparison = prog_core::compare_workspace_content(captured_workspace, &current);
    (comparison.validity == prog_core::WorkspaceValidity::Changed).then(|| {
        "prog stop gate: files changed after the last passing test run; rerun the tests (or tell the user they were not rerun) before finishing.".to_string()
    })
}

/// Whether a captured run executes tests, or only builds or lints code.
/// A passing build or lint run is no evidence that tests pass, so it never
/// satisfies the gate's staleness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Test,
    Build,
    Lint,
}

impl RunKind {
    fn of(payload: &Value) -> Self {
        let argv = payload
            .pointer("/command/argv")
            .and_then(Value::as_array)
            .map(|argv| argv.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        let program = argv
            .first()
            .and_then(|program| Path::new(program).file_name())
            .and_then(|name| name.to_str());
        let subcommand = argv
            .iter()
            .skip(1)
            .find(|arg| !arg.starts_with('+'))
            .copied();
        match (program, subcommand) {
            (Some("rustc"), _) | (Some("cargo"), Some("build" | "check" | "rustc")) => Self::Build,
            (Some("cargo"), Some("clippy")) => Self::Lint,
            _ => Self::Test,
        }
    }
}

/// When a run ended, for ordering runs captured within the same second.
fn run_end(observation: &prog_core::ObservationRecord, payload: &Value) -> (String, String) {
    let ended = payload
        .pointer("/command/ended_at")
        .and_then(Value::as_str)
        .and_then(|ended| chrono::DateTime::parse_from_rfc3339(ended).ok())
        .or_else(|| chrono::DateTime::parse_from_rfc3339(&observation.captured_at).ok())
        .map(|time| {
            time.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Nanos, true)
        })
        .unwrap_or_default();
    (ended, observation.observation_id.clone())
}

/// Phrases with which a final message tells the user that verification is
/// still open. Matched case-insensitively as plain substrings; a match makes
/// the gate stand aside, because the user has already been told.
const OPEN_VERIFICATION_PHRASES: &[&str] = &[
    "still fail",
    "still broken",
    "still red",
    "not fixed",
    "isn't fixed",
    "is not fixed",
    "remain failing",
    "remains failing",
    "failures remain",
    "not proven",
    "unproven",
    "could not verify",
    "couldn't verify",
    "cannot verify",
    "can't verify",
    "not verified",
    "unverified",
    "did not run",
    "didn't run",
    "have not run",
    "haven't run",
    "has not been run",
    "have not been run",
    "were not run",
    "was not run",
    "wasn't run",
    "weren't run",
    "not rerun",
    "did not rerun",
    "didn't rerun",
    "without running",
    "not run the tests",
    "not run any tests",
];

/// Phrases are kept narrow so that a claim of success cannot match one: no
/// bare "tests are failing" (it also matches "no tests are failing") and no
/// "tests fail" (it also matches "the tests failed before the fix").
fn discloses_open_verification(message: &str) -> bool {
    let message = message
        .to_lowercase()
        .replace('\u{2019}', "'")
        // "did not run into problems" is not "did not run".
        .replace(" run into", " ran-into");
    OPEN_VERIFICATION_PHRASES
        .iter()
        .any(|phrase| message.contains(phrase))
}

/// Session start time and, when the project is a Git worktree, the
/// workspace at that time. Markers written before the workspace was recorded
/// hold only an RFC 3339 timestamp.
fn parse_session_marker(
    marker: &str,
) -> Option<(
    chrono::DateTime<chrono::FixedOffset>,
    Option<prog_core::WorkspaceState>,
)> {
    let marker = marker.trim();
    if !marker.starts_with('{') {
        return Some((chrono::DateTime::parse_from_rfc3339(marker).ok()?, None));
    }
    let value: Value = serde_json::from_str(marker).ok()?;
    let started = chrono::DateTime::parse_from_rfc3339(value.get("started_at")?.as_str()?).ok()?;
    let workspace = value
        .get("workspace")
        .filter(|workspace| !workspace.is_null())
        .and_then(|workspace| serde_json::from_value(workspace.clone()).ok());
    Some((started, workspace))
}

/// The receipt for a run, compared the same way its PostToolUse receipt was.
fn receipt_for(
    store: &Store,
    observation: &prog_core::ObservationRecord,
    payload: &Value,
) -> prog_core::Receipt {
    let baseline = comparison_baseline(store, observation).ok().flatten();
    build_receipt(
        ReceiptSubject {
            observation,
            payload,
            cursor: None,
        },
        baseline.as_ref().map(Baseline::as_receipt_baseline),
    )
}

fn is_safe_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

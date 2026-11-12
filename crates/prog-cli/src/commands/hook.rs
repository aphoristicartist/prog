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
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
};

use crate::commands::{
    final_message, harness_log,
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
    host_hook(input, Host::ClaudeCode)
}

/// Run one Codex hook event read from stdin (#304). Codex speaks the same
/// event model and JSON shape; it has no `CLAUDE_ENV_FILE`, so the `prog harness
/// codex` launcher, not SessionStart, puts the shims on `PATH`.
pub(crate) fn codex_hook(input: &str) -> Option<Value> {
    host_hook(input, Host::Codex)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    ClaudeCode,
    Codex,
}

impl Host {
    fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }

    fn agent(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude",
            Self::Codex => "Codex",
        }
    }
}

fn host_hook(input: &str, host: Host) -> Option<Value> {
    let input: Value = serde_json::from_str(input).ok()?;
    let event = input.get("hook_event_name")?.as_str()?;
    let project = match host {
        Host::ClaudeCode => std::env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from),
        Host::Codex => None,
    }
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
            session_start(&input, &project, &store_dir, explicit_store.is_some(), host);
            None
        }
        "PostToolUse" | "PostToolUseFailure" if input["tool_name"] == "Bash" => {
            post_tool_use(&input, &store_dir, host).map(|text| {
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": event,
                        "additionalContext": text,
                    }
                })
            })
        }
        "Stop" => stop_event(&input, &store_dir, host),
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

fn session_start(
    input: &Value,
    project: &Path,
    store_dir: &Path,
    explicit_store: bool,
    host: Host,
) {
    let shims = default_shim_dir();
    if shim_install(&ShimInstallArgs {
        dir: Some(shims.clone()),
        tools: Vec::new(),
    })
    .is_err()
    {
        return;
    }
    if let Some(env_file) = std::env::var_os("CLAUDE_ENV_FILE").filter(|_| host == Host::ClaudeCode)
    {
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

/// Receipts for runs the shims captured during this tool call, or a notice
/// that a test command ran without a capture. Logged either way.
fn post_tool_use(input: &Value, store_dir: &Path, host: Host) -> Option<String> {
    let receipts = pending_receipts(store_dir);
    let session = input.get("session_id").and_then(Value::as_str);
    let command = input
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Claude Code can run more than one hook for one tool call (PostToolUse
    // and PostToolUseFailure); the first consumes the receipts, and the
    // others must not then report the command as uncaptured.
    let sibling = session
        .filter(|session| is_safe_name(session))
        .is_some_and(|session| repeated_tool_call(store_dir, session, input, command));
    let uncaptured = (receipts.is_none() && !sibling)
        .then(|| test_route(command))
        .flatten();
    if let (Some(_), Some(session)) = (&uncaptured, session.filter(|session| is_safe_name(session)))
    {
        // Lets the stop gate tell that its newest captured run is not the
        // newest test run.
        let sessions = store_dir.join(SESSIONS_DIR);
        if fs::create_dir_all(&sessions).is_ok() {
            let _ = fs::write(
                sessions.join(format!("{session}{UNCAPTURED_SUFFIX}")),
                Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
            );
        }
    }
    // Only test-relevant tool calls are logged, which keeps the log small.
    if receipts.is_some() || uncaptured.is_some() {
        harness_log::append(
            store_dir,
            host.name(),
            "post_tool_use",
            json!({
                "session": session.map(harness_log::short_hash),
                "captured": receipts.as_ref().map(|(receipts, _)| receipts.clone()).unwrap_or_default(),
                "uncaptured_runner": uncaptured,
            }),
        );
    }
    match (receipts, uncaptured) {
        (Some((_, text)), _) => Some(text),
        (None, Some(route)) => Some(format!(
            "prog: no capture for this test command (`{route}`): it ran outside prog's shims, or the run could not be recorded, so there is no verdict"
        )),
        (None, None) => None,
    }
}

const UNCAPTURED_SUFFIX: &str = ".uncaptured";
const LAST_TOOL_SUFFIX: &str = ".last-tool";
/// Without a `tool_use_id`, hooks for the same command this close together
/// are taken to be for one tool call.
const SIBLING_HOOK_WINDOW_MS: i64 = 10_000;

/// Whether an earlier hook already handled this tool call, recording this
/// one either way. Keyed by `tool_use_id` (sent by Claude Code and Codex),
/// else by the command within a short window.
fn repeated_tool_call(store_dir: &Path, session: &str, input: &Value, command: &str) -> bool {
    let sessions = store_dir.join(SESSIONS_DIR);
    let path = sessions.join(format!("{session}{LAST_TOOL_SUFFIX}"));
    let id = input.get("tool_use_id").and_then(Value::as_str);
    let key = match id {
        Some(id) => format!("id:{id}"),
        None => format!("command:{}", harness_log::short_hash(command)),
    };
    let now = Utc::now();
    let previous = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let repeated = previous.is_some_and(|previous| {
        previous["key"].as_str() == Some(key.as_str())
            && (id.is_some()
                || previous["at"]
                    .as_str()
                    .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                    .is_some_and(|at| {
                        (now - at.with_timezone(&Utc)).num_milliseconds() <= SIBLING_HOOK_WINDOW_MS
                    }))
    });
    if fs::create_dir_all(&sessions).is_ok() {
        let _ = fs::write(
            &path,
            json!({"key": key, "at": now.to_rfc3339_opts(SecondsFormat::Millis, true)}).to_string(),
        );
    }
    repeated
}

/// Pending receipts as log records (verdict, provider, invocation hash) and
/// their joined text.
fn pending_receipts(store_dir: &Path) -> Option<(Vec<Value>, String)> {
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
    if report.text.is_empty() {
        return None;
    }
    let records = report
        .receipts
        .iter()
        .map(|receipt| {
            let observation = store.get_observation(&receipt.observation_id).ok().flatten();
            json!({
                "verdict": receipt.verdict,
                "provider": observation.as_ref().and_then(|observation| observation.provider.clone()),
                "invocation": observation
                    .as_ref()
                    .map(|observation| harness_log::short_hash(&observation.invocation_fingerprint)),
            })
        })
        .collect();
    Some((records, report.text))
}

/// The test runner a shell command invokes, read from its words: used only
/// to say that no capture happened, never to produce a verdict, so a miss
/// costs nothing and a false match only adds a notice.
fn test_route(command: &str) -> Option<String> {
    command
        .split(['\n', ';', '|', '&', '(', ')'])
        .find_map(|segment| route_of_words(&segment.split_whitespace().collect::<Vec<_>>()))
}

fn route_of_words(words: &[&str]) -> Option<String> {
    let mut words = words
        .iter()
        .map(|word| word.trim_matches(|ch| matches!(ch, '"' | '\'')))
        .skip_while(|word| {
            (word.contains('=') && !word.starts_with('-'))
                || matches!(
                    *word,
                    "env" | "time" | "command" | "exec" | "nohup" | "caffeinate"
                )
        });
    let program = Path::new(words.next()?).file_name()?.to_str()?.to_string();
    let rest = words.collect::<Vec<_>>();
    let first = rest
        .iter()
        .find(|word| !word.starts_with(['-', '+']))
        .copied();
    let runner = |word: &str| matches!(word, "jest" | "vitest" | "playwright" | "mocha");
    let python = program == "python"
        || program
            .strip_prefix("python3")
            .is_some_and(|minor| minor.is_empty() || minor.starts_with('.'));
    match program.as_str() {
        "pytest" | "py.test" | "jest" | "vitest" | "mocha" | "ctest" => Some(program),
        "playwright" if first == Some("test") => Some("playwright test".into()),
        "cargo" if matches!(first, Some("test" | "nextest" | "bench")) => {
            Some(format!("cargo {}", first?))
        }
        "go" | "deno" | "bun" if first == Some("test") => Some(format!("{program} test")),
        "npm" | "pnpm" | "yarn" | "bun" => match first {
            Some("test" | "t") => Some(format!("{program} test")),
            Some("run" | "run-script") => rest
                .iter()
                .find(|word| !word.starts_with('-') && **word != "run" && **word != "run-script")
                .filter(|script| script.starts_with("test") || script.contains("e2e"))
                .map(|script| format!("{program} run {script}")),
            Some("exec" | "x" | "dlx") => rest
                .iter()
                .find(|word| !matches!(**word, "exec" | "x" | "dlx") && !word.starts_with('-'))
                .filter(|tool| runner(tool))
                .map(|tool| format!("{program} exec {tool}")),
            Some(tool) if program != "npm" && runner(tool) => Some(format!("{program} {tool}")),
            _ => None,
        },
        "npx" | "bunx" | "pnpx" => first
            .filter(|tool| runner(tool))
            .map(|tool| format!("{program} {tool}")),
        "uv" | "poetry" if first == Some("run") => {
            let index = rest.iter().position(|word| *word == "run")?;
            route_of_words(
                &rest[index + 1..]
                    .iter()
                    .copied()
                    .filter(|word| !word.starts_with("--"))
                    .collect::<Vec<_>>(),
            )
        }
        "node" if rest.contains(&"--test") => Some("node --test".into()),
        _ if python => {
            let index = rest.iter().position(|word| *word == "-m")?;
            rest.get(index + 1)
                .filter(|module| matches!(**module, "pytest" | "unittest"))
                .map(|module| format!("{program} -m {module}"))
        }
        _ => None,
    }
}

fn stop_gate_enabled() -> bool {
    ["PROG_STOP_GATE", "CLAUDE_PLUGIN_OPTION_STOP_GATE"]
        .iter()
        .filter_map(std::env::var_os)
        .any(|value| matches!(value.to_str(), Some("1" | "true")))
}

/// What the newest captured evidence says when a turn ends, independent of
/// whether the gate is enabled (the decision log records it either way).
struct Evidence {
    /// The rule that would block, and the block's text.
    finding: Option<(&'static str, String)>,
    /// The newest test run's verdict and identity, for the log.
    newest_test: Option<Value>,
    /// Captured test, build, and lint runs this session.
    runs: usize,
    /// Tests the newest test run reports failing or erroring.
    failing_tests: Vec<String>,
}

/// Handle a Stop event: evaluate the evidence, decide, log, and answer.
///
/// Only test, build, and lint runs captured during this session count, and
/// only the newest of each kind decides: an older run of a different
/// invocation (`cargo test -p a adds`) is superseded by a later broader one
/// (`cargo test`) even though the store keeps them in separate chains. The
/// gate never blocks on missing evidence (no run captured), and stands aside
/// when a test command ran outside the shims after the newest captured run:
/// both misfire whenever tests run where prog cannot see them. It blocks at
/// most once in a row (`stop_hook_active`), and not at all when the final
/// message already tells the user the tests are failing, unproven, or were not
/// run. When the final message claims success that the evidence contradicts,
/// the block quotes the claim, and a second stop shows the user a warning
/// instead of blocking again (#305).
fn stop_event(input: &Value, store_dir: &Path, host: Host) -> Option<Value> {
    let enabled = stop_gate_enabled();
    if !enabled && !harness_log::logging_enabled() {
        return None;
    }
    let session = input
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|session| is_safe_name(session))?;
    let repeat = input["stop_hook_active"].as_bool() == Some(true);
    let message = input
        .get("last_assistant_message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let claim = final_message::success_claim(message);
    let evidence = assess_stop(store_dir, session);
    // A message that names a test prog saw failing, and claims nothing, is
    // telling the user about that failure.
    let discloses = final_message::discloses_open_verification(message)
        || (claim.is_none() && final_message::names_any_test(message, &evidence.failing_tests));
    let contradiction = match (&claim, &evidence.finding) {
        (Some(claim), Some((rule, _))) if !discloses => match *rule {
            "test_failing" | "test_stale" | "test_unresolved" => Some(claim),
            "test_not_proven" if claim.kind == final_message::ClaimKind::Fixed => Some(claim),
            _ => None,
        },
        _ => None,
    };
    let decision = match (&evidence.finding, repeat, discloses) {
        (_, true, _) => "skip_stop_hook_active",
        (Some(_), false, true) => "stand_aside",
        (Some(_), false, false) if enabled => "block",
        (Some(_), false, false) => "would_block",
        (None, false, _) => "pass",
    };
    harness_log::append(
        store_dir,
        host.name(),
        "stop",
        json!({
            "session": harness_log::short_hash(session),
            "gate_enabled": enabled,
            "decision": decision,
            "rule": evidence.finding.as_ref().map(|(rule, _)| *rule),
            "newest_test": evidence.newest_test,
            "runs": evidence.runs,
            "message": {
                "claims_success": claim.is_some(),
                "claim_kind": claim.as_ref().map(|claim| match claim.kind {
                    final_message::ClaimKind::Fixed => "fixed",
                    final_message::ClaimKind::Passing => "passing",
                }),
                "discloses_gap": discloses,
            },
            "contradiction": contradiction.is_some(),
        }),
    );
    if !enabled {
        return None;
    }
    let (rule, text) = evidence.finding?;
    match decision {
        "block" => {
            let said = contradiction
                .map(|claim| format!("prog stop gate: you said \"{}\", but", claim.quote))
                .unwrap_or_else(|| "prog stop gate:".to_string());
            Some(json!({"decision": "block", "reason": format!("{said} {text}")}))
        }
        "skip_stop_hook_active" => contradiction.map(|claim| {
            // The agent was already blocked once and still claims success:
            // tell the user directly instead of blocking again.
            let who = host.agent();
            let lead = text.split(". ").next().unwrap_or_default();
            let headline = text.lines().nth(1).unwrap_or_default();
            json!({
                "systemMessage": format!(
                    "prog: {who} said \"{}\", but {lead} ({rule}). {headline}",
                    claim.quote
                )
            })
        }),
        _ => None,
    }
}

/// The newest captured runs of this session, judged.
fn assess_stop(store_dir: &Path, session: &str) -> Evidence {
    let none = |runs| Evidence {
        finding: None,
        newest_test: None,
        runs,
        failing_tests: Vec::new(),
    };
    let Some(started) = fs::read_to_string(store_dir.join(SESSIONS_DIR).join(session))
        .ok()
        .and_then(|marker| parse_session_marker(&marker))
        .map(|(started, _)| started)
    else {
        return none(0);
    };
    let Ok(store) = Store::open_silently(store_dir) else {
        return none(0);
    };
    let Ok(list) = store.list_observations(100) else {
        return none(0);
    };
    let mut runs = list
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
    let count = runs.len();
    // A test command that ran outside the shims after a captured run may have
    // changed what stands; prog cannot know, so that run no longer decides.
    let uncaptured_at = fs::read_to_string(
        store_dir
            .join(SESSIONS_DIR)
            .join(format!("{session}{UNCAPTURED_SUFFIX}")),
    )
    .ok()
    .and_then(|at| chrono::DateTime::parse_from_rfc3339(at.trim()).ok())
    .map(|at| {
        at.with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Nanos, true)
    });
    let superseded_by_uncaptured = |index: usize| {
        let (observation, payload, _) = &runs[index];
        uncaptured_at
            .as_deref()
            .is_some_and(|at| at > run_end(observation, payload.as_value()).0.as_str())
    };

    // The newest run of each kind is what stands now. A lint run is
    // superseded only by a newer lint run; a build run also by a newer test
    // run, which compiles the same code.
    let newest = |kind: RunKind| runs.iter().position(|(_, _, run_kind)| *run_kind == kind);
    let newest_test = newest(RunKind::Test);
    // Whether a run's captured content equals the current content: Some(true)
    // only when every file a test run could see is unchanged since it ran.
    let mut current_workspace = None;
    let mut ran_on_current_content = |index: usize| {
        let captured = runs[index]
            .0
            .workspace_state
            .as_ref()
            .filter(|captured| captured.applicable)?;
        let root = captured.root.as_deref()?;
        let current = current_workspace.get_or_insert_with(|| prog_core::capture_workspace(root));
        match prog_core::compare_workspace_content(captured, current).validity {
            prog_core::WorkspaceValidity::Unchanged => Some(true),
            prog_core::WorkspaceValidity::Changed => Some(false),
            _ => None,
        }
    };
    // A revert check runs the suite on other content (`git stash`, then the
    // tests, then `git stash pop`). When the newest test run saw content that
    // is gone again, the newest run on exactly the current content is what
    // stands for that content; without one, the newest run decides as before.
    let current_content_run = match newest_test {
        Some(index) if ran_on_current_content(index) == Some(false) => (index + 1..runs.len())
            .filter(|later| runs[*later].2 == RunKind::Test)
            .find(|later| ran_on_current_content(*later) == Some(true)),
        _ => None,
    };
    let judged_test = current_content_run.or(newest_test);
    // Tests that failed in this session and were not named passing since, up
    // to the judged run. A newer run of another invocation (fewer tests, a
    // quiet run) replaces the older run's verdict, never the failures it
    // named: otherwise a narrower rerun would clear them (#303).
    let open_failures = judged_test
        .map(|judged| {
            let mut failing = BTreeMap::new();
            for (_, payload, kind) in runs[judged..].iter().rev() {
                if *kind != RunKind::Test {
                    continue;
                }
                let Some(population) = TestPopulation::from_payload(payload.as_value()) else {
                    continue;
                };
                for (id, status) in population.tests {
                    match status.as_str() {
                        "failed" | "error" => {
                            failing.insert(id, true);
                        }
                        "passed" => {
                            failing.insert(id, false);
                        }
                        _ => {}
                    }
                }
            }
            failing
                .into_iter()
                .filter_map(|(id, open)| open.then_some(id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let failing_tests = judged_test
        .and_then(|index| TestPopulation::from_payload(runs[index].1.as_value()))
        .map(|population| {
            population
                .tests
                .into_iter()
                .filter(|(_, status)| matches!(status.as_str(), "failed" | "error"))
                .map(|(id, _)| id)
                .chain(open_failures.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| open_failures.clone());
    let newest_test_record = judged_test.map(|index| {
        let (observation, payload, _) = &runs[index];
        let receipt = receipt_for(&store, observation, payload.as_value());
        let record = json!({
            "verdict": receipt.verdict,
            "provider": observation.provider,
            "invocation": harness_log::short_hash(&observation.invocation_fingerprint),
            "superseded_by_uncaptured_run": superseded_by_uncaptured(index),
            "older_run_on_current_content": current_content_run.is_some(),
        });
        (receipt, record)
    });
    let evidence = |finding| Evidence {
        finding,
        failing_tests: failing_tests.clone(),
        newest_test: newest_test_record
            .as_ref()
            .map(|(_, record)| record.clone()),
        runs: count,
    };
    for (kind, label, rule) in [
        (RunKind::Lint, "lint", "lint_failing"),
        (RunKind::Build, "build", "build_failing"),
    ] {
        let Some(index) = newest(kind) else {
            continue;
        };
        if kind == RunKind::Build
            && (newest_test.is_some_and(|test| test < index) || superseded_by_uncaptured(index))
        {
            continue;
        }
        let (observation, payload, _) = &runs[index];
        let receipt = receipt_for(&store, observation, payload.as_value());
        if receipt.verdict == ReceiptVerdict::Failing {
            return evidence(Some((
                rule,
                format!(
                    "the latest {label} run is failing. Fix it, or tell the user plainly why it is still open.\n{}",
                    receipt.text
                ),
            )));
        }
    }
    let (Some(index), Some((receipt, _))) = (judged_test, &newest_test_record) else {
        return evidence(None);
    };
    if superseded_by_uncaptured(index) {
        return evidence(None);
    }
    let lead = match receipt.verdict {
        ReceiptVerdict::Failing => Some(("test_failing", "the latest test run is failing")),
        ReceiptVerdict::NotProven => Some((
            "test_not_proven",
            "the latest test run does not prove the earlier failure fixed",
        )),
        ReceiptVerdict::Fixed | ReceiptVerdict::Clean => None,
    };
    if let Some((rule, lead)) = lead {
        return evidence(Some((
            rule,
            format!(
                "{lead}. Fix it, or tell the user plainly why it is still open.\n{}",
                receipt.text
            ),
        )));
    }
    if !open_failures.is_empty() {
        let shown = open_failures
            .iter()
            .take(3)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let more = match open_failures.len() {
            0..=3 => String::new(),
            count => format!(" and {} more", count - 3),
        };
        return evidence(Some((
            "test_unresolved",
            format!(
                "tests that failed earlier in this session have not passed by name since: {shown}{more}. Rerun them (verbosely, so each test is named), or tell the user plainly why they are still open.\n{}",
                receipt.text
            ),
        )));
    }
    // The newest test run passed: block only if a file's content changed
    // after it. Staging or committing what was tested changes no content.
    let (observation, _, _) = &runs[index];
    let changed = observation
        .workspace_state
        .as_ref()
        .filter(|captured| captured.applicable)
        .and_then(|captured| {
            let current = prog_core::capture_workspace(captured.root.as_deref()?);
            Some(prog_core::compare_workspace_content(captured, &current).validity)
        })
        == Some(prog_core::WorkspaceValidity::Changed);
    evidence(changed.then(|| {
        (
            "test_stale",
            "files changed after the last passing test run; rerun the tests (or tell the user they were not rerun) before finishing.".to_string(),
        )
    }))
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

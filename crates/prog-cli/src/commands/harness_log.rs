//! Local log of hook decisions (#314): one JSON line per Stop and per
//! PostToolUse event, so how often the stop gate blocks, why, and what the
//! agent did next come from real use instead of transcript replays.
//!
//! The log stores no message text, no command text beyond a runner name, and
//! only hashes of session ids and invocation fingerprints. It lives in the
//! project's store, is bounded (5 MiB per month, three months), is never
//! transmitted, and `PROG_HARNESS_LOG=0` turns it off. Every failure is silent:
//! logging must never change what a hook prints.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{Datelike, Duration, SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::*;

pub(crate) const HARNESS_LOG_DIR: &str = "harness-log";
pub(crate) const HARNESS_LOG_SCHEMA: &str = "prog.harness_log.v1";
const HARNESS_LOG_SUMMARY_SCHEMA: &str = "prog.harness_log_summary.v1";
const MAX_LOG_FILE_BYTES: u64 = 5 * 1024 * 1024;
const RETAINED_MONTHS: i32 = 3;
const DEFAULT_SINCE_DAYS: i64 = 30;
const MAX_LISTED_ENTRIES: usize = 200;

pub(crate) fn logging_enabled() -> bool {
    !matches!(
        std::env::var("PROG_HARNESS_LOG").as_deref(),
        Ok("0" | "false" | "off")
    )
}

/// A short, stable, non-reversible id for a session or invocation.
pub(crate) fn short_hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))[..16].to_string()
}

/// Append one entry. `fields` must already be free of message and command
/// text; this adds the schema, time, and host.
pub(crate) fn append(store_dir: &Path, host: &str, event: &str, fields: Value) {
    if !logging_enabled() {
        return;
    }
    let dir = store_dir.join(HARNESS_LOG_DIR);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = Utc::now();
    let path = dir.join(format!("{:04}-{:02}.jsonl", now.year(), now.month()));
    if fs::metadata(&path).is_ok_and(|meta| meta.len() >= MAX_LOG_FILE_BYTES) {
        return;
    }
    let mut entry = Map::new();
    entry.insert("schema".into(), HARNESS_LOG_SCHEMA.into());
    entry.insert(
        "at".into(),
        now.to_rfc3339_opts(SecondsFormat::Millis, true).into(),
    );
    entry.insert("host".into(), host.into());
    entry.insert("event".into(), event.into());
    if let Value::Object(fields) = fields {
        entry.extend(fields);
    }
    let Ok(mut line) = serde_json::to_string(&Value::Object(entry)) else {
        return;
    };
    line.push('\n');
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(line.as_bytes());
    }
    prune(&dir, now.year() * 12 + now.month() as i32 - 1);
}

/// Remove month files older than the retention window.
fn prune(dir: &Path, current_month_index: i32) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(index) = name.to_str().and_then(month_index) else {
            continue;
        };
        if current_month_index - index >= RETAINED_MONTHS {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn month_index(file_name: &str) -> Option<i32> {
    let stem = file_name.strip_suffix(".jsonl")?;
    let (year, month) = stem.split_once('-')?;
    let year = year.parse::<i32>().ok()?;
    let month = month.parse::<i32>().ok()?;
    (1..=12).contains(&month).then_some(year * 12 + month - 1)
}

/// `prog harness log`: recent entries, or a summary of them.
pub(crate) fn harness_log_report(args: &HarnessLogArgs) -> Result<Value> {
    let since = match &args.since {
        Some(since) => parse_since(since)?,
        None => Utc::now() - Duration::days(DEFAULT_SINCE_DAYS),
    };
    let stores = log_stores(args)?;
    let mut entries = Vec::new();
    for store in &stores {
        entries.extend(read_entries(&store.join(HARNESS_LOG_DIR), since));
    }
    entries.sort_by(|left, right| {
        left["at"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["at"].as_str().unwrap_or_default())
    });
    let stores = stores
        .iter()
        .map(|store| store.display().to_string())
        .collect::<Vec<_>>();
    if args.summary {
        return Ok(summarize(&entries, &stores, since));
    }
    let total = entries.len();
    let skipped = total.saturating_sub(MAX_LISTED_ENTRIES);
    Ok(json!({
        "schema": HARNESS_LOG_SCHEMA,
        "since": since.to_rfc3339_opts(SecondsFormat::Secs, true),
        "stores": stores,
        "total": total,
        "omitted_older": skipped,
        "entries": entries.into_iter().skip(skipped).collect::<Vec<_>>(),
    }))
}

fn parse_since(since: &str) -> Result<chrono::DateTime<Utc>> {
    let bad = |reason: String| CoreError::BadArgs {
        operation: "harness log".to_string(),
        reason,
    };
    if let Some(days) = since.strip_suffix('d') {
        let days = days
            .parse::<i64>()
            .map_err(|_| bad(format!("--since {since:?}: expected <N>d or RFC 3339")))?;
        return Ok(Utc::now() - Duration::days(days));
    }
    chrono::DateTime::parse_from_rfc3339(since)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| bad(format!("--since must be <N>d or RFC 3339: {error}")))
}

/// The stores whose logs the report reads: an explicit `PROG_DIR`, the
/// project's own store, or every per-project store.
fn log_stores(args: &HarnessLogArgs) -> Result<Vec<PathBuf>> {
    if args.all_projects {
        let parent = crate::commands::shim::project_store_dir(&args.project)
            .parent()
            .map(Path::to_path_buf);
        let mut stores = parent
            .and_then(|parent| fs::read_dir(parent).ok())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.join(HARNESS_LOG_DIR).is_dir())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        stores.sort();
        return Ok(stores);
    }
    if let Some(dir) = std::env::var_os("PROG_DIR").filter(|dir| !dir.is_empty()) {
        return Ok(vec![PathBuf::from(dir)]);
    }
    Ok(vec![crate::commands::shim::project_store_dir(
        &args.project,
    )])
}

fn read_entries(dir: &Path, since: chrono::DateTime<Utc>) -> Vec<Value> {
    let Ok(files) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for file in files.flatten() {
        if file.file_name().to_str().and_then(month_index).is_none() {
            continue;
        }
        let Ok(text) = fs::read_to_string(file.path()) else {
            continue;
        };
        entries.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|entry| entry["schema"] == HARNESS_LOG_SCHEMA)
                .filter(|entry| {
                    entry["at"]
                        .as_str()
                        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                        .is_some_and(|at| at.with_timezone(&Utc) >= since)
                }),
        );
    }
    entries
}

/// Aggregates for deciding whether the gate is worth its interruptions:
/// blocks per turn end, by rule, repeats, and what the agent did next.
fn summarize(entries: &[Value], stores: &[String], since: chrono::DateTime<Utc>) -> Value {
    let mut turn_ends = 0u64;
    let mut gate_enabled_turn_ends = 0u64;
    let mut blocks = 0u64;
    let mut would_block = 0u64;
    let mut stood_aside = 0u64;
    let mut second_stops = 0u64;
    let mut contradictions = 0u64;
    let mut by_rule = BTreeMap::<String, u64>::new();
    let mut blocks_per_session = BTreeMap::<String, u64>::new();
    let mut repeat_blocks = 0u64;
    let mut captured_test_commands = 0u64;
    let mut uncaptured_test_commands = 0u64;
    let mut after = BTreeMap::<&str, u64>::new();
    // Per session: the last Stop's rule, and a block awaiting its next run.
    let mut last_rule = BTreeMap::<String, Option<String>>::new();
    let mut awaiting = BTreeMap::<String, Option<String>>::new();
    let mut sessions = BTreeSet::<String>::new();

    for entry in entries {
        let session = entry["session"].as_str().unwrap_or_default().to_string();
        sessions.insert(session.clone());
        match entry["event"].as_str() {
            Some("stop") => {
                turn_ends += 1;
                let enabled = entry["gate_enabled"].as_bool() == Some(true);
                gate_enabled_turn_ends += u64::from(enabled);
                let decision = entry["decision"].as_str().unwrap_or_default();
                let rule = entry["rule"].as_str().map(str::to_string);
                if entry["contradiction"].as_bool() == Some(true) {
                    contradictions += 1;
                }
                match decision {
                    "block" | "would_block" => {
                        if decision == "block" {
                            blocks += 1;
                        } else {
                            would_block += 1;
                        }
                        let rule_name = rule.clone().unwrap_or_else(|| "unknown".to_string());
                        *by_rule.entry(rule_name).or_default() += 1;
                        *blocks_per_session.entry(session.clone()).or_default() += 1;
                        if last_rule.get(&session).cloned().flatten() == rule {
                            repeat_blocks += 1;
                        }
                        awaiting.insert(
                            session.clone(),
                            entry["newest_test"]["invocation"]
                                .as_str()
                                .map(str::to_string),
                        );
                        last_rule.insert(session, rule);
                    }
                    "stand_aside" => {
                        stood_aside += 1;
                        last_rule.insert(session, None);
                    }
                    "skip_stop_hook_active" => second_stops += 1,
                    _ => {
                        last_rule.insert(session, None);
                    }
                }
            }
            Some("post_tool_use") => {
                let captured = entry["captured"].as_array().cloned().unwrap_or_default();
                if !captured.is_empty() {
                    captured_test_commands += 1;
                }
                if entry["uncaptured_runner"].is_string() {
                    uncaptured_test_commands += 1;
                }
                if let Some(blocked_invocation) = awaiting.remove(&session) {
                    if let Some(first) = captured.first() {
                        let same = blocked_invocation.is_some()
                            && first["invocation"].as_str() == blocked_invocation.as_deref();
                        *after
                            .entry(if same {
                                "same_invocation"
                            } else {
                                "different_invocation"
                            })
                            .or_default() += 1;
                    } else if entry["uncaptured_runner"].is_string() {
                        *after.entry("uncaptured_test_command").or_default() += 1;
                    } else {
                        // Not a test command; keep waiting for one.
                        awaiting.insert(session, blocked_invocation);
                    }
                }
            }
            _ => {}
        }
    }
    for _ in awaiting.values() {
        *after.entry("no_test_run").or_default() += 1;
    }
    let ratio = |part: u64, whole: u64| {
        if whole == 0 {
            Value::Null
        } else {
            json!((part as f64 / whole as f64 * 1000.0).round() / 1000.0)
        }
    };
    let test_commands = captured_test_commands + uncaptured_test_commands;
    json!({
        "schema": HARNESS_LOG_SUMMARY_SCHEMA,
        "since": since.to_rfc3339_opts(SecondsFormat::Secs, true),
        "stores": stores,
        "sessions": sessions.len(),
        "turn_ends": turn_ends,
        "gate_enabled_turn_ends": gate_enabled_turn_ends,
        "blocks": blocks,
        "would_block_with_gate_off": would_block,
        "blocks_per_turn_end": ratio(blocks + would_block, turn_ends),
        "by_rule": by_rule,
        "repeat_blocks": repeat_blocks,
        "sessions_with_more_than_3_blocks": blocks_per_session.values().filter(|count| **count > 3).count(),
        "stood_aside_for_disclosure": stood_aside,
        "second_stops_let_through": second_stops,
        "claims_contradicting_evidence": contradictions,
        "test_commands": {
            "captured": captured_test_commands,
            "uncaptured": uncaptured_test_commands,
            "captured_share": ratio(captured_test_commands, test_commands),
        },
        "after_block": after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_files_are_recognized_and_old_ones_pruned() {
        assert_eq!(month_index("2026-11.jsonl"), Some(2026 * 12 + 10));
        assert_eq!(month_index("2026-13.jsonl"), None);
        assert_eq!(month_index("notes.jsonl"), None);
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "2026-07.jsonl",
            "2026-08.jsonl",
            "2026-09.jsonl",
            "2026-11.jsonl",
            "keep.txt",
        ] {
            fs::write(dir.path().join(name), "{}\n").unwrap();
        }
        prune(dir.path(), 2026 * 12 + 10);
        let mut left = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        left.sort();
        assert_eq!(left, ["2026-09.jsonl", "2026-11.jsonl", "keep.txt"]);
    }

    #[test]
    fn summary_counts_blocks_repeats_and_what_followed() {
        let stop = |session: &str, decision: &str, rule: Option<&str>, invocation: Option<&str>| {
            json!({
                "schema": HARNESS_LOG_SCHEMA, "at": "2026-11-10T00:00:00.000Z",
                "event": "stop", "session": session, "gate_enabled": true,
                "decision": decision, "rule": rule,
                "newest_test": invocation.map(|invocation| json!({"invocation": invocation})),
            })
        };
        let run = |session: &str, invocation: Option<&str>, uncaptured: Option<&str>| {
            json!({
                "schema": HARNESS_LOG_SCHEMA, "at": "2026-11-10T00:00:00.000Z",
                "event": "post_tool_use", "session": session,
                "captured": invocation.map(|invocation| vec![json!({"invocation": invocation})]).unwrap_or_default(),
                "uncaptured_runner": uncaptured,
            })
        };
        let entries = vec![
            stop("a", "block", Some("test_failing"), Some("i1")),
            stop("a", "skip_stop_hook_active", None, None),
            run("a", None, None),
            run("a", Some("i1"), None),
            stop("a", "block", Some("test_failing"), Some("i1")),
            run("a", None, Some("pytest")),
            stop("a", "pass", None, None),
            stop("b", "would_block", Some("test_stale"), Some("i2")),
            stop("b", "stand_aside", None, None),
        ];
        let summary = summarize(&entries, &[], Utc::now());
        assert_eq!(summary["turn_ends"], 6);
        assert_eq!(summary["blocks"], 2);
        assert_eq!(summary["would_block_with_gate_off"], 1);
        assert_eq!(summary["by_rule"]["test_failing"], 2);
        assert_eq!(summary["repeat_blocks"], 1);
        assert_eq!(summary["stood_aside_for_disclosure"], 1);
        assert_eq!(summary["second_stops_let_through"], 1);
        assert_eq!(summary["after_block"]["same_invocation"], 1);
        assert_eq!(summary["after_block"]["uncaptured_test_command"], 1);
        assert_eq!(summary["after_block"]["no_test_run"], 1);
        assert_eq!(summary["test_commands"]["captured"], 1);
        assert_eq!(summary["test_commands"]["uncaptured"], 1);
    }
}

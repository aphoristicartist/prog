//! Moves over captured collections: structured `run` stdout, the overview
//! map, `select`, `coverage`, keyed record deltas, automatic changes since
//! the last capture, and the compact view.

use std::{fs, path::Path};

use serde_json::{Value, json};

mod support;

use support::{prog, stdout};

fn issues(states: &[(u64, &str)]) -> Value {
    Value::Array(
        states
            .iter()
            .map(|(number, state)| {
                json!({
                    "id": 1000 + number,
                    "number": number,
                    "state": state,
                    "title": format!("issue {number}"),
                    "user": {"login": format!("user{}", number % 3)},
                    "body": format!("{} end{number}", "lorem ipsum ".repeat(30)),
                })
            })
            .collect(),
    )
}

fn many_issues(count: u64) -> Value {
    let states = (1..=count)
        .map(|number| (number, if number % 3 == 0 { "closed" } else { "open" }))
        .collect::<Vec<_>>();
    issues(&states)
}

fn json_output(args: &[&str]) -> Value {
    let output = prog(args);
    assert!(output.status.success(), "{args:?}: {}", stdout(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A script that prints the file it is given, so a "command" can return JSON
/// that changes between runs.
fn printer(dir: &Path) -> String {
    let script = dir.join("print.py");
    fs::write(
        &script,
        "import sys\nsys.stdout.write(open(sys.argv[1]).read())\n",
    )
    .unwrap();
    script.to_str().unwrap().to_string()
}

#[test]
fn run_parses_json_stdout_into_navigable_records_with_an_overview() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_str().unwrap();
    let data = dir.path().join("issues.json");
    fs::write(&data, many_issues(30).to_string()).unwrap();
    let script = printer(dir.path());
    let envelope = json_output(&[
        "--dir",
        store,
        "run",
        "--",
        "python3",
        &script,
        data.to_str().unwrap(),
    ]);
    let overview = &envelope["overview"];
    assert_eq!(overview["path"], "/stdout/json", "{envelope:#}");
    assert_eq!(overview["items"], 30);
    assert_eq!(overview["key_candidates"][0], "/id");
    let state = overview["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["field"] == "/state")
        .unwrap();
    assert_eq!(
        state["values"],
        json!([{"value": "open", "count": 20}, {"value": "closed", "count": 10}])
    );

    let cursor = envelope["cursor"].as_str().unwrap();
    let selected = json_output(&[
        "--dir",
        store,
        "select",
        cursor,
        "--where",
        "state=closed",
        "--fields",
        "number,user/login",
        "--limit",
        "2",
    ]);
    assert_eq!(selected["schema"], "prog.select");
    assert_eq!(selected["path"], "/stdout/json");
    assert_eq!(
        (selected["total"].as_u64(), selected["matched"].as_u64()),
        (Some(30), Some(10))
    );
    assert_eq!(selected["rows"][0]["path"], "/stdout/json/2");
    assert_eq!(
        selected["rows"][0]["value"],
        json!({"number": 3, "user": {"login": "user0"}})
    );
    assert_eq!(selected["next_offset"], 2);

    // Every row path is a real address of the same cursor.
    let evidence = json_output(&[
        "--dir",
        store,
        "evidence",
        cursor,
        "--path",
        "/stdout/json/2/number",
    ]);
    assert_eq!(evidence["excerpt"], 3);
}

/// `go test -json` prints JSON Lines that are verification evidence. They
/// must stay text evidence, never become generic records, so test verdicts
/// and receipts cannot change.
#[test]
fn test_runner_json_output_is_never_reparsed_as_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_str().unwrap();
    let tool = dir.path().join("go");
    fs::write(
        &tool,
        concat!(
            "#!/bin/sh\n",
            "echo '{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestA\"}'\n",
            "echo '{\"Action\":\"pass\",\"Package\":\"p\",\"Test\":\"TestA\",\"Elapsed\":0}'\n",
            "echo '{\"Action\":\"pass\",\"Package\":\"p\",\"Elapsed\":0}'\n",
        ),
    )
    .unwrap();
    fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let envelope = json_output(&[
        "--dir",
        store,
        "run",
        "--",
        tool.to_str().unwrap(),
        "test",
        "-json",
        "./...",
    ]);
    let cursor = envelope["cursor"].as_str().unwrap();
    let stdout_view = json_output(&[
        "--dir", store, "expand", cursor, "--path", "/stdout", "--depth", "1",
    ]);
    assert_eq!(stdout_view["data_preview"]["format"], "text");
    assert!(stdout_view["data_preview"].get("json").is_none());
    assert!(envelope.get("overview").is_none());
}

#[test]
fn ndjson_stdout_becomes_one_record_per_line() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_str().unwrap();
    let data = dir.path().join("events.ndjson");
    let lines = (0..12)
        .map(|index| {
            json!({"seq": index, "level": if index % 4 == 0 { "error" } else { "info" }})
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&data, lines).unwrap();
    let script = printer(dir.path());
    let envelope = json_output(&[
        "--dir",
        store,
        "run",
        "--",
        "python3",
        &script,
        data.to_str().unwrap(),
    ]);
    let cursor = envelope["cursor"].as_str().unwrap();
    let errors = json_output(&[
        "--dir",
        store,
        "select",
        cursor,
        "--where",
        "level=error",
        "--fields",
        "seq",
    ]);
    assert_eq!(errors["matched"], 3);
    assert_eq!(errors["rows"][2]["value"], json!({"seq": 8}));
}

#[test]
fn rerunning_a_command_reports_changed_records_and_delta_lists_them_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_str().unwrap();
    let data = dir.path().join("issues.json");
    let script = printer(dir.path());
    let argv = [
        "--dir",
        store,
        "run",
        "--",
        "python3",
        &script,
        data.to_str().unwrap(),
    ];
    fs::write(
        &data,
        issues(&[(1, "open"), (2, "open"), (3, "open")]).to_string(),
    )
    .unwrap();
    let first = json_output(&argv);
    fs::write(
        &data,
        issues(&[(1, "open"), (2, "closed"), (4, "open")]).to_string(),
    )
    .unwrap();
    let second = json_output(&argv);

    let records = &second["changes_since"]["records"];
    assert_eq!(records["status"], "compared", "{second:#}");
    assert_eq!(records["key"], "/id");
    assert_eq!(
        records["counts"],
        json!({"added": 1, "removed": 1, "changed": 1, "unchanged": 1})
    );

    let baseline = first["observation"]["observation_id"].as_str().unwrap();
    let subject = second["observation"]["observation_id"].as_str().unwrap();
    let delta = json_output(&[
        "--dir", store, "delta", baseline, subject, "--key", "number",
    ]);
    let records = &delta["records"];
    assert_eq!(records["key"], "/number");
    assert_eq!(records["key_declared"], true);
    assert_eq!(
        records["added"],
        json!([{"key": 4, "path": "/stdout/json/2"}])
    );
    assert_eq!(
        records["removed"],
        json!([{"key": 3, "path": "/stdout/json/2"}])
    );
    assert_eq!(
        records["changed"][0]["fields"],
        json!([{"field": "/state", "before": "open", "after": "closed"}])
    );
    assert!(
        records["reasons"][0]
            .as_str()
            .unwrap()
            .contains("captured collections only")
    );
}

#[test]
fn record_delta_refuses_a_truncated_capture() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_str().unwrap();
    let data = dir.path().join("issues.json");
    let script = printer(dir.path());
    fs::write(&data, many_issues(40).to_string()).unwrap();
    let full = json_output(&[
        "--dir",
        store,
        "run",
        "--",
        "python3",
        &script,
        data.to_str().unwrap(),
    ]);
    // The second capture is cut short: its JSON never parses, and a record
    // missing from it must not be reported as removed.
    let cut = json_output(&[
        "--dir",
        store,
        "run",
        "--max-stdout-bytes",
        "2048",
        "--",
        "python3",
        &script,
        data.to_str().unwrap(),
    ]);
    let delta = json_output(&[
        "--dir",
        store,
        "delta",
        full["observation"]["observation_id"].as_str().unwrap(),
        cut["observation"]["observation_id"].as_str().unwrap(),
        "--records-path",
        "/stdout/json",
    ]);
    assert_ne!(delta["records"]["status"], "compared", "{delta:#}");
    assert!(delta["records"].get("removed").is_none());
}

#[test]
fn reobserving_a_file_reports_what_changed_even_for_large_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_str().unwrap();
    let file = dir.path().join("live.json");
    fs::write(&file, many_issues(300).to_string()).unwrap();
    json_output(&["--dir", store, "observe", "--file", file.to_str().unwrap()]);
    let mut changed = many_issues(300);
    changed[16]["state"] = json!("closed");
    fs::write(&file, changed.to_string()).unwrap();
    let second = json_output(&["--dir", store, "observe", "--file", file.to_str().unwrap()]);
    let records = &second["changes_since"]["records"];
    assert_eq!(
        records["counts"]["changed"], 1,
        "{:#}",
        second["changes_since"]
    );
    assert_eq!(records["counts"]["unchanged"], 299);
}

#[test]
fn coverage_counts_records_returned_this_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_str().unwrap();
    let file = dir.path().join("issues.json");
    fs::write(&file, many_issues(20).to_string()).unwrap();
    let envelope = json_output(&["--dir", store, "observe", "--file", file.to_str().unwrap()]);
    let cursor = envelope["cursor"].as_str().unwrap();
    // The first view previews five records.
    let coverage = json_output(&["--dir", store, "coverage", cursor]);
    assert_eq!(coverage["schema"], "prog.coverage");
    assert_eq!(
        (coverage["items"].as_u64(), coverage["returned"].as_u64()),
        (Some(20), Some(5))
    );
    assert_eq!(coverage["not_returned_ranges"], json!(["5-19"]));

    json_output(&[
        "--dir", store, "select", cursor, "--offset", "10", "--limit", "3", "--fields", "id",
    ]);
    json_output(&["--dir", store, "evidence", cursor, "--path", "/19/title"]);
    let coverage = json_output(&["--dir", store, "coverage", cursor]);
    assert_eq!(coverage["returned"], 9);
    assert_eq!(coverage["not_returned_ranges"], json!(["5-9", "13-18"]));
}

#[test]
fn compact_view_keeps_data_and_caveats_and_drops_bookkeeping() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_str().unwrap();
    let file = dir.path().join("issues.json");
    fs::write(&file, many_issues(200).to_string()).unwrap();
    let full = prog(&["--dir", store, "observe", "--file", file.to_str().unwrap()]);
    let compact = prog(&[
        "--dir",
        store,
        "--format",
        "compact",
        "observe",
        "--file",
        file.to_str().unwrap(),
    ]);
    assert!(compact.status.success(), "{}", stdout(&compact));
    let value: Value = serde_json::from_slice(&compact.stdout).unwrap();
    assert!(value["cursor"].as_str().unwrap().starts_with("pc1_"));
    assert_eq!(
        value["overview"]["fields"]["/state"],
        "string; open=134, closed=66"
    );
    assert_eq!(value["data"].as_array().unwrap().len(), 2);
    assert!(
        value["omitted"]
            .to_string()
            .contains("200 items, showing 5")
    );
    for dropped in [
        "disclosure_verdict",
        "action_templates",
        "provenance",
        "disclosure_budget",
    ] {
        assert!(value.get(dropped).is_none(), "{dropped}");
    }
    assert!(
        compact.stdout.len() * 2 < full.stdout.len(),
        "compact {} B vs full {} B",
        compact.stdout.len(),
        full.stdout.len()
    );

    // The environment selects the format too; a flag overrides it.
    let output = support::prog_with_env(
        &[
            "--dir",
            store,
            "coverage",
            value["cursor"].as_str().unwrap(),
        ],
        &[("PROG_FORMAT", "compact")],
    );
    assert!(output.status.success());
    let output = support::prog_with_env(
        &[
            "--dir",
            store,
            "--format",
            "full",
            "observe",
            "--file",
            file.to_str().unwrap(),
        ],
        &[("PROG_FORMAT", "compact")],
    );
    let full_again: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(full_again.get("disclosure_verdict").is_some());
}

#[test]
fn search_preview_shows_the_match_in_a_long_string() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_str().unwrap();
    let file = dir.path().join("log.json");
    let body = format!(
        "{} ERROR: connection reset by peer",
        "lorem ipsum ".repeat(80)
    );
    fs::write(&file, json!([{"body": body}, {"body": "fine"}]).to_string()).unwrap();
    let envelope = json_output(&["--dir", store, "observe", "--file", file.to_str().unwrap()]);
    let cursor = envelope["cursor"].as_str().unwrap();
    let search = json_output(&["--dir", store, "search", cursor, "Connection Reset"]);
    let preview = search["hits"][0]["preview"].as_str().unwrap();
    assert!(preview.starts_with("..."), "{preview}");
    assert!(preview.contains("connection reset by peer"), "{preview}");
}

/// #285 and I2 for the new persistence path: vendor credentials in command
/// text, parsed JSON stdout, and observed JSON never reach the store.
#[test]
fn vendor_credentials_never_reach_the_store_through_any_capture_path() {
    let alnum = |len: usize| -> String {
        "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4Y5z6"
            .chars()
            .cycle()
            .take(len)
            .collect()
    };
    let tokens = [
        format!("gh{}_{}", "p", alnum(36)),
        format!("AK{}{}", "IA", "IOSFODNN7EXAMPLE"),
        format!("sk-{}-api03-{}", "ant", alnum(60)),
        format!("xo{}-123456789012-{}", "xb", alnum(24)),
        format!("sk_{}_{}", "live", alnum(24)),
    ];
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("store");
    let store = store_dir.to_str().unwrap();
    let text = dir.path().join("log.txt");
    fs::write(
        &text,
        tokens
            .iter()
            .map(|token| format!("pushing with {token} to origin\n"))
            .collect::<String>(),
    )
    .unwrap();
    let records = dir.path().join("records.json");
    fs::write(
        &records,
        Value::Array(
            tokens
                .iter()
                .enumerate()
                .map(|(index, token)| json!({"id": index, "note": format!("key {token} here"), "benign": "kept"}))
                .collect(),
        )
        .to_string(),
    )
    .unwrap();
    let script = printer(dir.path());
    for file in [&text, &records] {
        let envelope = json_output(&[
            "--dir",
            store,
            "run",
            "--",
            "python3",
            &script,
            file.to_str().unwrap(),
        ]);
        assert!(!envelope.to_string().contains(&tokens[0]));
    }
    json_output(&[
        "--dir",
        store,
        "observe",
        "--file",
        records.to_str().unwrap(),
    ]);

    let mut stored = Vec::new();
    for entry in walk(&store_dir) {
        stored.extend(fs::read(entry).unwrap());
    }
    let stored = String::from_utf8_lossy(&stored);
    for token in &tokens {
        assert!(
            !stored.contains(token.as_str()),
            "a credential reached the store"
        );
    }
    assert!(stored.contains("kept"), "benign values survive");
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

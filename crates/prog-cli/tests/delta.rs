//! Integration coverage for explicit observation delta.

use std::fs;

use serde_json::Value;

mod support;

use support::*;

#[test]
fn explicit_delta_reports_new_and_resolved_findings_for_repeated_command() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let state = dir.path().join("state.txt");
    let script = dir.path().join("emit.py");
    fs::write(
        &script,
        "from pathlib import Path\nprint(Path(__import__('sys').argv[1]).read_text())\n",
    )
    .unwrap();
    fs::write(&state, "error old failure\n").unwrap();
    let first = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--selection-scope",
        "full-suite",
        "--selection-exhaustive",
        "--",
        "python3",
        script.to_str().unwrap(),
        state.to_str().unwrap(),
    ]);
    assert!(first.status.success(), "{}", stdout(&first));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let first_id = first["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();
    fs::write(&state, "error new failure\n").unwrap();
    let second = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--selection-scope",
        "full-suite",
        "--selection-exhaustive",
        "--",
        "python3",
        script.to_str().unwrap(),
        state.to_str().unwrap(),
    ]);
    assert!(second.status.success(), "{}", stdout(&second));
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    let second_id = second["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(second["changes_since"]["baseline_observation_id"], first_id);
    assert_eq!(second["changes_since"]["counts"]["new"], 1);
    assert_eq!(second["changes_since"]["counts"]["resolved"], 1);
    let delta = prog(&["--dir", dir_arg, "delta", &first_id, &second_id]);
    assert!(delta.status.success(), "{}", stdout(&delta));
    let delta: Value = serde_json::from_slice(&delta.stdout).unwrap();
    assert_eq!(delta["schema"], "prog.observation_delta");
    assert_eq!(delta["assessment"]["can_prove_absence"], true);
    assert_eq!(delta["counts"]["new"], 1);
    assert_eq!(delta["counts"]["resolved"], 1);
    assert!(delta["findings"].as_array().unwrap().iter().all(|finding| {
        matches!(finding["status"].as_str(), Some("new") | Some("resolved"))
            && finding["evidence_ref"]["path"].is_string()
            && finding["availability"] == "recoverable"
    }));
}

#[test]
fn delta_output_obeys_the_requested_disclosure_budget() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let state = dir.path().join("state.txt");
    let script = dir.path().join("emit.py");
    fs::write(
        &script,
        "from pathlib import Path\nprint(Path(__import__('sys').argv[1]).read_text())\n",
    )
    .unwrap();
    fs::write(&state, "error alpha failure\nerror beta failure\n").unwrap();
    let first = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--selection-scope",
        "full-suite",
        "--selection-exhaustive",
        "--",
        "python3",
        script.to_str().unwrap(),
        state.to_str().unwrap(),
    ]);
    assert!(first.status.success(), "{}", stdout(&first));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let first_id = first["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();
    fs::write(&state, "error gamma failure\nerror delta failure\n").unwrap();
    let second = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--selection-scope",
        "full-suite",
        "--selection-exhaustive",
        "--",
        "python3",
        script.to_str().unwrap(),
        state.to_str().unwrap(),
    ]);
    assert!(second.status.success(), "{}", stdout(&second));
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    let second_id = second["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    // A budget too small to hold even the compact delta rendering fails
    // closed with the standard `budget_too_small` error rather than
    // silently truncating which findings the delta reports.
    let too_small = prog_with_budget(dir_arg, 128, &["delta", &first_id, &second_id]);
    assert!(!too_small.status.success());
    let too_small_value: Value = serde_json::from_slice(&too_small.stdout).unwrap();
    assert_eq!(too_small_value["error"]["kind"], "budget_too_small");

    // A generous budget succeeds and reports actual usage under the request.
    let generous = prog_with_budget(dir_arg, 65_536, &["delta", &first_id, &second_id]);
    assert!(generous.status.success(), "{}", stdout(&generous));
    let generous_value: Value = serde_json::from_slice(&generous.stdout).unwrap();
    let actual_bytes = generous_value["disclosure_budget"]["actual_bytes"]
        .as_u64()
        .unwrap();
    assert!(actual_bytes <= 65_536);
    // The generic text extractor emits a whole-payload finding alongside
    // each per-line finding, so two fully-disjoint two-line iterations
    // report 3 new/3 resolved (2 per-line + 1 whole-payload), not 2/2.
    assert_eq!(generous_value["counts"]["new"], 3);
    assert_eq!(generous_value["counts"]["resolved"], 3);
}

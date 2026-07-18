//! Integration coverage for cache management and retention.

use std::fs;

use serde_json::{Value, json};

mod support;

use support::*;

#[test]
fn cache_list_and_purge_are_real_json_commands() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let list = prog(&["--dir", dir_arg, "cache", "list"]);
    assert!(list.status.success());
    assert_eq!(stderr(&list), "");
    let value: Value = serde_json::from_slice(&list.stdout).expect("stdout must be JSON");
    assert_eq!(value["entries"], json!([]));

    let purge = prog(&["--dir", dir_arg, "cache", "purge", "--all"]);
    assert!(purge.status.success());
    assert_eq!(stderr(&purge), "");
    let value: Value = serde_json::from_slice(&purge.stdout).expect("stdout must be JSON");
    assert_eq!(value["purged_entries"], 0);
    assert_eq!(value["purged_payloads"], 0);
    assert_eq!(value["purged_cursors"], 0);
}

#[test]
fn cache_payload_budget_evicts_payloads_but_retains_observation_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let file = dir.path().join("large.json");
    fs::write(
        &file,
        serde_json::to_vec(&json!({"items": vec!["x".repeat(64); 32]})).unwrap(),
    )
    .unwrap();

    let observed = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--name",
        "large",
    ]);
    assert!(observed.status.success(), "{}", stdout(&observed));
    let observed: Value = serde_json::from_slice(&observed.stdout).unwrap();
    let cursor = observed["cursor"].as_str().unwrap().to_string();
    let observation_id = observed["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    // No quota is implicit: the freshly stored payload remains recoverable
    // until the explicit quota command is invoked.
    let before_quota = prog(&["--dir", dir_arg, "expand", &cursor]);
    assert!(before_quota.status.success(), "{}", stdout(&before_quota));

    let quota = prog(&[
        "--dir",
        dir_arg,
        "cache",
        "purge",
        "--payload-budget-bytes",
        "0",
    ]);
    assert!(quota.status.success(), "{}", stdout(&quota));
    let quota: Value = serde_json::from_slice(&quota.stdout).unwrap();
    assert_eq!(quota["max_payload_bytes"], 0);
    assert_eq!(quota["evicted_entries"], 1);
    assert_eq!(quota["evicted_payloads"], 1);
    assert_eq!(quota["evicted_cursors"], 1);
    assert_eq!(quota["metadata_only_observations"], 1);

    let expanded = prog(&["--dir", dir_arg, "expand", &cursor]);
    assert!(!expanded.status.success());
    let expanded: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(expanded["error"]["kind"], "cursor_not_found");

    let observations = prog(&["--dir", dir_arg, "cache", "observations"]);
    assert!(observations.status.success(), "{}", stdout(&observations));
    let observations: Value = serde_json::from_slice(&observations.stdout).unwrap();
    assert_eq!(
        observations["observations"][0]["observation_id"],
        observation_id
    );
    assert_eq!(
        observations["observations"][0]["availability"],
        "metadata_only"
    );
}

#[test]
fn persistent_retention_budget_evicts_on_write_without_minting_broken_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let configured = prog(&[
        "--dir",
        dir_arg,
        "cache",
        "retention",
        "--max-payload-bytes",
        "0",
        "--max-age-seconds",
        "60",
    ]);
    assert!(configured.status.success(), "{}", stdout(&configured));
    let configured: Value = serde_json::from_slice(&configured.stdout).unwrap();
    assert_eq!(configured["budget"]["source"], "store_policy");
    assert_eq!(configured["budget"]["max_payload_bytes"], 0);
    assert_eq!(configured["budget"]["max_age_seconds"], 60);
    assert_eq!(configured["storage_budget"], configured["budget"]);

    let file = dir.path().join("retained.json");
    fs::write(&file, br#"{"items":["retention test"]}"#).unwrap();
    let observed = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--name",
        "retained",
    ]);
    assert!(observed.status.success(), "{}", stdout(&observed));
    let observed: Value = serde_json::from_slice(&observed.stdout).unwrap();
    assert!(observed["cursor"].is_null());
    assert_eq!(observed["cache"]["status"], "skipped");
    assert_eq!(observed["observation"]["availability"], "metadata_only");
    assert_eq!(observed["storage_budget"]["max_payload_bytes"], 0);
    assert_eq!(
        observed["capture_budget"],
        observed["observation"]["capture"]["budget"]
    );
    assert!(
        observed["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| {
                warning
                    .as_str()
                    .unwrap()
                    .contains("retention policy evicted")
            })
    );

    let reopened = prog(&["--dir", dir_arg, "cache", "retention"]);
    assert!(reopened.status.success(), "{}", stdout(&reopened));
    let reopened: Value = serde_json::from_slice(&reopened.stdout).unwrap();
    assert_eq!(reopened["max_payload_bytes"], 0);
    assert_eq!(reopened["max_age_seconds"], 60);

    let cleared = prog(&[
        "--dir",
        dir_arg,
        "cache",
        "retention",
        "--clear-max-payload-bytes",
        "--clear-max-age-seconds",
    ]);
    assert!(cleared.status.success(), "{}", stdout(&cleared));
    let cleared: Value = serde_json::from_slice(&cleared.stdout).unwrap();
    assert!(cleared["budget"]["max_payload_bytes"].is_null());
    assert!(cleared["budget"]["max_age_seconds"].is_null());

    let recovered = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--name",
        "retained-after-clear",
    ]);
    assert!(recovered.status.success(), "{}", stdout(&recovered));
    let recovered: Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(recovered["cache"]["status"], "stored");
    assert!(recovered["cursor"].as_str().is_some());
}

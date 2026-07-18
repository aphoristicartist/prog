//! Integration coverage for cursor-path navigation.

use std::fs;

use serde_json::{Value, json};

mod support;

use support::*;

#[test]
fn paths_filters_and_planner_actions_cover_omission_reasons() {
    let dir = tempfile::tempdir().unwrap();
    let wide = (0..30)
        .map(|index| (format!("field_{index:02}"), json!(index)))
        .collect::<serde_json::Map<_, _>>();
    let payload = json!({
        "deep": {"a": {"b": {"c": {"d": {"leaf": "value"}}}}},
        "items": (0..12)
            .map(|index| json!({
                "id": index,
                "body": "body ".to_string() + &"x".repeat(500),
                "token": format!("secret-token-{index}")
            }))
            .collect::<Vec<_>>(),
        "large": "z".repeat(600),
        "wide": Value::Object(wide)
    });
    let file = dir.path().join("planner.json");
    fs::write(&file, serde_json::to_vec(&payload).unwrap()).unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let observed = prog(&[
        "--dir",
        dir_arg,
        "observe",
        "--file",
        file.to_str().unwrap(),
        "--mime",
        "application/json",
        "--name",
        "planner",
    ]);
    assert!(observed.status.success(), "{}", stdout(&observed));
    let envelope: Value = serde_json::from_slice(&observed.stdout).unwrap();
    let cursor = envelope["cursor"].as_str().unwrap();
    let first_action = &envelope["next_actions"][0];
    assert_eq!(first_action["kind"], "evidence");
    assert_eq!(first_action["priority"], 90);
    assert_eq!(first_action["omitted_reason"], "large_string");
    assert_eq!(first_action["argv"][0], "prog");
    assert_eq!(first_action["argv"][1], "evidence");
    assert_eq!(first_action["argv"][2], cursor);
    assert_eq!(
        first_action["offline"],
        "uses cached redacted payload; does not contact upstream"
    );

    let expanded_string = prog(&[
        "--dir", dir_arg, "expand", cursor, "--path", "/large", "--limit", "1000",
    ]);
    assert!(
        expanded_string.status.success(),
        "{}",
        stdout(&expanded_string)
    );
    let expanded_string: Value = serde_json::from_slice(&expanded_string.stdout).unwrap();
    assert_eq!(expanded_string["data_preview"].as_str().unwrap().len(), 600);
    assert!(expanded_string["omitted"].as_array().unwrap().is_empty());
    assert_eq!(
        expanded_string["observation"]["completeness"]["status"],
        "complete"
    );
    assert_eq!(
        expanded_string["observation"]["completeness"]["preview_complete"],
        true
    );
    assert_eq!(
        expanded_string["observation"]["completeness"]["path_scoped"],
        true
    );
    assert_eq!(
        expanded_string["observation"]["completeness"]["root_path"],
        "/large"
    );

    for (reason, expected_path) in [
        ("large_string", "/large"),
        ("long_array", "/items"),
        ("many_fields", "/wide"),
        ("deep_object", "/deep/a/b/c"),
        ("redacted", "/items/0/token"),
    ] {
        let mut args = vec![
            "--dir",
            dir_arg,
            "paths",
            cursor,
            "--reason",
            reason,
            "--omitted-only",
        ];
        if reason == "large_string" {
            args.extend(["--field", "large"]);
        }
        let paths = prog(&args);
        assert!(paths.status.success(), "{}", stdout(&paths));
        let listing: Value = serde_json::from_slice(&paths.stdout).unwrap();
        assert!(
            listing["paths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["path"] == expected_path && entry["omitted_reason"] == reason),
            "paths for {reason} should include {expected_path}: {}",
            stdout(&paths)
        );
        assert!(
            listing["next_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["path"] == expected_path
                    && action["omitted_reason"] == reason
                    && action["argv"][3] == "--path"),
            "next actions for {reason} should include {expected_path}: {}",
            stdout(&paths)
        );
    }

    let token_paths = prog(&[
        "--dir",
        dir_arg,
        "paths",
        cursor,
        "--field",
        "token",
        "--expandable-only",
    ]);
    assert!(token_paths.status.success(), "{}", stdout(&token_paths));
    let filtered: Value = serde_json::from_slice(&token_paths.stdout).unwrap();
    assert!(
        filtered["paths"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["path"].as_str().unwrap().contains("token"))
    );
    assert!(!stdout(&token_paths).contains("secret-token"));

    let bad_reason = prog(&[
        "--dir",
        dir_arg,
        "paths",
        cursor,
        "--reason",
        "semantic_table",
    ]);
    assert!(!bad_reason.status.success());
    let error: Value = serde_json::from_slice(&bad_reason.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown omission reason")
    );
}

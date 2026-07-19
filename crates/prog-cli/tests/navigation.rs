//! Integration coverage for offline cursor navigation.

use serde_json::Value;

mod support;

use support::*;

#[test]
fn evidence_navigation_workflow_is_offline_scoped_bounded_and_session_backed() {
    let dir = tempfile::tempdir().unwrap();
    let run = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "run",
        "--",
        "sh",
        "-c",
        "echo 'error[E0308]: mismatched types' >&2; exit 1",
    ]);
    assert!(run.status.success(), "{}", stdout(&run));
    let envelope: Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(envelope["findings"][0]["path"], "/failure_sections/0");
    assert!(run.stdout.len() <= 16 * 1024);
    let cursor = envelope["cursor"].as_str().unwrap();

    let inspect = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "inspect",
        cursor,
        "--goal",
        "find the root cause",
        "--kind",
        "error",
    ]);
    assert!(inspect.status.success(), "{}", stdout(&inspect));
    let inspect_value: Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(inspect_value["normalized_goal"], "root_cause");
    assert_eq!(inspect_value["findings"][0]["path"], "/failure_sections/0");
    assert!(inspect.stdout.len() <= 16 * 1024);

    let evidence = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "evidence",
        cursor,
        "--path",
        "/failure_sections/0",
    ]);
    assert!(evidence.status.success(), "{}", stdout(&evidence));
    let evidence_value: Value = serde_json::from_slice(&evidence.stdout).unwrap();
    assert_eq!(evidence_value["schema"], "prog.evidence");
    assert_eq!(evidence_value["line_range"]["start"], 1);
    assert!(
        evidence_value["source_command"]
            .as_str()
            .unwrap()
            .contains("sh")
    );

    let search = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "search",
        cursor,
        "mismatched",
        "--path",
        "/failure_sections",
    ]);
    assert!(search.status.success(), "{}", stdout(&search));
    let search_value: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert!(search_value["hits"].as_array().unwrap().iter().all(|hit| {
        hit["path"]
            .as_str()
            .unwrap()
            .starts_with("/failure_sections")
    }));

    let escape = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "find",
        cursor,
        "--kind",
        "error",
        "--path",
        "/outside",
    ]);
    assert!(!escape.status.success());
    let escape_value: Value = serde_json::from_slice(&escape.stdout).unwrap();
    assert!(matches!(
        escape_value["error"]["kind"].as_str(),
        Some("path_not_found" | "path_outside_boundary")
    ));

    let session = prog(&["--dir", dir.path().to_str().unwrap(), "session", "show"]);
    assert!(session.status.success(), "{}", stdout(&session));
    let session_value: Value = serde_json::from_slice(&session.stdout).unwrap();
    let kinds = session_value["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"run"));
    assert!(kinds.contains(&"inspect"));
    assert!(kinds.contains(&"evidence"));
    assert!(kinds.contains(&"search"));
}

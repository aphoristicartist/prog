use std::{fs, os::unix::fs::PermissionsExt};

use serde_json::Value;

mod support;

use support::{prog, stdout};

fn observation_id(value: &Value) -> &str {
    value["observation"]["observation_id"].as_str().unwrap()
}

fn observation_record(dir: &str, observation_id: &str) -> Value {
    let listed = prog(&["--dir", dir, "cache", "observations", "--limit", "20"]);
    assert!(listed.status.success(), "{}", stdout(&listed));
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    listed["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["observation_id"] == observation_id)
        .cloned()
        .unwrap_or_else(|| panic!("missing observation {observation_id}: {listed:#}"))
}

#[test]
fn pytest_provider_is_persisted_and_compatible_across_text_and_json() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let state = dir.path().join("state.txt");
    let pytest = dir.path().join("pytest");
    fs::write(
        &pytest,
        r#"#!/usr/bin/env python3
from pathlib import Path
import json
import sys
mode = Path(sys.argv[1]).read_text().strip()
if mode == "structured":
    print(json.dumps({
        "exitcode": 1,
        "summary": {"failed": 1, "total": 1},
        "tests": [{
            "nodeid": "tests/test_api.py::test_total[€]",
            "outcome": "failed",
            "call": {"crash": {"message": "AssertionError: wrong total"}}
        }]
    }))
else:
    print("FAILED tests/test_api.py::test_total[€] - AssertionError: wrong total")
    print("================ 1 failed in 0.01s ================")
sys.exit(1)
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&pytest).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&pytest, permissions).unwrap();

    fs::write(&state, "baseline").unwrap();
    let first = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--comparison-family",
        "pytest-suite",
        "--",
        pytest.to_str().unwrap(),
        state.to_str().unwrap(),
    ]);
    assert!(first.status.success(), "{}", stdout(&first));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let first_record = observation_record(dir_arg, observation_id(&first));
    assert_eq!(first_record["provider"], "pytest.v1");
    assert_eq!(first_record["parser"], "pytest.v1");
    assert_eq!(first_record["selection"]["scopes"][0], "pytest:all");
    assert_eq!(first_record["capture"]["can_prove_absence"], true);

    let expanded = prog(&[
        "--dir",
        dir_arg,
        "expand",
        first["cursor"].as_str().unwrap(),
        "--path",
        "/provider",
    ]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let expanded: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert_eq!(expanded["data_preview"]["provider"], "pytest.v1");
    assert_eq!(
        expanded["data_preview"]["normalized"]["tests"][0]["node_id"],
        "tests/test_api.py::test_total[€]"
    );

    fs::write(&state, "structured").unwrap();
    let second = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--comparison-family",
        "pytest-suite",
        "--",
        pytest.to_str().unwrap(),
        state.to_str().unwrap(),
    ]);
    assert!(second.status.success(), "{}", stdout(&second));
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    let second_record = observation_record(dir_arg, observation_id(&second));
    assert_eq!(second_record["provider"], "pytest.v1");
    assert_eq!(second_record["parser"], "pytest.v1");
    assert_eq!(second_record["capture"]["can_prove_absence"], true);
    assert_eq!(
        second["data_preview"]["provider"]["input_format"],
        "pytest_json_report"
    );

    let delta = prog(&[
        "--dir",
        dir_arg,
        "delta",
        observation_id(&first),
        observation_id(&second),
    ]);
    assert!(delta.status.success(), "{}", stdout(&delta));
    let delta: Value = serde_json::from_slice(&delta.stdout).unwrap();
    assert_eq!(delta["assessment"]["normalization_compatible"], true);
    assert!(
        delta["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["status"] == "persisting"
                && finding["subject_path"]
                    .as_str()
                    .is_some_and(|path| path.starts_with("/provider/normalized/tests/"))),
        "{delta:#}"
    );
}

#[test]
fn early_stopped_pytest_capture_cannot_prove_absence() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let pytest = dir.path().join("pytest");
    fs::write(
        &pytest,
        "#!/bin/sh\nprintf '%s\\n' 'FAILED tests/test_api.py::test_one - AssertionError' 'stopping after 1 failures' '1 failed in 0.01s'\nexit 1\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&pytest).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&pytest, permissions).unwrap();

    let output = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--",
        pytest.to_str().unwrap(),
        "-x",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    let record = observation_record(dir_arg, observation_id(&output));
    assert_eq!(record["capture"]["can_prove_absence"], false, "{record:#}");
    assert_eq!(
        record["capture"]["stop_reason"], "derivation_windowed",
        "{record:#}"
    );
    assert_eq!(record["selection"]["exhaustive"], false, "{record:#}");
}

#[test]
fn malformed_cargo_provider_keeps_raw_bytes_and_generic_findings() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let cargo = dir.path().join("cargo");
    fs::write(
        &cargo,
        "#!/bin/sh\nprintf '%s\\n' '{malformed}' 'error: could not compile `fixture`' >&2\nexit 101\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&cargo).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&cargo, permissions).unwrap();

    let output = prog(&[
        "--dir",
        dir_arg,
        "run",
        "--",
        cargo.to_str().unwrap(),
        "test",
        "--lib",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    let record = observation_record(dir_arg, observation_id(&output));
    assert_eq!(record["provider"], "cargo_rustc.v1");
    assert_eq!(record["capture"]["can_prove_absence"], false);
    assert!(
        output["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| matches!(
                finding["kind"].as_str(),
                Some("compile_error" | "error_message" | "run_failure")
            )),
        "{output:#}"
    );

    let expanded = prog(&[
        "--dir",
        dir_arg,
        "expand",
        output["cursor"].as_str().unwrap(),
        "--path",
        "/stderr/text",
    ]);
    assert!(expanded.status.success(), "{}", stdout(&expanded));
    let expanded: Value = serde_json::from_slice(&expanded.stdout).unwrap();
    assert!(
        expanded["data_preview"]
            .as_str()
            .unwrap()
            .contains("could not compile")
    );
}

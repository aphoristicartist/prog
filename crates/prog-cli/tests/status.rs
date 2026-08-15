//! Agent-facing status facade contract tests.

use serde_json::Value;

mod support;

use support::{prog, stdout};

fn observation_id(value: &Value) -> &str {
    value["observation"]["observation_id"].as_str().unwrap()
}

#[test]
fn status_composes_canonical_readiness_and_delta_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let first = prog(&["--dir", dir_arg, "run", "--", "true"]);
    assert!(first.status.success(), "{}", stdout(&first));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let second = prog(&["--dir", dir_arg, "run", "--", "true"]);
    assert!(second.status.success(), "{}", stdout(&second));
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();

    let direct = prog(&[
        "--dir",
        dir_arg,
        "delta",
        observation_id(&first),
        observation_id(&second),
    ]);
    assert!(direct.status.success(), "{}", stdout(&direct));
    let direct: Value = serde_json::from_slice(&direct.stdout).unwrap();

    let status = prog(&[
        "--dir",
        dir_arg,
        "status",
        "--baseline",
        observation_id(&first),
        "--subject",
        observation_id(&second),
    ]);
    assert!(status.status.success(), "{}", stdout(&status));
    assert!(status.stdout.len() <= 16 * 1024);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["schema"], "prog.status");
    assert_eq!(status["readiness"]["configured"], false);
    assert_eq!(status["readiness"]["ready"], false);
    assert_eq!(status["delta"]["assessment"], direct["assessment"]);
    assert_eq!(status["delta"]["counts"], direct["counts"]);
    assert_eq!(
        status["delta"]["baseline_observation_id"],
        observation_id(&first)
    );
    assert_eq!(
        status["delta"]["subject_observation_id"],
        observation_id(&second)
    );
}

#[test]
fn status_requires_a_complete_delta_pair() {
    let dir = tempfile::tempdir().unwrap();
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "status",
        "--baseline",
        "po1_missing",
    ]);
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "cli_usage");
}

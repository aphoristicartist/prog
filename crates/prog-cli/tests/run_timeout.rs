//! `prog run --timeout-ms` semantics (#286): `0` disables prog's own deadline
//! so the harness timeout governs long suites; positive values still kill.

use serde_json::Value;

mod support;

use support::{prog, stdout};

fn run(timeout_ms: &str) -> Value {
    let store = tempfile::tempdir().unwrap();
    let output = prog(&[
        "--dir",
        store.path().to_str().unwrap(),
        "run",
        "--timeout-ms",
        timeout_ms,
        "--",
        "sleep",
        "0.5",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn zero_timeout_disables_the_prog_deadline() {
    let envelope = run("0");
    let command = &envelope["data_preview"]["command"];
    assert_eq!(command["timed_out"], false, "{envelope:#}");
    assert_eq!(command["exit_code"], 0, "{envelope:#}");
    assert_eq!(
        envelope["observation"]["capture"]["stop_reason"],
        "complete"
    );
    for limit in envelope["capture_budget"]["limits"].as_array().unwrap() {
        assert!(
            limit.get("max_duration_ms").is_none_or(Value::is_null),
            "{limit:#}"
        );
    }
}

#[test]
fn positive_timeout_still_kills_the_command() {
    let envelope = run("50");
    assert_eq!(
        envelope["data_preview"]["command"]["timed_out"], true,
        "{envelope:#}"
    );
    assert_eq!(
        envelope["capture_budget"]["limits"][0]["max_duration_ms"],
        50
    );
}

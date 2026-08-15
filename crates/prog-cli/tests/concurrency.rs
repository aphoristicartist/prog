//! Process-level store contention regressions for parallel agent harnesses.

use std::{
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

fn prog_command(store: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
    command
        .current_dir(store.parent().expect("store has a fixture parent"))
        .arg("--dir")
        .arg(store);
    command
}

fn run_capture(store: &Path, script: &str, extra_args: &[&str]) -> Child {
    let mut command = prog_command(store);
    command
        .args(["run", "--", "sh", "-c", script, "sh"])
        .args(extra_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn prog capture")
}

fn assert_success(output: &Output, context: &str) -> Value {
    assert!(
        output.status.success(),
        "{context} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{context} returned non-JSON: {error}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn concurrent_captures_share_one_store_without_lock_failures() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".prog");
    let children = (0..4)
        .map(|index| {
            run_capture(
                &store,
                "sleep 0.3; printf 'agent-%s' \"$1\"",
                &[&index.to_string()],
            )
        })
        .collect::<Vec<_>>();

    for (index, child) in children.into_iter().enumerate() {
        let output = child.wait_with_output().expect("wait for prog capture");
        let envelope = assert_success(&output, &format!("parallel capture {index}"));
        assert_eq!(envelope["schema"], "prog.disclosure");
        assert_eq!(envelope["source_id"], "run");
    }

    let observations = prog_command(&store)
        .args(["cache", "observations", "--limit", "10"])
        .output()
        .expect("list concurrent observations");
    let observations = assert_success(&observations, "list concurrent observations");
    let run_observations = observations["observations"]
        .as_array()
        .expect("observation list")
        .iter()
        .filter(|observation| observation["source_id"] == "run")
        .count();
    assert_eq!(run_observations, 4, "every successful capture must persist");
}

#[test]
fn navigation_succeeds_while_an_unrelated_capture_is_running() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join(".prog");
    let marker = dir.path().join("child-started");

    let seed = run_capture(&store, "printf seed", &[])
        .wait_with_output()
        .expect("wait for seed capture");
    let seed = assert_success(&seed, "seed capture");
    let cursor = seed["cursor"].as_str().expect("seed cursor");

    let marker_arg = marker.to_string_lossy().into_owned();
    let mut slow = run_capture(&store, "touch \"$1\"; sleep 1; printf slow", &[&marker_arg]);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "slow child never reached its running state"
    );

    let started = Instant::now();
    let navigation = prog_command(&store)
        .args(["evidence", cursor, "--path", "/command/argv"])
        .output()
        .expect("run evidence navigation");
    let navigation = assert_success(&navigation, "concurrent evidence navigation");
    assert_eq!(navigation["schema"], "prog.evidence");
    assert!(
        started.elapsed() < Duration::from_millis(750),
        "navigation waited for the unrelated one-second capture"
    );
    assert!(
        slow.try_wait().expect("poll slow capture").is_none(),
        "the slow capture should still be running when navigation returns"
    );

    let slow = slow.wait_with_output().expect("wait for slow capture");
    assert_success(&slow, "slow capture");
}

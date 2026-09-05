#![cfg(unix)]

use serde_json::Value;
use std::{
    fs,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::{Child, Command};

mod support;
use support::{prog, stdout};

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("inherited_pipes.py"),
            include_str!("../../prog-adapters/tests/fixtures/inherited_pipes.py"),
        )
        .unwrap();
        Self(dir)
    }

    fn spawn(
        &self,
        stream: &str,
        group: &str,
        lifetime: &str,
        parent_delay: &str,
        timeout: &str,
    ) -> Child {
        Command::new(env!("CARGO_BIN_EXE_prog"))
            .current_dir(self.0.path())
            .args([
                "--dir",
                self.0.path().to_str().unwrap(),
                "run",
                "--preserve-exit-code",
                "--selection-scope",
                "full-suite",
                "--selection-exhaustive",
                "--timeout-ms",
                timeout,
                "--",
                "python3",
                self.0.path().join("inherited_pipes.py").to_str().unwrap(),
                self.0.path().to_str().unwrap(),
                stream,
                group,
                lifetime,
                parent_delay,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    async fn pid(&self, name: &str) -> i32 {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if let Ok(text) = fs::read_to_string(self.0.path().join(name))
                    && let Ok(pid) = text.parse()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the fixture did not start")
    }

    async fn assert_holder_terminated(&self) {
        let pid = self.pid("holder.pid").await;
        for _ in 0..40 {
            if !process_running(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("descendant {pid} survived cleanup of the reaped parent's process group");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Detached holders intentionally outlive capture; clean up test-owned
        // descendants on both success and assertion failure.
        for name in ["holder.pid", "parent.pid"] {
            if let Ok(text) = fs::read_to_string(self.0.path().join(name))
                && let Ok(pid) = text.parse::<i32>()
                && process_running(pid)
            {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
}

fn process_running(pid: i32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

fn assert_partial_evidence(value: &Value, stop_reason: &str) {
    assert_eq!(value["observation"]["capture"]["stop_reason"], stop_reason);
    assert_eq!(value["observation"]["capture"]["can_prove_absence"], false);
    assert_eq!(
        value["data_preview"]["stdout"]["text"],
        "stdout before parent exit"
    );
    assert_eq!(
        value["data_preview"]["stderr"]["text"],
        "stderr before parent exit"
    );
}

#[tokio::test]
async fn run_deadline_covers_exited_parent_and_each_inherited_stream() {
    for stream in ["stdout", "stderr", "both"] {
        for group in ["same-group", "detached"] {
            let fixture = Fixture::new();
            let child = fixture.spawn(stream, group, "10", "0", "1000");
            let started = Instant::now();
            let output = tokio::time::timeout(Duration::from_secs(4), child.wait_with_output())
                .await
                .expect("capture exceeded the independent guard deadline")
                .unwrap();
            assert_eq!(output.status.code(), Some(124), "{}", stdout(&output));
            assert!(started.elapsed() < Duration::from_secs(4));
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_partial_evidence(&value, "timeout");
            assert_eq!(
                value["observation"]["capture"]["budget"]["limits"][0]["max_duration_ms"],
                1000
            );
            // Reopen the store and recover the retained partial evidence offline.
            let expanded = prog(&[
                "--dir",
                fixture.0.path().to_str().unwrap(),
                "expand",
                value["cursor"].as_str().unwrap(),
                "--path",
                "/stdout/text",
            ]);
            assert!(expanded.status.success(), "{}", stdout(&expanded));
            let expanded: Value = serde_json::from_slice(&expanded.stdout).unwrap();
            assert_eq!(expanded["data_preview"], "stdout before parent exit");
            if group == "same-group" {
                fixture.assert_holder_terminated().await;
            }
        }
    }
}

#[tokio::test]
async fn run_cancellation_is_honored_after_the_parent_is_reaped() {
    for group in ["same-group", "detached"] {
        let fixture = Fixture::new();
        let child = fixture.spawn("both", group, "10", "0", "10000");
        let wrapper_pid = child.id().unwrap() as i32;
        let parent_pid = fixture.pid("parent.pid").await;
        fixture.pid("holder.pid").await;
        tokio::time::timeout(Duration::from_secs(4), async {
            while unsafe { libc::kill(parent_pid, 0) } == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the immediate parent was not reaped before cancellation");
        assert_eq!(unsafe { libc::kill(wrapper_pid, libc::SIGTERM) }, 0);
        let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
            .await
            .expect("cancellation waited for inherited pipes")
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(128 + libc::SIGTERM),
            "{}",
            stdout(&output)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_partial_evidence(&value, "cancelled");
        assert_eq!(value["data_preview"]["command"]["signal"], libc::SIGTERM);
        if group == "same-group" {
            fixture.assert_holder_terminated().await;
        }
    }
}

#[tokio::test]
async fn run_pipe_drainage_does_not_receive_a_fresh_timeout() {
    let fixture = Fixture::new();
    let child = fixture.spawn("both", "same-group", "10", "1.5", "2000");
    let output = tokio::time::timeout(Duration::from_secs(4), child.wait_with_output())
        .await
        .expect("capture exceeded the independent guard deadline")
        .unwrap();
    assert_eq!(output.status.code(), Some(124), "{}", stdout(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    // Capture duration excludes store initialization and persistence, already
    // bounded by the independent wall-clock guard above. A fresh two-second
    // drain timeout after the parent's 1.5-second delay would exceed this cap.
    assert!(
        value["data_preview"]["command"]["duration_ms"]
            .as_u64()
            .unwrap()
            < 3000,
        "{value}"
    );
    assert_partial_evidence(&value, "timeout");
    fixture.assert_holder_terminated().await;
}

#[tokio::test]
async fn run_captures_short_lived_descendants_after_parent_exit() {
    for group in ["same-group", "detached"] {
        let fixture = Fixture::new();
        let child = fixture.spawn("both", group, "0.1", "0", "2000");
        let output = tokio::time::timeout(Duration::from_secs(4), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(output.status.success(), "{}", stdout(&output));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            value["data_preview"]["stdout"]["text"],
            "stdout before parent exit\nstdout from descendant"
        );
        assert_eq!(
            value["data_preview"]["stderr"]["text"],
            "stderr before parent exit\nstderr from descendant"
        );
        assert_eq!(value["observation"]["capture"]["stop_reason"], "complete");
        assert_eq!(value["observation"]["capture"]["can_prove_absence"], true);
    }
}

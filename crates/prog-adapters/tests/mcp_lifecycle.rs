#![cfg(unix)]

use std::{collections::BTreeMap, future::Future, process::Command, time::Duration};

use prog_adapters::mcp::{McpSource, McpTaskStatus};
use serde_json::json;
use tempfile::TempDir;

struct Fixture {
    source: McpSource,
    dir: TempDir,
}

impl Fixture {
    fn new(mode: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("server.py");
        std::fs::write(&script, include_str!("fixtures/mcp_lifecycle.py")).unwrap();
        Self {
            source: McpSource {
                id: "lifecycle".to_string(),
                command: "python3".to_string(),
                args: vec![
                    script.to_string_lossy().into_owned(),
                    mode.to_string(),
                    dir.path().to_string_lossy().into_owned(),
                ],
                env: BTreeMap::new(),
                timeout_ms: 300,
                max_content_bytes: 1024,
                max_stderr_bytes: 1024,
                max_schema_depth: 32,
            },
            dir,
        }
    }

    fn pid(&self, name: &str) -> u32 {
        std::fs::read_to_string(self.dir.path().join(name))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn methods(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("methods.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    async fn assert_stopped(&self) {
        for name in ["parent.pid", "holder.pid"] {
            let pid = self.pid(name);
            guarded(async {
                while running(pid) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Also runs on assertion/guard failures against the old implementation.
        for name in ["parent.pid", "holder.pid"] {
            if let Ok(contents) = std::fs::read_to_string(self.dir.path().join(name))
                && let Ok(pid) = contents.trim().parse::<i32>()
            {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
}

fn running(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    output.status.success()
        && String::from_utf8_lossy(&output.stdout)
            .trim()
            .chars()
            .next()
            .is_some_and(|state| state != 'Z')
}

async fn guarded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("independent lifecycle test deadline")
}

#[tokio::test]
async fn stalled_stderr_preserves_success_and_truthful_redacted_partial_diagnostics() {
    let fixture = Fixture::new("stderr");
    let result = guarded(fixture.source.call_tool("read", &json!({})))
        .await
        .unwrap();
    assert!(result.data.to_string().contains("fixture result"));
    let stderr = &result.diagnostics.stderr;
    assert_eq!(stderr["head"][0], "diagnostic marker");
    assert_eq!(stderr["complete"], false);
    assert_eq!(stderr["stop_reason"], "timeout");
    assert!(stderr["byte_count"].is_null());
    assert!(stderr["observed_byte_count"].as_u64().unwrap() > 0);
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("secret-mcp-token")
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("stderr collection is incomplete"))
    );
    fixture.assert_stopped().await;
}

#[tokio::test]
async fn finite_and_empty_stderr_reach_eof_with_exact_counts() {
    for mode in ["finite", "empty"] {
        let fixture = Fixture::new(mode);
        let result = guarded(fixture.source.call_tool("read", &json!({})))
            .await
            .unwrap();
        let stderr = result.diagnostics.stderr;
        assert_eq!(stderr["complete"], true);
        assert_eq!(stderr["stop_reason"], "eof");
        assert_eq!(stderr["byte_count"], stderr["observed_byte_count"]);
        assert_eq!(stderr["truncated"], false);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        if mode == "empty" {
            assert_eq!(stderr["byte_count"], 0);
            assert_eq!(stderr["line_count"], 0);
        } else {
            assert_eq!(stderr["head"][2], "late diagnostic");
            fixture.assert_stopped().await;
        }
    }
}

#[tokio::test]
async fn discovery_and_shutdown_timeout_clean_up_connection_groups() {
    let fixture = Fixture::new("stderr");
    let discovered = guarded(fixture.source.discover()).await.unwrap();
    assert_eq!(discovered.profile.operations[0].id, "read");
    assert_eq!(discovered.diagnostics.stderr["complete"], false);
    assert!(!discovered.warnings.is_empty());
    fixture.assert_stopped().await;

    let fixture = Fixture::new("shutdown_timeout");
    let result = guarded(fixture.source.call_tool("read", &json!({})))
        .await
        .unwrap();
    assert!(result.data.to_string().contains("fixture result"));
    assert_eq!(result.diagnostics.stderr["stop_reason"], "shutdown_timeout");
    assert!(result.diagnostics.stderr["byte_count"].is_null());
    fixture.assert_stopped().await;
}

#[tokio::test]
async fn connection_and_request_failures_clean_up_descendants() {
    for mode in [
        "handshake_timeout",
        "handshake_error",
        "request_timeout",
        "request_error",
    ] {
        let fixture = Fixture::new(mode);
        assert!(
            guarded(fixture.source.call_tool("read", &json!({})))
                .await
                .is_err(),
            "{mode}"
        );
        fixture.assert_stopped().await;
    }
}

#[tokio::test]
async fn dropping_handshake_request_or_shutdown_cleans_up_in_live_runtime() {
    for (mode, ready) in [
        ("handshake_timeout", "initialize"),
        ("request_timeout", "tools/call"),
        ("shutdown_timeout", "tools/call"),
    ] {
        let mut fixture = Fixture::new(mode);
        fixture.source.timeout_ms = 5_000;
        let source = fixture.source.clone();
        let task = tokio::spawn(async move { source.call_tool("read", &json!({})).await });
        guarded(async {
            while !fixture.methods().iter().any(|method| method == ready) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if mode == "shutdown_timeout" {
            guarded(async {
                while !fixture.dir.path().join("shutdown.ready").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await;
        }
        task.abort();
        assert!(guarded(task).await.unwrap_err().is_cancelled());
        fixture.assert_stopped().await;
    }
}

#[tokio::test]
async fn escaped_pipe_holder_is_bounded_without_claiming_it_was_terminated() {
    let fixture = Fixture::new("escaped");
    let result = guarded(fixture.source.call_tool("read", &json!({})))
        .await
        .unwrap();
    assert_eq!(result.diagnostics.stderr["complete"], false);
    assert_eq!(result.diagnostics.stderr["stop_reason"], "timeout");
    assert!(running(fixture.pid("holder.pid")));
}

#[tokio::test]
async fn cleanup_does_not_add_task_polls_retries_or_cancellation() {
    for (action, method) in [
        ("start", "tools/call"),
        ("get", "tasks/get"),
        ("result", "tasks/result"),
        ("cancel", "tasks/cancel"),
    ] {
        let fixture = Fixture::new("stderr");
        match action {
            "start" => {
                let result = guarded(fixture.source.call_tool_as_task("read", &json!({}), None))
                    .await
                    .unwrap();
                assert_eq!(result.task.status, McpTaskStatus::Working);
                assert_eq!(result.diagnostics.stderr["complete"], false);
            }
            "get" => {
                guarded(fixture.source.get_task("fixture-task"))
                    .await
                    .unwrap();
            }
            "result" => {
                guarded(fixture.source.get_task_result("fixture-task"))
                    .await
                    .unwrap();
            }
            "cancel" => {
                guarded(fixture.source.cancel_task("fixture-task"))
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            fixture
                .methods()
                .iter()
                .filter(|method| !method.starts_with("notifications/"))
                .cloned()
                .collect::<Vec<_>>(),
            ["initialize", method]
        );
        fixture.assert_stopped().await;
    }
}

#[tokio::test]
async fn successful_shutdown_leaves_workers_that_released_transport_pipes_running() {
    let fixture = Fixture::new("worker");
    let result = guarded(fixture.source.call_tool_as_task("read", &json!({}), None))
        .await
        .unwrap();
    assert_eq!(result.task.status, McpTaskStatus::Working);
    assert_eq!(result.diagnostics.stderr["complete"], true);
    assert!(running(fixture.pid("holder.pid")));
}

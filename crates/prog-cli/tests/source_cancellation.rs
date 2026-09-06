#![cfg(unix)]

use std::{
    fs,
    process::Stdio,
    time::{Duration, Instant},
};

use prog_core::Store;
use serde_json::{Value, json};
use tokio::process::{Child, Command};

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
        command
            .current_dir(self.0.path())
            .arg("--dir")
            .arg(self.0.path().join("store"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    async fn run(&self, args: &[&str]) -> Value {
        let output =
            tokio::time::timeout(Duration::from_secs(5), self.command().args(args).output())
                .await
                .expect("fixture setup exceeded its independent deadline")
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    async fn wait_pid(&self, name: &str) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            if let Ok(value) = fs::read_to_string(self.0.path().join(name))
                && let Ok(pid) = value.parse()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "fixture did not start");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn observations(&self) -> usize {
        Store::open(self.0.path().join("store"))
            .unwrap()
            .list_observations(100)
            .unwrap()
            .observations
            .len()
    }

    async fn cancel(&self, child: Child, signal: i32, before: usize) {
        let parent = self.wait_pid("parent.pid").await;
        let holder = self.wait_pid("holder.pid").await;
        assert_eq!(unsafe { libc::kill(child.id().unwrap() as i32, signal) }, 0);
        let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
            .await
            .expect("cancelled source call exceeded its independent deadline")
            .unwrap();
        assert!(!output.status.success());
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["kind"], "call_cancelled");
        assert_eq!(value["error"]["retryable"], false);
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("effects may be unknown")
        );
        assert!(value.get("observation").is_none());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-cancel-secret"));
        assert_eq!(
            self.observations(),
            before,
            "interruption must not create a complete observation"
        );
        for pid in [parent, holder] {
            let deadline = Instant::now() + Duration::from_secs(1);
            while running(pid) {
                assert!(
                    Instant::now() < deadline,
                    "owned source process {pid} survived cancellation"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

fn running(pid: i32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&output.stdout);
    !state.trim().is_empty() && !state.trim().starts_with('Z')
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for name in ["parent.pid", "holder.pid"] {
            if let Ok(value) = fs::read_to_string(self.0.path().join(name))
                && let Ok(pid) = value.parse::<i32>()
                && running(pid)
            {
                let identity = std::process::Command::new("ps")
                    .args(["-o", "command=", "-p", &pid.to_string()])
                    .output()
                    .unwrap();
                if String::from_utf8_lossy(&identity.stdout)
                    .contains(self.0.path().to_str().unwrap())
                {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn cli_source_cancellation_stops_live_and_reaped_parent_groups_without_success_evidence() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        for delay in ["0", "10"] {
            let fixture = Fixture(tempfile::tempdir().unwrap());
            let script = fixture.0.path().join("inherited_pipes.py");
            fs::write(
                &script,
                include_str!("../../prog-adapters/tests/fixtures/inherited_pipes.py"),
            )
            .unwrap();
            fixture
                .run(&[
                    "source",
                    "add-cli",
                    "fixture",
                    "--operation",
                    "read",
                    "--read-only",
                    "--",
                    "python3",
                    script.to_str().unwrap(),
                    fixture.0.path().to_str().unwrap(),
                    "both",
                    "same-group",
                    "10",
                    delay,
                ])
                .await;
            let before = fixture.observations();
            let child = fixture
                .command()
                .args(["call", "fixture", "read", "--args", "{}"])
                .spawn()
                .unwrap();
            if delay == "0" {
                let parent = fixture.wait_pid("parent.pid").await;
                let deadline = Instant::now() + Duration::from_secs(2);
                while running(parent) {
                    assert!(Instant::now() < deadline, "parent did not exit");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            fixture.cancel(child, signal, before).await;
        }
    }
}

#[tokio::test]
async fn mcp_source_cancellation_stops_server_and_stderr_holder_without_success_evidence() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let fixture = Fixture(tempfile::tempdir().unwrap());
        let script = fixture.0.path().join("server.py");
        fs::write(&script, r#"import json, os, signal, sys, time
from pathlib import Path
signal.alarm(10)
def reply(mid, value):
    print(json.dumps({'jsonrpc':'2.0','id':mid,'result':value}), flush=True)
for line in sys.stdin:
    message = json.loads(line)
    mid = message.get('id')
    if mid is None: continue
    if message['method'] == 'initialize':
        reply(mid, {'protocolVersion':message['params']['protocolVersion'], 'capabilities':{'tools':{}},
                    'serverInfo':{'name':'cancel-fixture','version':'1'}})
    elif message['method'] == 'tools/list':
        reply(mid, {'tools':[{'name':'read','inputSchema':{'type':'object'},'annotations':{'readOnlyHint':True}}]})
    elif message['method'] == 'tools/call':
        Path('parent.pid').write_text(str(os.getpid()))
        print('API_KEY=synthetic-cancel-secret', file=sys.stderr, flush=True)
        if os.fork() == 0:
            signal.alarm(10)
            Path('holder.pid').write_text(str(os.getpid()))
            time.sleep(10)
            os._exit(0)
        time.sleep(10)
"#).unwrap();
        let seed = fixture.0.path().join("seed.json");
        fs::write(
            &seed,
            json!({"command":"python3","args":[script],"timeout_ms":10000}).to_string(),
        )
        .unwrap();
        fixture
            .run(&[
                "discover",
                "fixture",
                "--kind",
                "mcp",
                "--seed",
                seed.to_str().unwrap(),
            ])
            .await;
        let before = fixture.observations();
        let child = fixture
            .command()
            .args(["call", "fixture", "read", "--args", "{}"])
            .spawn()
            .unwrap();
        fixture.cancel(child, signal, before).await;
    }
}

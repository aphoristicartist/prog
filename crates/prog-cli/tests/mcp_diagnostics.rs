#![cfg(unix)]

use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

use prog_core::Store;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn run(&self, args: &[&str]) -> Value {
        let output_path = self.dir.path().join("stdout.json");
        let mut child = Command::new(env!("CARGO_BIN_EXE_prog"))
            .current_dir(self.dir.path())
            .arg("--dir")
            .arg(self.dir.path().join("store"))
            .args(args)
            .stdout(fs::File::create(&output_path).unwrap())
            .stderr(fs::File::create(self.dir.path().join("stderr.txt")).unwrap())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("CLI exceeded independent test deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = fs::read_to_string(output_path).unwrap();
        assert!(status.success(), "{output}");
        serde_json::from_str(&output).unwrap()
    }

    fn pids(&self) -> Vec<i32> {
        fs::read_to_string(self.dir.path().join("pids"))
            .unwrap_or_default()
            .lines()
            .map(|line| line.parse().unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for pid in self.pids() {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

#[test]
fn interrupted_mcp_stderr_is_truthful_redacted_persisted_evidence() {
    let fixture = Fixture {
        dir: tempfile::tempdir().unwrap(),
    };
    let script = fixture.dir.path().join("server.py");
    fs::write(&script, r#"import json, os, signal, subprocess, sys
signal.alarm(10)
child = subprocess.Popen([sys.executable, '-c', 'import signal,time; signal.alarm(10); time.sleep(8)'],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=sys.stderr)
with open('pids', 'a') as pids:
    pids.write(str(os.getpid()) + '\n' + str(child.pid) + '\n')
print('diagnostic marker', file=sys.stderr, flush=True)
print('Authorization: Bearer secret-mcp-token', file=sys.stderr, flush=True)
print('{"password":\n"MULTILINE_MCP_SECRET"}', file=sys.stderr, flush=True)
def reply(message_id, result):
    print(json.dumps({'jsonrpc': '2.0', 'id': message_id, 'result': result}), flush=True)
for line in sys.stdin:
    message = json.loads(line)
    message_id = message.get('id')
    if message_id is None:
        continue
    method = message['method']
    if method == 'initialize':
        reply(message_id, {'protocolVersion': message['params']['protocolVersion'],
              'capabilities': {'tools': {}}, 'serverInfo': {'name': 'diagnostics', 'version': '1'}})
    elif method == 'tools/list':
        reply(message_id, {'tools': [{'name': 'read', 'inputSchema': {'type': 'object'},
              'annotations': {'readOnlyHint': True}}]})
    elif method == 'tools/call':
        reply(message_id, {'structuredContent': {'received': True}, 'content': []})
"#).unwrap();
    let seed = fixture.dir.path().join("seed.json");
    fs::write(
        &seed,
        json!({"command": "python3", "args": [script], "timeout_ms": 300}).to_string(),
    )
    .unwrap();
    fixture.run(&[
        "discover",
        "fixture",
        "--kind",
        "mcp",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    let envelope = fixture.run(&["call", "fixture", "read", "--args", "{}"]);
    assert_eq!(envelope["data_preview"]["received"], true);
    let stderr = &envelope["provenance"]["adapter"]["diagnostics"]["stderr"];
    assert_eq!(stderr["head"][0], "diagnostic marker");
    assert_eq!(stderr["complete"], false);
    assert_eq!(stderr["stop_reason"], "timeout");
    assert!(stderr["byte_count"].is_null());
    assert!(stderr["observed_byte_count"].as_u64().unwrap() > 0);
    assert!(!envelope.to_string().contains("secret-mcp-token"));
    assert!(!envelope.to_string().contains("MULTILINE_MCP_SECRET"));

    let store = Store::open(fixture.dir.path().join("store")).unwrap();
    let observation = store
        .get_observation(envelope["observation"]["observation_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let persisted = serde_json::to_value(observation).unwrap();
    assert_eq!(
        persisted["provenance"]["adapter"]["diagnostics"]["stderr"],
        *stderr
    );
    drop(store);
    let stored = fs::read(fixture.dir.path().join("store/cache/data.redb")).unwrap();
    assert!(
        !stored
            .windows(b"secret-mcp-token".len())
            .any(|bytes| bytes == b"secret-mcp-token")
    );
    assert!(
        !stored
            .windows(b"MULTILINE_MCP_SECRET".len())
            .any(|bytes| bytes == b"MULTILINE_MCP_SECRET")
    );

    assert_eq!(fixture.pids().len(), 4);
    for pid in fixture.pids() {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let output = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout);
            if !output.status.success() || state.trim().is_empty() || state.trim().starts_with('Z')
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned process {pid} survived CLI exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

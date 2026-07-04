use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};
use tempfile::TempDir;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const MAX_ENVELOPE_BYTES: usize = 16 * 1024;
const MIN_RATIO: f64 = 10.0;
const ITEM_COUNT: usize = 260;
const BODY_BYTES: usize = 2048;

#[derive(Debug)]
struct Fixture {
    name: &'static str,
    source_id: &'static str,
    operation: &'static str,
    array_path: &'static str,
    target_path: &'static str,
    raw_bytes: usize,
    _tempdir: Option<TempDir>,
    _server: Option<MockServer>,
}

#[derive(Debug)]
struct EvalRow {
    fixture: &'static str,
    task: &'static str,
    raw_bytes: usize,
    prog_bytes: usize,
}

fn prog(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_prog"))
        .arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("prog binary should run")
}

#[tokio::test]
async fn token_economics_eval_smoke() {
    let tempdir = tempfile::tempdir().unwrap();
    let http = setup_http_fixture(tempdir.path()).await;
    let cli = setup_cli_fixture(tempdir.path());
    let mcp = setup_mcp_fixture(tempdir.path());

    let rows = [http, cli, mcp]
        .iter()
        .flat_map(|fixture| run_fixture_tasks(tempdir.path(), fixture))
        .collect::<Vec<_>>();

    for row in &rows {
        assert!(
            row.ratio() >= MIN_RATIO,
            "{} {} ratio too low: {:.1}x",
            row.fixture,
            row.task,
            row.ratio()
        );
    }

    let report = markdown_report(&rows);
    if std::env::var_os("PROG_TOKEN_EVAL_UPDATE").is_some() {
        fs::write(repo_root().join("docs/token-economics.md"), &report).unwrap();
        println!("{report}");
    }
}

async fn setup_http_fixture(root: &Path) -> Fixture {
    let server = MockServer::start().await;
    let payload = fixture_payload("items", "http");
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(payload.clone()))
        .mount(&server)
        .await;

    let seed = root.join("http-seed.json");
    fs::write(
        &seed,
        serde_json::to_vec_pretty(&json!({
            "kind": "http",
            "base_url": server.uri(),
            "operations": [{
                "name": "list",
                "method": "GET",
                "path": "/items"
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    discover(root, "http_eval", "http", &seed);

    Fixture {
        name: "HTTP",
        source_id: "http_eval",
        operation: "list",
        array_path: "/items",
        target_path: "/items/42/body",
        raw_bytes: serde_json::to_vec(&payload).unwrap().len(),
        _tempdir: None,
        _server: Some(server),
    }
}

fn setup_cli_fixture(root: &Path) -> Fixture {
    let payload = fixture_payload("items", "cli");
    let payload_path = root.join("cli-payload.json");
    fs::write(&payload_path, serde_json::to_vec(&payload).unwrap()).unwrap();
    let command = format!(
        "import pathlib; print(pathlib.Path({:?}).read_text())",
        payload_path.to_string_lossy()
    );
    let seed = root.join("cli-seed.json");
    fs::write(
        &seed,
        serde_json::to_vec_pretty(&json!({
            "kind": "cli",
            "operations": [{
                "name": "list",
                "command": "python3",
                "args": ["-c", command],
                "effect": read_only_effect()
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    discover(root, "cli_eval", "cli", &seed);

    Fixture {
        name: "CLI",
        source_id: "cli_eval",
        operation: "list",
        array_path: "/items",
        target_path: "/items/42/body",
        raw_bytes: serde_json::to_vec(&payload).unwrap().len(),
        _tempdir: None,
        _server: None,
    }
}

fn setup_mcp_fixture(root: &Path) -> Fixture {
    let tempdir = tempfile::tempdir().unwrap();
    let payload = fixture_payload("results", "mcp");
    let payload_path = tempdir.path().join("mcp-payload.json");
    fs::write(&payload_path, serde_json::to_vec(&payload).unwrap()).unwrap();
    let script = tempdir.path().join("fixture_mcp.py");
    fs::write(&script, MCP_SERVER).unwrap();
    let seed = root.join("mcp-seed.json");
    fs::write(
        &seed,
        serde_json::to_vec_pretty(&json!({
            "kind": "mcp",
            "command": "python3",
            "args": [script, payload_path]
        }))
        .unwrap(),
    )
    .unwrap();
    discover(root, "mcp_eval", "mcp", &seed);

    Fixture {
        name: "MCP",
        source_id: "mcp_eval",
        operation: "search_docs",
        array_path: "/results",
        target_path: "/results/42/body",
        raw_bytes: serde_json::to_vec(&payload).unwrap().len(),
        _tempdir: Some(tempdir),
        _server: None,
    }
}

fn run_fixture_tasks(root: &Path, fixture: &Fixture) -> Vec<EvalRow> {
    let shape = call(root, fixture.source_id, fixture.operation);
    let shape_bytes = checked_stdout_len(&shape);

    let count_call = call(root, fixture.source_id, fixture.operation);
    let count_cursor = cursor(&count_call);
    let count_expand = prog(
        root,
        &[
            "expand",
            &count_cursor,
            "--path",
            fixture.array_path,
            "--fields",
            "state",
            "--limit",
            &ITEM_COUNT.to_string(),
            "--depth",
            "3",
        ],
    );
    assert_success(&count_expand);
    let count_bytes = checked_stdout_len(&count_call) + checked_stdout_len(&count_expand);

    let target_call = call(root, fixture.source_id, fixture.operation);
    let target_cursor = cursor(&target_call);
    let target_expand = prog(
        root,
        &["expand", &target_cursor, "--path", fixture.target_path],
    );
    assert_success(&target_expand);
    let target_bytes = checked_stdout_len(&target_call) + checked_stdout_len(&target_expand);

    vec![
        EvalRow {
            fixture: fixture.name,
            task: "Discover shape",
            raw_bytes: fixture.raw_bytes,
            prog_bytes: shape_bytes,
        },
        EvalRow {
            fixture: fixture.name,
            task: "Count states",
            raw_bytes: fixture.raw_bytes,
            prog_bytes: count_bytes,
        },
        EvalRow {
            fixture: fixture.name,
            task: "Target body",
            raw_bytes: fixture.raw_bytes,
            prog_bytes: target_bytes,
        },
    ]
}

fn discover(root: &Path, source_id: &str, kind: &str, seed: &Path) {
    let output = prog(
        root,
        &[
            "discover",
            source_id,
            "--kind",
            kind,
            "--seed",
            seed.to_str().unwrap(),
        ],
    );
    assert_success(&output);
}

fn call(root: &Path, source_id: &str, operation: &str) -> Output {
    let output = prog(root, &["call", source_id, operation, "--args", "{}"]);
    assert_success(&output);
    output
}

fn checked_stdout_len(output: &Output) -> usize {
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let envelope_bytes = value["summary"]["envelope_bytes"].as_u64().unwrap() as usize;
    assert!(
        envelope_bytes <= MAX_ENVELOPE_BYTES,
        "envelope_bytes exceeded budget: {envelope_bytes}"
    );
    assert!(
        output.stdout.len() <= MAX_ENVELOPE_BYTES,
        "stdout exceeded budget: {}",
        output.stdout.len()
    );
    output.stdout.len()
}

fn cursor(output: &Output) -> String {
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    value["cursor"].as_str().unwrap().to_string()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}

fn fixture_payload(array_key: &str, prefix: &str) -> Value {
    json!({
        array_key: (0..ITEM_COUNT).map(|index| {
            json!({
                "id": index,
                "state": if index % 3 == 0 { "open" } else { "closed" },
                "title": format!("{prefix} document {index}"),
                "body": format!("{prefix}-{index}-{}", "x".repeat(BODY_BYTES))
            })
        }).collect::<Vec<_>>(),
        "meta": {
            "fixture": prefix,
            "item_count": ITEM_COUNT
        }
    })
}

fn read_only_effect() -> Value {
    json!({
        "read_only": true,
        "mutating": false,
        "network": false,
        "shell": false,
        "sensitive": false,
        "cacheable": true,
        "requires_confirmation": false
    })
}

fn markdown_report(rows: &[EvalRow]) -> String {
    let mut output = String::from(
        "# Token economics eval\n\n\
         Token counts use the project heuristic `bytes / 4`, rounded up. Raw cost is the full fixture payload entering context. prog cost is the sum of every bounded envelope or expansion stdout consumed for the task, including the initial call envelope before any expansion. This is not a latency benchmark or a model-success benchmark.\n\n\
         Regenerate this table with `PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture`.\n\n\
         | Fixture | Task | Raw tokens | prog tokens | Ratio |\n\
         |---|---:|---:|---:|---:|\n",
    );
    for row in rows {
        output.push_str(&format!(
            "| {} | {} | {} | {} | {:.1}x |\n",
            row.fixture,
            row.task,
            approx_tokens(row.raw_bytes),
            approx_tokens(row.prog_bytes),
            row.ratio()
        ));
    }
    output
}

fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
}

impl EvalRow {
    fn ratio(&self) -> f64 {
        approx_tokens(self.raw_bytes) as f64 / approx_tokens(self.prog_bytes).max(1) as f64
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

const MCP_SERVER: &str = r#"
import json
import pathlib
import sys

payload_path = pathlib.Path(sys.argv[1])

def send_result(message_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": message_id, "result": result}) + "\n")
    sys.stdout.flush()

def send_error(message_id, code, message):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": message_id, "error": {"code": code, "message": message}}) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    message_id = message.get("id")
    if message_id is None:
        continue
    if method == "initialize":
        send_result(message_id, {
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "prog-eval-fixture", "version": "1.0.0"},
        })
    elif method == "tools/list":
        send_result(message_id, {"tools": [{
            "name": "search_docs",
            "description": "Return the eval fixture payload",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": True},
        }]})
    elif method == "tools/call":
        send_result(message_id, {
            "content": [{"type": "text", "text": "structured eval payload"}],
            "structuredContent": json.loads(payload_path.read_text()),
            "isError": False,
        })
    else:
        send_error(message_id, -32601, f"unknown method: {method}")
"#;

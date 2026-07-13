use std::{
    ffi::OsStr,
    fs,
    path::Path,
    process::{Command, Output},
};

use serde_json::{Map, Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const CASES_PER_TRANSPORT: usize = 120;
const TRANSPORTS: usize = 3;
const MAX_ENVELOPE_BYTES: u64 = 16 * 1024;

fn prog<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_prog"))
        .args(args)
        .output()
        .expect("prog binary should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be utf8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be utf8")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
    assert_eq!(stderr(output), "");
}

fn json_output(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout should be JSON")
}

#[tokio::test]
async fn messy_transport_matrix_covers_360_scenarios() {
    let cases = messy_cases();
    let dir = tempfile::tempdir().unwrap();
    let cases_path = dir.path().join("cases.json");
    fs::write(&cases_path, serde_json::to_vec(&cases).unwrap()).unwrap();

    let mut scenarios = 0usize;
    scenarios += run_http_matrix(dir.path(), &cases).await;
    scenarios += run_cli_matrix(dir.path(), &cases_path);
    scenarios += run_mcp_matrix(dir.path(), &cases_path);

    assert_eq!(scenarios, CASES_PER_TRANSPORT * TRANSPORTS);
}

async fn run_http_matrix(root: &Path, cases: &[Value]) -> usize {
    let server = MockServer::start().await;
    for (id, payload) in cases.iter().enumerate() {
        Mock::given(method("GET"))
            .and(path(format!("/case/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(payload))
            .mount(&server)
            .await;
    }

    let store = root.join("http-store");
    let seed = root.join("http-seed.json");
    fs::write(
        &seed,
        serde_json::to_vec_pretty(&json!({
            "kind": "http",
            "base_url": server.uri(),
            "operations": [{
                "name": "messy",
                "method": "GET",
                "path": "/case/{id}",
                "input_schema": id_schema(),
                "effect": read_only_effect(true, false)
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    discover(&store, "http_matrix", "http", &seed);
    for id in 0..cases.len() {
        validate_scenario(&store, "http_matrix", "messy", id);
    }
    cases.len()
}

fn run_cli_matrix(root: &Path, cases_path: &Path) -> usize {
    let store = root.join("cli-store");
    let script = root.join("messy_cli.py");
    fs::write(&script, MESSY_CLI).unwrap();
    let seed = root.join("cli-seed.json");
    fs::write(
        &seed,
        serde_json::to_vec_pretty(&json!({
            "kind": "cli",
            "operations": [{
                "name": "messy",
                "command": "python3",
                "args": [
                    script.to_string_lossy(),
                    cases_path.to_string_lossy(),
                    "{id}"
                ],
                "input_schema": id_schema(),
                "effect": read_only_effect(true, false)
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    discover(&store, "cli_matrix", "cli", &seed);
    for id in 0..CASES_PER_TRANSPORT {
        validate_scenario(&store, "cli_matrix", "messy", id);
    }
    CASES_PER_TRANSPORT
}

fn run_mcp_matrix(root: &Path, cases_path: &Path) -> usize {
    let store = root.join("mcp-store");
    let script = root.join("messy_mcp.py");
    fs::write(&script, MESSY_MCP).unwrap();
    let seed = root.join("mcp-seed.json");
    fs::write(
        &seed,
        serde_json::to_vec_pretty(&json!({
            "kind": "mcp",
            "command": "python3",
            "args": [
                script.to_string_lossy(),
                cases_path.to_string_lossy()
            ],
            "timeout_ms": 5000
        }))
        .unwrap(),
    )
    .unwrap();

    discover(&store, "mcp_matrix", "mcp", &seed);
    for id in 0..CASES_PER_TRANSPORT {
        validate_scenario(&store, "mcp_matrix", "messy_case", id);
    }
    CASES_PER_TRANSPORT
}

fn discover(store: &Path, source_id: &str, kind: &str, seed: &Path) {
    let output = prog([
        "--dir",
        store.to_str().unwrap(),
        "discover",
        source_id,
        "--kind",
        kind,
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert_success(&output);
    let value = json_output(&output);
    assert_eq!(value["operations_found"], 1);
    assert_eq!(value["effects_assumed"].as_array().unwrap().len(), 0);
}

fn validate_scenario(store: &Path, source_id: &str, operation: &str, id: usize) {
    let args = format!(r#"{{"id":{id}}}"#);
    let call = prog([
        "--dir",
        store.to_str().unwrap(),
        "call",
        source_id,
        operation,
        "--args",
        &args,
    ]);
    assert_success(&call);
    let text = stdout(&call);
    assert!(
        !text.contains("secret-value-"),
        "{source_id} case {id} leaked token-like field"
    );
    assert!(
        !text.contains("password-value-"),
        "{source_id} case {id} leaked password-like field"
    );

    let envelope: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(envelope["schema"], "prog.disclosure");
    assert_eq!(envelope["source_id"], source_id);
    assert_eq!(envelope["operation"], operation);
    assert_eq!(envelope["cache"]["status"], "stored");
    assert!(
        envelope["summary"]["envelope_bytes"].as_u64().unwrap() <= MAX_ENVELOPE_BYTES,
        "{source_id} case {id} call envelope exceeded bound"
    );
    assert!(envelope["cursor"].as_str().unwrap().starts_with("pc1_"));
    assert!(
        !envelope["schema_hints"].as_object().unwrap().is_empty(),
        "{source_id} case {id} should expose schema hints"
    );

    let cursor = envelope["cursor"].as_str().unwrap();
    let expand = prog([
        "--dir",
        store.to_str().unwrap(),
        "expand",
        cursor,
        "--path",
        "/items",
        "--limit",
        "3",
        "--depth",
        "3",
    ]);
    assert_success(&expand);
    let expanded = json_output(&expand);
    assert_eq!(expanded["cache"]["status"], "hit");
    assert!(
        expanded["summary"]["envelope_bytes"].as_u64().unwrap() <= MAX_ENVELOPE_BYTES,
        "{source_id} case {id} expand envelope exceeded bound"
    );
    assert!(expanded["data_preview"].is_array());

    if id.is_multiple_of(17) {
        let secret = prog([
            "--dir",
            store.to_str().unwrap(),
            "expand",
            cursor,
            "--path",
            "/details/token",
        ]);
        assert_success(&secret);
        let secret_text = stdout(&secret);
        assert!(!secret_text.contains(&format!("secret-value-{id}")));
        let secret = serde_json::from_str::<Value>(&secret_text).unwrap();
        assert!(
            secret["omitted"]
                .as_array()
                .unwrap()
                .iter()
                .any(|region| region["reason"] == "redacted"),
            "{source_id} case {id} should mark secret expansion as redacted"
        );
    }

    if id.is_multiple_of(29) {
        let cached = prog([
            "--dir",
            store.to_str().unwrap(),
            "call",
            source_id,
            operation,
            "--args",
            &args,
        ]);
        assert_success(&cached);
        let cached = json_output(&cached);
        assert_eq!(cached["cache"]["status"], "hit");
    }
}

fn messy_cases() -> Vec<Value> {
    (0..CASES_PER_TRANSPORT).map(messy_case).collect()
}

fn messy_case(id: usize) -> Value {
    let item_count = match id % 8 {
        0 => 0,
        1 => 1,
        2 => 3,
        3 => 5,
        4 => 8,
        5 => 13,
        6 => 34,
        _ => 89,
    };
    let items = (0..item_count)
        .map(|index| {
            json!({
                "index": index,
                "state": state(id + index),
                "title": format!("case-{id}-item-{index}"),
                "body": repeated("body", id, index, 32 + ((id + index) % 9) * 80),
                "score": numeric_value(id, index),
                "flags": {
                    "even": index.is_multiple_of(2),
                    "third": index.is_multiple_of(3)
                }
            })
        })
        .collect::<Vec<_>>();

    let mut many_fields = Map::new();
    for field in 0..(4 + (id % 19)) {
        many_fields.insert(
            format!("field_{field:02}"),
            json!(format!("v-{id}-{field}")),
        );
    }

    let mut root = Map::new();
    root.insert("id".to_string(), json!(id));
    root.insert("state".to_string(), json!(state(id)));
    root.insert("items".to_string(), Value::Array(items));
    root.insert(
        "details".to_string(),
        json!({
            "token": format!("secret-value-{id}"),
            "password": format!("password-value-{id}"),
            "nested": nested_value(id, 0),
            "optional": if id.is_multiple_of(4) { Value::Null } else { json!(format!("optional-{id}")) },
            "slash/key": format!("slash-{id}"),
            "tilde~key": format!("tilde-{id}"),
            "space key": format!("space-{id}")
        }),
    );
    root.insert("many_fields".to_string(), Value::Object(many_fields));
    root.insert(
        "mixed".to_string(),
        json!([
            id,
            format!("string-{id}"),
            id.is_multiple_of(2),
            Value::Null,
            {"nested_array": [id, id + 1, id + 2]}
        ]),
    );
    if id.is_multiple_of(6) {
        root.insert(
            "large_note".to_string(),
            json!(repeated("note", id, 0, 2048)),
        );
    }
    if id.is_multiple_of(10) {
        root.insert("empty_object".to_string(), json!({}));
        root.insert("empty_array".to_string(), json!([]));
    }
    Value::Object(root)
}

fn nested_value(id: usize, depth: usize) -> Value {
    if depth >= 7 {
        return json!({"leaf": format!("leaf-{id}-{depth}")});
    }
    json!({
        "depth": depth,
        "label": format!("nested-{id}-{depth}"),
        "next": nested_value(id, depth + 1)
    })
}

fn numeric_value(id: usize, index: usize) -> Value {
    match (id + index) % 4 {
        0 => json!(index),
        1 => json!((id as i64) - (index as i64)),
        2 => json!((id as f64) / ((index + 1) as f64)),
        _ => Value::Null,
    }
}

fn repeated(prefix: &str, id: usize, index: usize, len: usize) -> String {
    let seed = format!("{prefix}-{id}-{index}-");
    seed.repeat((len / seed.len()).saturating_add(1))[..len].to_string()
}

fn state(value: usize) -> &'static str {
    match value % 5 {
        0 => "open",
        1 => "closed",
        2 => "merged",
        3 => "queued",
        _ => "blocked",
    }
}

fn id_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": {"type": "integer"}
        }
    })
}

fn read_only_effect(network: bool, shell: bool) -> Value {
    json!({
        "read_only": true,
        "mutating": false,
        "network": network,
        "shell": shell,
        "sensitive": false,
        "cacheable": true,
        "requires_confirmation": false
    })
}

const MESSY_CLI: &str = r#"
import json
import pathlib
import sys

cases = json.loads(pathlib.Path(sys.argv[1]).read_text())
case_id = int(sys.argv[2])
print(json.dumps(cases[case_id], separators=(",", ":")))
"#;

const MESSY_MCP: &str = r#"
import json
import pathlib
import sys

CASES = json.loads(pathlib.Path(sys.argv[1]).read_text())


def send_result(message_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": message_id, "result": result}) + "\n")
    sys.stdout.flush()


def send_error(message_id, code, message):
    sys.stdout.write(json.dumps({
        "jsonrpc": "2.0",
        "id": message_id,
        "error": {"code": code, "message": message},
    }) + "\n")
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
            "serverInfo": {"name": "messy-matrix", "version": "1.0.0"},
        })
    elif method == "tools/list":
        send_result(message_id, {
            "tools": [{
                "name": "messy_case",
                "description": "Return one generated messy matrix payload",
                "inputSchema": {
                    "type": "object",
                    "required": ["id"],
                    "properties": {"id": {"type": "integer"}},
                },
                "annotations": {"readOnlyHint": True},
            }]
        })
    elif method == "tools/call":
        params = message.get("params", {})
        args = params.get("arguments", {})
        case_id = int(args.get("id"))
        send_result(message_id, {
            "content": [{"type": "text", "text": f"case {case_id}"}],
            "structuredContent": CASES[case_id],
            "isError": False,
        })
    else:
        send_error(message_id, -32601, f"unknown method: {method}")
"#;

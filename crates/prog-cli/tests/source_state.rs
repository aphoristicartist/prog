use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{method, path},
};

mod support;

use support::{prog, prog_with_env, stdout};

struct RotatingResponder {
    calls: AtomicUsize,
    second_was_conditional: Arc<AtomicBool>,
}

impl Respond for RotatingResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call > 0 {
            self.second_was_conditional.store(
                request.headers.get("if-none-match").is_some(),
                Ordering::SeqCst,
            );
        }
        let version = call + 1;
        ResponseTemplate::new(200)
            .insert_header("etag", format!("\"v{version}\""))
            .set_body_json(json!({"id": 7, "version": version}))
    }
}

struct FailingRefreshResponder {
    calls: AtomicUsize,
}

impl Respond for FailingRefreshResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(200)
                .insert_header("etag", "\"v1\"")
                .set_body_json(json!({"id": 7, "state": "prior"}))
        } else {
            ResponseTemplate::new(503).set_body_json(json!({"error": "temporary failure"}))
        }
    }
}

fn write_seed(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    path
}

fn latest_observation(dir: &str) -> Value {
    let listed = prog(&["--dir", dir, "cache", "observations", "--limit", "1"]);
    assert!(listed.status.success(), "{}", stdout(&listed));
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    listed["observations"][0].clone()
}

fn observation(dir: &str, observation_id: &str) -> Value {
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

#[tokio::test]
async fn profile_selector_hashes_raw_state_before_payload_redaction_and_marks_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/entity/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 7,
            "meta": {
                "change/token": "tenant-secret-version-value",
                "expires": "2020-01-01T00:00:00Z"
            }
        })))
        .mount(&server)
        .await;
    let seed = write_seed(
        dir.path(),
        "selector.json",
        &json!({
            "kind": "http",
            "base_url": server.uri(),
            "operations": [{
                "name": "get",
                "method": "GET",
                "path": "/entity/{id}",
                "input_schema": {
                    "type": "object",
                    "properties": {"id": {"type": "integer"}},
                    "required": ["id"]
                },
                "source_state": {
                    "path": "/meta/change~1token",
                    "expires_at_path": "/meta/expires"
                },
                "effect": {
                    "read_only": true,
                    "mutating": false,
                    "network": true,
                    "shell": false,
                    "sensitive": false,
                    "cacheable": true,
                    "requires_confirmation": false
                }
            }]
        })
        .to_string(),
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "selector",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let called = prog(&[
        "--dir",
        dir_arg,
        "call",
        "selector",
        "get",
        "--args",
        r#"{"id":7}"#,
    ]);
    assert!(called.status.success(), "{}", stdout(&called));
    assert!(!stdout(&called).contains("tenant-secret-version-value"));
    let called_value: Value = serde_json::from_slice(&called.stdout).unwrap();
    assert_eq!(
        called_value["evidence_ref"]["source_validity"],
        "validator_expired"
    );
    assert_eq!(
        called_value["evidence_ref"]["source_state_kind"],
        "change_token"
    );
    let record = latest_observation(dir_arg);
    assert_eq!(record["source_state"]["kind"], "change_token");
    assert!(
        record["source_state"]["value"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(record["source_validity"], "validator_expired");
    assert_eq!(record["source_state"]["validity"], "validator_expired");
    assert!(!record.to_string().contains("tenant-secret-version-value"));
}

#[tokio::test]
async fn invalid_http_validator_retains_response_but_cannot_be_reused() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/entity"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "unquoted-invalid")
                .set_body_json(json!({"id": 7, "state": "retained"})),
        )
        .mount(&server)
        .await;
    let seed = write_seed(
        dir.path(),
        "invalid-etag.json",
        &json!({
            "kind": "http",
            "base_url": server.uri(),
            "operations": [{
                "name": "get",
                "method": "GET",
                "path": "/entity",
                "effect": {
                    "read_only": true,
                    "mutating": false,
                    "network": true,
                    "shell": false,
                    "sensitive": false,
                    "cacheable": true,
                    "requires_confirmation": false
                }
            }]
        })
        .to_string(),
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "invalid-etag",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let called = prog(&[
        "--dir",
        dir_arg,
        "call",
        "invalid-etag",
        "get",
        "--args",
        "{}",
    ]);
    assert!(called.status.success(), "{}", stdout(&called));
    let called: Value = serde_json::from_slice(&called.stdout).unwrap();
    assert_eq!(called["data_preview"]["state"], "retained");
    assert!(
        called["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("invalid HTTP validator"))
    );
    let record = latest_observation(dir_arg);
    assert!(record["source_state"].is_null());
    assert_eq!(record["source_validity"], "validator_unavailable");
}

#[tokio::test]
async fn refresh_200_records_rotated_validator_and_source_changed() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let conditional = Arc::new(AtomicBool::new(false));
    Mock::given(method("GET"))
        .and(path("/entity"))
        .respond_with(RotatingResponder {
            calls: AtomicUsize::new(0),
            second_was_conditional: conditional.clone(),
        })
        .expect(2)
        .mount(&server)
        .await;
    let seed = write_seed(
        dir.path(),
        "rotation.json",
        &json!({
            "kind": "http",
            "base_url": server.uri(),
            "operations": [{
                "name": "get",
                "method": "GET",
                "path": "/entity",
                "effect": {
                    "read_only": true,
                    "mutating": false,
                    "network": true,
                    "shell": false,
                    "sensitive": false,
                    "cacheable": true,
                    "requires_confirmation": false
                }
            }]
        })
        .to_string(),
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "rotation",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let first = prog(&["--dir", dir_arg, "call", "rotation", "get", "--args", "{}"]);
    assert!(first.status.success(), "{}", stdout(&first));
    let refreshed = prog(&[
        "--dir",
        dir_arg,
        "call",
        "rotation",
        "get",
        "--args",
        "{}",
        "--refresh",
    ]);
    assert!(refreshed.status.success(), "{}", stdout(&refreshed));
    let refreshed: Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(refreshed["source_validity"], "source_changed");
    assert_eq!(refreshed["data_preview"]["version"], 2);
    assert!(conditional.load(Ordering::SeqCst));
    let record = observation(
        dir_arg,
        refreshed["observation"]["observation_id"].as_str().unwrap(),
    );
    assert_eq!(record["source_state"]["value"], "\"v2\"");
    assert_eq!(record["source_validity"], "source_changed");
}

#[tokio::test]
async fn auth_policy_change_prevents_cross_boundary_conditional_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let conditional = Arc::new(AtomicBool::new(false));
    Mock::given(method("GET"))
        .and(path("/entity"))
        .respond_with(RotatingResponder {
            calls: AtomicUsize::new(0),
            second_was_conditional: conditional.clone(),
        })
        .expect(2)
        .mount(&server)
        .await;
    let seed = write_seed(
        dir.path(),
        "auth-boundary.json",
        &json!({
            "kind": "http",
            "base_url": server.uri(),
            "auth": [{
                "name": "api",
                "env": "PROG_TEST_AUTH_TOKEN",
                "header": "authorization",
                "format": "Bearer {value}"
            }],
            "operations": [{
                "name": "get",
                "method": "GET",
                "path": "/entity",
                "effect": {
                    "read_only": true,
                    "mutating": false,
                    "network": true,
                    "shell": false,
                    "sensitive": false,
                    "cacheable": true,
                    "requires_confirmation": false
                }
            }]
        })
        .to_string(),
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discovered = prog_with_env(
        &[
            "--dir",
            dir_arg,
            "discover",
            "auth-boundary",
            "--kind",
            "http",
            "--seed",
            seed.to_str().unwrap(),
        ],
        &[("PROG_TEST_AUTH_TOKEN", "principal-one")],
    );
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let first = prog_with_env(
        &[
            "--dir",
            dir_arg,
            "call",
            "auth-boundary",
            "get",
            "--args",
            "{}",
        ],
        &[("PROG_TEST_AUTH_TOKEN", "principal-one")],
    );
    assert!(first.status.success(), "{}", stdout(&first));
    let refreshed = prog_with_env(
        &[
            "--dir",
            dir_arg,
            "call",
            "auth-boundary",
            "get",
            "--args",
            "{}",
            "--refresh",
        ],
        &[("PROG_TEST_AUTH_TOKEN", "principal-two")],
    );
    assert!(refreshed.status.success(), "{}", stdout(&refreshed));
    let refreshed: Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(refreshed["source_validity"], "validator_unavailable");
    assert!(!conditional.load(Ordering::SeqCst));
    assert!(
        !latest_observation(dir_arg)
            .to_string()
            .contains("principal-one")
    );
    assert!(
        !latest_observation(dir_arg)
            .to_string()
            .contains("principal-two")
    );
}

#[tokio::test]
async fn failed_refresh_is_separate_evidence_and_never_relabels_prior_cache_current() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/entity"))
        .respond_with(FailingRefreshResponder {
            calls: AtomicUsize::new(0),
        })
        .expect(2)
        .mount(&server)
        .await;
    let seed = write_seed(
        dir.path(),
        "failed-refresh.json",
        &json!({
            "kind": "http",
            "base_url": server.uri(),
            "operations": [{
                "name": "get",
                "method": "GET",
                "path": "/entity",
                "effect": {
                    "read_only": true,
                    "mutating": false,
                    "network": true,
                    "shell": false,
                    "sensitive": false,
                    "cacheable": true,
                    "requires_confirmation": false
                }
            }]
        })
        .to_string(),
    );
    let dir_arg = dir.path().to_str().unwrap();
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "failed-refresh",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let first = prog(&[
        "--dir",
        dir_arg,
        "call",
        "failed-refresh",
        "get",
        "--args",
        "{}",
    ]);
    assert!(first.status.success(), "{}", stdout(&first));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();

    let failed = prog(&[
        "--dir",
        dir_arg,
        "call",
        "failed-refresh",
        "get",
        "--args",
        "{}",
        "--refresh",
    ]);
    assert!(!failed.status.success(), "{}", stdout(&failed));
    let failed: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failed["source_validity"], "refresh_failed");
    assert_eq!(failed["received_error"], true);
    assert!(failed["cursor"].is_string());
    assert!(
        failed["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("prior cache entry"))
    );
    let failed_record = observation(
        dir_arg,
        failed["observation"]["observation_id"].as_str().unwrap(),
    );
    assert_eq!(failed_record["source_validity"], "refresh_failed");

    let cached = prog(&[
        "--dir",
        dir_arg,
        "call",
        "failed-refresh",
        "get",
        "--args",
        "{}",
    ]);
    assert!(cached.status.success(), "{}", stdout(&cached));
    let cached: Value = serde_json::from_slice(&cached.stdout).unwrap();
    assert_eq!(cached["data_preview"]["state"], "prior");
    assert_eq!(
        cached["observation"]["observation_id"],
        first["observation"]["observation_id"]
    );
}

#[test]
fn mcp_last_modified_annotation_becomes_hashed_modification_state() {
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let script = dir.path().join("mcp-state.py");
    fs::write(
        &script,
        r#"import json
import sys

def reply(message_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": message_id, "result": result}), flush=True)

for line in sys.stdin:
    request = json.loads(line)
    message_id = request.get("id")
    if message_id is None:
        continue
    method = request.get("method")
    if method == "initialize":
        reply(message_id, {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}, "resources": {}, "prompts": {}}, "serverInfo": {"name": "state-fixture", "version": "1.0"}})
    elif method == "tools/list":
        reply(message_id, {"tools": [{"name": "read_entity", "inputSchema": {"type": "object", "properties": {}}, "annotations": {"readOnlyHint": True}}]})
    elif method == "resources/list":
        reply(message_id, {"resources": []})
    elif method == "prompts/list":
        reply(message_id, {"prompts": []})
    elif method == "tools/call":
        reply(message_id, {"content": [{"type": "resource_link", "name": "entity", "uri": "fixture://entity/7", "annotations": {"lastModified": "2026-07-17T00:00:00Z"}}], "isError": False})
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": message_id, "error": {"code": -32601, "message": "unknown method"}}), flush=True)
"#,
    )
    .unwrap();
    let seed = write_seed(
        dir.path(),
        "mcp-state.json",
        &json!({"command": "python3", "args": [script]}).to_string(),
    );
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "mcp-state",
        "--kind",
        "mcp",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let called = prog(&[
        "--dir",
        dir_arg,
        "call",
        "mcp-state",
        "read_entity",
        "--args",
        "{}",
    ]);
    assert!(called.status.success(), "{}", stdout(&called));
    let record = latest_observation(dir_arg);
    assert_eq!(record["source_state"]["kind"], "mcp_modification");
    assert_eq!(record["source_state"]["provider"], "mcp");
    assert!(
        record["source_state"]["value"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        !record["source_state"]["value"]
            .as_str()
            .unwrap()
            .contains("2026-07-17")
    );
}

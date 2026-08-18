use std::{
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde_json::{Value, json};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::path};

mod support;

use support::{prog, stdout};

#[derive(Clone)]
struct EntityState {
    version: u64,
    state: String,
}

struct EntityResponder {
    state: Arc<Mutex<EntityState>>,
    methods: Arc<Mutex<Vec<String>>>,
}

struct FailingReadbackResponder {
    calls: AtomicUsize,
}

impl Respond for FailingReadbackResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "entity-1", "version": 1, "state": "old"
            }))
        } else {
            ResponseTemplate::new(503).set_body_json(json!({"error": "unavailable"}))
        }
    }
}

impl Respond for EntityResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.methods
            .lock()
            .unwrap()
            .push(request.method.as_str().to_string());
        let state = self.state.lock().unwrap().clone();
        ResponseTemplate::new(200)
            .insert_header("etag", format!("\"v{}\"", state.version))
            .set_body_json(json!({
                "id": "entity-1",
                "version": state.version,
                "state": state.state
            }))
    }
}

fn discover_entity(dir: &Path, server: &MockServer) {
    let seed = dir.join("entity-source.json");
    fs::write(
        &seed,
        json!({
            "kind": "http",
            "base_url": server.uri(),
            "operations": [{
                "name": "get",
                "method": "GET",
                "path": "/entity/1",
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
    )
    .unwrap();
    let output = prog(&[
        "--dir",
        dir.to_str().unwrap(),
        "discover",
        "entity",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
}

fn capture_pre(dir: &Path) -> String {
    let output = prog(&[
        "--dir",
        dir.to_str().unwrap(),
        "call",
        "entity",
        "get",
        "--args",
        "{}",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    output["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn begin(dir: &Path, pre: &str, expected: &str, window_ms: Option<u64>) -> Value {
    let mut argv = vec![
        "--dir".to_string(),
        dir.to_str().unwrap().to_string(),
        "verification".to_string(),
        "begin".to_string(),
        "--pre-observation".to_string(),
        pre.to_string(),
        "--read-args".to_string(),
        "{}".to_string(),
        "--identity-path".to_string(),
        "/id".to_string(),
        "--version-path".to_string(),
        "/version".to_string(),
        "--expected".to_string(),
        expected.to_string(),
    ];
    if let Some(window_ms) = window_ms {
        argv.push("--eventual-consistency-ms".to_string());
        argv.push(window_ms.to_string());
    }
    let argv = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let output = prog(&argv);
    assert!(output.status.success(), "{}", stdout(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn external_change_is_verified_by_repeatable_get_only_readbacks() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(EntityState {
        version: 1,
        state: "old".to_string(),
    }));
    let methods = Arc::new(Mutex::new(Vec::new()));
    Mock::given(path("/entity/1"))
        .respond_with(EntityResponder {
            state: state.clone(),
            methods: methods.clone(),
        })
        .mount(&server)
        .await;
    discover_entity(dir.path(), &server);
    let pre = capture_pre(dir.path());
    let intent = begin(dir.path(), &pre, r#"{"/state":"new"}"#, None);

    // This models the external mutation step. `prog verification` never owns
    // or executes the mutation operation.
    *state.lock().unwrap() = EntityState {
        version: 2,
        state: "new".to_string(),
    };
    for _ in 0..2 {
        let output = prog(&[
            "--dir",
            dir.path().to_str().unwrap(),
            "verification",
            "readback",
            intent["intent_id"].as_str().unwrap(),
        ]);
        assert!(output.status.success(), "{}", stdout(&output));
        let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["status"], "verified");
        assert_eq!(receipt["pre_observation_id"], pre);
        assert!(receipt["readback_observation_id"].is_string());
        assert!(receipt["assessment"].is_object());
    }
    assert!(methods.lock().unwrap().iter().all(|method| method == "GET"));

    let readiness = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "session",
        "obligation-list",
    ]);
    assert!(readiness.status.success(), "{}", stdout(&readiness));
    let readiness: Value = serde_json::from_slice(&readiness.stdout).unwrap();
    assert_eq!(readiness["ready"], true);
    assert_eq!(readiness["evaluations"][0]["status"], "passed");
}

#[tokio::test]
async fn mismatches_are_failed_or_pending_only_inside_declared_window() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(EntityState {
        version: 1,
        state: "old".to_string(),
    }));
    Mock::given(path("/entity/1"))
        .respond_with(EntityResponder {
            state: state.clone(),
            methods: Arc::new(Mutex::new(Vec::new())),
        })
        .mount(&server)
        .await;
    discover_entity(dir.path(), &server);
    let pre = capture_pre(dir.path());
    let mismatched = begin(dir.path(), &pre, r#"{"/state":"never"}"#, None);
    *state.lock().unwrap() = EntityState {
        version: 2,
        state: "other".to_string(),
    };
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "verification",
        "readback",
        mismatched["intent_id"].as_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["status"], "mismatched");

    let pending = begin(dir.path(), &pre, r#"{"/state":"eventual"}"#, Some(60_000));
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "verification",
        "readback",
        pending["intent_id"].as_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["status"], "pending");
}

#[tokio::test]
async fn transport_failure_records_a_receipt_without_claiming_success() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/entity/1"))
        .respond_with(FailingReadbackResponder {
            calls: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    discover_entity(dir.path(), &server);
    let pre = capture_pre(dir.path());
    let intent = begin(dir.path(), &pre, r#"{"/state":"new"}"#, None);
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "verification",
        "readback",
        intent["intent_id"].as_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["status"], "readback_failed");
    assert!(receipt["readback_observation_id"].is_string());

    let readiness = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "session",
        "obligation-list",
    ]);
    let readiness: Value = serde_json::from_slice(&readiness.stdout).unwrap();
    assert_eq!(readiness["ready"], false);
    assert_eq!(readiness["evaluations"][0]["status"], "unverifiable");
}

#[tokio::test]
async fn begin_rejects_a_mutating_readback_operation_before_execution() {
    let dir = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/entity/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "entity-1", "version": 1, "state": "old"
        })))
        .mount(&server)
        .await;
    discover_entity(dir.path(), &server);
    let pre = capture_pre(dir.path());

    let seed = dir.path().join("mutating-source.json");
    fs::write(
        &seed,
        json!({
            "kind": "http",
            "base_url": server.uri(),
            "operations": [{
                "name": "update",
                "method": "POST",
                "path": "/entity/1",
                "effect": {
                    "read_only": false,
                    "mutating": true,
                    "network": true,
                    "shell": false,
                    "sensitive": true,
                    "cacheable": false,
                    "requires_confirmation": true
                }
            }]
        })
        .to_string(),
    )
    .unwrap();
    let discovered = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "discover",
        "mutator",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let output = prog(&[
        "--dir",
        dir.path().to_str().unwrap(),
        "verification",
        "begin",
        "--pre-observation",
        &pre,
        "--source-id",
        "mutator",
        "--read-operation",
        "update",
        "--read-args",
        "{}",
        "--identity-path",
        "/id",
        "--version-path",
        "/version",
        "--expected",
        r#"{"/state":"new"}"#,
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("proven read-only"));
}

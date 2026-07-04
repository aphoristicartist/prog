use std::{collections::BTreeMap, time::Duration};

use prog_adapters::http::{HttpOperation, HttpSource};
use prog_core::{AuthRef, RedactionPolicy, Store, new_cache_entry};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

#[tokio::test]
async fn executes_json_request_with_encoded_path_query_and_body_template() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/a%20b/prog/issues"))
        .and(query_param("q", "state=open label:bug"))
        .and(body_json(json!({"title": "Fix it", "count": 2})))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "abc")
                .set_body_json(json!({"items": [{"id": 1}], "has_more": true})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let source = source(
        &server,
        HttpOperation {
            id: "create_issue".to_string(),
            method: "POST".to_string(),
            path: "/repos/{owner}/{repo}/issues".to_string(),
            query: map([("q", "state={state} label:{label}")]),
            headers: BTreeMap::new(),
            json_body: Some(json!({"title": "{title}", "count": "{count}"})),
            timeout_ms: Some(2_000),
            max_response_bytes: Some(64 * 1024),
            sensitive_args: Vec::new(),
        },
    );

    let result = source
        .execute_with_env(
            "create_issue",
            &json!({
                "owner": "a b",
                "repo": "prog",
                "state": "open",
                "label": "bug",
                "title": "Fix it",
                "count": 2
            }),
            &|_| None,
        )
        .await
        .unwrap();

    assert_eq!(result.data["items"][0]["id"], 1);
    assert_eq!(result.pagination, Some(json!({"has_more": true})));
    assert_eq!(result.provenance.status, 200);
    assert_eq!(result.provenance.selected_headers["etag"], "abc");
    assert!(
        result
            .provenance
            .final_url
            .contains("/repos/a%20b/prog/issues")
    );
    assert!(
        result
            .provenance
            .final_url
            .contains("q=state%3Dopen+label%3Abug")
    );
}

#[tokio::test]
async fn rejects_missing_and_unknown_args_with_names() {
    let server = MockServer::start().await;
    let source = source(
        &server,
        HttpOperation {
            id: "list".to_string(),
            method: "GET".to_string(),
            path: "/repos/{owner}/{repo}".to_string(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            json_body: None,
            timeout_ms: None,
            max_response_bytes: None,
            sensitive_args: Vec::new(),
        },
    );

    let err = source
        .execute_with_env("list", &json!({"owner": "a", "extra": true}), &|_| None)
        .await
        .unwrap_err();

    assert_eq!(err.kind(), "bad_args");
    let message = err.to_string();
    assert!(message.contains("repo"));
    assert!(message.contains("extra"));
}

#[tokio::test]
async fn wraps_non_json_text_and_truncates_at_byte_cap() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/logs"))
        .respond_with(ResponseTemplate::new(200).set_body_string("line1\nline2\nline3\nline4"))
        .mount(&server)
        .await;

    let source = source(
        &server,
        HttpOperation {
            id: "logs".to_string(),
            method: "GET".to_string(),
            path: "/logs".to_string(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            json_body: None,
            timeout_ms: Some(2_000),
            max_response_bytes: Some(11),
            sensitive_args: Vec::new(),
        },
    );

    let result = source
        .execute_with_env("logs", &json!({}), &|_| None)
        .await
        .unwrap();

    assert_eq!(result.data["format"], "text");
    assert_eq!(result.data["truncated"], true);
    assert_eq!(result.data["byte_count"], 11);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("max_response_bytes"))
    );
    assert!(result.provenance.truncated);
}

#[tokio::test]
async fn maps_non_success_status_to_structured_error_with_bounded_preview() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": "not found",
            "body": "x".repeat(4096)
        })))
        .mount(&server)
        .await;

    let source = source(
        &server,
        HttpOperation {
            id: "missing".to_string(),
            method: "GET".to_string(),
            path: "/missing".to_string(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            json_body: None,
            timeout_ms: Some(2_000),
            max_response_bytes: Some(64 * 1024),
            sensitive_args: Vec::new(),
        },
    );

    let err = source
        .execute_with_env("missing", &json!({}), &|_| None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "http_status");
    let rendered = serde_json::to_string(&err.envelope()).unwrap();
    assert!(rendered.contains("not found"));
    assert!(rendered.len() < 16 * 1024);
}

#[tokio::test]
async fn request_timeout_is_structured() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(100)))
        .mount(&server)
        .await;

    let source = source(
        &server,
        HttpOperation {
            id: "slow".to_string(),
            method: "GET".to_string(),
            path: "/slow".to_string(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            json_body: None,
            timeout_ms: Some(10),
            max_response_bytes: Some(64 * 1024),
            sensitive_args: Vec::new(),
        },
    );

    let err = source
        .execute_with_env("slow", &json!({}), &|_| None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "http_timeout");
}

#[tokio::test]
async fn pagination_signals_are_hints_and_not_auto_followed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("link", "<https://example.test/items?page=2>; rel=\"next\"")
                .set_body_json(json!({"items": [1], "next_page": 2})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let source = source(
        &server,
        HttpOperation {
            id: "items".to_string(),
            method: "GET".to_string(),
            path: "/items".to_string(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            json_body: None,
            timeout_ms: Some(2_000),
            max_response_bytes: Some(64 * 1024),
            sensitive_args: Vec::new(),
        },
    );

    let result = source
        .execute_with_env("items", &json!({}), &|_| None)
        .await
        .unwrap();

    assert_eq!(result.pagination.as_ref().unwrap()["next_page"], 2);
    assert!(
        result.pagination.as_ref().unwrap()["link_rel_next"]
            .as_str()
            .unwrap()
            .contains("rel=\"next\"")
    );
}

#[tokio::test]
async fn auth_header_is_injected_but_never_lands_in_provenance_or_store() {
    let server = MockServer::start().await;
    let secret = "SECRET_TOKEN_123";
    Mock::given(method("GET"))
        .and(path("/secure"))
        .and(header("authorization", format!("Bearer {secret}")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", format!("session={secret}"))
                .insert_header("x-ratelimit-remaining", "42")
                .set_body_json(json!({"token": secret, "ok": true})),
        )
        .mount(&server)
        .await;

    let mut source = source(
        &server,
        HttpOperation {
            id: "secure".to_string(),
            method: "GET".to_string(),
            path: "/secure".to_string(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            json_body: None,
            timeout_ms: Some(2_000),
            max_response_bytes: Some(64 * 1024),
            sensitive_args: Vec::new(),
        },
    );
    source.auth = vec![AuthRef {
        name: "api".to_string(),
        env: "API_TOKEN".to_string(),
        header: Some("authorization".to_string()),
        format: Some("Bearer {value}".to_string()),
        extra: serde_json::Map::new(),
    }];

    let result = source
        .execute_with_env("secure", &json!({}), &|name| {
            (name == "API_TOKEN").then(|| secret.to_string())
        })
        .await
        .unwrap();

    let provenance = serde_json::to_string(&result.provenance).unwrap();
    assert!(!provenance.contains(secret));
    assert!(
        !result
            .provenance
            .selected_headers
            .contains_key("set-cookie")
    );
    assert_eq!(
        result.provenance.selected_headers["x-ratelimit-remaining"],
        "42"
    );

    let redacted = RedactionPolicy::default().apply_persistence(&result.data).0;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let payload_hash = store.put_payload(&redacted).unwrap();
    let entry = new_cache_entry(
        "auth-call".to_string(),
        payload_hash,
        "http".to_string(),
        "secure".to_string(),
        serde_json::to_vec(&redacted).unwrap().len() as u64,
        60,
    );
    store.put_entry("auth-call", &entry).unwrap();

    let stored = std::fs::read_dir(dir.path())
        .unwrap()
        .flat_map(|entry| walk(entry.unwrap().path()))
        .flat_map(|path| std::fs::read(path).unwrap_or_default())
        .collect::<Vec<_>>();
    assert!(!String::from_utf8_lossy(&stored).contains(secret));
}

fn source(server: &MockServer, operation: HttpOperation) -> HttpSource {
    HttpSource {
        id: "test-http".to_string(),
        base_url: server.uri(),
        timeout_ms: 30_000,
        max_response_bytes: 2 * 1024 * 1024,
        default_headers: BTreeMap::new(),
        response_header_allowlist: vec![
            "etag".to_string(),
            "x-ratelimit-remaining".to_string(),
            "content-type".to_string(),
            "set-cookie".to_string(),
        ],
        auth: Vec::new(),
        operations: vec![operation],
    }
}

fn map<const N: usize>(values: [(&str, &str); N]) -> BTreeMap<String, String> {
    values
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn walk(path: std::path::PathBuf) -> Vec<std::path::PathBuf> {
    if path.is_file() {
        return vec![path];
    }
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            files.extend(walk(entry.path()));
        }
    }
    files
}

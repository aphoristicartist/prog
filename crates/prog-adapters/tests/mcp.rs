use std::{collections::BTreeMap, time::Instant};

use prog_adapters::mcp::McpSource;
use prog_core::{PreviewPolicy, project};
use serde_json::json;
use tempfile::TempDir;

const FIXTURE_MCP_SERVER: &str = r#"
import json
import sys
import time

MODE = sys.argv[1] if len(sys.argv) > 1 else "normal"

TOOLS = [
    {
        "name": "search_docs",
        "description": "Search fixture documentation",
        "inputSchema": {
            "type": "object",
            "required": ["query"],
            "properties": {"query": {"type": "string"}},
        },
        "outputSchema": {
            "type": "object",
            "properties": {
                "results": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "body": {"type": "string"},
                        },
                    },
                }
            },
        },
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "danger",
        "description": "Tool without readOnlyHint must fail closed",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "conflicting_hints",
        "description": "Destructive hint tightens a read-only hint",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True, "destructiveHint": True},
    },
    {
        "name": "external_ref",
        "description": "Schema containing an external ref",
        "inputSchema": {"type": "object", "properties": {}},
        "outputSchema": {"$ref": "https://example.invalid/schema.json"},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "json_text",
        "description": "Returns JSON as a text content block",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "tool_error",
        "description": "Returns isError",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "slow",
        "description": "Sleeps past the client timeout",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True},
    },
]

RESOURCES = [
    {
        "uri": "fixture://doc",
        "name": "fixture_doc",
        "description": "Fixture JSON document",
        "mimeType": "application/json",
    }
]

PROMPTS = [
    {
        "name": "summarize",
        "description": "Summarize fixture material",
        "arguments": [{"name": "topic", "required": True}],
    }
]


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


def structured_results():
    return {
        "results": [
            {"title": f"Doc {index}", "body": "x" * 80}
            for index in range(40)
        ]
    }


for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    message_id = message.get("id")
    if message_id is None:
        if method == "notifications/initialized" and MODE == "crash":
            sys.exit(2)
        continue

    if method == "initialize":
        send_result(message_id, {
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
            "serverInfo": {"name": "fixture-mcp", "version": "1.0.0"},
            "instructions": "Fixture MCP server",
        })
    elif MODE == "crash":
        sys.exit(2)
    elif method == "tools/list":
        send_result(message_id, {"tools": TOOLS})
    elif method == "resources/list":
        send_result(message_id, {"resources": RESOURCES})
    elif method == "prompts/list":
        send_result(message_id, {"prompts": PROMPTS})
    elif method == "tools/call":
        name = message.get("params", {}).get("name")
        if name == "search_docs":
            send_result(message_id, {
                "content": [{"type": "text", "text": "structured result"}],
                "structuredContent": structured_results(),
                "isError": False,
            })
        elif name == "json_text":
            send_result(message_id, {
                "content": [{"type": "text", "text": "{\"ok\":true,\"items\":[1,2]}"}],
                "isError": False,
            })
        elif name == "tool_error":
            send_result(message_id, {
                "content": [{"type": "text", "text": "bad fixture input"}],
                "isError": True,
            })
        elif name == "slow":
            time.sleep(5)
            send_result(message_id, {"content": [{"type": "text", "text": "late"}]})
        else:
            send_error(message_id, -32602, "unknown tool")
    elif method == "resources/read":
        send_result(message_id, {
            "contents": [{
                "uri": "fixture://doc",
                "mimeType": "application/json",
                "text": "{\"doc\":\"fixture\",\"items\":[1,2,3]}",
            }]
        })
    else:
        send_error(message_id, -32601, f"unknown method: {method}")
"#;

#[tokio::test]
async fn discovers_tools_resources_prompts_and_declared_output_schema() {
    let fixture = fixture("normal");

    let discovery = fixture.source.discover().await.unwrap();

    assert_eq!(discovery.profile.kind, prog_core::SourceKind::Mcp);
    assert_eq!(
        discovery.provenance.protocol_version.as_deref(),
        Some("2025-11-25")
    );

    let search = operation(&discovery.profile, "search_docs");
    assert_eq!(search.input_schema["required"][0], "query");
    assert_eq!(
        search.declared_output_schema.as_ref().unwrap()["properties"]["results"]["type"],
        "array"
    );
    assert!(search.effects.read_only);
    assert!(!search.effects.mutating);
    assert!(!search.effects.requires_confirmation);

    let danger = operation(&discovery.profile, "danger");
    assert!(!danger.effects.read_only);
    assert!(danger.effects.mutating);
    assert!(danger.effects.requires_confirmation);

    let conflicting = operation(&discovery.profile, "conflicting_hints");
    assert!(!conflicting.effects.read_only);
    assert!(conflicting.effects.mutating);
    assert!(conflicting.effects.requires_confirmation);

    assert!(discovery.profile.operations.iter().any(|operation| {
        operation.id == "resource:fixture_doc"
            && operation.effects.read_only
            && operation.extra["invocation"]["mcp"]["kind"] == "resource"
    }));
    assert!(discovery.profile.operations.iter().any(|operation| {
        operation.id == "prompt:summarize"
            && operation.input_schema["required"][0] == "topic"
            && operation.extra["invocation"]["mcp"]["kind"] == "prompt"
    }));
}

#[tokio::test]
async fn external_refs_are_preserved_without_dereferencing() {
    let fixture = fixture("normal");

    let discovery = fixture.source.discover().await.unwrap();

    let operation = operation(&discovery.profile, "external_ref");
    assert_eq!(
        operation.declared_output_schema.as_ref().unwrap()["$ref"],
        "https://example.invalid/schema.json"
    );
    assert!(
        discovery
            .warnings
            .iter()
            .any(|warning| warning.contains("external $ref"))
    );
}

#[tokio::test]
async fn schema_import_is_depth_bounded() {
    let mut fixture = fixture("normal");
    fixture.source.max_schema_depth = 2;

    let discovery = fixture.source.discover().await.unwrap();

    assert!(
        discovery
            .warnings
            .iter()
            .any(|warning| warning.contains("max_schema_depth"))
    );
}

#[tokio::test]
async fn calls_tool_prefers_structured_content_and_can_project_large_result() {
    let fixture = fixture("normal");

    let result = fixture
        .source
        .call_tool("search_docs", &json!({"query": "rust"}))
        .await
        .unwrap();

    assert_eq!(result.data["results"].as_array().unwrap().len(), 40);
    assert!(result.provenance.structured_content);

    let projection = project(
        &result.data,
        &PreviewPolicy {
            max_envelope_bytes: 512,
            ..PreviewPolicy::default()
        },
        "",
    );
    assert!(!projection.omitted.is_empty());
}

#[tokio::test]
async fn content_text_falls_back_to_json_detection() {
    let fixture = fixture("normal");

    let result = fixture
        .source
        .call_tool("json_text", &json!({}))
        .await
        .unwrap();

    assert_eq!(result.data["ok"], true);
    assert_eq!(result.data["items"], json!([1, 2]));
    assert!(!result.provenance.structured_content);
}

#[tokio::test]
async fn reads_resource_and_detects_json_text() {
    let fixture = fixture("normal");

    let result = fixture.source.read_resource("fixture://doc").await.unwrap();

    assert_eq!(result.data["doc"], "fixture");
    assert_eq!(result.data["items"], json!([1, 2, 3]));
}

#[tokio::test]
async fn tool_is_error_maps_to_structured_error() {
    let fixture = fixture("normal");

    let error = fixture
        .source
        .call_tool("tool_error", &json!({}))
        .await
        .unwrap_err();

    assert_eq!(error.kind(), "mcp_tool_error");
    let rendered = serde_json::to_string(&error.envelope()).unwrap();
    assert!(rendered.contains("bad fixture input"));
    assert!(rendered.len() < 2048);
}

#[tokio::test]
async fn call_timeout_is_structured_and_bounded() {
    let mut fixture = fixture("normal");
    fixture.source.timeout_ms = 200;
    let started = Instant::now();

    let error = fixture
        .source
        .call_tool("slow", &json!({}))
        .await
        .unwrap_err();

    assert_eq!(error.kind(), "mcp_timeout");
    assert!(started.elapsed().as_secs() < 2);
}

#[tokio::test]
async fn crashing_server_returns_actionable_error_not_hang() {
    let mut fixture = fixture("crash");
    fixture.source.timeout_ms = 500;
    let started = Instant::now();

    let error = fixture.source.discover().await.unwrap_err();

    assert_eq!(error.kind(), "mcp_transport");
    assert!(started.elapsed().as_secs() < 2);
}

struct Fixture {
    _tempdir: TempDir,
    source: McpSource,
}

fn fixture(mode: &str) -> Fixture {
    let tempdir = tempfile::tempdir().unwrap();
    let script = tempdir.path().join("fixture_mcp.py");
    std::fs::write(&script, FIXTURE_MCP_SERVER).unwrap();

    Fixture {
        source: McpSource {
            id: "fixture_mcp".to_string(),
            command: "python3".to_string(),
            args: vec![script.to_string_lossy().into_owned(), mode.to_string()],
            env: BTreeMap::new(),
            timeout_ms: 2_000,
            max_content_bytes: 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
            max_schema_depth: 32,
        },
        _tempdir: tempdir,
    }
}

fn operation<'a>(
    profile: &'a prog_core::SourceProfile,
    id: &str,
) -> &'a prog_core::OperationProfile {
    profile
        .operations
        .iter()
        .find(|operation| operation.id == id)
        .unwrap()
}

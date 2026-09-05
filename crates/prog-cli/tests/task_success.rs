use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const ITEM_COUNT: usize = 260;
const BODY_BYTES: usize = 1024;

#[path = "support/baseline_strategies.rs"]
mod strategies;
use strategies::{Source as TaskSource, Step, StrategyInput, Task, assert_success, timed_prog};

#[derive(Clone)]
struct TaskScenario {
    id: String,
    prompt: String,
    artifact: String,
    source: TaskSource,
    raw_bytes: Vec<u8>,
    lookup_path: String,
    answer: String,
    counterexample: bool,
}

#[derive(Debug, Clone, Serialize)]
struct TaskMetric {
    scenario_id: String,
    prompt: String,
    artifact: String,
    task_mode: &'static str,
    strategy: &'static str,
    available: bool,
    evidence_available: bool,
    artifact_bytes: usize,
    response_bytes: usize,
    input_tokens: usize,
    tool_calls: usize,
    expansion_count: usize,
    cache_hits: usize,
    wall_time_ms: u128,
    public_lookup_path: String,
    counterexample: bool,
    steps: Vec<Step>,
}

#[tokio::test]
async fn known_path_recoverability_eval_smoke() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut keep_servers = Vec::new();
    let scenarios = setup_scenarios(tempdir.path(), &mut keep_servers).await;
    assert!(
        scenarios.len() >= 10,
        "known-path recoverability eval should include at least 10 scenarios"
    );

    let metrics = scenarios
        .iter()
        .flat_map(|scenario| run_scenario(tempdir.path(), scenario))
        .collect::<Vec<_>>();

    for metric in &metrics {
        assert_eq!(
            metric.response_bytes,
            metric.steps.iter().map(|s| s.response_bytes).sum::<usize>()
        );
        assert_eq!(metric.task_mode, "known_path_recoverability");
        if metric.strategy == "prog_expand" {
            assert!(
                metric.evidence_available,
                "public selector must recover evidence for {}",
                metric.scenario_id
            );
        }
        if !metric.available {
            assert!(!metric.evidence_available);
        }
    }

    let report = markdown_report(&metrics);
    let metrics_json = serde_json::to_string_pretty(&metrics).unwrap();
    if std::env::var_os("PROG_TASK_EVAL_UPDATE").is_some() {
        let root = repo_root();
        fs::write(root.join("docs/task-success-eval.md"), &report).unwrap();
        fs::write(
            root.join("fixtures/evals/task-success-metrics.json"),
            format!("{metrics_json}\n"),
        )
        .unwrap();
        println!("{report}");
    } else {
        assert!(repo_root().join("docs/task-success-eval.md").exists());
        assert!(
            repo_root()
                .join("fixtures/evals/task-success-metrics.json")
                .exists()
        );
    }
}

async fn setup_scenarios(root: &Path, keep_servers: &mut Vec<MockServer>) -> Vec<TaskScenario> {
    let mut scenarios = Vec::new();

    let (http_source, http_raw, server) = setup_http_source(root).await;
    keep_servers.push(server);
    scenarios.extend(item_scenarios("http", "HTTP", http_source, http_raw));

    let (cli_source, cli_raw) = setup_cli_source(root);
    scenarios.extend(item_scenarios("cli", "CLI", cli_source, cli_raw));

    let (mcp_source, mcp_raw) = setup_mcp_source(root);
    scenarios.extend(item_scenarios("mcp", "MCP", mcp_source, mcp_raw));

    scenarios.push(observe_json_scenario());
    scenarios.push(observe_ndjson_scenario());
    scenarios.push(observe_text_scenario());
    scenarios.push(tiny_counterexample_scenario());

    scenarios
}

async fn setup_http_source(root: &Path) -> (TaskSource, Vec<u8>, MockServer) {
    let server = MockServer::start().await;
    let payload = item_payload("items", "http");
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(payload.clone()))
        .mount(&server)
        .await;
    let seed = root.join("http-task-seed.json");
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
    discover(root, "task_http", "http", &seed);
    (
        TaskSource::Call {
            source_id: "task_http".to_string(),
            operation: "list".to_string(),
        },
        serde_json::to_vec(&payload).unwrap(),
        server,
    )
}

fn setup_cli_source(root: &Path) -> (TaskSource, Vec<u8>) {
    let payload = item_payload("items", "cli");
    let payload_path = root.join("task-cli-payload.json");
    fs::write(&payload_path, serde_json::to_vec(&payload).unwrap()).unwrap();
    let command = format!(
        "import pathlib,sys; sys.stdout.buffer.write(pathlib.Path({:?}).read_bytes())",
        payload_path.to_string_lossy()
    );
    let seed = root.join("cli-task-seed.json");
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
    discover(root, "task_cli", "cli", &seed);
    (
        TaskSource::Call {
            source_id: "task_cli".to_string(),
            operation: "list".to_string(),
        },
        serde_json::to_vec(&payload).unwrap(),
    )
}

fn setup_mcp_source(root: &Path) -> (TaskSource, Vec<u8>) {
    let payload = item_payload("results", "mcp");
    let payload_path = root.join("task-mcp-payload.json");
    fs::write(&payload_path, serde_json::to_vec(&payload).unwrap()).unwrap();
    let script = root.join("task_mcp.py");
    fs::write(&script, MCP_SERVER).unwrap();
    let seed = root.join("mcp-task-seed.json");
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
    discover(root, "task_mcp", "mcp", &seed);
    (
        TaskSource::Call {
            source_id: "task_mcp".to_string(),
            operation: "search_docs".to_string(),
        },
        serde_json::to_vec(&payload).unwrap(),
    )
}

fn item_scenarios(
    prefix: &str,
    artifact: &str,
    source: TaskSource,
    raw_bytes: Vec<u8>,
) -> Vec<TaskScenario> {
    let array = if prefix == "mcp" { "results" } else { "items" };
    [42usize, 128, 211]
        .into_iter()
        .map(|index| {
            let field = if index == 42 { "body" } else { "lookup_code" };
            let answer = if field == "body" {
                format!("{prefix}-body-{index}-")
            } else {
                format!("{prefix}-code-{index}")
            };
            TaskScenario {
                id: format!("{prefix}-{field}-{index}"),
                prompt: format!("Find {field} for {artifact} item {index}."),
                artifact: artifact.to_string(),
                source: source.clone(),
                raw_bytes: raw_bytes.clone(),
                lookup_path: format!("/{array}/{index}/{field}"),
                answer,
                counterexample: false,
            }
        })
        .collect()
}

fn observe_json_scenario() -> TaskScenario {
    let payload = item_payload("items", "json");
    TaskScenario {
        id: "observe-json-body-150".to_string(),
        prompt: "Find the JSON observed item 150 body.".to_string(),
        artifact: "Observed JSON".to_string(),
        source: TaskSource::Observe {
            name: "task-json".to_string(),
            mime: "application/json".to_string(),
            bytes: serde_json::to_vec(&payload).unwrap(),
        },
        raw_bytes: serde_json::to_vec(&payload).unwrap(),
        lookup_path: "/items/150/body".to_string(),
        answer: "json-body-150-".to_string(),
        counterexample: false,
    }
}

fn observe_ndjson_scenario() -> TaskScenario {
    let mut lines = Vec::new();
    for index in 0..ITEM_COUNT {
        lines.push(
            serde_json::to_string(&json!({
                "index": index,
                "message": format!("ndjson-message-{index}-{}", "n".repeat(BODY_BYTES / 2)),
                "lookup_code": format!("ndjson-code-{index}")
            }))
            .unwrap(),
        );
    }
    let raw = format!("{}\n", lines.join("\n")).into_bytes();
    TaskScenario {
        id: "observe-ndjson-message-170".to_string(),
        prompt: "Find the NDJSON record 170 message.".to_string(),
        artifact: "Observed NDJSON".to_string(),
        source: TaskSource::Observe {
            name: "task-ndjson".to_string(),
            mime: "application/x-ndjson".to_string(),
            bytes: raw.clone(),
        },
        raw_bytes: raw,
        lookup_path: "/records/170/message".to_string(),
        answer: "ndjson-message-170-".to_string(),
        counterexample: false,
    }
}

fn observe_text_scenario() -> TaskScenario {
    let raw = (0..ITEM_COUNT)
        .map(|index| {
            format!(
                "2026-07-06T12:00:00Z INFO log-line-{index}-{}",
                "t".repeat(256)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes();
    TaskScenario {
        id: "observe-text-line-180".to_string(),
        prompt: "Find observed text log line 180.".to_string(),
        artifact: "Observed Text".to_string(),
        source: TaskSource::Observe {
            name: "task-text".to_string(),
            mime: "text/plain".to_string(),
            bytes: raw.clone(),
        },
        raw_bytes: raw,
        lookup_path: "/lines/180/text".to_string(),
        answer: "log-line-180-".to_string(),
        counterexample: false,
    }
}

fn tiny_counterexample_scenario() -> TaskScenario {
    let payload = json!({"answer": "tiny-answer", "note": "raw is cheaper here"});
    let raw = serde_json::to_vec(&payload).unwrap();
    TaskScenario {
        id: "tiny-payload-counterexample".to_string(),
        prompt: "Read the tiny answer.".to_string(),
        artifact: "Tiny JSON".to_string(),
        source: TaskSource::Observe {
            name: "task-tiny".to_string(),
            mime: "application/json".to_string(),
            bytes: raw.clone(),
        },
        raw_bytes: raw,
        lookup_path: "/answer".to_string(),
        answer: "tiny-answer".to_string(),
        counterexample: true,
    }
}

fn strategy_input(scenario: &TaskScenario) -> StrategyInput {
    // This suite explicitly supplies a lookup path. Only the expected answer
    // remains private to grading. Terms come from the public selector, not it.
    let term = if scenario.lookup_path.ends_with("/text") {
        scenario
            .lookup_path
            .split('/')
            .rev()
            .nth(1)
            .unwrap_or("text")
    } else {
        scenario.lookup_path.rsplit('/').next().unwrap_or("")
    };
    StrategyInput {
        prompt: format!(
            "{} Public lookup selector: {}.",
            scenario.prompt, scenario.lookup_path
        ),
        source: scenario.source.clone(),
        raw_bytes: scenario.raw_bytes.clone(),
        task: Task::KnownPath {
            selector: scenario.lookup_path.clone(),
            grep_term: Some(term.to_string()),
        },
    }
}

fn run_scenario(root: &Path, scenario: &TaskScenario) -> Vec<TaskMetric> {
    let input = strategy_input(scenario);
    [
        ("raw", "raw_context"),
        ("simple_truncation", "head_tail_truncation"),
        ("native_json_selection", "native_field_selection"),
        ("rtk_grep_filter", "rtk_grep_filter"),
        ("prog_call_only", "prog_envelope_only"),
        ("prog_expand", "prog_retrieve"),
    ]
    .into_iter()
    .map(|(label, strategy)| {
        let execution = strategies::run(root, &input, strategy);
        // No strategy runner can read scenario.answer. Grade only afterward.
        let available = execution.unavailable.is_none();
        let evidence_available =
            available && String::from_utf8_lossy(&execution.evidence).contains(&scenario.answer);
        let response_bytes = execution.response_bytes();
        TaskMetric {
            scenario_id: scenario.id.clone(),
            prompt: input.prompt.clone(),
            artifact: scenario.artifact.clone(),
            task_mode: input.mode(),
            strategy: label,
            available,
            evidence_available,
            artifact_bytes: input.raw_bytes.len(),
            response_bytes,
            input_tokens: approx_tokens(response_bytes),
            tool_calls: execution.tool_calls(),
            expansion_count: execution.steps.iter().filter(|s| s.expansion).count(),
            cache_hits: execution.steps.iter().filter(|s| s.cache_hit).count(),
            wall_time_ms: execution.elapsed_ms,
            public_lookup_path: scenario.lookup_path.clone(),
            counterexample: scenario.counterexample,
            steps: execution.steps,
        }
    })
    .collect()
}

fn discover(root: &Path, source_id: &str, kind: &str, seed: &Path) {
    let output = timed_prog(
        root,
        &[
            "discover",
            source_id,
            "--kind",
            kind,
            "--seed",
            seed.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&output.output);
}

fn item_payload(array_key: &str, prefix: &str) -> Value {
    json!({
        array_key: (0..ITEM_COUNT).map(|index| {
            json!({
                "id": index,
                "title": format!("{prefix} title {index}"),
                "lookup_code": format!("{prefix}-code-{index}"),
                "state": if index % 3 == 0 { "open" } else { "closed" },
                "body": format!("{prefix}-body-{index}-{}", "x".repeat(BODY_BYTES))
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

fn strategy_rows<'a>(metrics: &'a [TaskMetric], strategy: &str) -> Vec<&'a TaskMetric> {
    metrics
        .iter()
        .filter(|metric| metric.strategy == strategy)
        .collect()
}

fn markdown_report(metrics: &[TaskMetric]) -> String {
    let mut output = String::from(
        "# Known-path recoverability eval\n\n\
         This deterministic suite supplies the exact lookup selector in every task and grades evidence availability after execution. It does not measure discovery or actual-agent task success. Real-agent outcomes require separate live trials. The historical filenames remain for compatibility.\n\n\
         The expected answer is private to grading. Line-search terms are derived from the public selector, never from the answer. Native JSON selection is unavailable for non-JSON artifacts. The raw fixture is supplied outside model context to every strategy; source setup is excluded. All actual strategy stdout, including initial capture and expansion, is counted. No model answer tokens are generated, and timings are local measurements rather than assumed jq/RTK latency.\n\n\
         Regenerate this report and the raw metrics with `PROG_TASK_EVAL_UPDATE=1 cargo test -p prog-cli --test task_success -- --nocapture`.\n\n\
         ## Aggregate\n\n\
         | Strategy | Evidence available | Attempted | Unavailable | Response bytes | Approx. input tokens | Tool calls | Expansions | Cache hits |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for strategy in [
        "raw",
        "simple_truncation",
        "native_json_selection",
        "rtk_grep_filter",
        "prog_call_only",
        "prog_expand",
    ] {
        let rows = strategy_rows(metrics, strategy);
        output.push_str(&format!(
            "| {strategy} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            rows.iter()
                .filter(|metric| metric.evidence_available)
                .count(),
            rows.iter().filter(|metric| metric.available).count(),
            rows.iter().filter(|metric| !metric.available).count(),
            rows.iter()
                .map(|metric| metric.response_bytes)
                .sum::<usize>(),
            rows.iter().map(|metric| metric.input_tokens).sum::<usize>(),
            rows.iter().map(|metric| metric.tool_calls).sum::<usize>(),
            rows.iter()
                .map(|metric| metric.expansion_count)
                .sum::<usize>(),
            rows.iter().map(|metric| metric.cache_hits).sum::<usize>()
        ));
    }
    output.push_str(
        "\n## Scenarios\n\n\
         | Scenario | Artifact | Public lookup path | Counterexample |\n\
         |---|---|---|---:|\n",
    );
    let mut seen = BTreeMap::new();
    for metric in metrics {
        seen.entry(metric.scenario_id.clone()).or_insert(metric);
    }
    for metric in seen.values() {
        output.push_str(&format!(
            "| {} | {} | `{}` | {} |\n",
            metric.scenario_id, metric.artifact, metric.public_lookup_path, metric.counterexample
        ));
    }
    output.push_str(
        "\n## Counterexamples\n\n\
         The tiny payload scenario remains visible without a required cost ordering. Successful `prog_expand` rows prove that supplied paths are recoverable; they do not prove an agent discovered the path or solved the task.\n",
    );
    output
}

fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
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
            "serverInfo": {"name": "task-success-fixture", "version": "1.0.0"},
        })
    elif method == "tools/list":
        send_result(message_id, {"tools": [{
            "name": "search_docs",
            "description": "Return the task-success fixture payload",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": True},
        }]})
    elif method == "tools/call":
        send_result(message_id, {
            "content": [{"type": "text", "text": "task-success payload"}],
            "structuredContent": json.loads(payload_path.read_text()),
            "isError": False,
        })
    else:
        send_error(message_id, -32601, f"unknown method: {method}")
"#;

#[test]
fn known_path_inputs_do_not_consult_the_expected_answer() {
    let original = observe_text_scenario();
    let mut changed = original.clone();
    changed.answer = "grader-only-answer".to_string();
    let first = strategy_input(&original);
    let second = strategy_input(&changed);
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap()
    );
    assert_eq!(first.mode(), "known_path_recoverability");
    assert!(first.prompt.contains(&original.lookup_path));
    assert_eq!(first.selector(), Some(original.lookup_path.as_str()));
}

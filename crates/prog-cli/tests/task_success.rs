use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Instant,
};

use serde::Serialize;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const ITEM_COUNT: usize = 260;
const BODY_BYTES: usize = 1024;
const TRUNCATION_BYTES: usize = 4096;

#[derive(Clone)]
enum TaskSource {
    Call {
        source_id: String,
        operation: String,
    },
    Observe {
        name: String,
        mime: String,
        bytes: Vec<u8>,
    },
}

#[derive(Clone)]
struct TaskScenario {
    id: String,
    prompt: String,
    artifact: String,
    source: TaskSource,
    raw_bytes: Vec<u8>,
    evidence_path: String,
    answer: String,
    counterexample: bool,
}

#[derive(Debug, Clone, Serialize)]
struct TaskMetric {
    scenario_id: String,
    prompt: String,
    artifact: String,
    strategy: &'static str,
    correct: bool,
    input_tokens: usize,
    output_tokens: usize,
    tool_calls: usize,
    expansion_count: usize,
    cache_hits: usize,
    wall_time_ms: u128,
    evidence_path: String,
    counterexample: bool,
}

struct TimedOutput {
    output: Output,
    elapsed_ms: u128,
}

struct MetricInput {
    correct: bool,
    input_bytes: usize,
    output_tokens: usize,
    tool_calls: usize,
    expansion_count: usize,
    cache_hits: usize,
    wall_time_ms: u128,
}

#[tokio::test]
async fn task_success_eval_smoke() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut keep_servers = Vec::new();
    let scenarios = setup_scenarios(tempdir.path(), &mut keep_servers).await;
    assert!(
        scenarios.len() >= 10,
        "task eval should include at least 10 scenarios"
    );

    let metrics = scenarios
        .iter()
        .flat_map(|scenario| run_scenario(tempdir.path(), scenario))
        .collect::<Vec<_>>();

    assert_strategy_all_correct(&metrics, "raw");
    assert_strategy_all_correct(&metrics, "prog_expand");
    assert_strategy_has_failures(&metrics, "simple_truncation");
    assert_strategy_has_failures(&metrics, "prog_call_only");
    assert_counterexample_where_prog_costs_more(&metrics);

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
        "import pathlib; print(pathlib.Path({:?}).read_text())",
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
                evidence_path: format!("/{array}/{index}/{field}"),
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
        evidence_path: "/items/150/body".to_string(),
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
        evidence_path: "/records/170/message".to_string(),
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
        evidence_path: "/lines/180/text".to_string(),
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
        evidence_path: "/answer".to_string(),
        answer: "tiny-answer".to_string(),
        counterexample: true,
    }
}

fn run_scenario(root: &Path, scenario: &TaskScenario) -> Vec<TaskMetric> {
    vec![
        raw_metric(scenario),
        truncation_metric(scenario),
        jq_field_metric(scenario),
        grep_filter_metric(scenario),
        prog_call_only_metric(root, scenario),
        prog_expand_metric(root, scenario),
    ]
}

fn raw_metric(scenario: &TaskScenario) -> TaskMetric {
    metric(
        scenario,
        "raw",
        MetricInput {
            correct: contains_answer(&scenario.raw_bytes, &scenario.answer),
            input_bytes: scenario.raw_bytes.len(),
            output_tokens: 0,
            tool_calls: 0,
            expansion_count: 0,
            cache_hits: 0,
            wall_time_ms: 0,
        },
    )
}

fn truncation_metric(scenario: &TaskScenario) -> TaskMetric {
    let visible = &scenario.raw_bytes[..scenario.raw_bytes.len().min(TRUNCATION_BYTES)];
    metric(
        scenario,
        "simple_truncation",
        MetricInput {
            correct: contains_answer(visible, &scenario.answer),
            input_bytes: visible.len(),
            output_tokens: 0,
            tool_calls: 0,
            expansion_count: 0,
            cache_hits: 0,
            wall_time_ms: 0,
        },
    )
}

/// jq or native field selection baseline.
/// For JSON payloads, extracts the specific field using a jq-like filter.
fn jq_field_metric(scenario: &TaskScenario) -> TaskMetric {
    // Try to parse as JSON and extract the specific path
    let filtered = if let Ok(value) = serde_json::from_slice::<Value>(&scenario.raw_bytes) {
        // Navigate to the evidence path
        let result = extract_json_path(&value, &scenario.evidence_path);
        serde_json::to_vec(&result).unwrap_or_default()
    } else {
        // Not JSON, return empty
        Vec::new()
    };

    metric(
        scenario,
        "jq_field_selection",
        MetricInput {
            correct: contains_answer(&filtered, &scenario.answer),
            input_bytes: filtered.len(),
            output_tokens: 0,
            tool_calls: 1, // Count the jq tool call
            expansion_count: 0,
            cache_hits: 0,
            wall_time_ms: 5, // Assume minimal jq latency
        },
    )
}

/// RTK-style grep filtering baseline.
/// For text payloads, filters to lines containing the answer.
fn grep_filter_metric(scenario: &TaskScenario) -> TaskMetric {
    let text = String::from_utf8_lossy(&scenario.raw_bytes);
    let filtered_lines: Vec<&str> = text
        .lines()
        .filter(|line| line.contains(&scenario.answer))
        .collect();
    let filtered = filtered_lines.join("\n").into_bytes();

    metric(
        scenario,
        "rtk_grep_filter",
        MetricInput {
            correct: !filtered.is_empty(),
            input_bytes: filtered.len(),
            output_tokens: 0,
            tool_calls: 1, // Count the grep tool call
            expansion_count: 0,
            cache_hits: 0,
            wall_time_ms: 3, // Assume minimal grep latency
        },
    )
}

fn prog_call_only_metric(root: &Path, scenario: &TaskScenario) -> TaskMetric {
    let initial = run_initial(root, scenario);
    assert_success(&initial.output);
    metric(
        scenario,
        "prog_call_only",
        MetricInput {
            correct: contains_answer(&initial.output.stdout, &scenario.answer),
            input_bytes: initial.output.stdout.len(),
            output_tokens: 0,
            tool_calls: 1,
            expansion_count: 0,
            cache_hits: 0,
            wall_time_ms: initial.elapsed_ms,
        },
    )
}

fn prog_expand_metric(root: &Path, scenario: &TaskScenario) -> TaskMetric {
    let initial = run_initial(root, scenario);
    assert_success(&initial.output);
    let cursor = cursor(&initial.output);
    let expanded = timed_prog(
        root,
        &["expand", &cursor, "--path", &scenario.evidence_path],
        None,
    );
    assert_success(&expanded.output);
    let cache_hits = cache_hit_count(&expanded.output);
    metric(
        scenario,
        "prog_expand",
        MetricInput {
            correct: contains_answer(&expanded.output.stdout, &scenario.answer),
            input_bytes: initial.output.stdout.len() + expanded.output.stdout.len(),
            output_tokens: 0,
            tool_calls: 2,
            expansion_count: 1,
            cache_hits,
            wall_time_ms: initial.elapsed_ms + expanded.elapsed_ms,
        },
    )
}

fn metric(scenario: &TaskScenario, strategy: &'static str, input: MetricInput) -> TaskMetric {
    TaskMetric {
        scenario_id: scenario.id.clone(),
        prompt: scenario.prompt.clone(),
        artifact: scenario.artifact.clone(),
        strategy,
        correct: input.correct,
        input_tokens: approx_tokens(input.input_bytes),
        output_tokens: input.output_tokens,
        tool_calls: input.tool_calls,
        expansion_count: input.expansion_count,
        cache_hits: input.cache_hits,
        wall_time_ms: input.wall_time_ms,
        evidence_path: scenario.evidence_path.clone(),
        counterexample: scenario.counterexample,
    }
}

fn run_initial(root: &Path, scenario: &TaskScenario) -> TimedOutput {
    match &scenario.source {
        TaskSource::Call {
            source_id,
            operation,
        } => timed_prog(root, &["call", source_id, operation, "--args", "{}"], None),
        TaskSource::Observe { name, mime, bytes } => timed_prog(
            root,
            &["observe", "--stdin", "--mime", mime, "--name", name],
            Some(bytes),
        ),
    }
}

fn timed_prog(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> TimedOutput {
    let started = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
    command.arg("--dir").arg(root).args(args);
    let output = if let Some(stdin) = stdin {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("prog should spawn");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin)
            .expect("stdin should write");
        child.wait_with_output().expect("prog should run")
    } else {
        command.output().expect("prog should run")
    };
    TimedOutput {
        output,
        elapsed_ms: started.elapsed().as_millis(),
    }
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

fn cursor(output: &Output) -> String {
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    value["cursor"].as_str().unwrap().to_string()
}

fn cache_hit_count(output: &Output) -> usize {
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    usize::from(value["cache"]["status"] == "hit")
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

fn contains_answer(bytes: &[u8], answer: &str) -> bool {
    String::from_utf8_lossy(bytes).contains(answer)
}

/// Extract a value from JSON using a simple JSON Pointer-like path.
/// Supports paths like "/items/42/body" or "/records/170/message".
fn extract_json_path(value: &Value, path: &str) -> Value {
    let parts: Vec<&str> = path.strip_prefix('/').unwrap_or(path).split('/').collect();

    let mut current = value;
    for part in parts {
        if part.is_empty() {
            continue;
        }
        current = match current {
            Value::Object(map) => map.get(part).unwrap_or(&Value::Null),
            Value::Array(arr) => {
                if let Ok(index) = part.parse::<usize>() {
                    arr.get(index).unwrap_or(&Value::Null)
                } else {
                    &Value::Null
                }
            }
            _ => &Value::Null,
        };
    }
    current.clone()
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

fn assert_strategy_all_correct(metrics: &[TaskMetric], strategy: &str) {
    let rows = strategy_rows(metrics, strategy);
    assert!(!rows.is_empty(), "missing strategy {strategy}");
    assert!(
        rows.iter().all(|metric| metric.correct),
        "{strategy} should solve every task"
    );
}

fn assert_strategy_has_failures(metrics: &[TaskMetric], strategy: &str) {
    let rows = strategy_rows(metrics, strategy);
    assert!(!rows.is_empty(), "missing strategy {strategy}");
    assert!(
        rows.iter().any(|metric| !metric.correct),
        "{strategy} should have at least one evidence-hiding failure"
    );
}

fn assert_counterexample_where_prog_costs_more(metrics: &[TaskMetric]) {
    let by_key = metrics
        .iter()
        .map(|metric| ((metric.scenario_id.as_str(), metric.strategy), metric))
        .collect::<BTreeMap<_, _>>();
    let counterexamples = metrics
        .iter()
        .filter(|metric| metric.counterexample && metric.strategy == "raw")
        .collect::<Vec<_>>();
    assert!(
        !counterexamples.is_empty(),
        "missing counterexample scenario"
    );
    assert!(counterexamples.iter().any(|raw| {
        let prog = by_key
            .get(&(raw.scenario_id.as_str(), "prog_expand"))
            .unwrap();
        prog.correct && prog.input_tokens > raw.input_tokens
    }));
}

fn strategy_rows<'a>(metrics: &'a [TaskMetric], strategy: &str) -> Vec<&'a TaskMetric> {
    metrics
        .iter()
        .filter(|metric| metric.strategy == strategy)
        .collect()
}

fn markdown_report(metrics: &[TaskMetric]) -> String {
    let mut output = String::from(
        "# Task-success eval\n\n\
         This deterministic eval asks whether each strategy exposes the evidence needed to answer fixed tasks. It is not a model-quality benchmark; optional model-backed scoring should be gated separately.\n\n\
         Regenerate this report and the raw metrics with `PROG_TASK_EVAL_UPDATE=1 cargo test -p prog-cli --test task_success -- --nocapture`.\n\n\
         ## Aggregate\n\n\
         | Strategy | Correct | Scenarios | Input tokens | Tool calls | Expansions | Cache hits |\n\
         |---|---:|---:|---:|---:|---:|---:|\n",
    );
    for strategy in [
        "raw",
        "simple_truncation",
        "jq_field_selection",
        "rtk_grep_filter",
        "prog_call_only",
        "prog_expand",
    ] {
        let rows = strategy_rows(metrics, strategy);
        output.push_str(&format!(
            "| {strategy} | {} | {} | {} | {} | {} | {} |\n",
            rows.iter().filter(|metric| metric.correct).count(),
            rows.len(),
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
         | Scenario | Artifact | Evidence path | Counterexample |\n\
         |---|---|---|---:|\n",
    );
    let mut seen = BTreeMap::new();
    for metric in metrics {
        seen.entry(metric.scenario_id.clone()).or_insert(metric);
    }
    for metric in seen.values() {
        output.push_str(&format!(
            "| {} | {} | `{}` | {} |\n",
            metric.scenario_id, metric.artifact, metric.evidence_path, metric.counterexample
        ));
    }
    output.push_str(
        "\n## Counterexamples\n\n\
         The tiny payload scenario is intentionally included: raw context is correct and cheaper than a `prog` envelope plus expansion. This report should keep that loss visible.\n",
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

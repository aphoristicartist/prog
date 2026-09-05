use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

const ITEM_COUNT: usize = 220;
const BODY_BYTES: usize = 768;
const STANDARD_OUTPUT_TOKENS: usize = 64;
const CAVEMAN_OUTPUT_TOKENS: usize = 8;
const FABLE_INPUT_PRICE_PER_MILLION: f64 = 10.0;
const FABLE_OUTPUT_PRICE_PER_MILLION: f64 = 50.0;

#[path = "support/eval_reports.rs"]
mod eval_reports;
#[path = "support/baseline_strategies.rs"]
mod strategies;
use strategies::{
    Execution, STRATEGIES, Source as BaselineSource, Step, StrategyInput, Task, assert_success,
    timed_prog,
};

#[derive(Clone)]
struct GradingOracle {
    evidence_path: Option<String>,
    answer: Option<String>,
}

#[derive(Clone)]
struct BaselineScenario {
    id: String,
    artifact: String,
    input: StrategyInput,
    oracle: GradingOracle,
    counterexample: bool,
}

#[derive(Debug, Clone, Serialize)]
struct BaselineMetric {
    scenario_id: String,
    prompt: String,
    artifact: String,
    task_mode: &'static str,
    strategy: String,
    available: bool,
    evidence_available: bool,
    outcome: &'static str,
    artifact_bytes: usize,
    response_bytes: usize,
    input_tokens: usize,
    assumed_answer_tokens: usize,
    tool_calls: usize,
    expansion_count: usize,
    cache_hits: usize,
    wall_time_ms: u128,
    illustrative_model_cost_usd: f64,
    oracle_path: Option<String>,
    selected_path: Option<String>,
    counterexample: bool,
    steps: Vec<Step>,
    notes: Vec<String>,
}

#[tokio::test]
async fn competitive_baseline_eval_smoke() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut keep_servers = Vec::new();
    let scenarios = setup_scenarios(tempdir.path(), &mut keep_servers).await;
    assert!(
        scenarios.len() >= 10,
        "competitive eval should cover at least 10 scenarios"
    );

    let metrics = scenarios
        .iter()
        .flat_map(|scenario| run_scenario(tempdir.path(), scenario))
        .collect::<Vec<_>>();

    assert_measurement_contract(&metrics);

    if std::env::var_os("PROG_BASELINE_EVAL_UPDATE").is_some() {
        let root = repo_root();
        fs::write(
            root.join("fixtures/evals/competitive-baseline-metrics.json"),
            format!("{}\n", serde_json::to_string_pretty(&metrics).unwrap()),
        )
        .unwrap();
        eval_reports::write_documents(&root);
    } else {
        assert!(repo_root().join("docs/competitive-baselines.md").exists());
        assert!(
            repo_root()
                .join("fixtures/evals/competitive-baseline-metrics.json")
                .exists()
        );
    }
}

/// Known-path pagination recoverability: all page markers are recovered, and
/// cost includes the capture plus every lookup. No cost ordering is required.
#[tokio::test]
async fn pagination_recoverability_counts_capture_and_every_lookup() {
    let root = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    // 5-page cursor chain. Each page carries a large body so the raw
    // aggregate is clearly more expensive than one bounded envelope.
    let page_tokens = ["start", "t2", "t3", "t4", "t5"];
    let mut raw_total_bytes = 0usize;
    for (index, token) in page_tokens.iter().enumerate() {
        let is_last = index + 1 == page_tokens.len();
        let body = json!({
            "items": [{
                "id": index + 1,
                "marker": format!("page-{}-marker", index + 1),
                "body": "x".repeat(2048)
            }],
            "next_cursor": if is_last { Value::Null } else { json!(page_tokens[index + 1]) },
            "has_more": !is_last
        });
        // Drop the null next_cursor on the last page for realism.
        let body = if is_last {
            let mut b = body;
            b["next_cursor"].take();
            b
        } else {
            body
        };
        raw_total_bytes += serde_json::to_vec(&body).unwrap().len();
        Mock::given(method("GET"))
            .and(path("/issues"))
            .and(query_param("page_token", *token))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }

    let seed = root.path().join("pag.json");
    fs::write(
        &seed,
        serde_json::to_vec_pretty(&json!({
            "kind": "http",
            "base_url": server.uri(),
            "operations": [{
                "name": "list",
                "method": "GET",
                "path": "/issues",
                "query": {"page_token": "{page_token}"},
                "args": {"page_token": "string"},
                "effect": {
                    "read_only": true, "mutating": false, "network": true,
                    "shell": false, "sensitive": false, "cacheable": true,
                    "requires_confirmation": false
                }
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    discover(root.path(), "pag", "http", &seed);

    let mut execution = Execution::default();
    let envelope = execution.prog(
        root.path(),
        &[
            "call",
            "pag",
            "list",
            "--args",
            r#"{"page_token":"start"}"#,
            "--pages",
            "5",
        ],
        None,
    );
    assert_eq!(envelope["pagination"]["pages_fetched"], json!(5));
    assert!(execution.response_bytes() <= 16 * 1024);
    let pagination = &envelope["pagination"];

    // Correctness parity: every page's marker is recoverable via its own
    // per-page cursor, so no evidence was lost to the envelope budget.
    let pages = pagination["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 5);
    for page in pages {
        let cursor = page["cursor"].as_str().expect("page cursor");
        let value = execution.prog(
            root.path(),
            &["expand", cursor, "--path", "/items/0/marker"],
            None,
        );
        let marker = value["data_preview"]
            .as_str()
            .or_else(|| value["data_preview"]["value"].as_str())
            .unwrap_or("");
        let page_no = page["page"].as_u64().unwrap();
        assert!(
            marker.contains(&format!("page-{page_no}-marker")),
            "page {page_no} marker recoverable via its cursor, got {marker}"
        );
    }
    assert_eq!(execution.tool_calls(), 6);
    assert!(
        execution
            .steps
            .iter()
            .all(|step| step.response_bytes <= 16 * 1024)
    );
    println!(
        "{}",
        json!({
            "scope": "known_page_markers_recoverability",
            "raw_bytes": raw_total_bytes,
            "all_response_bytes": execution.response_bytes(),
            "tool_calls": execution.tool_calls(),
            "approx_input_tokens": approx_tokens(execution.response_bytes()),
        })
    );
}

async fn setup_scenarios(root: &Path, keep_servers: &mut Vec<MockServer>) -> Vec<BaselineScenario> {
    let mut scenarios = Vec::new();

    let (http_source, http_raw, server) = setup_http_source(root).await;
    keep_servers.push(server);
    scenarios.extend(item_scenarios("http", "HTTP API", http_source, http_raw));

    let (cli_source, cli_raw) = setup_cli_source(root);
    scenarios.extend(item_scenarios("cli", "CLI", cli_source, cli_raw));

    scenarios.push(log_scenario());
    scenarios.push(diff_scenario());
    scenarios.push(report_scenario());
    scenarios.push(tiny_counterexample_scenario());

    scenarios.extend([
        unknown_target_scenario("unknown-target-buried-fatal", Some(1_200), "FATAL", true),
        unknown_target_scenario("unknown-target-relocated-fatal", Some(347), "FATAL", true),
        unknown_target_scenario("unknown-target-unranked", Some(1_200), "NOTE", true),
        unknown_target_scenario("unknown-target-no-answer", None, "NOTE", false),
    ]);

    scenarios
}

async fn setup_http_source(root: &Path) -> (BaselineSource, Vec<u8>, MockServer) {
    let server = MockServer::start().await;
    let payload = item_payload("items", "http");
    Mock::given(method("GET"))
        .and(path("/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(payload.clone()))
        .mount(&server)
        .await;
    let seed = root.join("baseline-http-seed.json");
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
    discover(root, "baseline_http", "http", &seed);
    (
        BaselineSource::Call {
            source_id: "baseline_http".to_string(),
            operation: "list".to_string(),
        },
        serde_json::to_vec(&payload).unwrap(),
        server,
    )
}

fn setup_cli_source(root: &Path) -> (BaselineSource, Vec<u8>) {
    let payload = item_payload("items", "cli");
    let payload_path = root.join("baseline-cli-payload.json");
    fs::write(&payload_path, serde_json::to_vec(&payload).unwrap()).unwrap();
    let command = format!(
        "import pathlib,sys; sys.stdout.buffer.write(pathlib.Path({:?}).read_bytes())",
        payload_path.to_string_lossy()
    );
    let seed = root.join("baseline-cli-seed.json");
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
    discover(root, "baseline_cli", "cli", &seed);
    (
        BaselineSource::Call {
            source_id: "baseline_cli".to_string(),
            operation: "list".to_string(),
        },
        serde_json::to_vec(&payload).unwrap(),
    )
}

// A known-path fixture explicitly publishes its selector in the task. The
// separately declared grep term comes from the task vocabulary, never answer.
struct KnownTargetFixture {
    path: String,
    answer: String,
    grep_term: Option<String>,
}

fn known_scenario(
    id: String,
    prompt: &str,
    artifact: &str,
    source: BaselineSource,
    raw: Vec<u8>,
    target: KnownTargetFixture,
) -> BaselineScenario {
    BaselineScenario {
        id,
        artifact: artifact.to_string(),
        counterexample: false,
        input: StrategyInput {
            prompt: format!("{prompt} Public lookup selector: {}.", target.path),
            source,
            raw_bytes: raw,
            task: Task::KnownPath {
                selector: target.path.clone(),
                grep_term: target.grep_term,
            },
        },
        oracle: GradingOracle {
            evidence_path: Some(target.path),
            answer: Some(target.answer),
        },
    }
}

fn observed_source(name: &str, mime: &str, raw: &[u8]) -> BaselineSource {
    BaselineSource::Observe {
        name: name.to_string(),
        mime: mime.to_string(),
        bytes: raw.to_vec(),
    }
}

fn item_scenarios(
    prefix: &str,
    artifact: &str,
    source: BaselineSource,
    raw_bytes: Vec<u8>,
) -> Vec<BaselineScenario> {
    [42usize, 128, 190]
        .into_iter()
        .map(|index| {
            let field = if index == 42 { "body" } else { "lookup_code" };
            known_scenario(
                format!("{prefix}-{field}-{index}"),
                &format!("Read {field} for {artifact} item {index}."),
                artifact,
                source.clone(),
                raw_bytes.clone(),
                KnownTargetFixture {
                    path: format!("/items/{index}/{field}"),
                    answer: if field == "body" {
                        format!("{prefix}-body-{index}-")
                    } else {
                        format!("{prefix}-code-{index}")
                    },
                    grep_term: Some(field.to_string()),
                },
            )
        })
        .collect()
}

fn log_scenario() -> BaselineScenario {
    let raw = (0..ITEM_COUNT)
        .map(|index| {
            if index == 180 {
                format!(
                    "2026-07-06T12:00:00Z ERROR trace_id=log-target-{index} {}",
                    "x".repeat(256)
                )
            } else {
                format!(
                    "2026-07-06T12:00:00Z INFO trace_id=log-noise-{index} {}",
                    "n".repeat(256)
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes();
    known_scenario(
        "log-line-180".to_string(),
        "Read the failing log trace id.",
        "Text log",
        observed_source("baseline-log", "text/plain", &raw),
        raw,
        KnownTargetFixture {
            path: "/lines/180/text".to_string(),
            answer: "trace_id=log-target-180".to_string(),
            grep_term: Some("ERROR".to_string()),
        },
    )
}

fn diff_scenario() -> BaselineScenario {
    let mut lines = vec![
        "diff --git a/src/main.rs b/src/main.rs".to_string(),
        "index 1111111..2222222 100644".to_string(),
        "--- a/src/main.rs".to_string(),
        "+++ b/src/main.rs".to_string(),
    ];
    for index in 0..180 {
        lines.push(if index == 96 {
            format!("+    let sentinel = \"diff-target-{index}\";")
        } else {
            format!("+    let noise_{index} = \"{}\";", "d".repeat(160))
        });
    }
    let raw = lines.join("\n").into_bytes();
    known_scenario(
        "diff-added-sentinel".to_string(),
        "Read the added sentinel value.",
        "Unified diff",
        observed_source("baseline-diff", "text/x-diff", &raw),
        raw,
        KnownTargetFixture {
            path: "/lines/100/text".to_string(),
            answer: "diff-target-96".to_string(),
            grep_term: Some("sentinel".to_string()),
        },
    )
}

fn report_scenario() -> BaselineScenario {
    let results = (0..120).map(|index| json!({
        "ruleId": format!("RULE-{index}"), "level": if index == 90 { "error" } else { "warning" },
        "message": {"text": if index == 90 { "report-target-critical-null-deref".to_string() } else {format!("report-noise-{index}-{}", "r".repeat(256))}},
        "locations": [{"physicalLocation": {"artifactLocation": {"uri": format!("src/file_{index}.rs")}}}]
    })).collect::<Vec<_>>();
    let raw = serde_json::to_vec(&json!({"version":"2.1.0", "runs":[{"tool":{"driver":{"name":"fixture"}}, "results":results}]})).unwrap();
    known_scenario(
        "sarif-report-message".to_string(),
        "Read the critical SARIF report message.",
        "Structured report",
        observed_source("baseline-report", "application/json", &raw),
        raw,
        KnownTargetFixture {
            path: "/runs/0/results/90/message/text".to_string(),
            answer: "report-target-critical-null-deref".to_string(),
            grep_term: Some("error".to_string()),
        },
    )
}

fn tiny_counterexample_scenario() -> BaselineScenario {
    let raw =
        serde_json::to_vec(&json!({"answer":"tiny-baseline-answer", "note":"raw should win"}))
            .unwrap();
    let mut scenario = known_scenario(
        "tiny-baseline-counterexample".to_string(),
        "Read the tiny baseline answer.",
        "Tiny JSON",
        observed_source("baseline-tiny", "application/json", &raw),
        raw,
        KnownTargetFixture {
            path: "/answer".to_string(),
            answer: "tiny-baseline-answer".to_string(),
            grep_term: Some("answer".to_string()),
        },
    );
    scenario.counterexample = true;
    scenario
}

fn unknown_target_scenario(
    id: &str,
    target: Option<usize>,
    marker: &str,
    decoys: bool,
) -> BaselineScenario {
    let raw = (0..1_400).map(|index| {
        if Some(index) == target {
            format!("svc-cause-{index}: {marker} commit aborted checksum mismatch in page 8821 data corruption {}", "f".repeat(64))
        } else if decoys && index % 45 == 0 {
            format!("svc-retry-{index}: ERROR transient retry 3 of 5 backing off {}", "r".repeat(64))
        } else { format!("svc-noise-{index}: request completed ok {}", "n".repeat(64)) }
    }).collect::<Vec<_>>().join("\n").into_bytes();
    BaselineScenario {
        id: id.to_string(),
        artifact: "Text log".to_string(),
        counterexample: false,
        input: StrategyInput {
            prompt: "Find the root cause of the failure in this log.".to_string(),
            // Stable identity; neither name nor task contains the target position.
            source: observed_source("baseline-unknown-target", "text/plain", &raw),
            raw_bytes: raw,
            task: Task::UnknownTarget,
        },
        oracle: GradingOracle {
            evidence_path: target.map(|index| format!("/lines/{index}/text")),
            answer: target.map(|_| "checksum mismatch".to_string()),
        },
    }
}

fn run_scenario(root: &Path, scenario: &BaselineScenario) -> Vec<BaselineMetric> {
    STRATEGIES
        .into_iter()
        .map(|strategy| {
            let execution = strategies::run(root, &scenario.input, strategy);
            // Strategy execution has finished before any oracle is consulted.
            grade(scenario, strategy, execution)
        })
        .collect()
}

fn grade(scenario: &BaselineScenario, strategy: &str, mut execution: Execution) -> BaselineMetric {
    let available = execution.unavailable.is_none();
    let evidence_available = available
        && scenario
            .oracle
            .answer
            .as_ref()
            .is_some_and(|answer| contains_answer(&execution.evidence, answer))
        && execution
            .selected_path
            .as_ref()
            .is_none_or(|path| Some(path) == scenario.oracle.evidence_path.as_ref());
    let outcome = if !available {
        "not_attempted"
    } else if evidence_available {
        "evidence_available"
    } else {
        "insufficient_evidence"
    };
    if let Some(reason) = execution.unavailable.take() {
        execution.notes.push(reason);
    }
    let response_bytes = execution.response_bytes();
    let input_tokens = approx_tokens(response_bytes);
    let assumed_answer_tokens = answer_output_tokens(strategy, evidence_available);
    BaselineMetric {
        scenario_id: scenario.id.clone(),
        prompt: scenario.input.prompt.clone(),
        artifact: scenario.artifact.clone(),
        task_mode: scenario.input.mode(),
        strategy: strategy.to_string(),
        available,
        evidence_available,
        outcome,
        artifact_bytes: scenario.input.raw_bytes.len(),
        response_bytes,
        input_tokens,
        assumed_answer_tokens,
        tool_calls: execution.tool_calls(),
        expansion_count: execution.steps.iter().filter(|s| s.expansion).count(),
        cache_hits: execution.steps.iter().filter(|s| s.cache_hit).count(),
        wall_time_ms: execution.elapsed_ms,
        illustrative_model_cost_usd: token_cost(input_tokens, assumed_answer_tokens),
        oracle_path: scenario.oracle.evidence_path.clone(),
        selected_path: execution.selected_path,
        counterexample: scenario.counterexample,
        steps: execution.steps,
        notes: execution.notes,
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

fn contains_answer(bytes: &[u8], answer: &str) -> bool {
    String::from_utf8_lossy(bytes).contains(answer)
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

fn answer_output_tokens(strategy: &str, correct: bool) -> usize {
    if !correct {
        0
    } else if strategy == "caveman_terse_output" {
        CAVEMAN_OUTPUT_TOKENS
    } else {
        STANDARD_OUTPUT_TOKENS
    }
}

fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
}

fn token_cost(input_tokens: usize, output_tokens: usize) -> f64 {
    input_tokens as f64 * FABLE_INPUT_PRICE_PER_MILLION / 1_000_000.0
        + output_tokens as f64 * FABLE_OUTPUT_PRICE_PER_MILLION / 1_000_000.0
}

fn assert_measurement_contract(metrics: &[BaselineMetric]) {
    assert!(!metrics.is_empty());
    for metric in metrics {
        assert_eq!(
            metric.response_bytes,
            metric.steps.iter().map(|s| s.response_bytes).sum::<usize>()
        );
        assert_eq!(
            metric.tool_calls,
            metric
                .steps
                .iter()
                .filter(|s| !s.command.is_empty())
                .count()
        );
        assert_eq!(metric.input_tokens, approx_tokens(metric.response_bytes));
        if !metric.available {
            assert!(!metric.evidence_available);
            assert_eq!(metric.outcome, "not_attempted");
            assert!(metric.steps.is_empty());
        }
        if metric.task_mode == "known_path_recoverability"
            && matches!(
                metric.strategy.as_str(),
                "prog_retrieve" | "prog_repeated_cache"
            )
        {
            assert!(
                metric.evidence_available,
                "explicitly known path must remain recoverable: {}",
                metric.scenario_id
            );
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

#[test]
fn unknown_strategy_commands_ignore_grader_paths_and_answers() {
    let dir = tempfile::tempdir().unwrap();
    let original = unknown_target_scenario("isolation", Some(1200), "FATAL", true);
    let mut changed = original.clone();
    changed.oracle.evidence_path = Some("/secret-oracle-prefix/9999".to_string());
    changed.oracle.answer = Some("grader-only-answer".to_string());
    assert_eq!(
        serde_json::to_value(&original.input).unwrap(),
        serde_json::to_value(&changed.input).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&original.input.task).unwrap(),
        json!({"kind":"unknown_target"})
    );
    // Exercise the complete evaluation driver, including the boundary where
    // grading metadata is still in scope, so an orchestration-level fallback
    // cannot bypass the runner's narrower API without failing this regression.
    let first = run_scenario(dir.path(), &original);
    let second = run_scenario(dir.path(), &changed);
    for (first, second) in first.iter().zip(&second) {
        assert_eq!(first.strategy, second.strategy);
        let commands = |metric: &BaselineMetric| {
            metric
                .steps
                .iter()
                .map(|step| step.command.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            commands(first),
            commands(second),
            "{} consulted grading metadata",
            first.strategy
        );
        assert!(!second.evidence_available);
    }
    let mut wrong_path = original.clone();
    wrong_path.oracle.evidence_path = changed.oracle.evidence_path;
    for strategy in ["prog_retrieve", "prog_repeated_cache"] {
        let execution = strategies::run(dir.path(), &original.input, strategy);
        assert!(execution.selected_path.is_some());
        assert!(!grade(&wrong_path, strategy, execution).evidence_available);
    }
}

fn assert_paths_were_observed(execution: &Execution) {
    for (index, step) in execution.steps.iter().enumerate() {
        if !step.expansion {
            continue;
        }
        let path = step
            .command
            .windows(2)
            .find(|args| args[0] == "--path")
            .unwrap()[1]
            .as_str();
        assert!(
            execution.steps[..index].iter().any(|prior| {
                let response: Value = serde_json::from_slice(&prior.response).unwrap();
                response["findings"]
                    .as_array()
                    .is_some_and(|findings| findings.iter().any(|f| f["path"] == path))
            }),
            "retrieval path {path} must originate in an actual earlier response"
        );
    }
}

#[test]
fn unknown_retrieval_follows_observed_paths_after_causal_record_relocation() {
    let dir = tempfile::tempdir().unwrap();
    for target in [1200, 347] {
        let scenario = unknown_target_scenario("relocation", Some(target), "FATAL", true);
        for strategy in ["prog_retrieve", "prog_repeated_cache"] {
            let execution = strategies::run(dir.path(), &scenario.input, strategy);
            assert_paths_were_observed(&execution);
            assert_eq!(execution.selected_path, scenario.oracle.evidence_path);
            let measured = execution
                .steps
                .iter()
                .map(|step| step.response.len())
                .sum::<usize>();
            let metric = grade(&scenario, strategy, execution);
            assert_eq!(metric.response_bytes, measured);
            assert!(
                metric.evidence_available,
                "relocated fatal evidence must be recovered from its returned path"
            );
            assert_eq!(
                metric.tool_calls,
                if strategy == "prog_repeated_cache" {
                    3
                } else {
                    2
                }
            );
        }
    }
}

#[test]
fn unranked_and_absent_answers_have_no_oracle_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let unranked = unknown_target_scenario("unranked", Some(1200), "NOTE", true);
    let missing = unknown_target_scenario("missing", None, "NOTE", false);
    for scenario in [&unranked, &missing] {
        for strategy in [
            "prog_retrieve",
            "prog_repeated_cache",
            "broad_log_search",
            "file_capture_search",
        ] {
            let execution = strategies::run(dir.path(), &scenario.input, strategy);
            if strategy.starts_with("prog_") {
                assert_paths_were_observed(&execution);
                if scenario.oracle.answer.is_none() {
                    assert!(execution.selected_path.is_none());
                    assert_eq!(
                        execution.steps.len(),
                        2,
                        "initial capture and failed exploration both count"
                    );
                    assert_eq!(execution.steps[1].command[..2], ["prog", "inspect"]);
                }
            }
            let metric = grade(scenario, strategy, execution);
            assert_eq!(metric.outcome, "insufficient_evidence");
            assert!(metric.available);
            assert!(!metric.evidence_available);
        }
    }
}

#[test]
fn file_capture_and_broad_search_count_their_actual_responses() {
    let dir = tempfile::tempdir().unwrap();
    let scenario = unknown_target_scenario("broad", Some(1200), "FATAL", true);
    let unavailable = strategies::run(dir.path(), &scenario.input, "native_field_selection");
    let unavailable = grade(&scenario, "native_field_selection", unavailable);
    assert!(!unavailable.available);
    assert_eq!(unavailable.outcome, "not_attempted");
    assert_eq!(unavailable.tool_calls, 0);
    for strategy in ["broad_log_search", "file_capture_search"] {
        let execution = strategies::run(dir.path(), &scenario.input, strategy);
        let measured = execution
            .steps
            .iter()
            .map(|s| s.response.len())
            .sum::<usize>();
        if strategy == "file_capture_search" {
            assert_eq!(execution.steps.len(), 2);
            let receipt: Value = serde_json::from_slice(&execution.steps[0].response).unwrap();
            assert_eq!(receipt["bytes"], scenario.input.raw_bytes.len());
            assert!(measured > execution.steps[1].response.len());
        }
        let metric = grade(&scenario, strategy, execution);
        assert!(
            metric.evidence_available,
            "the declared broad search must retain the fatal line"
        );
        assert_eq!(metric.response_bytes, measured);
        assert_eq!(metric.input_tokens, measured.div_ceil(4));
    }
}

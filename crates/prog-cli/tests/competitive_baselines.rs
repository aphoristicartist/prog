use std::{
    collections::{BTreeMap, BTreeSet},
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
    matchers::{method, path, query_param},
};

const ITEM_COUNT: usize = 220;
const BODY_BYTES: usize = 768;
const TRUNCATION_BYTES: usize = 4096;
const STANDARD_OUTPUT_TOKENS: usize = 64;
const CAVEMAN_OUTPUT_TOKENS: usize = 8;
const FABLE_INPUT_PRICE_PER_MILLION: f64 = 10.0;
const FABLE_OUTPUT_PRICE_PER_MILLION: f64 = 50.0;

#[derive(Clone)]
enum BaselineSource {
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
struct BaselineScenario {
    id: String,
    prompt: String,
    artifact: String,
    source: BaselineSource,
    raw_bytes: Vec<u8>,
    evidence_path: String,
    evidence_range: String,
    answer: String,
    counterexample: bool,
}

#[derive(Debug, Clone, Serialize)]
struct BaselineMetric {
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
    cache_hit_rate: f64,
    wall_time_ms: u128,
    estimated_model_cost_usd: f64,
    evidence_path: String,
    evidence_range: String,
    hides_evidence: bool,
    counterexample: bool,
    notes: Vec<String>,
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

    assert_strategy_all_correct(&metrics, "raw_context");
    assert_strategy_all_correct(&metrics, "prog_paths_expand");
    assert_strategy_all_correct(&metrics, "prog_repeated_cache");
    assert_strategy_has_failures(&metrics, "head_tail_truncation");
    assert_strategy_has_failures(&metrics, "prog_envelope_only");
    assert_strategy_has_wins_and_losses(&metrics, "native_field_selection");
    assert_counterexample_where_raw_is_cheaper(&metrics);
    assert_every_scenario_has_evidence(&metrics);

    let report = markdown_report(&metrics);
    let metrics_json = serde_json::to_string_pretty(&metrics).unwrap();
    if std::env::var_os("PROG_BASELINE_EVAL_UPDATE").is_some() {
        let root = repo_root();
        fs::write(root.join("docs/competitive-baselines.md"), &report).unwrap();
        fs::write(
            root.join("fixtures/evals/competitive-baseline-metrics.json"),
            format!("{metrics_json}\n"),
        )
        .unwrap();
        println!("{report}");
    } else {
        assert!(repo_root().join("docs/competitive-baselines.md").exists());
        assert!(
            repo_root()
                .join("fixtures/evals/competitive-baseline-metrics.json")
                .exists()
        );
    }
}

/// Competitive baseline for upstream auto-pagination (issue #69). prog
/// prefetches N pages under one bounded envelope while raw page-by-page
/// fetching pays the full input cost of every page. Asserts the two things
/// that matter for the envelope-budget story: (1) prog's single envelope is
/// cheaper (in approx tokens) than the raw concatenation of all pages, and
/// (2) correctness is equal — every page's evidence is recoverable via its
/// own per-page cursor (no data is lost to the budget).
#[tokio::test]
async fn pagination_competitive_baseline_vs_raw_page_by_page() {
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

    // prog: one bounded envelope prefetching all 5 pages.
    let prog_call = timed_prog(
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
    assert_success(&prog_call.output);
    let envelope: Value = serde_json::from_slice(&prog_call.output.stdout).unwrap();
    let pagination = &envelope["pagination"];
    assert_eq!(pagination["pages_fetched"], json!(5));
    let prog_bytes = prog_call.output.stdout.len();

    // Raw: the concatenation of all 5 page bodies (what page-by-page fetching
    // would feed a model in aggregate).
    let prog_tokens = approx_tokens(prog_bytes);
    let raw_tokens = approx_tokens(raw_total_bytes);
    assert!(
        prog_tokens < raw_tokens,
        "prog envelope ({prog_tokens} tok / {prog_bytes} B) must be cheaper than raw page-by-page ({raw_tokens} tok / {raw_total_bytes} B)"
    );

    // Correctness parity: every page's marker is recoverable via its own
    // per-page cursor, so no evidence was lost to the envelope budget.
    let pages = pagination["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 5);
    for page in pages.iter().filter(|p| p["page"].as_u64() >= Some(2)) {
        let cursor = page["cursor"].as_str().expect("page cursor");
        let expanded = timed_prog(
            root.path(),
            &["expand", cursor, "--path", "/items/0/marker"],
            None,
        );
        assert_success(&expanded.output);
        let value: Value = serde_json::from_slice(&expanded.output.stdout).unwrap();
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
        "import pathlib; print(pathlib.Path({:?}).read_text())",
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
            let answer = if field == "body" {
                format!("{prefix}-body-{index}-")
            } else {
                format!("{prefix}-code-{index}")
            };
            BaselineScenario {
                id: format!("{prefix}-{field}-{index}"),
                prompt: format!("Find {field} for {artifact} item {index}."),
                artifact: artifact.to_string(),
                source: source.clone(),
                raw_bytes: raw_bytes.clone(),
                evidence_path: format!("/items/{index}/{field}"),
                evidence_range: format!("JSON pointer /items/{index}/{field}"),
                answer,
                counterexample: false,
            }
        })
        .collect()
}

fn log_scenario() -> BaselineScenario {
    let target = 180usize;
    let lines = (0..ITEM_COUNT)
        .map(|index| {
            if index == target {
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
        .collect::<Vec<_>>();
    let raw = lines.join("\n").into_bytes();
    BaselineScenario {
        id: "log-line-180".to_string(),
        prompt: "Find the failing log trace id on line 180.".to_string(),
        artifact: "Text log".to_string(),
        source: BaselineSource::Observe {
            name: "baseline-log".to_string(),
            mime: "text/plain".to_string(),
            bytes: raw.clone(),
        },
        raw_bytes: raw,
        evidence_path: "/lines/180/text".to_string(),
        evidence_range: "line 180".to_string(),
        answer: "trace_id=log-target-180".to_string(),
        counterexample: false,
    }
}

fn diff_scenario() -> BaselineScenario {
    let target = 96usize;
    let mut lines = vec![
        "diff --git a/src/main.rs b/src/main.rs".to_string(),
        "index 1111111..2222222 100644".to_string(),
        "--- a/src/main.rs".to_string(),
        "+++ b/src/main.rs".to_string(),
    ];
    for index in 0..180 {
        if index == target {
            lines.push(format!("+    let sentinel = \"diff-target-{index}\";"));
        } else {
            lines.push(format!("+    let noise_{index} = \"{}\";", "d".repeat(160)));
        }
    }
    let raw = lines.join("\n").into_bytes();
    let line_index = target + 4;
    BaselineScenario {
        id: "diff-added-sentinel".to_string(),
        prompt: "Find the added sentinel value in the diff.".to_string(),
        artifact: "Unified diff".to_string(),
        source: BaselineSource::Observe {
            name: "baseline-diff".to_string(),
            mime: "text/x-diff".to_string(),
            bytes: raw.clone(),
        },
        raw_bytes: raw,
        evidence_path: format!("/lines/{line_index}/text"),
        evidence_range: format!("diff line {line_index}"),
        answer: "diff-target-96".to_string(),
        counterexample: false,
    }
}

fn report_scenario() -> BaselineScenario {
    let target = 90usize;
    let results = (0..120)
        .map(|index| {
            json!({
                "ruleId": format!("RULE-{index}"),
                "level": if index == target { "error" } else { "warning" },
                "message": {
                    "text": if index == target {
                        "report-target-critical-null-deref".to_string()
                    } else {
                        format!("report-noise-{index}-{}", "r".repeat(256))
                    }
                },
                "locations": [{"physicalLocation": {"artifactLocation": {"uri": format!("src/file_{index}.rs")}}}]
            })
        })
        .collect::<Vec<_>>();
    let payload = json!({"version": "2.1.0", "runs": [{"tool": {"driver": {"name": "fixture"}}, "results": results}]});
    let raw = serde_json::to_vec(&payload).unwrap();
    BaselineScenario {
        id: "sarif-report-message".to_string(),
        prompt: "Find the critical SARIF report message.".to_string(),
        artifact: "Structured report".to_string(),
        source: BaselineSource::Observe {
            name: "baseline-report".to_string(),
            mime: "application/json".to_string(),
            bytes: raw.clone(),
        },
        raw_bytes: raw,
        evidence_path: "/runs/0/results/90/message/text".to_string(),
        evidence_range: "JSON pointer /runs/0/results/90/message/text".to_string(),
        answer: "report-target-critical-null-deref".to_string(),
        counterexample: false,
    }
}

fn tiny_counterexample_scenario() -> BaselineScenario {
    let payload = json!({"answer": "tiny-baseline-answer", "note": "raw should win"});
    let raw = serde_json::to_vec(&payload).unwrap();
    BaselineScenario {
        id: "tiny-baseline-counterexample".to_string(),
        prompt: "Read the tiny baseline answer.".to_string(),
        artifact: "Tiny JSON".to_string(),
        source: BaselineSource::Observe {
            name: "baseline-tiny".to_string(),
            mime: "application/json".to_string(),
            bytes: raw.clone(),
        },
        raw_bytes: raw,
        evidence_path: "/answer".to_string(),
        evidence_range: "JSON pointer /answer".to_string(),
        answer: "tiny-baseline-answer".to_string(),
        counterexample: true,
    }
}

fn run_scenario(root: &Path, scenario: &BaselineScenario) -> Vec<BaselineMetric> {
    vec![
        raw_metric(scenario),
        truncation_metric(scenario),
        native_field_metric(scenario),
        rtk_filter_metric(scenario),
        caveman_metric(scenario),
        prog_envelope_metric(root, scenario),
        prog_paths_expand_metric(root, scenario),
        prog_repeated_cache_metric(root, scenario),
    ]
}

fn raw_metric(scenario: &BaselineScenario) -> BaselineMetric {
    let correct = contains_answer(&scenario.raw_bytes, &scenario.answer);
    metric(
        scenario,
        "raw_context",
        MetricInput {
            correct,
            input_bytes: scenario.raw_bytes.len(),
            output_tokens: answer_output_tokens("raw_context", correct),
            tool_calls: 0,
            expansion_count: 0,
            cache_hits: 0,
            wall_time_ms: 0,
            notes: vec!["complete but pays full input cost".to_string()],
        },
    )
}

fn truncation_metric(scenario: &BaselineScenario) -> BaselineMetric {
    let visible = head_tail(&scenario.raw_bytes, TRUNCATION_BYTES);
    let correct = contains_answer(&visible, &scenario.answer);
    metric(
        scenario,
        "head_tail_truncation",
        MetricInput {
            correct,
            input_bytes: visible.len(),
            output_tokens: answer_output_tokens("head_tail_truncation", correct),
            tool_calls: 0,
            expansion_count: 0,
            cache_hits: 0,
            wall_time_ms: 0,
            notes: vec!["bounded but unrecoverable when evidence is omitted".to_string()],
        },
    )
}

fn native_field_metric(scenario: &BaselineScenario) -> BaselineMetric {
    let filtered = if let Ok(value) = serde_json::from_slice::<Value>(&scenario.raw_bytes) {
        serde_json::to_vec(&extract_json_path(&value, &scenario.evidence_path)).unwrap_or_default()
    } else {
        Vec::new()
    };
    let correct = contains_answer(&filtered, &scenario.answer);
    metric(
        scenario,
        "native_field_selection",
        MetricInput {
            correct,
            input_bytes: filtered.len(),
            output_tokens: answer_output_tokens("native_field_selection", correct),
            tool_calls: 1,
            expansion_count: 0,
            cache_hits: 0,
            wall_time_ms: 5,
            notes: vec!["best when the exact path is already known".to_string()],
        },
    )
}

fn rtk_filter_metric(scenario: &BaselineScenario) -> BaselineMetric {
    let text = String::from_utf8_lossy(&scenario.raw_bytes);
    let filtered = text
        .lines()
        .filter(|line| line.contains(&scenario.answer))
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes();
    let correct = contains_answer(&filtered, &scenario.answer);
    metric(
        scenario,
        "rtk_grep_filter",
        MetricInput {
            correct,
            input_bytes: filtered.len(),
            output_tokens: answer_output_tokens("rtk_grep_filter", correct),
            tool_calls: 1,
            expansion_count: 0,
            cache_hits: 0,
            wall_time_ms: 3,
            notes: vec!["wins on line-oriented artifacts when the grep term is known".to_string()],
        },
    )
}

fn caveman_metric(scenario: &BaselineScenario) -> BaselineMetric {
    let correct = contains_answer(&scenario.raw_bytes, &scenario.answer);
    metric(
        scenario,
        "caveman_terse_output",
        MetricInput {
            correct,
            input_bytes: scenario.raw_bytes.len(),
            output_tokens: answer_output_tokens("caveman_terse_output", correct),
            tool_calls: 0,
            expansion_count: 0,
            cache_hits: 0,
            wall_time_ms: 0,
            notes: vec!["reduces answer verbosity but not oversized tool input".to_string()],
        },
    )
}

fn prog_envelope_metric(root: &Path, scenario: &BaselineScenario) -> BaselineMetric {
    let initial = run_initial(root, scenario);
    assert_success(&initial.output);
    let correct = contains_answer(&initial.output.stdout, &scenario.answer);
    metric(
        scenario,
        "prog_envelope_only",
        MetricInput {
            correct,
            input_bytes: initial.output.stdout.len(),
            output_tokens: answer_output_tokens("prog_envelope_only", correct),
            tool_calls: 1,
            expansion_count: 0,
            cache_hits: cache_hit_count(&initial.output),
            wall_time_ms: initial.elapsed_ms,
            notes: vec!["first view is bounded and may intentionally hide evidence".to_string()],
        },
    )
}

fn prog_paths_expand_metric(root: &Path, scenario: &BaselineScenario) -> BaselineMetric {
    let initial = run_initial(root, scenario);
    assert_success(&initial.output);
    let cursor = cursor(&initial.output);
    let prefix = parent_path(&scenario.evidence_path);
    let paths = timed_prog(root, &["paths", &cursor, "--prefix", &prefix], None);
    assert_success(&paths.output);
    let expanded = timed_prog(
        root,
        &["expand", &cursor, "--path", &scenario.evidence_path],
        None,
    );
    assert_success(&expanded.output);
    let correct = contains_answer(&expanded.output.stdout, &scenario.answer);
    metric(
        scenario,
        "prog_paths_expand",
        MetricInput {
            correct,
            input_bytes: initial.output.stdout.len()
                + paths.output.stdout.len()
                + expanded.output.stdout.len(),
            output_tokens: answer_output_tokens("prog_paths_expand", correct),
            tool_calls: 3,
            expansion_count: 1,
            cache_hits: cache_hit_count(&paths.output) + cache_hit_count(&expanded.output),
            wall_time_ms: initial.elapsed_ms + paths.elapsed_ms + expanded.elapsed_ms,
            notes: vec!["bounded discovery plus exact cursor-backed expansion".to_string()],
        },
    )
}

fn prog_repeated_cache_metric(root: &Path, scenario: &BaselineScenario) -> BaselineMetric {
    let initial = run_initial(root, scenario);
    assert_success(&initial.output);
    let cursor = cursor(&initial.output);
    let first = timed_prog(
        root,
        &["expand", &cursor, "--path", &scenario.evidence_path],
        None,
    );
    assert_success(&first.output);
    let second = timed_prog(
        root,
        &["expand", &cursor, "--path", &scenario.evidence_path],
        None,
    );
    assert_success(&second.output);
    let correct = contains_answer(&second.output.stdout, &scenario.answer);
    metric(
        scenario,
        "prog_repeated_cache",
        MetricInput {
            correct,
            input_bytes: initial.output.stdout.len()
                + first.output.stdout.len()
                + second.output.stdout.len(),
            output_tokens: answer_output_tokens("prog_repeated_cache", correct),
            tool_calls: 3,
            expansion_count: 2,
            cache_hits: cache_hit_count(&first.output) + cache_hit_count(&second.output),
            wall_time_ms: initial.elapsed_ms + first.elapsed_ms + second.elapsed_ms,
            notes: vec![
                "repeated evidence inspection should be served from local cache".to_string(),
            ],
        },
    )
}

fn metric(
    scenario: &BaselineScenario,
    strategy: &'static str,
    input: MetricInput,
) -> BaselineMetric {
    let input_tokens = approx_tokens(input.input_bytes);
    let estimated_model_cost_usd = token_cost(input_tokens, input.output_tokens);
    BaselineMetric {
        scenario_id: scenario.id.clone(),
        prompt: scenario.prompt.clone(),
        artifact: scenario.artifact.clone(),
        strategy,
        correct: input.correct,
        input_tokens,
        output_tokens: input.output_tokens,
        tool_calls: input.tool_calls,
        expansion_count: input.expansion_count,
        cache_hits: input.cache_hits,
        cache_hit_rate: if input.tool_calls == 0 {
            0.0
        } else {
            input.cache_hits as f64 / input.tool_calls as f64
        },
        wall_time_ms: input.wall_time_ms,
        estimated_model_cost_usd,
        evidence_path: scenario.evidence_path.clone(),
        evidence_range: scenario.evidence_range.clone(),
        hides_evidence: !input.correct,
        counterexample: scenario.counterexample,
        notes: input.notes,
    }
}

fn run_initial(root: &Path, scenario: &BaselineScenario) -> TimedOutput {
    match &scenario.source {
        BaselineSource::Call {
            source_id,
            operation,
        } => timed_prog(root, &["call", source_id, operation, "--args", "{}"], None),
        BaselineSource::Observe { name, mime, bytes } => timed_prog(
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

fn head_tail(bytes: &[u8], max_bytes: usize) -> Vec<u8> {
    if bytes.len() <= max_bytes {
        return bytes.to_vec();
    }
    let half = max_bytes / 2;
    let mut output = Vec::with_capacity(max_bytes);
    output.extend_from_slice(&bytes[..half]);
    output.extend_from_slice(&bytes[bytes.len() - half..]);
    output
}

fn extract_json_path(value: &Value, path: &str) -> Value {
    let mut current = value;
    for part in path.strip_prefix('/').unwrap_or(path).split('/') {
        if part.is_empty() {
            continue;
        }
        current = match current {
            Value::Object(map) => map.get(part).unwrap_or(&Value::Null),
            Value::Array(values) => part
                .parse::<usize>()
                .ok()
                .and_then(|index| values.get(index))
                .unwrap_or(&Value::Null),
            _ => &Value::Null,
        };
    }
    current.clone()
}

fn parent_path(path: &str) -> String {
    let Some((parent, _)) = path.rsplit_once('/') else {
        return String::new();
    };
    parent.to_string()
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

fn assert_strategy_all_correct(metrics: &[BaselineMetric], strategy: &str) {
    let rows = strategy_rows(metrics, strategy);
    assert!(!rows.is_empty(), "missing strategy {strategy}");
    assert!(
        rows.iter().all(|metric| metric.correct),
        "{strategy} should solve every task"
    );
}

fn assert_strategy_has_failures(metrics: &[BaselineMetric], strategy: &str) {
    let rows = strategy_rows(metrics, strategy);
    assert!(!rows.is_empty(), "missing strategy {strategy}");
    assert!(
        rows.iter().any(|metric| !metric.correct),
        "{strategy} should have at least one hidden-evidence failure"
    );
}

fn assert_strategy_has_wins_and_losses(metrics: &[BaselineMetric], strategy: &str) {
    let rows = strategy_rows(metrics, strategy);
    assert!(!rows.is_empty(), "missing strategy {strategy}");
    assert!(rows.iter().any(|metric| metric.correct));
    assert!(rows.iter().any(|metric| !metric.correct));
}

fn assert_counterexample_where_raw_is_cheaper(metrics: &[BaselineMetric]) {
    let by_key = metrics
        .iter()
        .map(|metric| ((metric.scenario_id.as_str(), metric.strategy), metric))
        .collect::<BTreeMap<_, _>>();
    let raw_counterexamples = metrics
        .iter()
        .filter(|metric| metric.counterexample && metric.strategy == "raw_context")
        .collect::<Vec<_>>();
    assert!(!raw_counterexamples.is_empty());
    assert!(raw_counterexamples.iter().any(|raw| {
        let prog = by_key
            .get(&(raw.scenario_id.as_str(), "prog_paths_expand"))
            .unwrap();
        raw.correct && prog.correct && raw.estimated_model_cost_usd < prog.estimated_model_cost_usd
    }));
}

fn assert_every_scenario_has_evidence(metrics: &[BaselineMetric]) {
    let scenarios = metrics
        .iter()
        .map(|metric| metric.scenario_id.as_str())
        .collect::<BTreeSet<_>>();
    for scenario in scenarios {
        let row = metrics
            .iter()
            .find(|metric| metric.scenario_id == scenario)
            .unwrap();
        assert!(!row.evidence_path.is_empty());
        assert!(!row.evidence_range.is_empty());
    }
}

fn strategy_rows<'a>(metrics: &'a [BaselineMetric], strategy: &str) -> Vec<&'a BaselineMetric> {
    metrics
        .iter()
        .filter(|metric| metric.strategy == strategy)
        .collect()
}

fn markdown_report(metrics: &[BaselineMetric]) -> String {
    let mut output = String::from(
        "# Competitive baselines\n\n\
         This deterministic eval compares `prog` with raw context, truncation, native field selection, RTK-style filtering, Caveman-style terse output, and repeated cursor-backed cache use. Costs use the checked-in `models/fable-class-2026-07.json` illustrative price profile.\n\n\
         Regenerate this report and the raw metrics with `PROG_BASELINE_EVAL_UPDATE=1 cargo test -p prog-cli --test competitive_baselines -- --nocapture`.\n\n\
         ## Aggregate\n\n\
         | Strategy | Correct | Scenarios | Input tokens | Output tokens | Tool calls | Expansions | Cache hits | Est. Fable cost |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for strategy in [
        "raw_context",
        "head_tail_truncation",
        "native_field_selection",
        "rtk_grep_filter",
        "caveman_terse_output",
        "prog_envelope_only",
        "prog_paths_expand",
        "prog_repeated_cache",
    ] {
        let rows = strategy_rows(metrics, strategy);
        output.push_str(&format!(
            "| {strategy} | {} | {} | {} | {} | {} | {} | {} | {:.6} |\n",
            rows.iter().filter(|metric| metric.correct).count(),
            rows.len(),
            rows.iter().map(|metric| metric.input_tokens).sum::<usize>(),
            rows.iter()
                .map(|metric| metric.output_tokens)
                .sum::<usize>(),
            rows.iter().map(|metric| metric.tool_calls).sum::<usize>(),
            rows.iter()
                .map(|metric| metric.expansion_count)
                .sum::<usize>(),
            rows.iter().map(|metric| metric.cache_hits).sum::<usize>(),
            rows.iter()
                .map(|metric| metric.estimated_model_cost_usd)
                .sum::<f64>()
        ));
    }

    output.push_str(
        "\n## Scenarios\n\n\
         | Scenario | Artifact | Evidence | Counterexample |\n\
         |---|---|---|---:|\n",
    );
    let mut seen = BTreeMap::new();
    for metric in metrics {
        seen.entry(metric.scenario_id.clone()).or_insert(metric);
    }
    for metric in seen.values() {
        output.push_str(&format!(
            "| {} | {} | `{}` ({}) | {} |\n",
            metric.scenario_id,
            metric.artifact,
            metric.evidence_path,
            metric.evidence_range,
            metric.counterexample
        ));
    }

    output.push_str("\n## Wins, Losses, And Counterexamples\n\n");
    output.push_str("- Native field selection is the cheapest correct strategy when a JSON path is already known.\n");
    output.push_str("- RTK-style grep filtering wins on logs and diffs when the exact search term is known, but can return an entire minified JSON payload.\n");
    output.push_str("- Caveman-style terse output reduces answer tokens but leaves raw tool input cost unchanged.\n");
    output.push_str("- `prog_envelope_only` intentionally loses when the bounded first view hides required evidence.\n");
    output.push_str("- `prog_paths_expand` and `prog_repeated_cache` solve every scenario here, but the tiny payload counterexample is cheaper as raw context.\n");
    output
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

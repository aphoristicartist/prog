//! Deterministic replay layer for actual-agent evaluation (#139).
//!
//! Checked-in traces are explicitly synthetic grader fixtures. They prove
//! that state/evidence graders reject false completion and stale read-back
//! claims without a model or credentials. They do not support a claim that
//! real agents perform better.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::path};

mod support;

use support::{prog, repo_root, stdout};

const BLESS_COMMAND: &str =
    "PROG_AGENT_EVAL_BLESS=1 cargo test -p prog-cli --test agent_eval -- --nocapture";

#[derive(Debug, Deserialize)]
struct TraceSet {
    schema: String,
    harness_version: String,
    traces: Vec<AgentTrace>,
}

#[derive(Debug, Deserialize)]
struct AgentTrace {
    trace_id: String,
    workflow_id: String,
    arm: String,
    source: String,
    decision: String,
    citations: Vec<TraceCitation>,
    shown_false_findings: u64,
    expected_accepted: bool,
}

#[derive(Debug, Deserialize)]
struct TraceCitation {
    role: String,
    path: String,
}

#[derive(Debug)]
struct WorkflowOracle {
    expected_decision: &'static str,
    verified_outcome_proven: bool,
    store_dir: PathBuf,
    cursors: BTreeMap<String, String>,
    model_visible_tool_response_bytes: u64,
    correctness_checks: BTreeMap<String, bool>,
    _fixture: tempfile::TempDir,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AgentEvalReport {
    schema: String,
    harness_version: String,
    estimator: String,
    actual_agent_trials: u64,
    claim_eligible: bool,
    fixed_context: FixedContext,
    live_trials: Vec<LiveTrial>,
    uncertainty: Option<UncertaintyReport>,
    public_benchmarks: Vec<PublicBenchmarkReport>,
    strategy_status: Vec<StrategyStatus>,
    trace_results: Vec<TraceResult>,
    summary: AgentEvalSummary,
    limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LiveTrial {
    trial_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    benchmark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    arm: String,
    provider: String,
    model: String,
    model_version: Option<String>,
    harness_version: String,
    started_at: String,
    region: Option<String>,
    trial_seed: Option<String>,
    settings: BTreeMap<String, String>,
    dropout: Option<String>,
    #[serde(default)]
    timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claimed_success: Option<bool>,
    graders: BTreeMap<String, bool>,
    token_usage: LiveTokenUsage,
    tool_calls: u64,
    navigation_calls: u64,
    upstream_reruns: u64,
    model_visible_tool_response_bytes: u64,
    latency_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PublicBenchmarkReport {
    benchmark: String,
    synthetic: bool,
    harness: String,
    harness_version: String,
    provider: String,
    model: String,
    model_version: Option<String>,
    date: String,
    subset: String,
    seed: String,
    n_per_arm: BTreeMap<String, u64>,
    trials: Vec<LiveTrial>,
    wilson_95: Vec<ProportionInterval>,
    mcnemar: McNemarReport,
    false_completion_counts: BTreeMap<String, u64>,
    dropout_counts: BTreeMap<String, u64>,
    claim_eligible: bool,
    relative_effect_only: bool,
    contamination_caveat: Option<String>,
    interpretation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ProportionInterval {
    arm: String,
    metric: String,
    successes: u64,
    trials: u64,
    estimate: f64,
    lower: f64,
    upper: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct McNemarReport {
    method: String,
    complete_pairs: u64,
    raw_only: u64,
    prog_only: u64,
    exact_two_sided_p: f64,
}

#[derive(Debug, Deserialize)]
struct PublicBenchmarkDryRun {
    schema: String,
    synthetic: bool,
    benchmarks: Vec<PublicBenchmarkFixture>,
}

#[derive(Debug, Deserialize)]
struct PublicBenchmarkFixture {
    benchmark: String,
    harness: String,
    harness_version: String,
    date: String,
    subset: String,
    seed: String,
    relative_effect_only: bool,
    contamination_caveat: Option<String>,
    trials: Vec<LiveTrial>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct LiveTokenUsage {
    system_prompt_tokens: Option<u64>,
    tool_schema_tokens: Option<u64>,
    skill_tokens: Option<u64>,
    model_visible_tool_response_tokens: Option<u64>,
    provider_input_tokens: Option<u64>,
    provider_output_tokens: Option<u64>,
    provider_cached_input_tokens: Option<u64>,
    provider_reasoning_tokens: Option<u64>,
}

impl LiveTokenUsage {
    fn provider_total_tokens(&self) -> Option<u64> {
        self.provider_input_tokens?
            .checked_add(self.provider_output_tokens?)
    }

    fn required_fields_available(&self) -> bool {
        [
            self.system_prompt_tokens,
            self.tool_schema_tokens,
            self.skill_tokens,
            self.model_visible_tool_response_tokens,
            self.provider_input_tokens,
            self.provider_output_tokens,
        ]
        .iter()
        .all(Option::is_some)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UncertaintyReport {
    method: String,
    confidence_level: u64,
    intervals: Vec<UncertaintyInterval>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UncertaintyInterval {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    benchmark: Option<String>,
    arm: String,
    metric: String,
    trials: u64,
    lower: f64,
    median: f64,
    upper: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FixedContext {
    prog_skill_bytes: u64,
    prog_skill_estimated_tokens: u64,
    tool_schema_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StrategyStatus {
    strategy: String,
    live_trials: u64,
    performance_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TraceResult {
    trace_id: String,
    workflow_id: String,
    arm: String,
    source: String,
    accepted: bool,
    expected_accepted: bool,
    model_visible_tool_response_bytes: Option<u64>,
    estimated_tool_response_tokens: Option<u64>,
    graders: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct AgentEvalSummary {
    replay_traces: u64,
    expected_acceptances: u64,
    expected_rejections: u64,
    trace_expectations_passed: u64,
    adversarial_rejections: u64,
    false_completion_attempts: u64,
    false_completions_accepted: u64,
}

#[tokio::test]
async fn agent_eval_replay_rejects_false_coding_and_state_completions() {
    let root = repo_root();
    let traces: TraceSet = serde_json::from_slice(
        &fs::read(root.join("fixtures/agent-eval/replay-traces.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(traces.schema, "prog.agent_eval.trace_set");
    assert_eq!(traces.harness_version, "1");

    let coding = coding_narrowed_rerun_oracle();
    let state = state_token_oracle().await;
    assert!(coding.correctness_checks.values().all(|passed| *passed));
    assert!(state.correctness_checks.values().all(|passed| *passed));
    // The live outputs must be non-empty, but their exact byte sizes include
    // absolute temporary paths and therefore legitimately differ by runner.
    // Do not bless one machine's path lengths as a cross-platform metric.
    assert!(coding.model_visible_tool_response_bytes > 0);
    assert!(state.model_visible_tool_response_bytes > 0);
    let oracles = BTreeMap::from([
        ("coding_narrowed_rerun", coding),
        ("state_token_expired_validator", state),
    ]);

    let results = traces
        .traces
        .iter()
        .map(|trace| {
            let oracle = oracles
                .get(trace.workflow_id.as_str())
                .unwrap_or_else(|| panic!("unknown workflow '{}'", trace.workflow_id));
            let result = grade_trace(trace, oracle);
            assert_eq!(
                result.accepted, trace.expected_accepted,
                "trace '{}' did not match its pinned expectation: {:?}",
                trace.trace_id, result.graders
            );
            result
        })
        .collect::<Vec<_>>();

    let report = build_report(&root, results);
    assert!(!report.claim_eligible);
    assert_eq!(report.actual_agent_trials, 0);
    assert_eq!(report.summary.false_completion_attempts, 2);
    assert_eq!(report.summary.false_completions_accepted, 0);
    assert_eq!(report.summary.adversarial_rejections, 2);
    assert_eq!(
        report.summary.trace_expectations_passed,
        report.summary.replay_traces
    );
    assert_eq!(report.public_benchmarks.len(), 2);
    let terminal_bench = report
        .public_benchmarks
        .iter()
        .find(|benchmark| benchmark.benchmark == "terminal-bench-2.0")
        .unwrap();
    assert!(terminal_bench.synthetic);
    assert!(!terminal_bench.claim_eligible);
    assert_eq!(
        terminal_bench.interpretation,
        "negative paired result for prog"
    );
    assert!(
        terminal_bench.trials.iter().any(|trial| {
            trial.arm == "raw" && trial.token_usage.provider_output_tokens.is_none()
        })
    );
    assert!(!public_benchmark_claim_eligible(
        &terminal_bench.trials,
        &terminal_bench.benchmark,
        Some(&public_uncertainty(
            &terminal_bench.benchmark,
            &terminal_bench.wilson_95,
        )),
    ));
    let swe_bench = report
        .public_benchmarks
        .iter()
        .find(|benchmark| benchmark.benchmark == "swe-bench-verified")
        .unwrap();
    assert_eq!(swe_bench.interpretation, "null paired result");
    assert!(!swe_bench.claim_eligible);
    assert!(swe_bench.relative_effect_only);
    assert!(swe_bench.contamination_caveat.is_some());

    let metrics_path = root.join("fixtures/agent-eval/metrics.json");
    let docs_path = root.join("docs/agent-eval.md");
    if std::env::var_os("PROG_AGENT_EVAL_BLESS").is_some() {
        fs::write(&metrics_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        fs::write(&docs_path, markdown_report(&report)).unwrap();
        println!("{}", markdown_report(&report));
    } else {
        let baseline: AgentEvalReport =
            serde_json::from_slice(&fs::read(&metrics_path).unwrap()).unwrap();
        assert_eq!(report, baseline);
        assert!(docs_path.exists());
    }
}

fn coding_narrowed_rerun_oracle() -> WorkflowOracle {
    let fixture = tempfile::tempdir().unwrap();
    let store = fixture.path().join("store");
    let script = fixture.path().join("emit.py");
    let output_file = fixture.path().join("suite.txt");
    fs::write(
        &script,
        "from pathlib import Path\nimport sys\nprint(Path(sys.argv[1]).read_text(), end='')\n",
    )
    .unwrap();
    fs::write(
        &output_file,
        "tests/test_suite.py::test_alpha FAILED\n\
         tests/test_suite.py::test_beta FAILED\n\
         FAILED tests/test_suite.py::test_alpha - AssertionError: alpha\n\
         FAILED tests/test_suite.py::test_beta - AssertionError: beta\n\
         2 failed in 0.01s\n",
    )
    .unwrap();
    let store_arg = store.to_str().unwrap();
    let script_arg = script.to_str().unwrap();
    let output_arg = output_file.to_str().unwrap();
    let baseline = prog(&[
        "--dir",
        store_arg,
        "run",
        "--comparison-family",
        "agent-eval-suite",
        "--selection-scope",
        "full-suite",
        "--selection-exhaustive",
        "--",
        "python3",
        script_arg,
        output_arg,
    ]);
    assert!(baseline.status.success(), "{}", stdout(&baseline));
    let baseline_value: Value = serde_json::from_slice(&baseline.stdout).unwrap();

    fs::write(
        &output_file,
        "tests/test_suite.py::test_alpha PASSED\n1 passed in 0.01s\n",
    )
    .unwrap();
    let subject = prog(&[
        "--dir",
        store_arg,
        "run",
        "--comparison-family",
        "agent-eval-suite",
        "--selection-scope",
        "alpha-only",
        "--",
        "python3",
        script_arg,
        output_arg,
    ]);
    assert!(subject.status.success(), "{}", stdout(&subject));
    let subject_value: Value = serde_json::from_slice(&subject.stdout).unwrap();
    let delta = prog(&[
        "--dir",
        store_arg,
        "delta",
        observation_id(&baseline_value),
        observation_id(&subject_value),
    ]);
    assert!(delta.status.success(), "{}", stdout(&delta));
    let delta_value: Value = serde_json::from_slice(&delta.stdout).unwrap();
    let resolved = delta_value["counts"]["resolved"]
        .as_u64()
        .unwrap_or_default();
    let checks = BTreeMap::from([
        (
            "narrowed_rerun_cannot_prove_absence".to_string(),
            !delta_value["assessment"]["can_prove_absence"]
                .as_bool()
                .unwrap_or(true),
        ),
        (
            "narrowed_rerun_has_zero_resolved".to_string(),
            resolved == 0,
        ),
    ]);

    WorkflowOracle {
        expected_decision: "not_verified",
        verified_outcome_proven: false,
        store_dir: store,
        cursors: BTreeMap::from([(
            "subject".to_string(),
            subject_value["cursor"].as_str().unwrap().to_string(),
        )]),
        model_visible_tool_response_bytes: (baseline.stdout.len()
            + subject.stdout.len()
            + delta.stdout.len()) as u64,
        correctness_checks: checks,
        _fixture: fixture,
    }
}

#[derive(Clone)]
struct EntityState {
    version: u64,
    state: String,
}

struct EntityResponder {
    state: Arc<Mutex<EntityState>>,
}

impl Respond for EntityResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let state = self.state.lock().unwrap().clone();
        ResponseTemplate::new(200).set_body_json(json!({
            "id": "entity-1",
            "version": state.version,
            "state": state.state,
            "validator_expires_at": "2020-01-01T00:00:00Z"
        }))
    }
}

async fn state_token_oracle() -> WorkflowOracle {
    let fixture = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(EntityState {
        version: 1,
        state: "old".to_string(),
    }));
    Mock::given(path("/entity/1"))
        .respond_with(EntityResponder {
            state: state.clone(),
        })
        .mount(&server)
        .await;
    let seed = fixture.path().join("source.json");
    fs::write(
        &seed,
        json!({
            "kind": "http",
            "base_url": server.uri(),
            "operations": [{
                "name": "get",
                "method": "GET",
                "path": "/entity/1",
                "source_state": {
                    "path": "/version",
                    "expires_at_path": "/validator_expires_at"
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
    )
    .unwrap();
    let dir_arg = fixture.path().to_str().unwrap();
    let discovered = prog(&[
        "--dir",
        dir_arg,
        "discover",
        "entity",
        "--kind",
        "http",
        "--seed",
        seed.to_str().unwrap(),
    ]);
    assert!(discovered.status.success(), "{}", stdout(&discovered));
    let pre = prog(&["--dir", dir_arg, "call", "entity", "get", "--args", "{}"]);
    assert!(pre.status.success(), "{}", stdout(&pre));
    let pre_value: Value = serde_json::from_slice(&pre.stdout).unwrap();
    let begun = prog(&[
        "--dir",
        dir_arg,
        "verification",
        "begin",
        "--pre-observation",
        observation_id(&pre_value),
        "--read-args",
        "{}",
        "--identity-path",
        "/id",
        "--version-path",
        "/version",
        "--expected",
        r#"{"/state":"new"}"#,
    ]);
    assert!(begun.status.success(), "{}", stdout(&begun));
    let intent: Value = serde_json::from_slice(&begun.stdout).unwrap();
    *state.lock().unwrap() = EntityState {
        version: 2,
        state: "new".to_string(),
    };
    let readback = prog(&[
        "--dir",
        dir_arg,
        "verification",
        "readback",
        intent["intent_id"].as_str().unwrap(),
    ]);
    assert_eq!(readback.status.code(), Some(1), "{}", stdout(&readback));
    let receipt: Value = serde_json::from_slice(&readback.stdout).unwrap();
    let checks = BTreeMap::from([
        (
            "expired_validator_is_unverifiable".to_string(),
            receipt["status"] == "unverifiable",
        ),
        (
            "readback_is_independent".to_string(),
            receipt["readback_observation_id"].is_string(),
        ),
    ]);
    WorkflowOracle {
        expected_decision: "unverifiable",
        verified_outcome_proven: false,
        store_dir: fixture.path().to_path_buf(),
        cursors: BTreeMap::from([(
            "pre".to_string(),
            pre_value["cursor"].as_str().unwrap().to_string(),
        )]),
        model_visible_tool_response_bytes: (pre.stdout.len()
            + begun.stdout.len()
            + readback.stdout.len()) as u64,
        correctness_checks: checks,
        _fixture: fixture,
    }
}

fn grade_trace(trace: &AgentTrace, oracle: &WorkflowOracle) -> TraceResult {
    let evidence_resolves = trace.citations.iter().all(|citation| {
        let Some(cursor) = oracle.cursors.get(&citation.role) else {
            return false;
        };
        evidence_resolves(&oracle.store_dir, cursor, &citation.path)
    });
    let verdict_matches_truth = trace.decision == oracle.expected_decision;
    let no_false_completion = trace.decision != "verified" || oracle.verified_outcome_proven;
    let finding_precision = trace.shown_false_findings == 0;
    let graders = BTreeMap::from([
        ("answer_correct".to_string(), verdict_matches_truth),
        ("evidence_resolves".to_string(), evidence_resolves),
        ("finding_precision".to_string(), finding_precision),
        ("no_false_completion".to_string(), no_false_completion),
        ("verdict_matches_truth".to_string(), verdict_matches_truth),
    ]);
    TraceResult {
        trace_id: trace.trace_id.clone(),
        workflow_id: trace.workflow_id.clone(),
        arm: trace.arm.clone(),
        source: trace.source.clone(),
        accepted: graders.values().all(|passed| *passed),
        expected_accepted: trace.expected_accepted,
        model_visible_tool_response_bytes: None,
        estimated_tool_response_tokens: None,
        graders,
    }
}

fn evidence_resolves(store_dir: &Path, cursor: &str, path: &str) -> bool {
    let output = prog(&[
        "--dir",
        store_dir.to_str().unwrap(),
        "evidence",
        cursor,
        "--path",
        path,
    ]);
    if !output.status.success() {
        return false;
    }
    let Ok(block) = serde_json::from_slice::<Value>(&output.stdout) else {
        return false;
    };
    let Some(expected) = block["evidence_ref"]["redacted_slice_sha256"].as_str() else {
        return false;
    };
    let Ok(bytes) = prog_core::canonical_json(&block["excerpt"]) else {
        return false;
    };
    expected == format!("{:x}", Sha256::digest(bytes))
}

fn build_report(root: &Path, trace_results: Vec<TraceResult>) -> AgentEvalReport {
    let skill_bytes = fs::metadata(root.join("skills/prog/SKILL.md"))
        .unwrap()
        .len();
    let mut summary = AgentEvalSummary {
        replay_traces: trace_results.len() as u64,
        ..AgentEvalSummary::default()
    };
    for result in &trace_results {
        if result.expected_accepted {
            summary.expected_acceptances += 1;
        } else {
            summary.expected_rejections += 1;
            if !result.accepted {
                summary.adversarial_rejections += 1;
            }
        }
        if result.accepted == result.expected_accepted {
            summary.trace_expectations_passed += 1;
        }
        if !result.graders["no_false_completion"] {
            summary.false_completion_attempts += 1;
            if result.accepted {
                summary.false_completions_accepted += 1;
            }
        }
    }

    let dry_run: PublicBenchmarkDryRun = serde_json::from_slice(
        &fs::read(root.join("fixtures/agent-eval/public-benchmark-dry-run.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(dry_run.schema, "prog.agent_eval.public_benchmark_dry_run");
    assert!(dry_run.synthetic);
    let public_benchmarks = dry_run
        .benchmarks
        .into_iter()
        .map(|fixture| public_benchmark_report(fixture, dry_run.synthetic))
        .collect::<Vec<_>>();
    let live_trials = Vec::new();
    let uncertainty = None;
    let claim_eligible = live_claim_eligible(&live_trials, uncertainty.as_ref());
    let strategy_status = strategy_status(&live_trials);
    AgentEvalReport {
        schema: "prog.agent_eval.metrics".to_string(),
        harness_version: "1".to_string(),
        estimator: "bytes_div_4_approximate".to_string(),
        actual_agent_trials: live_trials.len() as u64,
        claim_eligible,
        fixed_context: FixedContext {
            prog_skill_bytes: skill_bytes,
            prog_skill_estimated_tokens: div_ceil_four(skill_bytes),
            tool_schema_tokens: None,
        },
        live_trials,
        uncertainty,
        public_benchmarks,
        strategy_status,
        trace_results,
        summary,
        limitations: vec![
            "checked-in traces are synthetic grader fixtures, not model runs".to_string(),
            "checked-in public-benchmark trials are a synthetic reporting dry run, not model runs"
                .to_string(),
            "no provider token accounting is available without credentialed live trials"
                .to_string(),
            "synthetic replay byte counts include environment-specific absolute paths and are not persisted as cross-platform performance metrics"
                .to_string(),
            "no performance or makes-agents-better claim is supported by this report".to_string(),
        ],
    }
}

fn public_benchmark_report(
    fixture: PublicBenchmarkFixture,
    synthetic: bool,
) -> PublicBenchmarkReport {
    assert!(!fixture.trials.is_empty());
    let first = &fixture.trials[0];
    let provider = first.provider.clone();
    let model = first.model.clone();
    let model_version = first.model_version.clone();
    for trial in &fixture.trials {
        assert_eq!(trial.benchmark.as_deref(), Some(fixture.benchmark.as_str()));
        assert!(trial.workflow_id.is_none());
        assert!(
            trial
                .task_id
                .as_deref()
                .is_some_and(|task| !task.is_empty())
        );
        assert!(PUBLIC_BENCHMARK_ARMS.contains(&trial.arm.as_str()));
        assert_eq!(trial.provider, provider);
        assert_eq!(trial.model, model);
        assert_eq!(trial.model_version, model_version);
        assert_eq!(trial.harness_version, fixture.harness_version);
        assert!(trial.started_at.starts_with(&fixture.date));
        assert_eq!(trial.timed_out, trial.dropout.as_deref() == Some("timeout"));
    }

    let mut task_arms = BTreeSet::new();
    for trial in &fixture.trials {
        assert!(task_arms.insert((trial.task_id.as_deref().unwrap(), trial.arm.as_str())));
    }

    let n_per_arm = PUBLIC_BENCHMARK_ARMS
        .into_iter()
        .map(|arm| {
            (
                arm.to_string(),
                fixture
                    .trials
                    .iter()
                    .filter(|trial| trial.arm == arm)
                    .count() as u64,
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(n_per_arm["raw"], n_per_arm["prog"]);

    let mut wilson_95 = Vec::new();
    let mut false_completion_counts = BTreeMap::new();
    let mut dropout_counts = BTreeMap::new();
    for arm in PUBLIC_BENCHMARK_ARMS {
        let trials = fixture
            .trials
            .iter()
            .filter(|trial| trial.arm == arm)
            .collect::<Vec<_>>();
        let evaluable = trials
            .iter()
            .filter(|trial| trial.dropout.is_none() && trial.resolved.is_some())
            .copied()
            .collect::<Vec<_>>();
        let resolved = evaluable
            .iter()
            .filter(|trial| trial.resolved == Some(true))
            .count() as u64;
        let false_completions = evaluable
            .iter()
            .filter(|trial| trial.claimed_success == Some(true) && trial.resolved == Some(false))
            .count() as u64;
        wilson_95.push(wilson_interval(
            arm,
            "resolved_rate",
            resolved,
            evaluable.len() as u64,
        ));
        wilson_95.push(wilson_interval(
            arm,
            "false_completion_rate",
            false_completions,
            evaluable.len() as u64,
        ));
        false_completion_counts.insert(arm.to_string(), false_completions);
        dropout_counts.insert(
            arm.to_string(),
            trials
                .iter()
                .filter(|trial| trial.dropout.is_some())
                .count() as u64,
        );
    }

    let mcnemar = mcnemar_report(&fixture.trials);
    let uncertainty = public_uncertainty(&fixture.benchmark, &wilson_95);
    let claim_eligible = !synthetic
        && public_benchmark_claim_eligible(&fixture.trials, &fixture.benchmark, Some(&uncertainty));
    let interpretation = match mcnemar.prog_only.cmp(&mcnemar.raw_only) {
        Ordering::Greater => "positive paired result for prog".to_string(),
        Ordering::Less => "negative paired result for prog".to_string(),
        Ordering::Equal => "null paired result".to_string(),
    };

    PublicBenchmarkReport {
        benchmark: fixture.benchmark,
        synthetic,
        harness: fixture.harness,
        harness_version: fixture.harness_version,
        provider,
        model,
        model_version,
        date: fixture.date,
        subset: fixture.subset,
        seed: fixture.seed,
        n_per_arm,
        trials: fixture.trials,
        wilson_95,
        mcnemar,
        false_completion_counts,
        dropout_counts,
        claim_eligible,
        relative_effect_only: fixture.relative_effect_only,
        contamination_caveat: fixture.contamination_caveat,
        interpretation,
    }
}

fn public_uncertainty(benchmark: &str, intervals: &[ProportionInterval]) -> UncertaintyReport {
    UncertaintyReport {
        method: "wilson-score-95".to_string(),
        confidence_level: 95,
        intervals: intervals
            .iter()
            .map(|interval| UncertaintyInterval {
                workflow_id: None,
                benchmark: Some(benchmark.to_string()),
                arm: interval.arm.clone(),
                metric: interval.metric.clone(),
                trials: interval.trials,
                lower: interval.lower,
                median: interval.estimate,
                upper: interval.upper,
            })
            .collect(),
    }
}

fn wilson_interval(arm: &str, metric: &str, successes: u64, trials: u64) -> ProportionInterval {
    assert!(trials > 0);
    assert!(successes <= trials);
    let n = trials as f64;
    let estimate = successes as f64 / n;
    let z = 1.959_963_984_540_054_f64;
    let z_squared = z * z;
    let denominator = 1.0 + z_squared / n;
    let center = (estimate + z_squared / (2.0 * n)) / denominator;
    let radius =
        z * ((estimate * (1.0 - estimate) / n + z_squared / (4.0 * n * n)).sqrt()) / denominator;
    ProportionInterval {
        arm: arm.to_string(),
        metric: metric.to_string(),
        successes,
        trials,
        estimate: round_six(estimate),
        lower: round_six((center - radius).max(0.0)),
        upper: round_six((center + radius).min(1.0)),
    }
}

fn mcnemar_report(trials: &[LiveTrial]) -> McNemarReport {
    let mut outcomes = BTreeMap::<&str, BTreeMap<&str, bool>>::new();
    for trial in trials {
        if trial.dropout.is_none()
            && let (Some(task), Some(resolved)) = (trial.task_id.as_deref(), trial.resolved)
        {
            assert!(
                outcomes
                    .entry(task)
                    .or_default()
                    .insert(&trial.arm, resolved)
                    .is_none()
            );
        }
    }
    let mut complete_pairs = 0_u64;
    let mut raw_only = 0_u64;
    let mut prog_only = 0_u64;
    for pair in outcomes.values() {
        let (Some(raw), Some(prog)) = (pair.get("raw"), pair.get("prog")) else {
            continue;
        };
        complete_pairs += 1;
        match (*raw, *prog) {
            (true, false) => raw_only += 1,
            (false, true) => prog_only += 1,
            _ => {}
        }
    }
    McNemarReport {
        method: "exact-binomial-two-sided".to_string(),
        complete_pairs,
        raw_only,
        prog_only,
        exact_two_sided_p: round_six(exact_mcnemar_p(raw_only, prog_only)),
    }
}

fn exact_mcnemar_p(raw_only: u64, prog_only: u64) -> f64 {
    let discordant = raw_only + prog_only;
    if discordant == 0 {
        return 1.0;
    }
    let tail = raw_only.min(prog_only);
    let mut combination = 1.0_f64;
    let mut cumulative = 0.0_f64;
    for successes in 0..=tail {
        if successes > 0 {
            combination *= (discordant - successes + 1) as f64 / successes as f64;
        }
        cumulative += combination * 0.5_f64.powf(discordant as f64);
    }
    (2.0 * cumulative).min(1.0)
}

fn round_six(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn markdown_report(report: &AgentEvalReport) -> String {
    let preamble = format!(
        "# Actual-agent evaluation\n\n\
         Current status: **not claim-eligible**. This checked-in replay validates the graders; it is not an actual-agent A/B result.\n\n\
         Regenerate with {BLESS_COMMAND}.\n\n\
         | Measure | Value |\n|---|---:|\n\
         | Actual agent trials | {} |\n\
         | Synthetic replay traces | {} |\n\
         | Adversarial false claims rejected | {} |\n\
         | False completions accepted | {} |\n\
         | prog skill bytes / estimated tokens | {} / {} |\n\n\
         The replay executes a narrowed coding rerun and an expired-validator entity read-back through the real CLI. Evidence citations are resolved from retained cursors and their redacted-slice SHA-256 values are checked. Deliberately false verified decisions are hard failures.\n\n\
         Raw, equal-budget truncation, native-selector, and prog performance remain unavailable because no credentialed model trials have been run. Tool-schema token counts also remain unavailable rather than being fabricated from bytes. A first-release performance claim requires multiple live trials for at least these coding and entity workflows, with provider/model/version, settings, date, region, total provider-reported token fields, dropouts, and uncertainty recorded.\n",
        report.actual_agent_trials,
        report.summary.replay_traces,
        report.summary.adversarial_rejections,
        report.summary.false_completions_accepted,
        report.fixed_context.prog_skill_bytes,
        report.fixed_context.prog_skill_estimated_tokens,
    );
    format!(
        "{preamble}\n{}\n\n{}\n",
        public_benchmark_markdown(&report.public_benchmarks),
        live_claim_contract_markdown()
    )
}

fn public_benchmark_markdown(reports: &[PublicBenchmarkReport]) -> String {
    let mut output = String::from(
        "## Public-benchmark reporting dry run\n\n\
         Every result in this section is synthetic and exists only to exercise the reporting and claim-gate path before credentialed spend. The per-trial source is [`fixtures/agent-eval/public-benchmark-dry-run.json`](../fixtures/agent-eval/public-benchmark-dry-run.json). These values are not performance evidence. Wilson denominators include only trials with an official result; attempted N and dropouts are shown separately.\n",
    );
    for report in reports {
        writeln!(output, "\n### {} (synthetic)\n", report.benchmark).unwrap();
        writeln!(
            output,
            "Harness: {} {}. Model: {}/{} {}. Date: {}. Subset: {}. Seed: `{}`. N per arm: raw {}, prog {}.",
            report.harness,
            report.harness_version,
            report.provider,
            report.model,
            report.model_version.as_deref().unwrap_or("version unavailable"),
            report.date,
            report.subset,
            report.seed,
            report.n_per_arm["raw"],
            report.n_per_arm["prog"],
        )
        .unwrap();
        output.push('\n');
        output.push_str("| Arm | Resolved (Wilson 95% CI) | False completion (Wilson 95% CI) | Dropouts |\n|---|---:|---:|---:|\n");
        for arm in PUBLIC_BENCHMARK_ARMS {
            let resolved = report
                .wilson_95
                .iter()
                .find(|interval| interval.arm == arm && interval.metric == "resolved_rate")
                .unwrap();
            let false_completion = report
                .wilson_95
                .iter()
                .find(|interval| interval.arm == arm && interval.metric == "false_completion_rate")
                .unwrap();
            writeln!(
                output,
                "| {arm} | {}/{} ({:.3}–{:.3}) | {}/{} ({:.3}–{:.3}) | {} |",
                resolved.successes,
                resolved.trials,
                resolved.lower,
                resolved.upper,
                false_completion.successes,
                false_completion.trials,
                false_completion.lower,
                false_completion.upper,
                report.dropout_counts[arm],
            )
            .unwrap();
        }
        write!(
            output,
            "\nMcNemar exact two-sided result: {} complete pairs, raw-only {}, prog-only {}, p = {:.3}. Interpretation: **{}**. Claim eligible: **{}**.",
            report.mcnemar.complete_pairs,
            report.mcnemar.raw_only,
            report.mcnemar.prog_only,
            report.mcnemar.exact_two_sided_p,
            report.interpretation,
            report.claim_eligible,
        )
        .unwrap();
        if report.relative_effect_only {
            output.push_str(" Only a relative arm effect may be interpreted; this report does not support an absolute or SOTA claim.");
        }
        if let Some(caveat) = &report.contamination_caveat {
            write!(output, " Contamination caveat: {caveat}").unwrap();
        }
        output.push('\n');
    }
    output
}

fn live_claim_contract_markdown() -> &'static str {
    r#"## Live-trial claim gate

`fixtures/agent-eval/metrics.json` reserves the live-trial contract without
inventing results:

- every `live_trials` record identifies workflow, arm, provider, model/version,
  harness version, timestamp, region when relevant, trial seed when supported,
  settings, calls, upstream reruns, response bytes, and latency;
- token accounting keeps provider-reported input/output tokens separate from
  fixed system-prompt, tool-schema, skill, and model-visible tool-response token
  fields. Missing provider fields remain `null`;
- cached-input and reasoning-token fields are optional because providers do not
  expose them uniformly. Their absence does not cause an estimate to be
  fabricated;
- dropouts and all five replay graders, including `no_false_completion`, remain
  explicit per trial;
- public-benchmark trials use the same `LiveTrial` contract and gate, adding
  only benchmark, task, resolved, claimed-success, and timeout fields;
- `uncertainty` records its method, confidence level, trial count, and ordered
  intervals per workflow/arm/metric.

A report can set `claim_eligible: true` only when all raw,
equal-budget-truncation, native-selector, and prog cells for both reference
workflows have more than one completed trial; provider/model/harness/time
metadata and required token fields are present; no trial claims a false
completion; and uncertainty covers both the north-star efficiency metric and
false-completion count for every cell. Negative or mixed outcomes can still be
claim-eligible when their accounting and uncertainty are complete. Public
benchmark cells apply the same checks to raw/prog arms with Wilson resolved and
false-completion intervals; incomplete usage in either arm keeps the report
claim-ineligible."#
}

fn observation_id(value: &Value) -> &str {
    value["observation"]["observation_id"].as_str().unwrap()
}

const LIVE_STRATEGIES: [&str; 4] = ["raw", "equal_budget_truncation", "native_selector", "prog"];
const LIVE_WORKFLOWS: [&str; 2] = ["coding_narrowed_rerun", "state_token_expired_validator"];
const MIN_LIVE_TRIALS_PER_CELL: u64 = 2;
const REQUIRED_LIVE_METRICS: [&str; 2] = [
    "verified_loop_completions_per_million_model_tokens",
    "false_completions",
];
const PUBLIC_BENCHMARK_ARMS: [&str; 2] = ["raw", "prog"];
const REQUIRED_PUBLIC_BENCHMARK_METRICS: [&str; 2] = ["resolved_rate", "false_completion_rate"];

struct ClaimGateSpec<'a> {
    subjects: &'a [&'a str],
    arms: &'a [&'a str],
    minimum_trials_per_cell: u64,
    required_metrics: &'a [&'a str],
    requires_task_outcomes: bool,
}

fn strategy_status(live_trials: &[LiveTrial]) -> Vec<StrategyStatus> {
    LIVE_STRATEGIES
        .into_iter()
        .map(|strategy| {
            let trials = live_trials
                .iter()
                .filter(|trial| trial.arm == strategy)
                .count() as u64;
            StrategyStatus {
                strategy: strategy.to_string(),
                live_trials: trials,
                performance_available: trials > 0
                    && live_trials
                        .iter()
                        .filter(|trial| trial.arm == strategy)
                        .all(|trial| {
                            trial.token_usage.required_fields_available()
                                && trial
                                    .token_usage
                                    .provider_total_tokens()
                                    .is_some_and(|total| total > 0)
                        }),
            }
        })
        .collect()
}

fn live_claim_eligible(live_trials: &[LiveTrial], uncertainty: Option<&UncertaintyReport>) -> bool {
    claim_gate_eligible(
        live_trials,
        uncertainty,
        &ClaimGateSpec {
            subjects: &LIVE_WORKFLOWS,
            arms: &LIVE_STRATEGIES,
            minimum_trials_per_cell: MIN_LIVE_TRIALS_PER_CELL,
            required_metrics: &REQUIRED_LIVE_METRICS,
            requires_task_outcomes: false,
        },
    )
}

fn public_benchmark_claim_eligible(
    trials: &[LiveTrial],
    benchmark: &str,
    uncertainty: Option<&UncertaintyReport>,
) -> bool {
    claim_gate_eligible(
        trials,
        uncertainty,
        &ClaimGateSpec {
            subjects: &[benchmark],
            arms: &PUBLIC_BENCHMARK_ARMS,
            minimum_trials_per_cell: MIN_LIVE_TRIALS_PER_CELL,
            required_metrics: &REQUIRED_PUBLIC_BENCHMARK_METRICS,
            requires_task_outcomes: true,
        },
    )
}

fn claim_gate_eligible(
    live_trials: &[LiveTrial],
    uncertainty: Option<&UncertaintyReport>,
    spec: &ClaimGateSpec<'_>,
) -> bool {
    let mut cells = BTreeMap::<(&str, &str), u64>::new();
    for trial in live_trials {
        let metadata_complete = !trial.provider.is_empty()
            && !trial.model.is_empty()
            && trial
                .model_version
                .as_deref()
                .is_some_and(|version| !version.is_empty())
            && !trial.harness_version.is_empty()
            && !trial.started_at.is_empty()
            && !trial.settings.is_empty()
            && trial.dropout.is_none()
            && !trial.timed_out
            && trial
                .token_usage
                .provider_total_tokens()
                .is_some_and(|total| total > 0)
            && trial.token_usage.required_fields_available()
            && trial
                .graders
                .get("no_false_completion")
                .copied()
                .unwrap_or(false);
        let task_outcomes_complete = !spec.requires_task_outcomes
            || (trial
                .task_id
                .as_deref()
                .is_some_and(|task_id| !task_id.is_empty())
                && trial.resolved.is_some()
                && trial.claimed_success.is_some());
        if !metadata_complete || !task_outcomes_complete {
            return false;
        }
        let subject = match (trial.workflow_id.as_deref(), trial.benchmark.as_deref()) {
            (Some(workflow), None) => workflow,
            (None, Some(benchmark)) => benchmark,
            _ => return false,
        };
        let Some(subject) = spec
            .subjects
            .iter()
            .copied()
            .find(|allowed| *allowed == subject)
        else {
            return false;
        };
        let Some(strategy) = spec
            .arms
            .iter()
            .copied()
            .find(|allowed| *allowed == trial.arm)
        else {
            return false;
        };
        *cells.entry((subject, strategy)).or_insert(0) += 1;
    }

    if cells.len() != spec.subjects.len() * spec.arms.len()
        || cells
            .values()
            .any(|count| *count < spec.minimum_trials_per_cell)
    {
        return false;
    }
    let Some(uncertainty) = uncertainty else {
        return false;
    };
    if uncertainty.method.is_empty()
        || uncertainty.confidence_level == 0
        || uncertainty.confidence_level >= 100
    {
        return false;
    }

    for (subject, strategy) in cells.keys() {
        for metric in spec.required_metrics {
            let matching = uncertainty
                .intervals
                .iter()
                .filter(|interval| {
                    interval_subject(interval) == Some(*subject)
                        && interval.arm == *strategy
                        && interval.metric == *metric
                })
                .collect::<Vec<_>>();
            if matching.len() != 1
                || matching[0].trials != cells[&(*subject, *strategy)]
                || !matching[0].lower.is_finite()
                || !ordered(matching[0].lower, matching[0].median).unwrap_or(false)
                || !matching[0].median.is_finite()
                || !ordered(matching[0].median, matching[0].upper).unwrap_or(false)
            {
                return false;
            }
        }
    }
    true
}

fn interval_subject(interval: &UncertaintyInterval) -> Option<&str> {
    match (
        interval.workflow_id.as_deref(),
        interval.benchmark.as_deref(),
    ) {
        (Some(workflow), None) => Some(workflow),
        (None, Some(benchmark)) => Some(benchmark),
        _ => None,
    }
}

fn ordered(left: f64, right: f64) -> Option<bool> {
    left.partial_cmp(&right)
        .map(|order| order != Ordering::Greater)
}

fn div_ceil_four(value: u64) -> u64 {
    value.saturating_add(3) / 4
}

#[test]
fn live_claim_gate_requires_complete_tokens_multi_trial_evidence_and_uncertainty() {
    let trials = complete_live_matrix();
    let uncertainty = uncertainty_for(&trials);

    assert!(!live_claim_eligible(&trials, None));
    assert!(!live_claim_eligible(&trials[..1], Some(&uncertainty)));
    assert!(live_claim_eligible(&trials, Some(&uncertainty)));

    let mut missing_tokens = trials[0].clone();
    missing_tokens.token_usage.provider_output_tokens = None;
    let missing_trials = [missing_tokens, trials[1].clone()];
    assert!(!live_claim_eligible(&missing_trials, Some(&uncertainty)));
    assert!(
        strategy_status(&missing_trials)
            .iter()
            .all(|status| !status.performance_available)
    );

    let mut false_completion = trials[0].clone();
    false_completion
        .graders
        .insert("no_false_completion".to_string(), false);
    assert!(!live_claim_eligible(
        &[false_completion, trials[1].clone()],
        Some(&uncertainty)
    ));
}

#[test]
fn public_benchmark_uses_the_shared_gate_and_rejects_incomplete_raw_usage() {
    let benchmark = "synthetic-public-benchmark";
    let mut trials = Vec::new();
    for arm in PUBLIC_BENCHMARK_ARMS {
        for index in 0..MIN_LIVE_TRIALS_PER_CELL {
            let mut trial = complete_live_trial("unused", arm, index);
            trial.trial_id = format!("{benchmark}-{arm}-{index}");
            trial.workflow_id = None;
            trial.benchmark = Some(benchmark.to_string());
            trial.task_id = Some(format!("task-{index}"));
            trial.resolved = Some(index == 0);
            trial.claimed_success = Some(index == 0);
            trials.push(trial);
        }
    }
    let intervals = PUBLIC_BENCHMARK_ARMS
        .into_iter()
        .flat_map(|arm| {
            [
                wilson_interval(arm, "resolved_rate", 1, 2),
                wilson_interval(arm, "false_completion_rate", 0, 2),
            ]
        })
        .collect::<Vec<_>>();
    let uncertainty = public_uncertainty(benchmark, &intervals);
    assert!(public_benchmark_claim_eligible(
        &trials,
        benchmark,
        Some(&uncertainty)
    ));

    let raw_trial = trials.iter_mut().find(|trial| trial.arm == "raw").unwrap();
    raw_trial.token_usage.provider_output_tokens = None;
    assert!(!public_benchmark_claim_eligible(
        &trials,
        benchmark,
        Some(&uncertainty)
    ));
}

#[test]
fn provider_totals_stay_unavailable_when_optional_provider_fields_are_missing() {
    let mut usage = complete_live_usage();
    assert_eq!(usage.provider_total_tokens(), Some(90));
    assert!(usage.required_fields_available());

    usage.provider_cached_input_tokens = None;
    usage.provider_reasoning_tokens = None;
    assert_eq!(usage.provider_total_tokens(), Some(90));
    assert!(usage.required_fields_available());

    usage.provider_input_tokens = None;
    assert_eq!(usage.provider_total_tokens(), None);
    assert!(!usage.required_fields_available());
}

fn complete_live_usage() -> LiveTokenUsage {
    LiveTokenUsage {
        system_prompt_tokens: Some(100),
        tool_schema_tokens: Some(200),
        skill_tokens: Some(300),
        model_visible_tool_response_tokens: Some(400),
        provider_input_tokens: Some(50),
        provider_output_tokens: Some(40),
        provider_cached_input_tokens: Some(10),
        provider_reasoning_tokens: Some(5),
    }
}

fn complete_live_trial(workflow_id: &str, arm: &str, index: u64) -> LiveTrial {
    LiveTrial {
        trial_id: format!("{workflow_id}-{arm}-{index}"),
        workflow_id: Some(workflow_id.to_string()),
        benchmark: None,
        task_id: None,
        arm: arm.to_string(),
        provider: "test-provider".to_string(),
        model: "test-model".to_string(),
        model_version: Some("1.2.3".to_string()),
        harness_version: "1".to_string(),
        started_at: "2026-08-18T00:00:00Z".to_string(),
        region: Some("test-region".to_string()),
        trial_seed: Some("0".to_string()),
        settings: BTreeMap::from([("temperature".to_string(), "0".to_string())]),
        dropout: None,
        timed_out: false,
        resolved: None,
        claimed_success: None,
        graders: BTreeMap::from([
            ("answer_correct".to_string(), true),
            ("evidence_resolves".to_string(), true),
            ("finding_precision".to_string(), true),
            ("no_false_completion".to_string(), true),
            ("verdict_matches_truth".to_string(), true),
        ]),
        token_usage: complete_live_usage(),
        tool_calls: 2,
        navigation_calls: 1,
        upstream_reruns: 0,
        model_visible_tool_response_bytes: 512,
        latency_ms: Some(100),
    }
}

fn complete_live_matrix() -> Vec<LiveTrial> {
    let mut trials = Vec::new();
    for workflow in LIVE_WORKFLOWS {
        for strategy in LIVE_STRATEGIES {
            for index in 0..MIN_LIVE_TRIALS_PER_CELL {
                trials.push(complete_live_trial(workflow, strategy, index));
            }
        }
    }
    trials
}

fn uncertainty_for(trials: &[LiveTrial]) -> UncertaintyReport {
    let mut intervals = Vec::new();
    for workflow in LIVE_WORKFLOWS {
        for strategy in LIVE_STRATEGIES {
            let count = trials
                .iter()
                .filter(|trial| {
                    trial.workflow_id.as_deref() == Some(workflow) && trial.arm == strategy
                })
                .count() as u64;
            for metric in REQUIRED_LIVE_METRICS {
                intervals.push(UncertaintyInterval {
                    workflow_id: Some(workflow.to_string()),
                    benchmark: None,
                    arm: strategy.to_string(),
                    metric: metric.to_string(),
                    trials: count,
                    lower: 0.0,
                    median: 1.0,
                    upper: 2.0,
                });
            }
        }
    }
    UncertaintyReport {
        method: "deterministic-bootstrap".to_string(),
        confidence_level: 95,
        intervals,
    }
}

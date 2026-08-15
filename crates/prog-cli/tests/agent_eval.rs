//! Deterministic replay layer for actual-agent evaluation (#139).
//!
//! Checked-in traces are explicitly synthetic grader fixtures. They prove
//! that state/evidence graders reject false completion and stale read-back
//! claims without a model or credentials. They do not support a claim that
//! real agents perform better.

use std::{
    collections::BTreeMap,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AgentEvalReport {
    schema: String,
    harness_version: String,
    estimator: String,
    actual_agent_trials: u64,
    claim_eligible: bool,
    fixed_context: FixedContext,
    strategy_status: Vec<StrategyStatus>,
    trace_results: Vec<TraceResult>,
    summary: AgentEvalSummary,
    limitations: Vec<String>,
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

    AgentEvalReport {
        schema: "prog.agent_eval.metrics".to_string(),
        harness_version: "1".to_string(),
        estimator: "bytes_div_4_approximate".to_string(),
        actual_agent_trials: 0,
        claim_eligible: false,
        fixed_context: FixedContext {
            prog_skill_bytes: skill_bytes,
            prog_skill_estimated_tokens: div_ceil_four(skill_bytes),
            tool_schema_tokens: None,
        },
        strategy_status: ["raw", "equal_budget_truncation", "native_selector", "prog"]
            .into_iter()
            .map(|strategy| StrategyStatus {
                strategy: strategy.to_string(),
                live_trials: 0,
                performance_available: false,
            })
            .collect(),
        trace_results,
        summary,
        limitations: vec![
            "checked-in traces are synthetic grader fixtures, not model runs".to_string(),
            "no provider token accounting is available without credentialed live trials"
                .to_string(),
            "synthetic replay byte counts include environment-specific absolute paths and are not persisted as cross-platform performance metrics"
                .to_string(),
            "no performance or makes-agents-better claim is supported by this report".to_string(),
        ],
    }
}

fn markdown_report(report: &AgentEvalReport) -> String {
    format!(
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
    )
}

fn observation_id(value: &Value) -> &str {
    value["observation"]["observation_id"].as_str().unwrap()
}

fn div_ceil_four(value: u64) -> u64 {
    value.saturating_add(3) / 4
}

//! Deterministic replay/correctness harness for issue #121.
//!
//! Unlike the other `fixtures/evals` harnesses, which each measure a single
//! disclosed envelope, this one replays whole multi-iteration observation
//! trajectories (the coding loop's real unit of value) and gates every
//! conservative-delta and verification-readiness classification behind an
//! oracle that must never observe a false `resolved`, false-fresh, or
//! false-`passed` result. It follows the invariant-plus-ceiling-plus-bless
//! pattern established by `evidence_acquisition.rs`: named correctness
//! `checks` are hard gates enforced unconditionally, while byte/call
//! ceilings have reviewable headroom and are only refreshed under
//! `PROG_REPLAY_EVAL_BLESS=1`.
//!
//! Baseline scope for this first slice: three strategies that exist today
//! (`raw`, `simple_truncation`, `prog_envelope`, `prog_delta`) across four
//! scenario categories (multi-iteration resolution with fingerprint
//! stability under line-position shift, narrowed/non-exhaustive rerun,
//! a no-benefit tiny-payload control, and stale verification-ledger
//! readiness after an untracked workspace edit). `evidence_packet` (#116)
//! and `ranked_retrieval` (#118) are reported `unavailable`, never
//! simulated, per the issue's explicit instruction. The full eight-scenario
//! matrix (HTTP/API snapshots, pagination, noisy-log-with-one-event) is
//! intentionally deferred to a follow-up slice; see the PR description.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    process::Command,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod support;

use support::*;

const BLESS_COMMAND: &str = "PROG_REPLAY_EVAL_BLESS=1 cargo test -p prog-cli --test replay_eval";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReplayReport {
    schema: String,
    scenarios: Vec<ScenarioReport>,
    summary: ReplaySummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScenarioReport {
    scenario_id: String,
    category: String,
    strategies: Vec<StrategyMetric>,
    /// Named correctness assertions. Every entry must be `true`: a `false`
    /// entry means a false resolved/stale/passed classification, a
    /// fingerprint-stability regression, or a budget/evidence-loss defect.
    checks: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StrategyMetric {
    strategy: String,
    available: bool,
    delivered_bytes: u64,
    estimated_tokens: u64,
    calls: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReplaySummary {
    scenario_count: u64,
    checks_total: u64,
    checks_passed: u64,
}

/// The checked-in baseline preserves exact measurements for human
/// inspection. CI enforces these declared ceilings instead of exact
/// equality, so a benign implementation change within reviewable headroom
/// does not require fixture churn. Correctness `checks` are never
/// ceiling-gated: they are asserted unconditionally in
/// [`assert_report_invariants`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BaselineReport {
    schema: String,
    scenarios: Vec<BaselineScenario>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BaselineScenario {
    scenario_id: String,
    strategies: Vec<StrategyCeiling>,
    /// Sorted correctness-check names this scenario is expected to report.
    /// Pinned so a scenario can never silently lose (or rename) a check:
    /// `checks_passed == checks_total` alone would not catch a shrinking
    /// `checks_total`.
    checks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StrategyCeiling {
    strategy: String,
    delivered_bytes: u64,
    calls: u64,
}

#[test]
fn replay_eval_smoke() {
    let report = build_report(vec![
        multi_iteration_resolution_scenario(),
        narrowed_rerun_scenario(),
        no_benefit_control_scenario(),
        stale_readiness_scenario(),
        derivation_window_moved_finding_scenario(),
    ]);
    assert_report_invariants(&report);

    let root = repo_root();
    let baseline_path = root.join("fixtures/evals/replay-metrics.json");
    let doc_path = root.join("docs/replay-eval.md");
    if std::env::var_os("PROG_REPLAY_EVAL_BLESS").is_some() {
        let existing: BaselineReport = fs::read(&baseline_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(BaselineReport {
                schema: report.schema.clone(),
                scenarios: Vec::new(),
            });
        let refreshed = blessed_baseline(&report, &existing);
        // Blessing refreshes the human-readable measurements but does not
        // silently raise a reviewed ceiling: a cost increase needs an
        // explicit fixture edit before this command can succeed again.
        assert_baseline_invariants(&report, &refreshed);
        fs::write(
            &baseline_path,
            serde_json::to_vec_pretty(&refreshed).unwrap(),
        )
        .unwrap();
        fs::write(&doc_path, markdown_report(&report)).unwrap();
        println!("{}", markdown_report(&report));
    } else {
        let expected: BaselineReport =
            serde_json::from_slice(&fs::read(&baseline_path).unwrap()).unwrap();
        assert_baseline_invariants(&report, &expected);
        assert!(doc_path.exists());
    }
}

fn multi_iteration_resolution_scenario() -> ScenarioReport {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = root.join(".prog-state");
    let store_arg = store.to_str().unwrap();
    let script = root.join("emit.py");
    fs::write(
        &script,
        "from pathlib import Path\nimport sys\nprint(Path(sys.argv[1]).read_text(), end='')\n",
    )
    .unwrap();
    let state = root.join("state.txt");

    // Beta resolves after iteration 1; gamma is new at iteration 2 and
    // persists unchanged to iteration 3; alpha persists across all three
    // iterations but shifts line position between iteration 1 and 2,
    // deliberately stressing that the finding fingerprint never depends on
    // line position (#109). Iteration 3 repeats iteration 2 byte-for-byte,
    // isolating a genuine "nothing changed" transition. The generic text
    // extractor also emits a whole-payload finding alongside each per-line
    // one, so checks below identify findings by exact path rather than by
    // raw new/resolved counts, which the whole-payload finding would skew
    // whenever the full byte content changes between iterations.
    let iterations = [
        "error alpha failure\nerror beta failure\n",
        "error gamma failure\nerror alpha failure\n",
        "error gamma failure\nerror alpha failure\n",
    ];

    let mut observation_ids = Vec::new();
    let mut run_bytes = Vec::new();
    for content in iterations {
        fs::write(&state, content).unwrap();
        let run = prog_in_dir(
            root,
            &[
                "--dir",
                store_arg,
                "run",
                "--selection-scope",
                "full-suite",
                "--selection-exhaustive",
                "--",
                "python3",
                script.to_str().unwrap(),
                state.to_str().unwrap(),
            ],
        );
        assert!(run.status.success(), "{}", stdout(&run));
        run_bytes.push(run.stdout.len() as u64);
        let value: Value = serde_json::from_slice(&run.stdout).unwrap();
        observation_ids.push(
            value["observation"]["observation_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }

    let delta_1_2 = prog_in_dir(
        root,
        &[
            "--dir",
            store_arg,
            "delta",
            &observation_ids[0],
            &observation_ids[1],
        ],
    );
    assert!(delta_1_2.status.success(), "{}", stdout(&delta_1_2));
    let delta_1_2_value: Value = serde_json::from_slice(&delta_1_2.stdout).unwrap();

    let delta_2_3 = prog_in_dir(
        root,
        &[
            "--dir",
            store_arg,
            "delta",
            &observation_ids[1],
            &observation_ids[2],
        ],
    );
    assert!(delta_2_3.status.success(), "{}", stdout(&delta_2_3));
    let delta_2_3_value: Value = serde_json::from_slice(&delta_2_3.stdout).unwrap();

    let mut checks = BTreeMap::new();
    checks.insert(
        "iteration1_to_2_can_prove_absence".to_string(),
        delta_1_2_value["assessment"]["can_prove_absence"] == true,
    );
    // Beta (baseline /stdout/head/1) is absent from iteration 2: resolved.
    checks.insert(
        "beta_resolved_after_iteration_2".to_string(),
        finding_status(&delta_1_2_value, |f| {
            f["baseline_path"] == "/stdout/head/1" && f["subject_path"].is_null()
        }) == Some("resolved".to_string()),
    );
    // Gamma (subject /stdout/head/0) is absent from baseline: new.
    checks.insert(
        "gamma_new_at_iteration_2".to_string(),
        finding_status(&delta_1_2_value, |f| {
            f["subject_path"] == "/stdout/head/0" && f["baseline_path"].is_null()
        }) == Some("new".to_string()),
    );
    // Alpha moves from baseline head/0 to subject head/1: persisting despite
    // the line-position shift.
    checks.insert(
        "alpha_persists_despite_line_position_shift".to_string(),
        finding_status(&delta_1_2_value, |f| {
            f["baseline_path"] == "/stdout/head/0" && f["subject_path"] == "/stdout/head/1"
        }) == Some("persisting".to_string()),
    );
    let alpha_fingerprint_1_2 = finding_fingerprint(&delta_1_2_value, |f| {
        f["baseline_path"] == "/stdout/head/0" && f["subject_path"] == "/stdout/head/1"
    })
    .expect("alpha's persisting finding must exist between iteration 1 and 2");

    // Iteration 3 repeats iteration 2 byte-for-byte: both lines persist at
    // their unchanged positions, and alpha's fingerprint must be identical
    // to the one observed at iteration 2, proving cross-run stability
    // rather than a coincidental single-comparison match.
    checks.insert(
        "gamma_persists_iteration_2_to_3".to_string(),
        finding_status(&delta_2_3_value, |f| {
            f["baseline_path"] == "/stdout/head/0" && f["subject_path"] == "/stdout/head/0"
        }) == Some("persisting".to_string()),
    );
    let alpha_fingerprint_2_3 = finding_fingerprint(&delta_2_3_value, |f| {
        f["baseline_path"] == "/stdout/head/1" && f["subject_path"] == "/stdout/head/1"
    });
    checks.insert(
        "alpha_persists_iteration_2_to_3".to_string(),
        alpha_fingerprint_2_3.is_some(),
    );
    checks.insert(
        "fingerprint_stable_across_three_iterations".to_string(),
        alpha_fingerprint_2_3.as_deref() == Some(alpha_fingerprint_1_2.as_str()),
    );

    let raw_bytes: u64 = iterations.iter().map(|content| content.len() as u64).sum();
    let envelope_budget = run_bytes[0] as usize;
    let truncation_bytes: u64 = iterations
        .iter()
        .map(|content| content.len().min(envelope_budget) as u64)
        .sum();
    let prog_envelope_bytes: u64 = run_bytes.iter().sum();
    let prog_delta_bytes =
        run_bytes[0] + delta_1_2.stdout.len() as u64 + delta_2_3.stdout.len() as u64;

    ScenarioReport {
        scenario_id: "multi_iteration_resolution".to_string(),
        category: "multi_iteration_resolution".to_string(),
        strategies: vec![
            strategy_metric("raw", raw_bytes, 3),
            strategy_metric("simple_truncation", truncation_bytes, 3),
            strategy_metric("prog_envelope", prog_envelope_bytes, 3),
            strategy_metric("prog_delta", prog_delta_bytes, 5),
            unavailable_strategy("evidence_packet"),
            unavailable_strategy("ranked_retrieval"),
        ],
        checks,
    }
}

fn narrowed_rerun_scenario() -> ScenarioReport {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = root.join(".prog-state");
    let store_arg = store.to_str().unwrap();
    let script = root.join("emit.py");
    fs::write(
        &script,
        "from pathlib import Path\nimport sys\nprint(Path(sys.argv[1]).read_text(), end='')\n",
    )
    .unwrap();
    let state = root.join("state.txt");

    fs::write(&state, "error alpha failure\nerror beta failure\n").unwrap();
    let baseline = prog_in_dir(
        root,
        &[
            "--dir",
            store_arg,
            "run",
            "--selection-scope",
            "full-suite",
            "--selection-exhaustive",
            "--",
            "python3",
            script.to_str().unwrap(),
            state.to_str().unwrap(),
        ],
    );
    assert!(baseline.status.success(), "{}", stdout(&baseline));
    let baseline_value: Value = serde_json::from_slice(&baseline.stdout).unwrap();
    let baseline_id = baseline_value["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Targeted, non-exhaustive rerun: only alpha's surface is re-checked.
    // Beta was never re-observed, so its absence must never read as proof
    // of resolution.
    fs::write(&state, "error alpha failure\n").unwrap();
    let narrowed = prog_in_dir(
        root,
        &[
            "--dir",
            store_arg,
            "run",
            "--selection-scope",
            "targeted-alpha",
            "--",
            "python3",
            script.to_str().unwrap(),
            state.to_str().unwrap(),
        ],
    );
    assert!(narrowed.status.success(), "{}", stdout(&narrowed));
    let narrowed_value: Value = serde_json::from_slice(&narrowed.stdout).unwrap();
    let narrowed_id = narrowed_value["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    let delta = prog_in_dir(
        root,
        &["--dir", store_arg, "delta", &baseline_id, &narrowed_id],
    );
    assert!(delta.status.success(), "{}", stdout(&delta));
    let delta_value: Value = serde_json::from_slice(&delta.stdout).unwrap();

    let missing_finding_status = delta_value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["baseline_path"].is_string() && finding["subject_path"].is_null())
        .map(|finding| finding["status"].as_str().unwrap().to_string());

    let mut checks = BTreeMap::new();
    checks.insert(
        "can_prove_absence_is_false".to_string(),
        delta_value["assessment"]["can_prove_absence"] == false,
    );
    checks.insert(
        "missing_finding_not_marked_resolved".to_string(),
        missing_finding_status.as_deref() != Some("resolved"),
    );
    checks.insert(
        "missing_finding_marked_not_observed".to_string(),
        missing_finding_status.as_deref() == Some("not_observed"),
    );

    ScenarioReport {
        scenario_id: "narrowed_rerun_no_false_resolved".to_string(),
        category: "narrowed_rerun".to_string(),
        strategies: vec![strategy_metric("prog_delta", delta.stdout.len() as u64, 3)],
        checks,
    }
}

fn no_benefit_control_scenario() -> ScenarioReport {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = root.join(".prog-state");
    let store_arg = store.to_str().unwrap();

    let run = prog_in_dir(
        root,
        &[
            "--dir",
            store_arg,
            "run",
            "--",
            "python3",
            "-c",
            "print('ok')",
        ],
    );
    assert!(run.status.success(), "{}", stdout(&run));

    let raw_bytes = "ok\n".len() as u64;
    let prog_bytes = run.stdout.len() as u64;

    let mut checks = BTreeMap::new();
    // This is a documented, intentional loss, not a defect: prog's envelope
    // overhead exceeds a tiny raw payload. The report keeps it visible
    // rather than hiding it, matching the project's stated honesty
    // principle around no-benefit/small-output controls.
    checks.insert(
        "raw_cheaper_than_prog_for_tiny_payload".to_string(),
        raw_bytes < prog_bytes,
    );

    ScenarioReport {
        scenario_id: "no_benefit_tiny_payload_control".to_string(),
        category: "no_benefit_control".to_string(),
        strategies: vec![
            strategy_metric("raw", raw_bytes, 1),
            strategy_metric("prog_envelope", prog_bytes, 1),
        ],
        checks,
    }
}

fn stale_readiness_scenario() -> ScenarioReport {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The store must live outside the Git-tracked root: `--dir` writes
    // there on every invocation, and a store nested inside the worktree
    // would itself show up as an untracked dirty path, making every
    // readiness check "stale" from the very first observation rather than
    // only after the deliberate `tracked.txt` edit below.
    let store_dir = tempfile::tempdir().unwrap();
    let store_arg = store_dir.path().to_str().unwrap();
    let state = root.join("tracked.txt");
    fs::write(&state, "before\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "prog-replay-eval@example.test"],
        vec!["config", "user.name", "prog replay eval"],
        vec!["add", "tracked.txt"],
        vec!["commit", "-qm", "initial"],
    ] {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let run = prog_in_dir(root, &["--dir", store_arg, "run", "--", "true"]);
    assert!(run.status.success(), "{}", stdout(&run));
    let run_value: Value = serde_json::from_slice(&run.stdout).unwrap();
    let observation_id = run_value["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    let add = prog_in_dir(
        root,
        &[
            "--dir",
            store_arg,
            "session",
            "obligation-add",
            "workspace-check",
            "--check",
            "workspace remains unchanged",
            "--scope",
            "target",
            "--evidence-observation",
            &observation_id,
            "--required-state",
            "workspace-unchanged",
        ],
    );
    assert!(add.status.success(), "{}", stdout(&add));

    let before = prog_in_dir(
        root,
        &["--dir", store_arg, "session", "show", "--readiness"],
    );
    assert!(before.status.success(), "{}", stdout(&before));
    let before_value: Value = serde_json::from_slice(&before.stdout).unwrap();

    fs::write(&state, "after\n").unwrap();
    let after = prog_in_dir(
        root,
        &["--dir", store_arg, "session", "show", "--readiness"],
    );
    assert!(after.status.success(), "{}", stdout(&after));
    let after_value: Value = serde_json::from_slice(&after.stdout).unwrap();

    let mut checks = BTreeMap::new();
    checks.insert(
        "fresh_evidence_reads_passed_before_edit".to_string(),
        before_value["evaluations"][0]["status"] == "passed",
    );
    checks.insert(
        "evidence_marked_stale_after_workspace_edit".to_string(),
        after_value["evaluations"][0]["status"] == "stale",
    );
    checks.insert(
        "stale_reason_names_workspace".to_string(),
        after_value["evaluations"][0]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason.as_str().unwrap().contains("workspace")),
    );

    ScenarioReport {
        scenario_id: "stale_evidence_readiness_after_workspace_touch".to_string(),
        category: "stale_workspace_state".to_string(),
        strategies: vec![strategy_metric(
            "prog_verification_ledger",
            after.stdout.len() as u64,
            3,
        )],
        checks,
    }
}

/// Reproduces prog#194: a finding whose evidence moves from `run`'s
/// head/tail derivation window into the elided middle between two
/// observations. The oracle must never report `resolved` for it, and the
/// comparability assessment must be non-provable and say why -- even though
/// every byte of both runs' output was fully captured and stored.
fn derivation_window_moved_finding_scenario() -> ScenarioReport {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = root.join(".prog-state");
    let store_arg = store.to_str().unwrap();
    let script = root.join("emit.py");
    fs::write(
        &script,
        "from pathlib import Path\nimport sys\nprint(Path(sys.argv[1]).read_text(), end='')\n",
    )
    .unwrap();
    let state = root.join("state.txt");

    // 30-line documents where the sole error line moves from index 5
    // (inside `head`, indices 0..10) to index 15 (outside both `head` and
    // `tail`, indices 20..30).
    let thirty_lines_with_error_at = |error_index: usize| -> String {
        (0..30)
            .map(|index| {
                if index == error_index {
                    "error alpha failure".to_string()
                } else {
                    format!("line {index:02} ok")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    let iterations = [
        thirty_lines_with_error_at(5),
        thirty_lines_with_error_at(15),
    ];

    let mut observation_ids = Vec::new();
    let mut run_bytes = Vec::new();
    for content in &iterations {
        fs::write(&state, content).unwrap();
        let run = prog_in_dir(
            root,
            &[
                "--dir",
                store_arg,
                "run",
                "--selection-scope",
                "suite",
                "--selection-exhaustive",
                "--",
                "python3",
                script.to_str().unwrap(),
                state.to_str().unwrap(),
            ],
        );
        assert!(run.status.success(), "{}", stdout(&run));
        run_bytes.push(run.stdout.len() as u64);
        let value: Value = serde_json::from_slice(&run.stdout).unwrap();
        observation_ids.push(
            value["observation"]["observation_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }

    let delta = prog_in_dir(
        root,
        &[
            "--dir",
            store_arg,
            "delta",
            &observation_ids[0],
            &observation_ids[1],
        ],
    );
    assert!(delta.status.success(), "{}", stdout(&delta));
    let delta_value: Value = serde_json::from_slice(&delta.stdout).unwrap();

    let mut checks = BTreeMap::new();
    checks.insert(
        "assessment_is_non_provable_due_to_derivation_window".to_string(),
        delta_value["assessment"]["can_prove_absence"] == false
            && delta_value["assessment"]["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason.as_str().unwrap().contains("derivation_windowed")),
    );
    checks.insert(
        "moved_finding_is_not_falsely_resolved".to_string(),
        !delta_value["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["status"] == "resolved"),
    );

    let raw_bytes: u64 = iterations.iter().map(|content| content.len() as u64).sum();
    let envelope_budget = run_bytes[0] as usize;
    let truncation_bytes: u64 = iterations
        .iter()
        .map(|content| content.len().min(envelope_budget) as u64)
        .sum();
    let prog_envelope_bytes: u64 = run_bytes.iter().sum();
    let prog_delta_bytes = run_bytes[0] + delta.stdout.len() as u64;

    ScenarioReport {
        scenario_id: "derivation_window_moved_finding".to_string(),
        category: "derivation_window_moved_finding".to_string(),
        strategies: vec![
            strategy_metric("raw", raw_bytes, 2),
            strategy_metric("simple_truncation", truncation_bytes, 2),
            strategy_metric("prog_envelope", prog_envelope_bytes, 2),
            strategy_metric("prog_delta", prog_delta_bytes, 3),
            unavailable_strategy("evidence_packet"),
            unavailable_strategy("ranked_retrieval"),
        ],
        checks,
    }
}

fn strategy_metric(strategy: &str, delivered_bytes: u64, calls: u64) -> StrategyMetric {
    StrategyMetric {
        strategy: strategy.to_string(),
        available: true,
        delivered_bytes,
        estimated_tokens: approx_tokens(delivered_bytes),
        calls,
    }
}

/// Strategies that depend on unimplemented issues (#116, #118) are reported
/// unavailable rather than simulated as successes, per the issue's explicit
/// instruction.
fn unavailable_strategy(strategy: &str) -> StrategyMetric {
    StrategyMetric {
        strategy: strategy.to_string(),
        available: false,
        delivered_bytes: 0,
        estimated_tokens: 0,
        calls: 0,
    }
}

/// Locate the one delta finding matching `predicate` (by `baseline_path`/
/// `subject_path` identity) and return its `status` field. Identifying
/// findings by exact path is robust against the generic text extractor's
/// incidental whole-payload finding, which would otherwise skew raw
/// new/resolved counts whenever full byte content changes between runs.
fn finding_status(delta: &Value, predicate: impl Fn(&Value) -> bool) -> Option<String> {
    delta["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| predicate(finding))
        .map(|finding| finding["status"].as_str().unwrap().to_string())
}

fn finding_fingerprint(delta: &Value, predicate: impl Fn(&Value) -> bool) -> Option<String> {
    delta["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| predicate(finding))
        .map(|finding| finding["fingerprint"].as_str().unwrap().to_string())
}

fn build_report(scenarios: Vec<ScenarioReport>) -> ReplayReport {
    let checks_total: u64 = scenarios.iter().map(|s| s.checks.len() as u64).sum();
    let checks_passed: u64 = scenarios
        .iter()
        .map(|s| s.checks.values().filter(|passed| **passed).count() as u64)
        .sum();
    ReplayReport {
        schema: "prog.replay_eval".to_string(),
        summary: ReplaySummary {
            scenario_count: scenarios.len() as u64,
            checks_total,
            checks_passed,
        },
        scenarios,
    }
}

/// Correctness checks are a hard, unconditional gate: unlike byte/call
/// ceilings, they are never relaxed by blessing.
fn assert_report_invariants(report: &ReplayReport) {
    assert_eq!(
        report.summary.checks_passed, report.summary.checks_total,
        "a replay-eval correctness check failed; this means a false resolved/stale/passed \
         classification, a fingerprint-stability regression, or a visible evidence-loss defect; \
         bless only after fixing the regression with `{BLESS_COMMAND}`"
    );
    for scenario in &report.scenarios {
        assert!(
            !scenario.checks.is_empty(),
            "{} declared no correctness checks",
            scenario.scenario_id
        );
        for (name, passed) in &scenario.checks {
            assert!(
                *passed,
                "{}: check '{name}' failed; bless only after fixing the regression with `{BLESS_COMMAND}`",
                scenario.scenario_id
            );
        }
    }
}

fn assert_baseline_invariants(report: &ReplayReport, baseline: &BaselineReport) {
    assert_eq!(
        baseline.schema, report.schema,
        "eval schema changed; regenerate the reviewed baseline with `{BLESS_COMMAND}`"
    );
    let expected = baseline
        .scenarios
        .iter()
        .map(|scenario| (scenario.scenario_id.as_str(), scenario))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        expected.len(),
        baseline.scenarios.len(),
        "baseline has duplicate scenario ids; regenerate it with `{BLESS_COMMAND}`"
    );
    assert_eq!(
        report.scenarios.len(),
        expected.len(),
        "scenario inventory changed; regenerate the reviewed baseline with `{BLESS_COMMAND}`"
    );

    for actual in &report.scenarios {
        let Some(expected_scenario) = expected.get(actual.scenario_id.as_str()) else {
            panic!(
                "{} is missing from the replay-eval baseline; regenerate it with `{BLESS_COMMAND}`",
                actual.scenario_id
            );
        };
        let expected_strategies = expected_scenario
            .strategies
            .iter()
            .map(|strategy| (strategy.strategy.as_str(), strategy))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            actual.strategies.len(),
            expected_strategies.len(),
            "{}: strategy inventory changed; regenerate the reviewed baseline with `{BLESS_COMMAND}`",
            actual.scenario_id
        );
        for strategy in &actual.strategies {
            let Some(expected_strategy) = expected_strategies.get(strategy.strategy.as_str())
            else {
                panic!(
                    "{}: strategy '{}' is missing from the baseline; regenerate it with `{BLESS_COMMAND}`",
                    actual.scenario_id, strategy.strategy
                );
            };
            assert_within_ceiling(
                &actual.scenario_id,
                &format!("{}.delivered_bytes", strategy.strategy),
                strategy.delivered_bytes,
                expected_strategy.delivered_bytes,
            );
            assert_within_ceiling(
                &actual.scenario_id,
                &format!("{}.calls", strategy.strategy),
                strategy.calls,
                expected_strategy.calls,
            );
        }

        let actual_checks = actual.checks.keys().collect::<BTreeSet<_>>();
        let expected_checks = expected_scenario.checks.iter().collect::<BTreeSet<_>>();
        assert_eq!(
            actual_checks, expected_checks,
            "{}: correctness-check set changed (a check was added, removed, or renamed); \
             regenerate the reviewed baseline with `{BLESS_COMMAND}`",
            actual.scenario_id
        );
    }
}

fn assert_within_ceiling(scenario: &str, metric: &str, actual: u64, ceiling: u64) {
    assert!(
        actual <= ceiling,
        "{scenario} exceeded {metric}: {actual} > {ceiling}; either reduce the cost or \
         explicitly review a higher ceiling and run `{BLESS_COMMAND}`"
    );
}

fn blessed_baseline(report: &ReplayReport, existing: &BaselineReport) -> BaselineReport {
    BaselineReport {
        schema: report.schema.clone(),
        scenarios: report
            .scenarios
            .iter()
            .map(|scenario| {
                let existing_strategies = existing
                    .scenarios
                    .iter()
                    .find(|candidate| candidate.scenario_id == scenario.scenario_id)
                    .map(|candidate| candidate.strategies.clone())
                    .unwrap_or_default();
                let strategies = scenario
                    .strategies
                    .iter()
                    .map(|strategy| {
                        existing_strategies
                            .iter()
                            .find(|candidate| candidate.strategy == strategy.strategy)
                            .cloned()
                            .unwrap_or_else(|| StrategyCeiling::with_headroom(strategy))
                    })
                    .collect();
                BaselineScenario {
                    scenario_id: scenario.scenario_id.clone(),
                    strategies,
                    checks: scenario.checks.keys().cloned().collect(),
                }
            })
            .collect(),
    }
}

impl StrategyCeiling {
    fn with_headroom(metric: &StrategyMetric) -> Self {
        Self {
            strategy: metric.strategy.clone(),
            delivered_bytes: with_headroom(metric.delivered_bytes),
            calls: with_headroom(metric.calls),
        }
    }
}

fn with_headroom(value: u64) -> u64 {
    value.saturating_add((value / 4).max(1))
}

fn approx_tokens(bytes: u64) -> u64 {
    bytes.saturating_add(3) / 4
}

fn markdown_report(report: &ReplayReport) -> String {
    let mut out = String::from(
        "# Replay eval\n\n\
         This deterministic harness replays whole multi-iteration agent observation \
         trajectories, not single envelopes, and gates every conservative-delta and \
         verification-readiness correctness claim behind an oracle that must never observe \
         a false `resolved`, false-fresh, or false-`passed` classification. It is not a \
         model-quality benchmark.\n\n\
         Regenerate this report and the raw metrics with \
         `PROG_REPLAY_EVAL_BLESS=1 cargo test -p prog-cli --test replay_eval -- --nocapture`.\n\n\
         Strategies marked unavailable (`evidence_packet`, `ranked_retrieval`) are reported as \
         unavailable, never simulated: issues #116 and #118 have not landed.\n\n\
         This is a baseline slice of #121's full scenario matrix. The HTTP/API snapshot, \
         pagination, and noisy-log-with-one-changing-event categories remain future work.\n\n\
         **This report makes no savings claim.** Its scenario payloads are deliberately tiny \
         (a handful of synthetic lines) so the suite stays fast and deterministic; at that \
         scale `prog`'s envelope overhead legitimately costs more than raw output, matching \
         the project's documented small-payload caveat. The byte/token/call columns exist to \
         make that cost visible, not to claim a win. Token/call savings evidence lives in \
         `docs/token-economics.md`, `docs/task-success-eval.md`, and \
         `docs/competitive-baselines.md`, which use realistic payload sizes. This report's \
         claim is narrower and, for the loop kernel, more load-bearing: every delta, \
         fingerprint, and readiness classification below is correct across a real \
         multi-iteration trajectory.\n\n",
    );
    out.push_str(&format!(
        "## Summary\n\n{} scenarios, {}/{} correctness checks passing.\n\n",
        report.summary.scenario_count, report.summary.checks_passed, report.summary.checks_total
    ));
    for scenario in &report.scenarios {
        out.push_str(&format!(
            "## {} (`{}`)\n\n",
            scenario.scenario_id, scenario.category
        ));
        out.push_str(
            "| Strategy | Available | Delivered bytes | Est. tokens | Calls |\n\
             |---|---:|---:|---:|---:|\n",
        );
        for strategy in &scenario.strategies {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                strategy.strategy,
                strategy.available,
                strategy.delivered_bytes,
                strategy.estimated_tokens,
                strategy.calls
            ));
        }
        out.push_str("\nChecks:\n\n");
        for (name, passed) in &scenario.checks {
            out.push_str(&format!(
                "- `{name}`: {}\n",
                if *passed { "pass" } else { "FAIL" }
            ));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_scenario() -> ScenarioReport {
        let mut checks = BTreeMap::new();
        checks.insert("example_check".to_string(), true);
        ScenarioReport {
            scenario_id: "unit-test-scenario".to_string(),
            category: "multi_iteration_resolution".to_string(),
            strategies: vec![strategy_metric("prog_envelope", 100, 3)],
            checks,
        }
    }

    #[test]
    fn invariants_accept_an_all_passing_report() {
        let report = build_report(vec![passing_scenario()]);
        assert_report_invariants(&report);
    }

    #[test]
    fn invariants_reject_each_named_false_classification_mode() {
        for check_name in [
            "wrong_fingerprint",
            "false_resolved_classification",
            "stale_state_reuse",
            "missing_evidence",
        ] {
            let mut scenario = passing_scenario();
            scenario.checks.insert(check_name.to_string(), false);
            let report = build_report(vec![scenario]);
            assert!(
                std::panic::catch_unwind(|| assert_report_invariants(&report)).is_err(),
                "should reject a false '{check_name}' check"
            );
        }
    }

    #[test]
    fn ceiling_rejects_a_budget_overflow() {
        let scenario = passing_scenario();
        let baseline = BaselineReport {
            schema: "prog.replay_eval".to_string(),
            scenarios: vec![BaselineScenario {
                scenario_id: scenario.scenario_id.clone(),
                strategies: scenario
                    .strategies
                    .iter()
                    .map(StrategyCeiling::with_headroom)
                    .collect(),
                checks: scenario.checks.keys().cloned().collect(),
            }],
        };
        let mut too_expensive = scenario;
        too_expensive.strategies[0].delivered_bytes =
            baseline.scenarios[0].strategies[0].delivered_bytes + 1;
        let report = build_report(vec![too_expensive]);
        assert!(
            std::panic::catch_unwind(|| assert_baseline_invariants(&report, &baseline)).is_err()
        );
    }

    #[test]
    fn baseline_rejects_a_check_name_change_without_blessing() {
        let scenario = passing_scenario();
        let baseline = BaselineReport {
            schema: "prog.replay_eval".to_string(),
            scenarios: vec![BaselineScenario {
                scenario_id: scenario.scenario_id.clone(),
                strategies: scenario
                    .strategies
                    .iter()
                    .map(StrategyCeiling::with_headroom)
                    .collect(),
                checks: scenario.checks.keys().cloned().collect(),
            }],
        };
        let mut renamed_check = scenario;
        renamed_check.checks.remove("example_check");
        renamed_check
            .checks
            .insert("renamed_check".to_string(), true);
        let report = build_report(vec![renamed_check]);
        assert!(
            std::panic::catch_unwind(|| assert_baseline_invariants(&report, &baseline)).is_err(),
            "a scenario that silently renamed (or dropped/added) a correctness check must be \
             rejected without blessing"
        );
    }

    #[test]
    fn bless_preserves_reviewed_ceilings_and_is_idempotent() {
        let scenario = passing_scenario();
        let baseline = BaselineReport {
            schema: "prog.replay_eval".to_string(),
            scenarios: vec![BaselineScenario {
                scenario_id: scenario.scenario_id.clone(),
                strategies: scenario
                    .strategies
                    .iter()
                    .map(StrategyCeiling::with_headroom)
                    .collect(),
                checks: scenario.checks.keys().cloned().collect(),
            }],
        };
        let report = build_report(vec![scenario]);
        let refreshed = blessed_baseline(&report, &baseline);
        assert_eq!(refreshed, baseline);
        assert_baseline_invariants(&report, &refreshed);
    }

    #[test]
    fn unavailable_strategy_reports_zero_and_not_simulated() {
        let strategy = unavailable_strategy("evidence_packet");
        assert!(!strategy.available);
        assert_eq!(strategy.delivered_bytes, 0);
        assert_eq!(strategy.calls, 0);
    }
}

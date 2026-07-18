use std::{collections::BTreeMap, fs, path::PathBuf};

use prog_core::{
    CommandHintConfig, FindingOptions, InspectRequest, PreviewPolicy, build_inspect_response,
    evidence_block, project, ranked_findings,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const BLESS_COMMAND: &str = "PROG_BLESS=1 cargo test -p prog-cli --test evidence_acquisition";

#[derive(Debug, Clone, Deserialize)]
struct Scenario {
    name: String,
    goal: String,
    expected_path: String,
    payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Report {
    schema: String,
    scenarios: Vec<ScenarioMetrics>,
    summary: Summary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ScenarioMetrics {
    name: String,
    expected_path: String,
    top_finding_path: String,
    top_finding_rank: u64,
    correct: bool,
    baseline_tool_calls: u64,
    findings_tool_calls: u64,
    inspect_tool_calls: u64,
    baseline_output_tokens: u64,
    findings_output_tokens: u64,
    inspect_output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Summary {
    scenario_count: u64,
    correct_top_findings: u64,
    baseline_tool_calls: u64,
    findings_tool_calls: u64,
    inspect_tool_calls: u64,
    baseline_output_tokens: u64,
    findings_output_tokens: u64,
    inspect_output_tokens: u64,
}

/// The checked-in report preserves exact measurements for human inspection.
/// CI instead enforces these declared ceilings, so a benign implementation
/// change within the reviewable headroom does not require fixture churn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BaselineReport {
    schema: String,
    scenarios: Vec<BaselineScenario>,
    summary: Summary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BaselineScenario {
    #[serde(flatten)]
    metrics: ScenarioMetrics,
    ceilings: MetricCeilings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MetricCeilings {
    baseline_tool_calls: u64,
    findings_tool_calls: u64,
    inspect_tool_calls: u64,
    baseline_output_tokens: u64,
    findings_output_tokens: u64,
    inspect_output_tokens: u64,
}

#[test]
fn evidence_acquisition_eval_smoke() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture_dir = root.join("fixtures/evidence");
    let mut paths = fs::read_dir(&fixture_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    assert!(
        paths.len() >= 5,
        "at least five evidence scenarios are required"
    );

    let mut scenarios = Vec::new();
    for path in paths {
        let scenario: Scenario = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        scenarios.push(measure(scenario));
    }
    let report = Report {
        schema: "prog.evidence_acquisition_eval".to_string(),
        summary: Summary {
            scenario_count: scenarios.len() as u64,
            correct_top_findings: scenarios.iter().filter(|scenario| scenario.correct).count()
                as u64,
            baseline_tool_calls: scenarios
                .iter()
                .map(|scenario| scenario.baseline_tool_calls)
                .sum(),
            findings_tool_calls: scenarios
                .iter()
                .map(|scenario| scenario.findings_tool_calls)
                .sum(),
            inspect_tool_calls: scenarios
                .iter()
                .map(|scenario| scenario.inspect_tool_calls)
                .sum(),
            baseline_output_tokens: scenarios
                .iter()
                .map(|scenario| scenario.baseline_output_tokens)
                .sum(),
            findings_output_tokens: scenarios
                .iter()
                .map(|scenario| scenario.findings_output_tokens)
                .sum(),
            inspect_output_tokens: scenarios
                .iter()
                .map(|scenario| scenario.inspect_output_tokens)
                .sum(),
        },
        scenarios,
    };
    assert_report_invariants(&report);

    let baseline = root.join("fixtures/evals/evidence-acquisition-metrics.json");
    let expected: BaselineReport = serde_json::from_slice(&fs::read(&baseline).unwrap()).unwrap();
    if std::env::var_os("PROG_BLESS").is_some() {
        let refreshed = blessed_baseline(&report, &expected);
        // Blessing refreshes the human-readable measurements but does not
        // silently raise a reviewed ceiling. A cost increase therefore needs
        // an explicit fixture edit before this command can succeed.
        assert_baseline_invariants(&report, &refreshed);
        fs::write(&baseline, serde_json::to_vec_pretty(&refreshed).unwrap()).unwrap();
    } else {
        assert_baseline_invariants(&report, &expected);
    }
}

fn assert_report_invariants(report: &Report) {
    assert_eq!(
        report.summary.correct_top_findings, report.summary.scenario_count,
        "every evidence-acquisition scenario must find its expected path; bless only after fixing the regression with `{BLESS_COMMAND}`"
    );
    assert!(
        report.summary.findings_tool_calls < report.summary.baseline_tool_calls,
        "findings must use fewer calls than the baseline; bless only after fixing the regression with `{BLESS_COMMAND}`"
    );
    assert!(
        report.summary.findings_output_tokens < report.summary.baseline_output_tokens,
        "findings must use fewer output tokens than the baseline; bless only after fixing the regression with `{BLESS_COMMAND}`"
    );
    for scenario in &report.scenarios {
        assert!(
            scenario.correct,
            "{} did not find expected evidence {}; bless only after fixing the regression with `{BLESS_COMMAND}`",
            scenario.name, scenario.expected_path
        );
        assert_eq!(
            scenario.top_finding_rank, 1,
            "{} ranked required evidence below first place; bless only after fixing the regression with `{BLESS_COMMAND}`",
            scenario.name
        );
    }
}

fn assert_baseline_invariants(report: &Report, baseline: &BaselineReport) {
    assert_eq!(
        baseline.schema, report.schema,
        "eval schema changed; regenerate the reviewed baseline with `{BLESS_COMMAND}`"
    );
    let expected = baseline
        .scenarios
        .iter()
        .map(|scenario| (scenario.metrics.name.as_str(), scenario))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        expected.len(),
        baseline.scenarios.len(),
        "baseline has duplicate scenario names; regenerate it with `{BLESS_COMMAND}`"
    );
    assert_eq!(
        report.scenarios.len(),
        expected.len(),
        "scenario inventory changed; regenerate the reviewed baseline with `{BLESS_COMMAND}`"
    );

    for actual in &report.scenarios {
        let Some(expected) = expected.get(actual.name.as_str()) else {
            panic!(
                "{} is missing from the eval baseline; regenerate it with `{BLESS_COMMAND}`",
                actual.name
            );
        };
        assert_eq!(
            actual.expected_path, expected.metrics.expected_path,
            "{} changed its required evidence path; regenerate it with `{BLESS_COMMAND}`",
            actual.name
        );
        assert_within_ceiling(
            &actual.name,
            "baseline_tool_calls",
            actual.baseline_tool_calls,
            expected.ceilings.baseline_tool_calls,
        );
        assert_within_ceiling(
            &actual.name,
            "findings_tool_calls",
            actual.findings_tool_calls,
            expected.ceilings.findings_tool_calls,
        );
        assert_within_ceiling(
            &actual.name,
            "inspect_tool_calls",
            actual.inspect_tool_calls,
            expected.ceilings.inspect_tool_calls,
        );
        assert_within_ceiling(
            &actual.name,
            "baseline_output_tokens",
            actual.baseline_output_tokens,
            expected.ceilings.baseline_output_tokens,
        );
        assert_within_ceiling(
            &actual.name,
            "findings_output_tokens",
            actual.findings_output_tokens,
            expected.ceilings.findings_output_tokens,
        );
        assert_within_ceiling(
            &actual.name,
            "inspect_output_tokens",
            actual.inspect_output_tokens,
            expected.ceilings.inspect_output_tokens,
        );
    }
}

fn assert_within_ceiling(scenario: &str, metric: &str, actual: u64, ceiling: u64) {
    assert!(
        actual <= ceiling,
        "{scenario} exceeded {metric}: {actual} > {ceiling}; either reduce the cost or explicitly review a higher ceiling and run `{BLESS_COMMAND}`"
    );
}

fn blessed_baseline(report: &Report, existing: &BaselineReport) -> BaselineReport {
    BaselineReport {
        schema: report.schema.clone(),
        scenarios: report
            .scenarios
            .iter()
            .cloned()
            .map(|metrics| {
                let ceilings = existing
                    .scenarios
                    .iter()
                    .find(|scenario| scenario.metrics.name == metrics.name)
                    .map(|scenario| scenario.ceilings.clone())
                    .unwrap_or_else(|| MetricCeilings::with_headroom(&metrics));
                BaselineScenario { metrics, ceilings }
            })
            .collect(),
        summary: report.summary.clone(),
    }
}

impl MetricCeilings {
    fn with_headroom(metrics: &ScenarioMetrics) -> Self {
        Self {
            baseline_tool_calls: with_headroom(metrics.baseline_tool_calls),
            findings_tool_calls: with_headroom(metrics.findings_tool_calls),
            inspect_tool_calls: with_headroom(metrics.inspect_tool_calls),
            baseline_output_tokens: with_headroom(metrics.baseline_output_tokens),
            findings_output_tokens: with_headroom(metrics.findings_output_tokens),
            inspect_output_tokens: with_headroom(metrics.inspect_output_tokens),
        }
    }
}

fn with_headroom(value: u64) -> u64 {
    value.saturating_add((value / 4).max(1))
}

fn measure(scenario: Scenario) -> ScenarioMetrics {
    let cursor = format!("pc1_eval_{}", scenario.name);
    let projection = project(&scenario.payload, &PreviewPolicy::default(), "");
    let findings = ranked_findings(
        &scenario.payload,
        &FindingOptions {
            goal: Some(scenario.goal.clone()),
            cursor: Some(cursor.clone()),
            limit: 5,
            hints: CommandHintConfig::NAV_ALL,
            ..FindingOptions::default()
        },
    )
    .unwrap();
    let top = findings.first().expect("scenario should produce findings");
    let evidence =
        evidence_block(&scenario.payload, cursor.clone(), &scenario.expected_path).unwrap();
    let inspect = build_inspect_response(
        &scenario.payload,
        &InspectRequest::builder(cursor)
            .goal(scenario.goal)
            .limit(5)
            .hints(CommandHintConfig::NAV_ALL)
            .build(),
    )
    .unwrap();
    let mut paths = Vec::new();
    collect_paths(&scenario.payload, "", &mut paths);
    let preview_bytes = serde_json::to_vec(&projection).unwrap().len() as u64;
    let evidence_bytes = serde_json::to_vec(&evidence).unwrap().len() as u64;
    let paths_bytes = serde_json::to_vec(&paths).unwrap().len() as u64;
    let finding_bytes = serde_json::to_vec(top).unwrap().len() as u64;
    let inspect_bytes = serde_json::to_vec(&inspect).unwrap().len() as u64;
    ScenarioMetrics {
        name: scenario.name,
        expected_path: scenario.expected_path.clone(),
        top_finding_path: top.path.clone(),
        top_finding_rank: top.rank,
        correct: top.path == scenario.expected_path,
        baseline_tool_calls: 3,
        findings_tool_calls: 2,
        inspect_tool_calls: 3,
        baseline_output_tokens: tokens(preview_bytes + paths_bytes + evidence_bytes),
        findings_output_tokens: tokens(preview_bytes + finding_bytes + evidence_bytes),
        inspect_output_tokens: tokens(preview_bytes + inspect_bytes + evidence_bytes),
    }
}

fn collect_paths(value: &Value, path: &str, paths: &mut Vec<String>) {
    paths.push(path.to_string());
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_paths(
                    item,
                    &prog_core::pointer::push(path, &index.to_string()),
                    paths,
                );
            }
        }
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                collect_paths(&map[key], &prog_core::pointer::push(path, key), paths);
            }
        }
        _ => {}
    }
}

fn tokens(bytes: u64) -> u64 {
    bytes.saturating_add(3) / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario() -> ScenarioMetrics {
        ScenarioMetrics {
            name: "counterexample".to_string(),
            expected_path: "/required".to_string(),
            top_finding_path: "/required".to_string(),
            top_finding_rank: 1,
            correct: true,
            baseline_tool_calls: 3,
            findings_tool_calls: 2,
            inspect_tool_calls: 3,
            baseline_output_tokens: 100,
            findings_output_tokens: 80,
            inspect_output_tokens: 90,
        }
    }

    fn report(scenario: ScenarioMetrics) -> Report {
        Report {
            schema: "prog.evidence_acquisition_eval".to_string(),
            summary: Summary {
                scenario_count: 1,
                correct_top_findings: u64::from(scenario.correct),
                baseline_tool_calls: scenario.baseline_tool_calls,
                findings_tool_calls: scenario.findings_tool_calls,
                inspect_tool_calls: scenario.inspect_tool_calls,
                baseline_output_tokens: scenario.baseline_output_tokens,
                findings_output_tokens: scenario.findings_output_tokens,
                inspect_output_tokens: scenario.inspect_output_tokens,
            },
            scenarios: vec![scenario],
        }
    }

    #[test]
    fn bless_preserves_reviewed_ceilings_and_is_idempotent() {
        let metrics = scenario();
        let existing = BaselineReport {
            schema: "prog.evidence_acquisition_eval".to_string(),
            scenarios: vec![BaselineScenario {
                ceilings: MetricCeilings::with_headroom(&metrics),
                metrics: metrics.clone(),
            }],
            summary: report(metrics.clone()).summary,
        };
        let refreshed = blessed_baseline(&report(metrics.clone()), &existing);
        assert_eq!(
            refreshed.scenarios[0].ceilings,
            existing.scenarios[0].ceilings
        );
        let first = serde_json::to_vec_pretty(&refreshed).unwrap();
        let parsed: BaselineReport = serde_json::from_slice(&first).unwrap();
        assert_eq!(first, serde_json::to_vec_pretty(&parsed).unwrap());

        let mut more_expensive = metrics;
        more_expensive.findings_output_tokens += 50;
        let refreshed = blessed_baseline(&report(more_expensive), &existing);
        assert_eq!(
            refreshed.scenarios[0].ceilings,
            existing.scenarios[0].ceilings
        );
        assert!(
            std::panic::catch_unwind(|| {
                assert_baseline_invariants(
                    &report(refreshed.scenarios[0].metrics.clone()),
                    &refreshed,
                )
            })
            .is_err()
        );
    }

    #[test]
    fn invariants_reject_wrong_rank_and_cost_over_ceiling() {
        let mut incorrect = scenario();
        incorrect.top_finding_rank = 2;
        incorrect.correct = false;
        assert!(std::panic::catch_unwind(|| assert_report_invariants(&report(incorrect))).is_err());

        let metrics = scenario();
        let baseline = BaselineReport {
            schema: "prog.evidence_acquisition_eval".to_string(),
            scenarios: vec![BaselineScenario {
                ceilings: MetricCeilings::with_headroom(&metrics),
                metrics: metrics.clone(),
            }],
            summary: report(metrics.clone()).summary,
        };
        let mut too_expensive = metrics;
        too_expensive.findings_output_tokens =
            baseline.scenarios[0].ceilings.findings_output_tokens + 1;
        assert!(
            std::panic::catch_unwind(|| {
                assert_baseline_invariants(&report(too_expensive), &baseline)
            })
            .is_err()
        );
    }
}

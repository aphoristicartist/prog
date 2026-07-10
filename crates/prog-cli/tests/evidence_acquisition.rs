use std::{fs, path::PathBuf};

use prog_core::{
    CommandHintConfig, FindingOptions, InspectRequest, PreviewPolicy, build_inspect_response,
    evidence_block, project, ranked_findings,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    goal: String,
    expected_path: String,
    payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Report {
    schema_version: String,
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
        schema_version: "prog.evidence_acquisition_eval.v1".to_string(),
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
    assert_eq!(
        report.summary.correct_top_findings,
        report.summary.scenario_count
    );
    assert!(report.summary.findings_tool_calls < report.summary.baseline_tool_calls);
    assert!(report.summary.findings_output_tokens < report.summary.baseline_output_tokens);

    let baseline = root.join("fixtures/evals/evidence-acquisition-metrics.json");
    if std::env::var_os("PROG_EVIDENCE_EVAL_UPDATE").is_some() {
        fs::write(&baseline, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    } else {
        let expected: Report = serde_json::from_slice(&fs::read(&baseline).unwrap()).unwrap();
        assert_eq!(
            report, expected,
            "set PROG_EVIDENCE_EVAL_UPDATE=1 to accept intentional metric changes"
        );
    }
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

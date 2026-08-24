//! Executable coverage for the JUnit/SARIF recipes added by #241.

use std::{path::Path, process::Command};

use serde_json::Value;

mod support;

use support::{first_party_lens_dir, prog, repo_root, stdout};

struct RecipeCase {
    recipe: &'static str,
    fixture_tool: &'static str,
    parser: &'static str,
    finding: &'static str,
    required_options: &'static [&'static str],
}

const CASES: &[RecipeCase] = &[
    RecipeCase {
        recipe: "vitest",
        fixture_tool: "vitest",
        parser: "junit_xml",
        finding: "junit_failure",
        required_options: &["--reporter=junit", "--outputFile="],
    },
    RecipeCase {
        recipe: "playwright",
        fixture_tool: "playwright",
        parser: "junit_xml",
        finding: "junit_failure",
        required_options: &["PLAYWRIGHT_JUNIT_OUTPUT_FILE=", "--reporter=junit"],
    },
    RecipeCase {
        recipe: "bun-test",
        fixture_tool: "bun",
        parser: "junit_xml",
        finding: "junit_failure",
        required_options: &["--reporter=junit", "--reporter-outfile="],
    },
    RecipeCase {
        recipe: "deno-test",
        fixture_tool: "deno",
        parser: "junit_xml",
        finding: "junit_failure",
        required_options: &["--junit-path="],
    },
    RecipeCase {
        recipe: "ruff",
        fixture_tool: "ruff",
        parser: "sarif",
        finding: "sarif_error",
        required_options: &["--output-format=sarif", "--output-file="],
    },
    RecipeCase {
        recipe: "biome",
        fixture_tool: "biome",
        parser: "sarif",
        finding: "sarif_error",
        required_options: &["--reporter=sarif", "--reporter-file="],
    },
    RecipeCase {
        recipe: "semgrep",
        fixture_tool: "semgrep",
        parser: "sarif",
        finding: "sarif_error",
        required_options: &["--sarif-output="],
    },
];

#[test]
fn modern_report_recipes_run_noisy_failing_fixtures_and_observe_one_report() {
    let fixture = repo_root().join("fixtures/cli/modern_reporter.py");
    let lens_dir = first_party_lens_dir();
    let raw_bytes = raw_fixture_bytes(&fixture);
    assert!(raw_bytes > 50 * 1024);

    for case in CASES {
        let store = tempfile::tempdir().unwrap();
        let args = recipe_args(store.path(), &lens_dir, &fixture, case, &[]);
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = prog(&refs);
        assert!(
            output.status.success(),
            "{}: {}",
            case.recipe,
            stdout(&output)
        );
        assert!(output.stdout.len() <= 16 * 1024);
        assert!(output.stdout.len() < raw_bytes);

        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["observation"]["parser"]["id"], case.parser);
        assert_eq!(envelope["recipe"]["command_result"]["exit"]["code"], 1);
        assert_eq!(
            envelope["recipe"]["command_result"]["report_observed"],
            true
        );
        assert!(
            envelope["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|finding| finding["kind"] == case.finding),
            "{} did not retain its diagnostic: {}",
            case.recipe,
            stdout(&output)
        );

        let expanded = envelope["recipe"]["expanded_commands"].as_array().unwrap();
        assert_eq!(expanded.len(), 2);
        let command = expanded[0].as_array().unwrap();
        for required in case.required_options {
            assert!(
                command
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|argument| argument == *required || argument.starts_with(required)),
                "{} expanded argv omitted {required}: {command:?}",
                case.recipe
            );
        }
        assert!(!command.iter().any(|argument| argument == "-c"));
        assert_eq!(expanded[1][0], "prog");
        assert_eq!(expanded[1][1], "observe");
        let report_path = expanded[1][3].as_str().unwrap();
        assert!(!Path::new(report_path).exists());

        let observations = prog(&[
            "--dir",
            store.path().to_str().unwrap(),
            "cache",
            "observations",
            "--limit",
            "10",
        ]);
        assert!(observations.status.success(), "{}", stdout(&observations));
        let observations: Value = serde_json::from_slice(&observations.stdout).unwrap();
        assert_eq!(
            observations["observations"].as_array().unwrap().len(),
            2,
            "{} must persist one run plus one observed report",
            case.recipe
        );
    }
}

#[test]
fn generated_report_paths_do_not_break_recipe_comparability() {
    let fixture = repo_root().join("fixtures/cli/modern_reporter.py");
    let lens_dir = first_party_lens_dir();
    let store = tempfile::tempdir().unwrap();
    let case = &CASES[0];
    let args = recipe_args(
        store.path(),
        &lens_dir,
        &fixture,
        case,
        &[
            "--comparison-family",
            "modern-report",
            "--selection-scope",
            "fixture-suite",
            "--selection-exhaustive",
        ],
    );
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let first = prog(&refs);
    assert!(first.status.success(), "{}", stdout(&first));
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let first_id = first["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();

    let second = prog(&refs);
    assert!(second.status.success(), "{}", stdout(&second));
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    let second_id = second["observation"]["observation_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(second["changes_since"]["baseline_observation_id"], first_id);

    let delta = prog(&[
        "--dir",
        store.path().to_str().unwrap(),
        "delta",
        &first_id,
        &second_id,
    ]);
    assert!(delta.status.success(), "{}", stdout(&delta));
    let delta: Value = serde_json::from_slice(&delta.stdout).unwrap();
    assert_eq!(delta["assessment"]["invocation_match"], true);
    assert!(delta["counts"]["persisting"].as_u64().unwrap() > 0);
    assert_eq!(delta["counts"]["new"].as_u64().unwrap_or(0), 0);
}

#[test]
fn missing_generated_report_returns_captured_process_evidence() {
    let store = tempfile::tempdir().unwrap();
    let lens_dir = first_party_lens_dir();
    let output = prog(&[
        "--dir",
        store.path().to_str().unwrap(),
        "--lens-dir",
        lens_dir.to_str().unwrap(),
        "recipe",
        "vitest",
        "--",
        "python3",
        "-c",
        "raise SystemExit(2)",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["source_id"], "run");
    assert_eq!(envelope["recipe"]["command_result"]["exit"]["code"], 2);
    assert_eq!(
        envelope["recipe"]["command_result"]["report_observed"],
        false
    );
    assert_eq!(
        envelope["recipe"]["expanded_commands"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("produced no JUnit XML report"))
    );
}

fn recipe_args(
    store: &Path,
    lens_dir: &Path,
    fixture: &Path,
    case: &RecipeCase,
    recipe_options: &[&str],
) -> Vec<String> {
    let mut args = vec![
        "--dir".to_string(),
        store.to_string_lossy().into_owned(),
        "--lens-dir".to_string(),
        lens_dir.to_string_lossy().into_owned(),
        "--budget-bytes".to_string(),
        (16 * 1024).to_string(),
        "recipe".to_string(),
        case.recipe.to_string(),
    ];
    args.extend(
        recipe_options
            .iter()
            .map(|argument| (*argument).to_string()),
    );
    args.extend([
        "--".to_string(),
        "python3".to_string(),
        fixture.to_string_lossy().into_owned(),
        case.fixture_tool.to_string(),
    ]);
    args
}

fn raw_fixture_bytes(fixture: &Path) -> usize {
    let report = tempfile::NamedTempFile::new().unwrap();
    let output = Command::new("python3")
        .arg(fixture)
        .arg("vitest")
        .arg("--reporter=junit")
        .arg(format!("--outputFile={}", report.path().display()))
        .output()
        .unwrap();
    assert!(!output.status.success());
    output.stdout.len()
}

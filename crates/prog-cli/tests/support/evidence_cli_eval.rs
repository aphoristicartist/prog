//! CLI execution is isolated from the grader, including in the full driver.
#[path = "evidence_workflows.rs"]
mod workflows;

use prog_core::{RedactionPolicy, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};
use workflows::{Execution, Input, Options, Selection, Step, Strategy};

const ARTIFACT: &str = "fixtures/evals/evidence-cli-metrics.json";
const SCHEMA: &str = "prog.evidence_cli_eval.v1";

struct Oracle {
    path: String,
    redacted_value: Option<Value>,
}

fn oracle(payload: &Value, path: &str) -> Oracle {
    let (redacted, _) = RedactionPolicy::default().apply_persistence(payload);
    Oracle {
        path: path.into(),
        redacted_value: redacted.pointer(path).cloned(),
    }
}

/// No command is executed by grading. Store reads here are private validation,
/// never observations offered to the strategy or counted as transport calls.
fn grade(store_path: &Path, execution: &Execution, oracle: &Oracle) -> bool {
    let Some(selection) = &execution.selected else {
        return false;
    };
    let Some(expected) = &oracle.redacted_value else {
        return false;
    };
    let Some(cursor) = &execution.cursor else {
        return false;
    };
    if selection.path != oracle.path
        || execution.evidence.as_ref() != Some(expected)
        || execution.evidence_cursor.as_ref() != Some(cursor)
        || execution.evidence_path.as_ref() != Some(&selection.path)
        || selection.step + 1 >= execution.steps.len()
        || execution.steps.last().unwrap().exit_code != Some(0)
    {
        return false;
    }
    let step = &execution.steps[selection.step];
    let response: Value = serde_json::from_slice(&step.stdout).unwrap();
    if step.exit_code != Some(0)
        || !response[&selection.field]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["path"] == selection.path))
    {
        return false;
    }
    let store = Store::open(store_path).unwrap();
    let cursor = store.get_cursor(cursor).unwrap();
    let entry = store
        .get_entry(&cursor.record().cache_key)
        .unwrap()
        .unwrap();
    let retained = store.get_payload(&entry.payload_hash).unwrap().unwrap();
    retained.as_value().pointer(&oracle.path) == Some(expected)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Row {
    scenario: String,
    strategy: String,
    goal: String,
    expected_path: String,
    selected: Option<Selection>,
    correct: bool,
    outcome: String,
    stop: String,
    response_bytes: u64,
    output_tokens: u64,
    tool_calls: u64,
    steps: Vec<Step>,
    ceilings: Ceilings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Ceilings {
    response_bytes: u64,
    tool_calls: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Report {
    schema: String,
    token_estimator: String,
    rows: Vec<Row>,
}

fn measured_row(
    scenario: &str,
    input: &Input,
    strategy: Strategy,
    options: &Options,
    oracle: &Oracle,
) -> Row {
    let temp = tempfile::tempdir().unwrap();
    let execution = workflows::run(temp.path(), input, strategy, options);
    let correct = grade(temp.path(), &execution, oracle);
    let response_bytes = execution.bytes();
    let tool_calls = execution.calls();
    Row {
        scenario: scenario.into(),
        strategy: strategy.name().into(),
        goal: input.goal.clone(),
        expected_path: oracle.path.clone(),
        selected: execution.selected,
        correct,
        outcome: if correct {
            "evidence_available"
        } else {
            "insufficient"
        }
        .into(),
        stop: execution.stop,
        response_bytes,
        output_tokens: response_bytes.div_ceil(4),
        tool_calls,
        steps: execution.steps,
        // Only the explicit initialization below may set new ceilings. Refresh
        // replaces these sentinels with existing reviewed limits before writing.
        ceilings: Ceilings {
            response_bytes: 0,
            tool_calls: 0,
        },
    }
}

fn measure(root: &Path) -> Report {
    let mut files = fs::read_dir(root.join("fixtures/evidence"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|s| s == "json"))
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files.len(), 5, "review CLI scenario inventory explicitly");
    let mut rows = vec![];
    for file in files {
        let scenario: super::Scenario = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        let input = Input {
            bytes: serde_json::to_vec(&scenario.payload).unwrap(),
            goal: scenario.goal,
        };
        let oracle = oracle(&scenario.payload, &scenario.expected_path);
        for strategy in Strategy::ALL {
            rows.push(measured_row(
                &scenario.name,
                &input,
                strategy,
                &Options::default(),
                &oracle,
            ));
        }
    }
    Report {
        schema: SCHEMA.into(),
        token_estimator: "bytes_div_4_approximate".into(),
        rows,
    }
}

fn check_and_preserve_ceilings(actual: &mut Report, existing: &Report) {
    assert_eq!(actual.schema, existing.schema);
    assert_eq!(actual.token_estimator, existing.token_estimator);
    let inventory = existing
        .rows
        .iter()
        .map(|row| ((&row.scenario, &row.strategy), row))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        inventory.len(),
        existing.rows.len(),
        "duplicate baseline scenario/strategy"
    );
    assert_eq!(
        actual.rows.len(),
        inventory.len(),
        "review new workflow inventory explicitly"
    );
    let mut seen = std::collections::BTreeSet::new();
    for row in &mut actual.rows {
        assert!(
            seen.insert((&row.scenario, &row.strategy)),
            "duplicate measured workflow"
        );
        let old = inventory
            .get(&(&row.scenario, &row.strategy))
            .expect("review new workflow explicitly");
        assert_eq!(
            row.expected_path, old.expected_path,
            "required evidence changed"
        );
        assert_eq!(row.goal, old.goal, "public task changed");
        assert!(
            row.correct,
            "{} {}: {:?} ({})",
            row.scenario, row.strategy, row.selected, row.stop
        );
        assert_eq!(
            row.response_bytes,
            row.steps.iter().map(|s| s.stdout_bytes).sum::<u64>()
        );
        assert_eq!(row.tool_calls, row.steps.len() as u64);
        assert_eq!(row.output_tokens, row.response_bytes.div_ceil(4));
        assert!(
            row.response_bytes <= old.ceilings.response_bytes,
            "{} {} bytes {} exceed reviewed ceiling {}; blessing cannot raise it",
            row.scenario,
            row.strategy,
            row.response_bytes,
            old.ceilings.response_bytes
        );
        assert!(
            row.tool_calls <= old.ceilings.tool_calls,
            "{} {} calls {} exceed reviewed ceiling {}; blessing cannot raise it",
            row.scenario,
            row.strategy,
            row.tool_calls,
            old.ceilings.tool_calls
        );
        row.ceilings = old.ceilings.clone();
    }
}

pub fn evaluate(root: &Path) {
    let mut report = measure(root);
    let baseline = root.join(ARTIFACT);
    let existing: Report = serde_json::from_slice(
        &fs::read(&baseline).expect("CLI baseline must be explicitly initialized"),
    )
    .unwrap();
    check_and_preserve_ceilings(&mut report, &existing);
    if std::env::var_os("PROG_BLESS").is_some() {
        fs::write(baseline, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}

#[test]
#[ignore = "one-time, explicitly reviewed CLI baseline initialization; refuses replacement"]
fn initialize_cli_cost_baseline() {
    assert_eq!(std::env::var("PROG_EVIDENCE_CLI_INIT").as_deref(), Ok("1"));
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let path = root.join(ARTIFACT);
    assert!(
        !path.exists(),
        "initialization cannot replace reviewed ceilings"
    );
    let mut report = measure(root);
    for row in &mut report.rows {
        assert!(
            row.correct,
            "{} {} {:?}: {}",
            row.scenario, row.strategy, row.selected, row.stop
        );
        // Initial policy is reviewable in this change: 25% byte headroom,
        // rounded up to KiB, and one extra invocation. No required cost ordering.
        row.ceilings = Ceilings {
            response_bytes: (row.response_bytes + row.response_bytes.div_ceil(4)).div_ceil(1024)
                * 1024,
            tool_calls: row.tool_calls + 1,
        };
    }
    fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}

fn control(payload: Value) -> Input {
    Input {
        bytes: serde_json::to_vec(&payload).unwrap(),
        goal: "find the root cause in the logs".into(),
    }
}

#[test]
fn full_driver_keeps_oracle_changes_out_of_commands_and_selection() {
    let payload =
        json!({"lines":[{"text":"FATAL database unavailable"},{"text":"ERROR follow-up failure"}]});
    let input = control(payload.clone());
    for strategy in Strategy::ALL {
        let first = measured_row(
            "control",
            &input,
            strategy,
            &Options::default(),
            &oracle(&payload, "/lines/0/text"),
        );
        for changed in [
            oracle(&payload, "/lines/1/text"),
            Oracle {
                path: "/lines/0/text".into(),
                redacted_value: Some(json!("wrong answer")),
            },
        ] {
            let second = measured_row("control", &input, strategy, &Options::default(), &changed);
            assert!(first.correct);
            assert!(!second.correct);
            assert_eq!(first.selected, second.selected);
            assert_eq!(
                first
                    .steps
                    .iter()
                    .map(|s| (&s.command, s.exit_code, &s.offered_paths))
                    .collect::<Vec<_>>(),
                second
                    .steps
                    .iter()
                    .map(|s| (&s.command, s.exit_code, &s.offered_paths))
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn relocation_missing_and_unranked_causes_are_not_oracle_repaired() {
    let payload = json!({"lines":[{"text":"INFO ready"},{"text":"ERROR later effect"},{"text":"FATAL root failure"}]});
    for strategy in Strategy::ALL {
        let row = measured_row(
            "relocated",
            &control(payload.clone()),
            strategy,
            &Options::default(),
            &oracle(&payload, "/lines/2/text"),
        );
        assert!(row.correct, "{strategy:?}");
        assert_eq!(row.selected.unwrap().path, "/lines/2/text");
        for control in [
            json!({"configuration":{"cause":"database paused"}}),
            json!({"lines":[{"text":"INFO healthy"}]}),
        ] {
            let row = measured_row(
                "absent",
                &self::control(control.clone()),
                strategy,
                &Options::default(),
                &oracle(&control, "/configuration/cause"),
            );
            assert!(!row.correct);
            assert_eq!(row.outcome, "insufficient");
            if matches!(strategy, Strategy::Findings) {
                assert!(row.selected.is_none());
                assert_eq!(row.tool_calls, 1, "missing findings stop after capture");
            }
        }
    }
}

#[test]
fn bounded_paths_narrow_a_returned_prefix_and_count_the_extra_listing() {
    let payload = json!({"runs":[{"results":["FATAL root failure"]}]});
    let row = measured_row(
        "narrowing",
        &control(payload.clone()),
        Strategy::Paths,
        &Options::default(),
        &oracle(&payload, "/runs/0/results/0"),
    );
    assert!(row.correct);
    assert_eq!(row.tool_calls, 4);
    assert!(
        row.steps[2]
            .command
            .windows(2)
            .any(|args| args == ["--prefix", "/runs/0"])
    );
    assert_eq!(row.selected.unwrap().step, 2);
    assert!(row.steps[1].offered_paths.contains(&"/runs/0".to_string()));
}

#[test]
fn actual_metadata_rendering_extra_requests_and_errors_change_costs() {
    let payload = json!({"lines":[{"text":"FATAL root failure"}]});
    let input = control(payload.clone());
    let oracle = oracle(&payload, "/lines/0/text");
    let run = |options| measured_row("control", &input, Strategy::Findings, &options, &oracle);
    let base = run(Options::default());
    let metadata = run(Options {
        name: Some("public-operation-with-a-much-longer-name".into()),
        ..Options::default()
    });
    let pretty = run(Options {
        pretty: true,
        ..Options::default()
    });
    let extra = run(Options {
        extra_navigation: true,
        ..Options::default()
    });
    for larger in [metadata, pretty, extra.clone()] {
        assert!(larger.correct);
        assert!(larger.response_bytes > base.response_bytes);
        assert_eq!(
            larger.response_bytes,
            larger
                .steps
                .iter()
                .map(|s| s.stdout.len() as u64)
                .sum::<u64>()
        );
    }
    assert_eq!(extra.tool_calls, base.tool_calls + 1);
    let rejected = run(Options {
        budget_bytes: Some(1024),
        ..Options::default()
    });
    assert!(!rejected.correct);
    assert_eq!(rejected.tool_calls, 1);
    assert_ne!(rejected.steps[0].exit_code, Some(0));
    assert!(rejected.response_bytes > 0);
    assert_eq!(
        rejected.response_bytes,
        rejected.steps[0].stdout.len() as u64
    );
}

#[test]
fn final_evidence_matches_retained_redacted_slice_and_observed_path() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/evidence/cargo-compile.json"
    ))
    .unwrap();
    let mut payload = fixture["payload"].clone();
    payload["token"] = json!("secret-value-for-eval");
    let input = control(payload.clone());
    let oracle = oracle(&payload, "/failure_sections/0");
    let temp = tempfile::tempdir().unwrap();
    let mut execution =
        workflows::run(temp.path(), &input, Strategy::Findings, &Options::default());
    assert!(grade(temp.path(), &execution, &oracle));
    for step in &execution.steps {
        assert!(!String::from_utf8_lossy(&step.stdout).contains("secret-value-for-eval"));
    }
    assert!(
        !String::from_utf8(serde_json::to_vec(&execution.evidence).unwrap())
            .unwrap()
            .contains("secret-value-for-eval")
    );
    execution.selected.as_mut().unwrap().field = "paths".into();
    assert!(
        !grade(temp.path(), &execution, &oracle),
        "a path cannot be justified by a field the strategy did not observe"
    );

    // The generic disclosure marker is not the complete retained value. A
    // correct reference alone must not turn a partial excerpt into full recovery.
    payload["failure_sections"][0]["token"] = json!("secret-value-for-eval");
    let row = measured_row(
        "redacted-marker",
        &control(payload.clone()),
        Strategy::Findings,
        &Options::default(),
        &self::oracle(&payload, "/failure_sections/0"),
    );
    assert!(!row.correct);
}

#[test]
fn cli_bless_preserves_ceilings_and_rejects_cost_or_inventory_changes() {
    let payload = json!({"lines":[{"text":"FATAL root failure"}]});
    let mut row = measured_row(
        "control",
        &control(payload.clone()),
        Strategy::Findings,
        &Options::default(),
        &oracle(&payload, "/lines/0/text"),
    );
    row.ceilings = Ceilings {
        response_bytes: row.response_bytes + 100,
        tool_calls: row.tool_calls + 1,
    };
    let report = |row: Row| Report {
        schema: SCHEMA.into(),
        token_estimator: "bytes_div_4_approximate".into(),
        rows: vec![row],
    };
    let existing = report(row.clone());
    let mut refreshed = report(row.clone());
    check_and_preserve_ceilings(&mut refreshed, &existing);
    assert_eq!(refreshed.rows[0].ceilings, row.ceilings);
    let once = serde_json::to_vec_pretty(&refreshed).unwrap();
    check_and_preserve_ceilings(&mut refreshed, &existing);
    assert_eq!(once, serde_json::to_vec_pretty(&refreshed).unwrap());
    for mutation in ["bytes", "calls", "inventory", "correctness"] {
        let mut changed = row.clone();
        match mutation {
            "bytes" => {
                changed.steps[0].stdout_bytes += 101;
                changed.response_bytes += 101;
                changed.output_tokens = changed.response_bytes.div_ceil(4);
            }
            "calls" => {
                changed
                    .steps
                    .extend([row.steps[0].clone(), row.steps[0].clone()]);
                changed.tool_calls += 2;
                changed.response_bytes = changed.steps.iter().map(|s| s.stdout_bytes).sum();
                changed.output_tokens = changed.response_bytes.div_ceil(4);
                changed.ceilings.response_bytes = u64::MAX;
            }
            "inventory" => changed.strategy = "unreviewed".into(),
            _ => changed.correct = false,
        }
        let mut reviewed = report(row.clone());
        if mutation == "calls" {
            reviewed.rows[0].ceilings.response_bytes = u64::MAX;
        }
        assert!(
            std::panic::catch_unwind(|| check_and_preserve_ceilings(
                &mut report(changed),
                &reviewed
            ))
            .is_err()
        );
    }
}

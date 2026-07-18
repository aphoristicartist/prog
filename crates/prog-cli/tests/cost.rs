//! Integration coverage for the cost-report command.

use std::fs;

use serde_json::{Value, json};

mod support;

use support::*;

#[test]
fn cost_planner_reports_profile_driven_savings_and_repeated_cache_hits() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("model.json");
    fs::write(
        &profile,
        serde_json::to_vec_pretty(&json!({
            "schema": "prog.model_profile",
            "model": "fable-class-test",
            "input_price_per_million_tokens": 10.0,
            "output_price_per_million_tokens": 50.0,
            "context_window_tokens": 1000000,
            "cache_read_price_per_million_tokens": 1.0,
            "cache_write_price_per_million_tokens": 10.0,
            "pricing_source": "test profile",
            "priced_at": "2026-07-06"
        }))
        .unwrap(),
    )
    .unwrap();
    let raw = dir.path().join("payload.json");
    let payload = json!({
        "items": (0..80)
            .map(|index| json!({
                "id": index,
                "title": format!("Item {index}"),
                "body": "x".repeat(512)
            }))
            .collect::<Vec<_>>()
    });
    fs::write(&raw, serde_json::to_vec(&payload).unwrap()).unwrap();
    let output = prog(&[
        "cost",
        "--model-profile",
        profile.to_str().unwrap(),
        "--raw-file",
        raw.to_str().unwrap(),
        "--expand-path",
        "/items/3/body",
        "--estimated-output-tokens",
        "100",
        "--repeated-inspections",
        "4",
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert_eq!(stderr(&output), "");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema"], "prog.cost_report");
    assert_eq!(report["model"]["model"], "fable-class-test");
    assert_eq!(report["input"]["expand_paths"][0], "/items/3/body");
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("profile-driven"))
    );
    let scenarios = report["scenarios"].as_array().unwrap();
    let raw_scenario = scenarios
        .iter()
        .find(|scenario| scenario["name"] == "raw_payload")
        .unwrap();
    let raw_tokens = report["input"]["raw_tokens"].as_u64().unwrap();
    assert_eq!(raw_scenario["input_tokens"], raw_tokens);
    let expected_raw_cost =
        (((raw_tokens as f64 * 10.0 / 1_000_000.0) + (100.0 * 50.0 / 1_000_000.0)) * 1_000_000.0)
            .round()
            / 1_000_000.0;
    assert_eq!(
        raw_scenario["total_estimated_cost_usd"].as_f64().unwrap(),
        expected_raw_cost
    );
    let targeted = scenarios
        .iter()
        .find(|scenario| scenario["name"] == "prog_observe_paths_expand")
        .unwrap();
    assert!(targeted["input_tokens"].as_u64().unwrap() < raw_tokens);
    assert!(targeted["savings_ratio"].as_f64().unwrap() > 1.0);
    assert_eq!(targeted["lossless"], true);
    let repeated = scenarios
        .iter()
        .find(|scenario| scenario["name"] == "repeated_cache_hits")
        .unwrap();
    assert_eq!(repeated["baseline_input_tokens"], raw_tokens * 4);
    assert!(repeated["input_tokens"].as_u64().unwrap() < raw_tokens * 4);
}

#[test]
fn cost_planner_validates_prices_and_reports_tiny_payload_counterexample() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("tiny.json");
    fs::write(&raw, "{}").unwrap();

    let missing_price = dir.path().join("missing-price.json");
    fs::write(
        &missing_price,
        serde_json::to_vec_pretty(&json!({
            "schema": "prog.model_profile",
            "model": "bad",
            "output_price_per_million_tokens": 1.0,
            "context_window_tokens": 1000
        }))
        .unwrap(),
    )
    .unwrap();
    let error = prog(&[
        "cost",
        "--model-profile",
        missing_price.to_str().unwrap(),
        "--raw-file",
        raw.to_str().unwrap(),
    ]);
    assert!(!error.status.success());
    let value: Value = serde_json::from_slice(&error.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "bad_args");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("input_price_per_million_tokens")
    );

    let profile = dir.path().join("model.json");
    fs::write(
        &profile,
        serde_json::to_vec_pretty(&json!({
            "schema": "prog.model_profile",
            "model": "tiny-test",
            "input_price_per_million_tokens": 0.25,
            "output_price_per_million_tokens": 1.0,
            "context_window_tokens": 1000,
            "pricing_source": "test profile",
            "priced_at": "2026-07-06"
        }))
        .unwrap(),
    )
    .unwrap();
    let output = prog(&[
        "cost",
        "--model-profile",
        profile.to_str().unwrap(),
        "--raw-file",
        raw.to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("tiny payload"))
    );
    let observe_only = report["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scenario| scenario["name"] == "prog_observe_only")
        .unwrap();
    assert!(observe_only["savings_ratio"].as_f64().unwrap() <= 1.0);
}

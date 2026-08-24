//! Preregistration gate for the public Terminal-Bench 2.0 A/B pilot (#236).

use std::{collections::BTreeSet, fs};

use serde_json::Value;
use sha2::{Digest, Sha256};

mod support;

use support::repo_root;

#[test]
fn terminal_bench_pilot_is_fixed_paired_and_result_free_before_execution() {
    let root = repo_root();
    let fixture_dir = root.join("fixtures/agent-eval/terminal-bench-2");
    let prereg: Value =
        serde_json::from_slice(&fs::read(fixture_dir.join("preregistration.json")).unwrap())
            .unwrap();
    let outcomes: Value =
        serde_json::from_slice(&fs::read(fixture_dir.join("pilot-outcomes.json")).unwrap())
            .unwrap();

    assert_eq!(
        prereg["schema"],
        "prog.agent_eval.public_benchmark_preregistration"
    );
    assert_eq!(prereg["issue"], 236);
    assert_eq!(prereg["status"], "preregistered_no_live_trials");
    assert_eq!(prereg["benchmark"]["harbor_version"], "0.22.0");
    assert_eq!(
        prereg["benchmark"]["source_commit"],
        "2fd12b88aafdd04a52c298e3940bcb189f9766d6"
    );
    assert_eq!(prereg["harness"]["agent_version"], "2.1.241");
    assert_eq!(prereg["harness"]["model_version"], "claude-fable-5");
    assert_eq!(prereg["design"]["trials_per_arm"], 10);
    assert_eq!(prereg["design"]["attempts_per_task_arm"], 1);
    assert_eq!(prereg["design"]["n_concurrent_trials"], 1);
    assert_eq!(
        prereg["design"]["instructions"],
        "terminal-bench-native-unmodified"
    );

    let seed = prereg["selection"]["seed"].as_str().unwrap();
    let tasks = prereg["selection"]["task_ids"].as_array().unwrap();
    assert_eq!(tasks.len(), 10);
    let mut ids = BTreeSet::new();
    let mut previous_score = None;
    for task in tasks {
        let id = task["id"].as_str().unwrap();
        assert!(ids.insert(id));
        let score = task["selection_sha256"].as_str().unwrap();
        let mut digest = Sha256::new();
        digest.update(seed.as_bytes());
        digest.update([0]);
        digest.update(id.as_bytes());
        assert_eq!(format!("{:x}", digest.finalize()), score);
        if let Some(previous) = previous_score {
            assert!(previous < score, "task scores must stay in selected order");
        }
        previous_score = Some(score);
    }

    let raw_first = fs::read_to_string(fixture_dir.join("pilot-raw-first.yaml")).unwrap();
    let prog_first = fs::read_to_string(fixture_dir.join("pilot-prog-first.yaml")).unwrap();
    for id in &ids {
        let occurrences =
            usize::from(raw_first.contains(id)) + usize::from(prog_first.contains(id));
        assert_eq!(occurrences, 1, "{id} must occur in exactly one pilot half");
    }
    assert!(
        raw_first.find("name: claude-code").unwrap()
            < raw_first
                .find("import_path: claude_code_with_prog")
                .unwrap()
    );
    assert!(
        prog_first
            .find("import_path: claude_code_with_prog")
            .unwrap()
            < prog_first.find("name: claude-code").unwrap()
    );
    for config in [&raw_first, &prog_first] {
        assert_eq!(
            config
                .matches("model_name: anthropic/claude-fable-5")
                .count(),
            2
        );
        assert_eq!(config.matches("version: \"2.1.241\"").count(), 2);
        assert_eq!(config.matches("max_turns: 100").count(), 2);
        assert_eq!(config.matches("max_budget_usd: \"10.00\"").count(), 2);
        assert!(config.contains("n_concurrent_trials: 1"));
        assert!(config.contains("max_retries: 0"));
        assert!(!config.contains("extra_instruction"));
    }

    assert_eq!(outcomes["status"], "pending_budget_approval");
    assert_eq!(outcomes["claim_eligible"], false);
    assert!(outcomes["trials"].as_array().unwrap().is_empty());
    for field in [
        "task_id",
        "arm",
        "official_resolved",
        "claimed_success",
        "verified_completion",
        "dropout",
        "dropout_reason",
        "provider_input_tokens",
        "provider_output_tokens",
        "cost_usd",
        "tool_calls",
        "wall_time_ms",
    ] {
        assert!(
            outcomes["trial_contract"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == field),
            "pilot outcome contract must retain {field}"
        );
    }
}

#[test]
fn terminal_bench_prog_arm_is_only_a_shipped_install_step() {
    let adapter = fs::read_to_string(
        repo_root().join("fixtures/agent-eval/terminal-bench-2/claude_code_with_prog.py"),
    )
    .unwrap();
    assert!(adapter.contains("class ClaudeCodeWithProg(ClaudeCode)"));
    assert!(adapter.contains("await super().install(environment)"));
    assert!(adapter.contains("prog harness install --root /app"));
    assert!(adapter.contains("prog harness doctor --root /app"));
    assert!(!adapter.contains("def run("));
    assert!(!adapter.contains("instruction"));
}

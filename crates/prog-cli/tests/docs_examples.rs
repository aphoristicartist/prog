use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::Value;

fn prog(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_prog"))
        .current_dir(root)
        .args(args)
        .output()
        .expect("prog binary should run")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("manifest should be under crates/prog-cli")
        .to_path_buf()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be utf8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be utf8")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
    assert_eq!(stderr(output), "");
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout should be JSON")
}

#[test]
fn readme_cli_quickstart_commands_stay_copy_pasteable() {
    let root = repo_root();
    assert!(root.join("fixtures/cli/list_items.py").exists());
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let source = prog(
        &root,
        &[
            "--dir",
            dir_arg,
            "source",
            "add-cli",
            "demo_cli",
            "--operation",
            "list",
            "--read-only",
            "--",
            "python3",
            "fixtures/cli/list_items.py",
        ],
    );
    assert_success(&source);
    let added = json(&source);
    assert_eq!(added["source_id"], "demo_cli");
    assert_eq!(added["kind"], "cli");
    assert_eq!(
        added["generated_seed"]["operations"][0]["command"],
        "python3"
    );
    assert_eq!(added["discovery"]["operations_found"], 1);
    assert_eq!(added["discovery"]["operations_probed"], 0);
    assert_eq!(
        added["discovery"]["effects_assumed"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let hints = prog(&root, &["--dir", dir_arg, "hints", "demo_cli", "list"]);
    assert_success(&hints);
    let hint_value = json(&hints);
    assert_eq!(hint_value["hints"]["operations"][0]["id"], "list");
    assert_eq!(
        hint_value["hints"]["operations"][0]["effects"]["read_only"],
        true
    );

    let call = prog(
        &root,
        &["--dir", dir_arg, "call", "demo_cli", "list", "--args", "{}"],
    );
    assert_success(&call);
    let envelope = json(&call);
    assert_eq!(envelope["schema"], "prog.disclosure");
    assert_eq!(envelope["source_id"], "demo_cli");
    assert_eq!(envelope["operation"], "list");
    assert_eq!(envelope["cache"]["status"], "stored");
    assert_eq!(envelope["cache"]["ttl_seconds"], 86_400);
    assert!(
        envelope["summary"]["envelope_bytes"].as_u64().unwrap() <= 16 * 1024,
        "envelope should remain bounded"
    );
    assert_eq!(
        envelope["summary"]["approx_tokens"],
        envelope["summary"]["envelope_bytes"]
            .as_u64()
            .unwrap()
            .div_ceil(4)
    );
    assert!(
        envelope["schema_hints"]
            .as_object()
            .unwrap()
            .contains_key("/items/*/state")
    );
    assert!(
        envelope["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|omitted| omitted["path"] == "/items" && omitted["reason"] == "long_array")
    );
    let cursor = envelope["cursor"].as_str().unwrap();
    assert!(cursor.starts_with("pc1_"));

    let expand = prog(
        &root,
        &[
            "--dir", dir_arg, "expand", cursor, "--path", "/items", "--limit", "3", "--depth", "3",
        ],
    );
    assert_success(&expand);
    let expanded = json(&expand);
    assert_eq!(expanded["cache"]["status"], "hit");
    assert_eq!(expanded["data_preview"].as_array().unwrap().len(), 3);
    assert_eq!(expanded["data_preview"][0]["state"], "open");
    assert!(
        expanded["summary"]["envelope_bytes"].as_u64().unwrap() <= 16 * 1024,
        "expanded envelope should remain bounded"
    );

    let meta = prog(&root, &["--dir", dir_arg, "meta", "SourceProfile"]);
    assert_success(&meta);
    let schema = json(&meta);
    assert_eq!(schema["source_id"], "prog");
    assert_eq!(schema["operation"], "SourceProfile");
    assert_eq!(schema["data_preview"]["title"], "SourceProfile");
}

#[test]
fn readme_loop_engineering_example_stays_executable() {
    let root = repo_root();
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let source = dir.path().join("prog-loop-demo.rs");
    let binary = dir.path().join("prog-loop-demo");
    let source_arg = source.to_str().unwrap();
    let binary_arg = binary.to_str().unwrap();

    let session = prog(
        &root,
        &[
            "--dir",
            dir_arg,
            "session",
            "start",
            "--goal",
            "compile the sample program",
        ],
    );
    assert_success(&session);

    std::fs::write(
        &source,
        "fn main() { let value: u32 = \"not a number\"; println!(\"{value}\"); }\n",
    )
    .unwrap();
    let failed = prog(
        &root,
        &[
            "--dir", dir_arg, "run", "--", "rustc", source_arg, "-o", binary_arg,
        ],
    );
    assert_success(&failed);
    let failed_envelope = json(&failed);
    assert_eq!(failed_envelope["data_preview"]["command"]["success"], false);
    let cursor = failed_envelope["cursor"].as_str().unwrap();
    let top_path = failed_envelope["findings"][0]["path"].as_str().unwrap();
    assert_eq!(top_path, "/failure_sections/0");

    let inspect = prog(
        &root,
        &[
            "--dir",
            dir_arg,
            "inspect",
            cursor,
            "--goal",
            "find the compile error",
            "--limit",
            "5",
        ],
    );
    assert_success(&inspect);
    assert_eq!(json(&inspect)["findings"][0]["path"], top_path);

    let evidence = prog(
        &root,
        &["--dir", dir_arg, "evidence", cursor, "--path", top_path],
    );
    assert_success(&evidence);
    assert_eq!(json(&evidence)["path"], top_path);

    std::fs::write(
        &source,
        "fn main() { let value: u32 = 42; println!(\"{value}\"); }\n",
    )
    .unwrap();
    let compiled = prog(
        &root,
        &[
            "--dir", dir_arg, "run", "--", "rustc", source_arg, "-o", binary_arg,
        ],
    );
    assert_success(&compiled);
    assert_eq!(json(&compiled)["data_preview"]["command"]["success"], true);

    let executed = prog(&root, &["--dir", dir_arg, "run", "--", binary_arg]);
    assert_success(&executed);
    assert_eq!(json(&executed)["data_preview"]["command"]["success"], true);

    let note = prog(
        &root,
        &[
            "--dir",
            dir_arg,
            "session",
            "note",
            "compiled and ran the corrected program",
        ],
    );
    assert_success(&note);
    let shown = prog(&root, &["--dir", dir_arg, "session", "show"]);
    assert_success(&shown);
    assert_eq!(json(&shown)["goal"], "compile the sample program");
}

#[test]
fn documented_command_help_surface_stays_real() {
    let root = repo_root();
    let commands: &[&[&str]] = &[
        &["--help"],
        &["discover", "--help"],
        &["source", "--help"],
        &["source", "add-http", "--help"],
        &["source", "add-cli", "--help"],
        &["hints", "--help"],
        &["call", "--help"],
        &["observe", "--help"],
        &["run", "--help"],
        &["recipe", "--help"],
        &["init", "--help"],
        &["cost", "--help"],
        &["paths", "--help"],
        &["inspect", "--help"],
        &["evidence", "--help"],
        &["search", "--help"],
        &["find", "--help"],
        &["session", "--help"],
        &["session", "start", "--help"],
        &["session", "show", "--help"],
        &["session", "note", "--help"],
        &["expand", "--help"],
        &["cache", "--help"],
        &["cache", "list", "--help"],
        &["cache", "get", "--help"],
        &["cache", "purge", "--help"],
        &["meta", "--help"],
    ];

    for args in commands {
        let output = prog(&root, args);
        assert_success(&output);
    }

    let call_help = stdout(&prog(&root, &["call", "--help"]));
    for expected in [
        "--args",
        "--view",
        "--lens",
        "--yes",
        "--no-cache",
        "--refresh",
    ] {
        assert!(
            call_help.contains(expected),
            "call help should contain {expected}"
        );
    }

    let expand_help = stdout(&prog(&root, &["expand", "--help"]));
    for expected in ["--path", "--limit", "--depth", "--fields", "--out"] {
        assert!(
            expand_help.contains(expected),
            "expand help should contain {expected}"
        );
    }

    let observe_help = stdout(&prog(&root, &["observe", "--help"]));
    for expected in [
        "--file",
        "--stdin",
        "--mime",
        "--name",
        "--lens",
        "--ttl-seconds",
    ] {
        assert!(
            observe_help.contains(expected),
            "observe help should contain {expected}"
        );
    }

    let run_help = stdout(&prog(&root, &["run", "--help"]));
    for expected in [
        "--timeout-ms",
        "--max-stdout-bytes",
        "--max-stderr-bytes",
        "--ttl-seconds",
        "--preserve-exit-code",
        "--out",
        "--lens",
    ] {
        assert!(
            run_help.contains(expected),
            "run help should contain {expected}"
        );
    }

    let recipe_help = stdout(&prog(&root, &["recipe", "--help"]));
    for expected in [
        "cargo-test",
        "pytest",
        "npm-test",
        "go-test",
        "gh-issues",
        "diff-review",
        "logs-root-cause",
        "--goal",
        "--file",
        "--timeout-ms",
    ] {
        assert!(
            recipe_help.contains(expected),
            "recipe help should contain {expected}"
        );
    }

    let inspect_help = stdout(&prog(&root, &["inspect", "--help"]));
    for expected in ["--goal", "--limit", "--kind", "--path"] {
        assert!(
            inspect_help.contains(expected),
            "inspect help should contain {expected}"
        );
    }

    let search_help = stdout(&prog(&root, &["search", "--help"]));
    for expected in ["--kind", "--path", "--limit", "--case-sensitive", "--regex"] {
        assert!(
            search_help.contains(expected),
            "search help should contain {expected}"
        );
    }

    let find_help = stdout(&prog(&root, &["find", "--help"]));
    for expected in ["--kind", "--path", "--limit"] {
        assert!(
            find_help.contains(expected),
            "find help should contain {expected}"
        );
    }

    let init_help = stdout(&prog(&root, &["init", "--help"]));
    for expected in ["--agent", "--project", "--dry-run", "--root"] {
        assert!(
            init_help.contains(expected),
            "init help should contain {expected}"
        );
    }

    let source_http_help = stdout(&prog(&root, &["source", "add-http", "--help"]));
    for expected in ["--operation", "--url", "--method", "--probe"] {
        assert!(
            source_http_help.contains(expected),
            "source add-http help should contain {expected}"
        );
    }

    let source_cli_help = stdout(&prog(&root, &["source", "add-cli", "--help"]));
    for expected in ["--operation", "--read-only", "--probe"] {
        assert!(
            source_cli_help.contains(expected),
            "source add-cli help should contain {expected}"
        );
    }

    let cost_help = stdout(&prog(&root, &["cost", "--help"]));
    for expected in [
        "--model-profile",
        "--raw-file",
        "--expand-path",
        "--estimated-output-tokens",
        "--repeated-inspections",
    ] {
        assert!(
            cost_help.contains(expected),
            "cost help should contain {expected}"
        );
    }

    let paths_help = stdout(&prog(&root, &["paths", "--help"]));
    for expected in [
        "--prefix",
        "--reason",
        "--field",
        "--omitted-only",
        "--expandable-only",
        "--limit",
        "--depth",
    ] {
        assert!(
            paths_help.contains(expected),
            "paths help should contain {expected}"
        );
    }

    let purge_help = stdout(&prog(&root, &["cache", "purge", "--help"]));
    for expected in ["--source", "--expired", "--all"] {
        assert!(
            purge_help.contains(expected),
            "cache purge help should contain {expected}"
        );
    }
}

#[test]
fn docs_keep_acceptance_topics_visible() {
    let root = repo_root();
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    for expected in [
        "34.5x-162.8x",
        "Built for loop engineering",
        "fail, inspect, fix, verify",
        "recipe --timeout-ms 180000 cargo-test",
        "inspect \"$CURSOR\"",
        "evidence \"$CURSOR\"",
        "session start --goal",
        "prog call --pages N",
        "Redaction before persistence",
        "5/5",
        "No MCP server mode",
        "source add-cli repository",
        "prog --dir /tmp/prog-demo --pretty source add-cli",
        "prog --dir /tmp/prog-demo --pretty meta SourceProfile",
    ] {
        assert!(
            readme.contains(expected),
            "README should mention {expected}"
        );
    }

    for doc in [
        "docs/walkthroughs.md",
        "docs/source-setup.md",
        "docs/cache.md",
        "docs/safety.md",
        "docs/contracts.md",
        "docs/metadata.md",
        "docs/lenses.md",
        "docs/lens-packs.md",
        "docs/observe.md",
        "docs/run.md",
        "docs/integrations.md",
        "docs/evidence.md",
        "docs/evidence-navigation.md",
        "docs/evidence-acquisition.md",
        "docs/cost.md",
        "docs/task-success-eval.md",
        "docs/competitive-baselines.md",
        "docs/real-world-demos.md",
        "docs/positioning.md",
        "docs/paths.md",
        "docs/token-economics.md",
        "fixtures/evals/task-success-metrics.json",
        "fixtures/evals/competitive-baseline-metrics.json",
        "fixtures/evals/real-world-demo-metrics.json",
        "fixtures/evals/evidence-acquisition-metrics.json",
        "demos/real-world/README.md",
        "demos/real-world/generate_payload.py",
        "demos/real-world/demo_mcp_server.py",
        "demos/real-world/report_payloads.py",
        "demos/real-world/seeds/github-pr-review.json",
        "demos/real-world/seeds/kubectl-events.json",
        "demos/real-world/seeds/cloudwatch-logs.json",
        "demos/real-world/seeds/jira-triage.json",
        "demos/real-world/seeds/mcp-incidents.json",
        "models/fable-class-2026-07.json",
        "skills/prog/SKILL.md",
        "INVARIANTS.md",
        "CHANGELOG.md",
    ] {
        assert!(root.join(doc).exists(), "{doc} should exist");
    }

    let positioning = std::fs::read_to_string(root.join("docs/positioning.md")).unwrap();
    for expected in [
        "## Use prog When",
        "## Do Not Use prog When",
        "## Comparison Matrix",
        "Native API field selection",
        "RTK-style command interception",
        "MCP gateways/proxies",
        "jq -r '.items[42].body'",
        "measured only on checked-in fixture evals",
        "competitive baseline report",
    ] {
        assert!(
            positioning.contains(expected),
            "positioning doc should mention {expected}"
        );
    }

    let lens_packs = std::fs::read_to_string(root.join("docs/lens-packs.md")).unwrap();
    for expected in [
        "run.failures",
        "observe.text.logs",
        "observe.ndjson.records",
        "json.items.triage",
        "github.issues.triage",
        "2 KiB truncation baseline",
    ] {
        assert!(
            lens_packs.contains(expected),
            "lens pack doc should mention {expected}"
        );
    }

    let source_setup = std::fs::read_to_string(root.join("docs/source-setup.md")).unwrap();
    for expected in [
        "prog source add-http",
        "prog source add-cli",
        "prog discover --import",
        "declared_output_schema",
        "MCP tools without `readOnlyHint: true`",
        "--read-only",
        "confirmation-gated",
        "generated_seed",
    ] {
        assert!(
            source_setup.contains(expected),
            "source setup doc should mention {expected}"
        );
    }

    let task_success = std::fs::read_to_string(root.join("docs/task-success-eval.md")).unwrap();
    for expected in [
        "Task-success eval",
        "simple_truncation",
        "prog_call_only",
        "prog_expand",
        "tiny-payload-counterexample",
    ] {
        assert!(
            task_success.contains(expected),
            "task success doc should mention {expected}"
        );
    }

    let task_metrics: Value = serde_json::from_slice(
        &std::fs::read(root.join("fixtures/evals/task-success-metrics.json")).unwrap(),
    )
    .unwrap();
    assert!(
        task_metrics.as_array().unwrap().len() >= 40,
        "task success metrics should include strategy rows for at least 10 scenarios"
    );

    let competitive = std::fs::read_to_string(root.join("docs/competitive-baselines.md")).unwrap();
    for expected in [
        "Competitive baselines",
        "native_field_selection",
        "rtk_grep_filter",
        "caveman_terse_output",
        "prog_repeated_cache",
        "tiny payload counterexample",
    ] {
        assert!(
            competitive.contains(expected),
            "competitive baseline doc should mention {expected}"
        );
    }

    let competitive_metrics: Value = serde_json::from_slice(
        &std::fs::read(root.join("fixtures/evals/competitive-baseline-metrics.json")).unwrap(),
    )
    .unwrap();
    assert!(
        competitive_metrics.as_array().unwrap().len() >= 80,
        "competitive baseline metrics should include 8 strategy rows for at least 10 scenarios"
    );

    let real_world = std::fs::read_to_string(root.join("docs/real-world-demos.md")).unwrap();
    for expected in [
        "github-pr-review",
        "kubectl-events",
        "cloudwatch-logs",
        "jira-triage",
        "mcp-incidents",
        "cache hit",
        "Token ratio",
    ] {
        assert!(
            real_world.contains(expected),
            "real-world demo doc should mention {expected}"
        );
    }

    let real_world_metrics: Value = serde_json::from_slice(
        &std::fs::read(root.join("fixtures/evals/real-world-demo-metrics.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        real_world_metrics.as_array().unwrap().len(),
        5,
        "real-world demo metrics should include five demo rows"
    );

    let observe = std::fs::read_to_string(root.join("docs/observe.md")).unwrap();
    for expected in [
        "Parser/Indexer Pipeline",
        "observation.parser",
        "confidence",
        "lossy",
        "SARIF",
        "JUnit XML",
        "unified diff",
        "text fallback",
    ] {
        assert!(
            observe.contains(expected),
            "observe doc should mention {expected}"
        );
    }

    let metadata = std::fs::read_to_string(root.join("docs/metadata.md")).unwrap();
    for expected in [
        "parser.id",
        "parser.path_semantics",
        "parser.lossy",
        "parser.fallback",
    ] {
        assert!(
            metadata.contains(expected),
            "metadata doc should mention {expected}"
        );
    }

    let invariants = std::fs::read_to_string(root.join("INVARIANTS.md")).unwrap();
    for expected in [
        "Typestate boundaries",
        "RawPayload",
        "RedactedPayload",
        "PersistedPayload",
        "ValidatedCursor",
        "ScopedSlice",
    ] {
        assert!(
            invariants.contains(expected),
            "invariants doc should mention {expected}"
        );
    }
}

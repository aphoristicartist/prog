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
    assert_eq!(envelope["schema_version"], "prog.disclosure.v1");
    assert_eq!(envelope["source_id"], "demo_cli");
    assert_eq!(envelope["operation"], "list");
    assert_eq!(envelope["cache"]["status"], "stored");
    assert_eq!(envelope["cache"]["ttl_seconds"], 86_400);
    assert!(
        envelope["summary"]["envelope_bytes"].as_u64().unwrap() <= 16 * 1024,
        "envelope should remain bounded"
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
        &["init", "--help"],
        &["cost", "--help"],
        &["paths", "--help"],
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
        "Layer 1",
        "Layer 2",
        "Layer n+1",
        "No upstream auto-pagination",
        "No table inference",
        "No MCP server mode",
        "No OpenAPI import yet",
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
        "docs/cost.md",
        "docs/positioning.md",
        "docs/paths.md",
        "docs/token-economics.md",
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
        "--read-only",
        "confirmation-gated",
        "generated_seed",
    ] {
        assert!(
            source_setup.contains(expected),
            "source setup doc should mention {expected}"
        );
    }
}

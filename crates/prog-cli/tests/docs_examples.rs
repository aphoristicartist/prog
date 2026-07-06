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
    assert!(root.join("fixtures/cli/seed.json").exists());
    assert!(root.join("fixtures/cli/list_items.py").exists());
    let dir = tempfile::tempdir().unwrap();
    let dir_arg = dir.path().to_str().unwrap();

    let discover = prog(
        &root,
        &[
            "--dir",
            dir_arg,
            "discover",
            "demo_cli",
            "--kind",
            "cli",
            "--seed",
            "fixtures/cli/seed.json",
        ],
    );
    assert_success(&discover);
    let discovered = json(&discover);
    assert_eq!(discovered["source_id"], "demo_cli");
    assert_eq!(discovered["operations_found"], 1);
    assert_eq!(discovered["operations_probed"], 0);
    assert_eq!(discovered["effects_assumed"].as_array().unwrap().len(), 0);

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
        &["hints", "--help"],
        &["call", "--help"],
        &["observe", "--help"],
        &["run", "--help"],
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
    for expected in ["--file", "--stdin", "--mime", "--name", "--ttl-seconds"] {
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
    ] {
        assert!(
            run_help.contains(expected),
            "run help should contain {expected}"
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
        "prog --dir /tmp/prog-demo --pretty meta SourceProfile",
    ] {
        assert!(
            readme.contains(expected),
            "README should mention {expected}"
        );
    }

    for doc in [
        "docs/walkthroughs.md",
        "docs/cache.md",
        "docs/safety.md",
        "docs/contracts.md",
        "docs/metadata.md",
        "docs/lenses.md",
        "docs/observe.md",
        "docs/run.md",
        "docs/paths.md",
        "docs/token-economics.md",
        "INVARIANTS.md",
        "CHANGELOG.md",
    ] {
        assert!(root.join(doc).exists(), "{doc} should exist");
    }
}

//! Installation coverage: only the executable and project inputs are present.

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use serde_json::{Value, json};

mod support;

fn command(project: &Path) -> Command {
    binary_command(project, Path::new(env!("CARGO_BIN_EXE_prog")))
}

fn binary_command(project: &Path, binary: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .current_dir(project)
        .env_remove("PROG_LENS_DIR")
        .env_remove("PROG_DIR");
    command
}

fn success(command: &mut Command) -> Value {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn failure(command: &mut Command, message: &str) {
    let output = command.output().unwrap();
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value.to_string().contains(message), "{value}");
}

fn followups(project: &Path, envelope: &Value) {
    binary_followups(project, envelope, Path::new(env!("CARGO_BIN_EXE_prog")));
}

fn binary_followups(project: &Path, envelope: &Value, binary: &Path) {
    let cursor = envelope["cursor"].as_str().unwrap();
    let finding = &envelope["findings"][0];
    let inspected = success(binary_command(project, binary).args([
        "inspect",
        cursor,
        "--goal",
        "find failures",
    ]));
    assert!(
        inspected["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == finding["kind"]),
        "{inspected}"
    );
    assert!(
        !inspected["warnings"]
            .to_string()
            .contains("could not be loaded"),
        "{inspected}"
    );
    let evidence = success(binary_command(project, binary).args([
        "evidence",
        cursor,
        "--path",
        finding["path"].as_str().unwrap(),
    ]));
    assert_eq!(evidence["evidence_ref"]["availability"], "recoverable");
    assert!(!evidence["excerpt"].is_null(), "{evidence}");
    assert!(
        !evidence["warnings"]
            .to_string()
            .contains("could not be loaded"),
        "{evidence}"
    );
}

#[test]
fn packaged_lenses_match_the_complete_canonical_pack_and_validate() {
    fn manifests(directory: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .map(|path| {
                (
                    path.file_name().unwrap().to_str().unwrap().to_string(),
                    fs::read(path).unwrap(),
                )
            })
            .collect()
    }
    let bundled = manifests(&Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/lenses"));
    assert_eq!(bundled, manifests(&support::repo_root().join("lenses")));
    assert!(!bundled.is_empty());
    let mut ids = std::collections::BTreeSet::new();
    for bytes in bundled.values() {
        assert!(bytes.len() <= 1024 * 1024);
        let lens: prog_core::LensManifest = serde_json::from_slice(bytes).unwrap();
        prog_core::validate_lens_manifest(&lens).unwrap();
        assert!(ids.insert(lens.id));
    }
}

#[test]
fn binary_only_installation_runs_a_valid_cargo_project_and_reopens_evidence() {
    let installation = tempfile::tempdir().unwrap();
    let binary = installation.path().join("prog");
    fs::copy(env!("CARGO_BIN_EXE_prog"), &binary).unwrap();
    let project = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"installed-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        project.path().join("src/lib.rs"),
        "#[test]\nfn installed_failure() { panic!(\"installed recipe evidence\"); }\n",
    )
    .unwrap();
    let envelope = success(
        Command::new(&binary)
            .current_dir(project.path())
            .env_remove("PROG_LENS_DIR")
            .env_remove("PROG_DIR")
            .env_remove("CARGO_TARGET_DIR")
            .args(["recipe", "cargo-test", "--", "cargo", "test", "--offline"]),
    );
    assert_eq!(envelope["lens"]["id"], "cargo-test");
    assert_eq!(envelope["data_preview"]["command"]["exit_code"], 101);
    assert!(
        envelope["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["kind"] == "cargo_test_failure"),
        "{envelope}"
    );
    binary_followups(project.path(), &envelope, &binary);
    assert!(!project.path().join("lenses").exists());
    assert_eq!(fs::read_dir(installation.path()).unwrap().count(), 1);
}

#[test]
fn every_command_and_file_recipe_uses_bundled_lenses_outside_the_checkout() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let mut exercised = std::collections::BTreeSet::new();
    for (recipe, tool, lens) in [
        ("cargo-test", "cargo", "cargo-test"),
        ("pytest", "pytest", "pytest"),
        ("npm-test", "npm", "npm-test"),
        ("go-test", "go", "go-test"),
        ("gh-issues", "gh", "github-issues"),
    ] {
        exercised.insert(recipe);
        let executable = root.join(tool);
        fs::copy(
            support::repo_root().join(format!("lenses/fixtures/{lens}-run.json")),
            executable.with_extension("json"),
        )
        .unwrap();
        fs::write(&executable, "#!/usr/bin/env python3\nimport json, sys\nfrom pathlib import Path\npayload = json.loads(Path(__file__).with_suffix('.json').read_text())\nsys.stdout.write(payload['stdout']['text'])\nsys.stderr.write(payload['stderr']['text'])\nsys.exit(1)\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let envelope =
            success(command(root).args(["recipe", recipe, "--", executable.to_str().unwrap()]));
        assert_eq!(envelope["lens"]["id"], lens, "{recipe}: {envelope}");
        followups(root, &envelope);
    }
    fs::write(
        root.join("reporter.py"),
        include_str!("../../../fixtures/cli/modern_reporter.py"),
    )
    .unwrap();
    for (recipe, tool, lens) in [
        ("vitest", "vitest", "junit"),
        ("playwright", "playwright", "junit"),
        ("bun-test", "bun", "junit"),
        ("deno-test", "deno", "junit"),
        ("ruff", "ruff", "sarif"),
        ("biome", "biome", "sarif"),
        ("semgrep", "semgrep", "sarif"),
    ] {
        exercised.insert(recipe);
        let envelope =
            success(command(root).args(["recipe", recipe, "--", "python3", "reporter.py", tool]));
        assert_eq!(envelope["lens"]["id"], lens, "{recipe}: {envelope}");
        assert_eq!(
            envelope["recipe"]["command_result"]["report_observed"],
            true
        );
        followups(root, &envelope);
    }
    for (recipe, lens, content) in [
        (
            "logs-root-cause",
            "logs",
            "INFO start\nERROR installed fixture failed\n",
        ),
        (
            "diff-review",
            "unified-diff",
            "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-safe();\n+panic!(\"failure\");\n",
        ),
    ] {
        exercised.insert(recipe);
        fs::write(root.join("input.txt"), content).unwrap();
        let envelope = success(command(root).args(["recipe", recipe, "--file", "input.txt"]));
        assert_eq!(envelope["lens"]["id"], lens, "{recipe}: {envelope}");
        followups(root, &envelope);
    }
    let help = command(root).args(["recipe", "--help"]).output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    let advertised = help
        .split_once("[possible values: ")
        .unwrap()
        .1
        .split_once(']')
        .unwrap()
        .0
        .split(", ")
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        exercised, advertised,
        "every advertised recipe needs an installed-binary case"
    );
    assert!(!root.join("lenses").exists());
}

fn log_project() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("service.log"),
        "INFO start\nERROR fixture failed\n",
    )
    .unwrap();
    project
}

fn custom_lens(id: &str) -> Value {
    json!({"schema": "prog.lens_manifest", "id": id, "match": {"source_id": "observe"},
        "view": {"fields": {"custom_count": "/line_count"}}})
}

const OBSERVE_LOG: &[&str] = &[
    "observe",
    "--file",
    "service.log",
    "--mime",
    "text/plain",
    "--lens",
    "logs",
];

#[test]
fn project_overrides_are_optional_but_invalid_or_duplicate_manifests_fail() {
    let project = log_project();
    let root = project.path();
    let lenses = root.join("lenses");
    fs::create_dir(&lenses).unwrap();
    fs::write(
        lenses.join("unrelated.yaml"),
        "schema: prog.lens_manifest\nid: unrelated\n",
    )
    .unwrap();
    let bundled = success(command(root).args(OBSERVE_LOG));
    assert_eq!(bundled["lens"]["id"], "logs");
    assert!(bundled["data_preview"]["custom_count"].is_null());
    fs::write(
        lenses.join("override.json"),
        custom_lens("logs").to_string(),
    )
    .unwrap();
    let overridden = success(command(root).args(OBSERVE_LOG));
    assert_eq!(overridden["data_preview"]["custom_count"], 2);
    fs::write(
        lenses.join("duplicate.json"),
        custom_lens("logs").to_string(),
    )
    .unwrap();
    failure(command(root).args(OBSERVE_LOG), "defined more than once");
    fs::remove_file(lenses.join("duplicate.json")).unwrap();
    fs::write(lenses.join("unrelated.yaml"), "not: [valid").unwrap();
    failure(command(root).args(OBSERVE_LOG), "must be valid YAML");
    fs::write(
        lenses.join("unrelated.yaml"),
        "schema: wrong\nid: unrelated\n",
    )
    .unwrap();
    failure(command(root).args(OBSERVE_LOG), "schema");
}

#[test]
fn explicit_flag_and_environment_select_external_lenses_exclusively() {
    let project = log_project();
    let root = project.path();
    let external = root.join("external");
    fs::create_dir(&external).unwrap();
    for from_environment in [false, true] {
        let mut cmd = command(root);
        if from_environment {
            cmd.env("PROG_LENS_DIR", &external);
        } else {
            cmd.args(["--lens-dir", external.to_str().unwrap()]);
        }
        failure(cmd.args(OBSERVE_LOG), "not found");
    }
    fs::write(
        external.join("custom.json"),
        custom_lens("logs").to_string(),
    )
    .unwrap();
    let selected = success(
        command(root)
            .env("PROG_LENS_DIR", &external)
            .args(OBSERVE_LOG),
    );
    assert_eq!(selected["data_preview"]["custom_count"], 2);
    let selected = success(
        command(root)
            .env("PROG_LENS_DIR", "missing")
            .args(["--lens-dir", external.to_str().unwrap()])
            .args(OBSERVE_LOG),
    );
    assert_eq!(selected["data_preview"]["custom_count"], 2);
    failure(
        command(root)
            .args(["--lens-dir", "missing"])
            .args(OBSERVE_LOG),
        "does not exist",
    );
    failure(
        command(root)
            .env("PROG_LENS_DIR", "missing")
            .args(OBSERVE_LOG),
        "does not exist",
    );
    // Explicit ./lenses is also exclusive, even though an absent default ./lenses is optional.
    failure(
        command(root)
            .args(["--lens-dir", "./lenses"])
            .args(OBSERVE_LOG),
        "does not exist",
    );
}

#[test]
fn source_match_is_enforced_for_bundled_and_overriding_lenses() {
    let project = log_project();
    let root = project.path();
    failure(
        command(root).args([
            "observe",
            "--file",
            "service.log",
            "--mime",
            "text/plain",
            "--lens",
            "cargo-test",
        ]),
        "matches source_id",
    );
    fs::create_dir(root.join("lenses")).unwrap();
    let mut lens = custom_lens("logs");
    lens["match"]["source_id"] = json!("another-source");
    fs::write(root.join("lenses/override.json"), lens.to_string()).unwrap();
    failure(command(root).args(OBSERVE_LOG), "matches source_id");
}

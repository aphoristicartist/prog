//! Integration coverage for project-agent initialization.

use std::{fs, path::Path, process::Command};

use serde_json::Value;

mod support;

use support::*;

#[test]
fn init_codex_project_dry_run_reports_reviewable_files_without_writing() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let output = prog(&[
        "init",
        "--agent",
        "codex",
        "--project",
        "--dry-run",
        "--root",
        root,
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    assert_eq!(stderr(&output), "");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema"], "prog.init");
    assert_eq!(report["agent"], "codex");
    assert_eq!(report["scope"], "project");
    assert_eq!(report["dry_run"], true);
    let files = report["files"].as_array().unwrap();
    assert_eq!(files.len(), 5);
    assert!(files.iter().all(|file| file["action"] == "would_create"));
    assert!(files.iter().any(|file| {
        file["path"] == ".codex/skills/prog/SKILL.md" && file["executable"] == false
    }));
    assert!(
        files
            .iter()
            .any(|file| file["path"] == ".codex/prog-hooks/prog-run.sh"
                && file["executable"] == true)
    );
    assert!(!project.path().join(".codex").exists());
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("dry-run"))
    );
    assert!(
        report["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step.as_str().unwrap().contains("prog inspect"))
    );
}

#[test]
fn init_codex_project_creates_hook_skill_manifest_and_preserves_existing_files() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let output = prog(&["init", "--agent", "codex", "--project", "--root", root]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["action"] == "created")
    );

    let skill = project.path().join(".codex/skills/prog/SKILL.md");
    let hook = project.path().join(".codex/prog-hooks/prog-run.sh");
    let manifest = project.path().join(".codex/prog-hooks/manifest.json");
    let uninstall = project.path().join(".codex/prog-hooks/uninstall.sh");
    assert!(skill.exists());
    assert!(hook.exists());
    assert!(manifest.exists());
    assert!(uninstall.exists());

    let skill_text = fs::read_to_string(&skill).unwrap();
    for expected in [
        "prog run",
        "prog observe",
        "prog inspect",
        "prog evidence",
        "EvidenceRef",
        "MCP is optional",
    ] {
        assert!(
            skill_text.contains(expected),
            "skill should contain {expected}"
        );
    }
    let manifest_value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(manifest_value["schema"], "prog.integration");
    assert_eq!(manifest_value["agent"], "codex");
    assert_eq!(manifest_value["mcp"]["status"], "optional");
    assert!(
        manifest_value["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file.as_str() == Some(".codex/prog-hooks/uninstall.sh"))
    );

    let prog_bin = Path::new(env!("CARGO_BIN_EXE_prog"));
    let prog_dir = prog_bin.parent().unwrap();
    let path = format!(
        "{}:{}",
        prog_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let hook_output = Command::new("sh")
        .arg(&hook)
        .args(["python3", "-c", "print('hooked')"])
        .current_dir(project.path())
        .env("PATH", path)
        .output()
        .expect("hook should run");
    assert!(hook_output.status.success(), "{}", stdout(&hook_output));
    let envelope: Value = serde_json::from_slice(&hook_output.stdout).unwrap();
    assert_eq!(envelope["source_id"], "run");
    assert_eq!(envelope["data_preview"]["stdout"]["text"], "hooked");
    assert!(envelope["cursor"].as_str().unwrap().starts_with("pc1_"));

    fs::write(&skill, "custom skill").unwrap();
    let rerun = prog(&["init", "--agent", "codex", "--project", "--root", root]);
    assert!(rerun.status.success(), "{}", stdout(&rerun));
    let rerun_report: Value = serde_json::from_slice(&rerun.stdout).unwrap();
    assert!(
        rerun_report["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["action"] == "exists")
    );
    assert_eq!(fs::read_to_string(&skill).unwrap(), "custom skill");
    assert!(
        rerun_report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("left unchanged"))
    );
}

#[test]
fn init_requires_project_scope_and_supports_each_documented_agent() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let missing_scope = prog(&["init", "--agent", "codex", "--root", root]);
    assert!(!missing_scope.status.success());
    assert_eq!(stderr(&missing_scope), "");
    let error: Value = serde_json::from_slice(&missing_scope.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--project")
    );

    for (agent, expected_skill) in [
        ("claude-code", ".claude/skills/prog/SKILL.md"),
        ("cursor", ".cursor/rules/prog.mdc"),
        ("gemini-cli", ".gemini/skills/prog/SKILL.md"),
    ] {
        let output = prog(&[
            "init",
            "--agent",
            agent,
            "--project",
            "--dry-run",
            "--root",
            root,
        ]);
        assert!(output.status.success(), "{}", stdout(&output));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["agent"], agent);
        assert!(
            report["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|file| file["path"] == expected_skill)
        );
    }
    assert!(!project.path().join(".claude").exists());
    assert!(!project.path().join(".cursor").exists());
    assert!(!project.path().join(".gemini").exists());
}

#[test]
fn non_codex_integrations_create_valid_agent_files_and_uninstall_cleanly() {
    for (agent, skill, hook_dir) in [
        (
            "claude-code",
            ".claude/skills/prog/SKILL.md",
            ".claude/prog-hooks",
        ),
        ("cursor", ".cursor/rules/prog.mdc", ".cursor/prog-hooks"),
        (
            "gemini-cli",
            ".gemini/skills/prog/SKILL.md",
            ".gemini/prog-hooks",
        ),
    ] {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_str().unwrap();
        let output = prog(&["init", "--agent", agent, "--project", "--root", root]);
        assert!(output.status.success(), "{}", stdout(&output));
        let skill_path = project.path().join(skill);
        assert!(skill_path.exists());
        let skill_text = fs::read_to_string(&skill_path).unwrap();
        assert!(skill_text.starts_with("---\n"));
        assert!(skill_text.contains("prog inspect"));

        let manifest_path = project.path().join(hook_dir).join("manifest.json");
        let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["agent"], agent);
        assert_eq!(
            manifest["commands"]["inspect"],
            "prog inspect <cursor> --goal <goal>"
        );

        let uninstall = project.path().join(hook_dir).join("uninstall.sh");
        let result = Command::new("sh")
            .arg(&uninstall)
            .current_dir(project.path())
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(!skill_path.exists());
        assert!(!project.path().join(hook_dir).exists());
    }
}

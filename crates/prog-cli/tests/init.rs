//! Integration coverage for project-agent initialization.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use serde_json::Value;
use sha2::{Digest, Sha256};

mod support;

use support::*;

#[test]
fn harness_install_and_doctor_prove_a_portable_agent_extension() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let dry_run = prog(&[
        "harness",
        "install",
        "--host",
        "agent-skills",
        "--dry-run",
        "--root",
        root,
    ]);
    assert!(dry_run.status.success(), "{}", stdout(&dry_run));
    let dry_run: Value = serde_json::from_slice(&dry_run.stdout).unwrap();
    assert_eq!(dry_run["schema"], "prog.harness.install");
    assert_eq!(dry_run["mode"], "explicit");
    assert_eq!(dry_run["hosts"], serde_json::json!(["agent-skills"]));
    assert!(
        dry_run["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["action"] == "would_create")
    );
    assert!(!project.path().join(".agents").exists());

    let installed = prog(&[
        "harness",
        "install",
        "--host",
        "agent-skills",
        "--root",
        root,
    ]);
    assert!(installed.status.success(), "{}", stdout(&installed));
    assert!(project.path().join(".agents/skills/prog/SKILL.md").exists());
    assert!(
        project
            .path()
            .join(".agents/prog-hooks/prog-run.sh")
            .exists()
    );

    let doctor = prog(&[
        "harness",
        "doctor",
        "--host",
        "agent-skills",
        "--root",
        root,
    ]);
    assert!(doctor.status.success(), "{}", stdout(&doctor));
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor["schema"], "prog.harness.doctor");
    assert_eq!(doctor["ready"], true);
    assert!(doctor["blockers"].as_array().unwrap().is_empty());

    fs::write(
        project.path().join(".agents/prog-hooks/prog-run.sh"),
        "modified",
    )
    .unwrap();
    let stale = prog(&[
        "harness",
        "doctor",
        "--host",
        "agent-skills",
        "--root",
        root,
    ]);
    assert!(!stale.status.success());
    let stale: Value = serde_json::from_slice(&stale.stdout).unwrap();
    assert_eq!(stale["ready"], false);
    assert!(
        stale["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker.as_str().unwrap().contains("unverified"))
    );
}

#[test]
fn harness_install_deduplicates_the_shared_agent_skill_across_hosts() {
    let project = tempfile::tempdir().unwrap();
    let output = prog(&[
        "harness",
        "install",
        "--host",
        "agent-skills",
        "--host",
        "codex",
        "--dry-run",
        "--root",
        project.path().to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let paths = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        paths
            .iter()
            .filter(|path| **path == ".agents/skills/prog/SKILL.md")
            .count(),
        1
    );
    assert!(paths.contains(&".agents/prog-hooks/prog-run.sh"));
    assert!(paths.contains(&".codex/prog-hooks/prog-run.sh"));
}

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
        file["path"] == ".agents/skills/prog/SKILL.md" && file["executable"] == false
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

    let skill = project.path().join(".agents/skills/prog/SKILL.md");
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
    assert_eq!(
        manifest_value["host_contract"]["integration"],
        "skill_and_explicit_argv_wrapper"
    );
    assert_eq!(
        manifest_value["host_contract"]["native_pre_tool_rewrite_installed"],
        false
    );
    assert_eq!(manifest_value["routing"]["semantic_substitution"], false);
    assert_eq!(
        manifest_value["facade"].as_object().unwrap().keys().count(),
        3
    );
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
    assert_eq!(stdout(&hook_output), "hooked\n");

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
fn codex_wrapper_preserves_process_contract_and_fails_over_before_execution() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let installed = prog(&["init", "--agent", "codex", "--project", "--root", root]);
    assert!(installed.status.success(), "{}", stdout(&installed));
    let wrapper = project.path().join(".codex/prog-hooks/prog-run.sh");
    let tools = project.path().join("fixture-bin");
    fs::create_dir(&tools).unwrap();
    let prog_bin = Path::new(env!("CARGO_BIN_EXE_prog"));
    let path = format!(
        "{}:{}:{}",
        prog_bin.parent().unwrap().display(),
        tools.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    write_executable(
        &tools.join("pytest"),
        r#"#!/usr/bin/env python3
import json, os, sys
print(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd(), "env": os.environ.get("PROG_FIXTURE_ENV")}), flush=True)
print("fixture-stderr", file=sys.stderr, flush=True)
raise SystemExit(7)
"#,
    );
    let captured = Command::new("sh")
        .arg(&wrapper)
        .args(["pytest", "space arg", "quote'\""])
        .current_dir(project.path())
        .env("PATH", &path)
        .env("PROG_FIXTURE_ENV", "inherited")
        .output()
        .unwrap();
    assert_eq!(captured.status.code(), Some(7));
    let envelope: Value = serde_json::from_slice(&captured.stdout).unwrap();
    assert_eq!(envelope["data_preview"]["command"]["exit_code"], 7);
    assert_eq!(envelope["data_preview"]["stderr"]["text"], "fixture-stderr");
    let child: Value =
        serde_json::from_str(envelope["data_preview"]["stdout"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(child["argv"], serde_json::json!(["space arg", "quote'\""]));
    assert_eq!(
        child["cwd"],
        project
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(child["env"], "inherited");

    write_executable(
        &tools.join("rustc"),
        "#!/usr/bin/env python3\nimport os, signal\nos.kill(os.getpid(), signal.SIGTERM)\n",
    );
    let signalled = Command::new("sh")
        .arg(&wrapper)
        .arg("rustc")
        .current_dir(project.path())
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(signalled.status.code(), Some(128 + libc::SIGTERM));
    let signalled: Value = serde_json::from_slice(&signalled.stdout).unwrap();
    assert_eq!(
        signalled["data_preview"]["command"]["signal"],
        libc::SIGTERM
    );

    write_executable(
        &tools.join("pytest"),
        "#!/usr/bin/env python3\nimport time\ntime.sleep(10)\n",
    );
    let timed_out = Command::new("sh")
        .arg(&wrapper)
        .arg("pytest")
        .current_dir(project.path())
        .env("PATH", &path)
        .env("PROG_HOOK_TIMEOUT_MS", "50")
        .output()
        .unwrap();
    assert_eq!(timed_out.status.code(), Some(124));
    let timed_out: Value = serde_json::from_slice(&timed_out.stdout).unwrap();
    assert_eq!(timed_out["data_preview"]["command"]["timed_out"], true);

    let cancelled = Command::new("sh")
        .arg(&wrapper)
        .arg("pytest")
        .current_dir(project.path())
        .env("PATH", &path)
        .env("PROG_HOOK_TIMEOUT_MS", "30000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(250));
    assert_eq!(
        unsafe { libc::kill(cancelled.id() as i32, libc::SIGTERM) },
        0
    );
    let cancelled = cancelled.wait_with_output().unwrap();
    assert_eq!(cancelled.status.code(), Some(128 + libc::SIGTERM));
    let cancelled: Value = serde_json::from_slice(&cancelled.stdout).unwrap();
    assert_eq!(cancelled["data_preview"]["command"]["cancelled"], true);
    assert_eq!(
        cancelled["observation"]["capture"]["stop_reason"],
        "cancelled"
    );
    assert_eq!(
        cancelled["observation"]["capture"]["can_prove_absence"],
        false
    );

    write_executable(
        &tools.join("kubectl"),
        "#!/usr/bin/env python3\nprint('stream-passthrough')\n",
    );
    let streaming = Command::new("sh")
        .arg(&wrapper)
        .args(["kubectl", "logs", "--follow", "pod/api"])
        .current_dir(project.path())
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(streaming.status.success());
    assert_eq!(stdout(&streaming), "stream-passthrough\n");

    write_executable(&tools.join("prog"), "#!/bin/sh\nexit 70\n");
    write_executable(
        &tools.join("pytest"),
        "#!/usr/bin/env python3\nimport sys\nprint('fallback-stdout')\nprint('fallback-stderr', file=sys.stderr)\n",
    );
    let fallback_path = format!(
        "{}:{}",
        tools.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fallback = Command::new("sh")
        .arg(&wrapper)
        .arg("pytest")
        .current_dir(project.path())
        .env("PATH", fallback_path)
        .output()
        .unwrap();
    assert!(fallback.status.success());
    assert_eq!(stdout(&fallback), "fallback-stdout\n");
    let fallback_stderr = stderr(&fallback);
    assert!(fallback_stderr.contains("prog.integration_fallback"));
    assert!(fallback_stderr.contains("route_failed"));
    assert!(fallback_stderr.contains("fallback-stderr"));
}

fn write_executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
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
        assert_eq!(manifest["facade"].as_object().unwrap().keys().count(), 3);
        assert_eq!(manifest["routing"]["semantic_substitution"], false);

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

#[test]
fn first_party_manifest_outputs_match_pre_refactor_golden_hashes() {
    let golden: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_slice(include_bytes!("fixtures/init-golden-sha256.json")).unwrap();
    for (agent, files) in golden {
        let project = tempfile::tempdir().unwrap();
        let output = prog(&[
            "init",
            "--agent",
            &agent,
            "--project",
            "--root",
            project.path().to_str().unwrap(),
        ]);
        assert!(output.status.success(), "{}", stdout(&output));
        for (relative_path, expected) in files {
            let bytes = fs::read(project.path().join(&relative_path)).unwrap();
            let actual = format!("{:x}", Sha256::digest(bytes));
            assert_eq!(
                actual, expected,
                "{agent} output changed for {relative_path}; review and update the golden only for an intentional contract change"
            );
        }
    }
}

#[test]
fn agents_md_target_appends_one_marked_section_without_overwriting() {
    let project = tempfile::tempdir().unwrap();
    let agents = project.path().join("AGENTS.md");
    fs::write(&agents, "# Owner instructions\n\nKeep this text.\n").unwrap();
    let args = [
        "init",
        "--agent",
        "agents-md",
        "--project",
        "--root",
        project.path().to_str().unwrap(),
    ];
    let first = prog(&args);
    assert!(first.status.success(), "{}", stdout(&first));
    let first_report: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_report["files"][0]["action"], "appended");
    let installed = fs::read_to_string(&agents).unwrap();
    assert!(installed.starts_with("# Owner instructions\n\nKeep this text.\n"));
    assert_eq!(installed.matches("<!-- prog:skill:start -->").count(), 1);
    assert_eq!(installed.matches("<!-- prog:skill:end -->").count(), 1);
    assert!(installed.contains("prog evidence"));
    assert!(!project.path().join(".prog").exists());

    let second = prog(&args);
    assert!(second.status.success(), "{}", stdout(&second));
    let second_report: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_report["files"][0]["action"], "exists");
    assert_eq!(fs::read_to_string(&agents).unwrap(), installed);
}

#[test]
fn print_skill_honors_frontmatter_and_writes_no_files() {
    let project = tempfile::tempdir().unwrap();
    for (flavor, expected_prefix, expected_fragment) in [
        ("yaml", "---\nname: prog", "Prefer this loop"),
        ("mdc", "---\ndescription:", "alwaysApply: false"),
        ("none", "# prog", "Prefer this loop"),
    ] {
        let output = prog_in_dir(
            project.path(),
            &["init", "--print-skill", "--frontmatter", flavor],
        );
        assert!(output.status.success(), "{}", stdout(&output));
        let skill = stdout(&output);
        assert!(skill.starts_with(expected_prefix), "{flavor}: {skill}");
        assert!(skill.contains(expected_fragment));
        assert_eq!(stderr(&output), "");
        assert_eq!(fs::read_dir(project.path()).unwrap().count(), 0);
    }
}

#[test]
fn unknown_agent_error_lists_built_in_and_external_manifests() {
    let project = tempfile::tempdir().unwrap();
    let manifests = tempfile::tempdir().unwrap();
    fs::write(
        manifests.path().join("zed.json"),
        r#"{
          "schema":"prog.integration_target",
          "agent":"zed",
          "skill_path":".zed/skills/prog.md",
          "hook_dir":".zed/prog-hooks",
          "frontmatter":"none",
          "write_mode":"create"
        }"#,
    )
    .unwrap();
    let output = prog(&[
        "init",
        "--agent",
        "missing",
        "--project",
        "--root",
        project.path().to_str().unwrap(),
        "--manifest-dir",
        manifests.path().to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    let message = error["error"]["message"].as_str().unwrap();
    for available in [
        "agents-md",
        "claude-code",
        "codex",
        "cursor",
        "gemini-cli",
        "zed",
    ] {
        assert!(
            message.contains(available),
            "missing {available}: {message}"
        );
    }
}

#[test]
fn external_manifest_adds_a_target_and_cannot_escape_the_project_root() {
    let project = tempfile::tempdir().unwrap();
    let manifests = tempfile::tempdir().unwrap();
    let manifest_path = manifests.path().join("zed.json");
    fs::write(
        &manifest_path,
        r#"{
          "schema":"prog.integration_target",
          "agent":"zed",
          "skill_path":".zed/skills/prog.md",
          "hook_dir":".zed/prog-hooks",
          "frontmatter":"none",
          "write_mode":"create"
        }"#,
    )
    .unwrap();
    let output = prog(&[
        "init",
        "--agent",
        "zed",
        "--project",
        "--root",
        project.path().to_str().unwrap(),
        "--manifest-dir",
        manifests.path().to_str().unwrap(),
    ]);
    assert!(output.status.success(), "{}", stdout(&output));
    let skill = fs::read_to_string(project.path().join(".zed/skills/prog.md")).unwrap();
    assert!(skill.starts_with("# prog"));
    assert!(project.path().join(".zed/prog-hooks/prog-run.sh").exists());

    fs::write(
        &manifest_path,
        r#"{
          "schema":"prog.integration_target",
          "agent":"escape",
          "skill_path":"../outside.md",
          "hook_dir":".escape/hooks",
          "frontmatter":"none",
          "write_mode":"create"
        }"#,
    )
    .unwrap();
    let rejected = prog(&[
        "init",
        "--agent",
        "escape",
        "--project",
        "--root",
        project.path().to_str().unwrap(),
        "--manifest-dir",
        manifests.path().to_str().unwrap(),
    ]);
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_eq!(error["error"]["kind"], "bad_args");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("project-relative")
    );
    assert!(!project.path().parent().unwrap().join("outside.md").exists());
}

//! Project-local agent integration command.

use crate::*;

pub(crate) fn init_integration(args: &InitArgs) -> Result<InitReport> {
    if !args.project {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: "pass --project; global shell installation is not implemented in V1"
                .to_string(),
        });
    }
    let root = project_root(&args.root)?;
    let specs = agent_project_init_files(args.agent);
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for spec in specs {
        let full_path = root.join(&spec.relative_path);
        let exists = full_path.exists();
        let (action, reason) = if exists {
            skipped = skipped.saturating_add(1);
            (
                "exists",
                Some("left existing file unchanged; remove it first to regenerate".to_string()),
            )
        } else if args.dry_run {
            ("would_create", None)
        } else {
            write_init_file(&full_path, &spec.content, spec.executable)?;
            ("created", None)
        };
        files.push(InitFileReport {
            path: spec.relative_path,
            full_path: full_path.to_string_lossy().to_string(),
            action,
            executable: spec.executable,
            reason,
        });
    }

    let mut warnings = Vec::new();
    if skipped > 0 {
        warnings.push(format!(
            "{skipped} existing integration file(s) were left unchanged"
        ));
    }
    if args.dry_run {
        warnings.push("dry-run only; no files were written".to_string());
    }

    Ok(InitReport {
        schema: "prog.init",
        agent: args.agent.as_str(),
        scope: "project",
        root: root.to_string_lossy().to_string(),
        dry_run: args.dry_run,
        files,
        next_steps: agent_init_next_steps(args.agent),
        warnings,
    })
}

fn project_root(root: &Path) -> Result<PathBuf> {
    let root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    if !root.exists() {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: format!("project root '{}' does not exist", root.display()),
        });
    }
    if !root.is_dir() {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: format!("project root '{}' is not a directory", root.display()),
        });
    }
    Ok(root)
}

fn agent_project_init_files(agent: AgentKind) -> Vec<InitFileSpec> {
    let (skill_path, hook_dir) = agent_integration_paths(agent);
    let hook_path = format!("{hook_dir}/prog-run.sh");
    let readme_path = format!("{hook_dir}/README.md");
    let manifest_path = format!("{hook_dir}/manifest.json");
    let uninstall_path = format!("{hook_dir}/uninstall.sh");
    let manifest_files = vec![
        skill_path.clone(),
        hook_path.clone(),
        readme_path.clone(),
        manifest_path.clone(),
        uninstall_path.clone(),
    ];
    let manifest = json!({
        "schema": "prog.integration",
        "agent": agent.as_str(),
        "scope": "project",
        "mcp": {
            "status": "optional",
            "reason": "CLI, skill, and hooks are the durable V1 contract"
        },
        "files": manifest_files,
        "commands": {
            "wrap_command": format!("{hook_path} <command...>"),
            "observe_file": "prog observe --file <path>",
            "inspect": "prog inspect <cursor> --goal <goal>",
            "search": "prog search <cursor> <query>",
            "evidence": "prog evidence <cursor> --path <json-pointer>",
            "expand": "prog expand <cursor> --path <json-pointer>"
        },
        "uninstall": format!("sh {uninstall_path}")
    });
    vec![
        InitFileSpec {
            relative_path: skill_path,
            content: agent_skill_content(agent),
            executable: false,
        },
        InitFileSpec {
            relative_path: hook_path,
            content: prog_run_hook(hook_dir),
            executable: true,
        },
        InitFileSpec {
            relative_path: readme_path,
            content: hook_readme(agent, hook_dir),
            executable: false,
        },
        InitFileSpec {
            relative_path: manifest_path,
            content: format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
            executable: false,
        },
        InitFileSpec {
            relative_path: uninstall_path,
            content: uninstall_hook(&manifest_files),
            executable: true,
        },
    ]
}

fn agent_integration_paths(agent: AgentKind) -> (String, &'static str) {
    match agent {
        AgentKind::Codex => (
            ".codex/skills/prog/SKILL.md".to_string(),
            ".codex/prog-hooks",
        ),
        AgentKind::ClaudeCode => (
            ".claude/skills/prog/SKILL.md".to_string(),
            ".claude/prog-hooks",
        ),
        AgentKind::Cursor => (".cursor/rules/prog.mdc".to_string(), ".cursor/prog-hooks"),
        AgentKind::GeminiCli => (
            ".gemini/skills/prog/SKILL.md".to_string(),
            ".gemini/prog-hooks",
        ),
    }
}

fn agent_skill_content(agent: AgentKind) -> String {
    if agent != AgentKind::Cursor {
        return PROG_AGENT_SKILL.to_string();
    }
    let body = PROG_AGENT_SKILL
        .strip_prefix("---\n")
        .and_then(|value| value.split_once("\n---\n"))
        .map_or(PROG_AGENT_SKILL, |(_, body)| body);
    format!(
        "---\ndescription: Use prog for bounded, cached evidence navigation over noisy commands, APIs, and files.\nglobs:\nalwaysApply: false\n---\n{body}"
    )
}

fn agent_init_next_steps(agent: AgentKind) -> Vec<String> {
    let (skill_path, hook_dir) = agent_integration_paths(agent);
    vec![
        format!("Review {skill_path} before relying on the generated integration"),
        format!("Route noisy commands through {hook_dir}/prog-run.sh <command...>"),
        "Use prog inspect <cursor> --goal <goal>, then prog evidence <cursor> --path <path>"
            .to_string(),
    ]
}

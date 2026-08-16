//! Project-local agent integration command.

use std::path::Component;

use serde::Deserialize;

use crate::*;

include!(concat!(env!("OUT_DIR"), "/integration_manifests.rs"));

const INTEGRATION_TARGET_SCHEMA: &str = "prog.integration_target";
const AGENTS_MARKER_START: &str = "<!-- prog:skill:start -->";
const AGENTS_MARKER_END: &str = "<!-- prog:skill:end -->";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IntegrationWriteMode {
    Create,
    AppendMarker,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrationTarget {
    schema: String,
    agent: String,
    skill_path: String,
    hook_dir: Option<String>,
    frontmatter: FrontmatterFlavor,
    write_mode: IntegrationWriteMode,
    #[serde(default)]
    capabilities: IntegrationCapabilities,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrationCapabilities {
    instruction_discovery: Option<String>,
    command_input: Option<String>,
    post_result: Option<String>,
    #[serde(default)]
    can_replace_result: bool,
    native_package: Option<String>,
}

pub(crate) fn init_integration(args: &InitArgs) -> Result<InitReport> {
    if !args.project {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: "pass --project; global shell installation is not implemented in V1"
                .to_string(),
        });
    }
    let root = project_root(&args.root)?;
    let targets = integration_targets(args.manifest_dir.as_deref())?;
    let requested = args.agent.as_deref().ok_or_else(|| CoreError::BadArgs {
        operation: "init".to_string(),
        reason: "pass --agent or use --print-skill".to_string(),
    })?;
    let target = targets
        .iter()
        .find(|target| target.agent == requested)
        .ok_or_else(|| CoreError::BadArgs {
            operation: "init".to_string(),
            reason: format!(
                "unknown agent manifest '{requested}'; available manifests: {}",
                targets
                    .iter()
                    .map(|target| target.agent.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })?;
    let specs = agent_project_init_files(target);
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for spec in specs {
        let full_path = root.join(&spec.relative_path);
        let exists = full_path.exists();
        let (action, reason) = if spec.append_marker {
            if exists && std::fs::read_to_string(&full_path)?.contains(AGENTS_MARKER_START) {
                skipped = skipped.saturating_add(1);
                (
                    "exists",
                    Some("existing prog marker section was left unchanged".to_string()),
                )
            } else if args.dry_run {
                (
                    if exists {
                        "would_append"
                    } else {
                        "would_create"
                    },
                    None,
                )
            } else {
                append_marked_init_file(&full_path, &spec.content)?;
                (if exists { "appended" } else { "created" }, None)
            }
        } else if exists {
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
        agent: target.agent.clone(),
        scope: "project",
        root: root.to_string_lossy().to_string(),
        dry_run: args.dry_run,
        files,
        next_steps: agent_init_next_steps(target),
        warnings,
    })
}

pub(crate) fn install_harness(args: &HarnessInstallArgs) -> Result<HarnessInstallReport> {
    let root = project_root(&args.root)?;
    let targets = integration_targets(args.manifest_dir.as_deref())?;
    let selected = select_harness_targets(&root, &targets, &args.hosts)?;
    let specs = merged_target_specs(&selected)?;
    let (files, warnings) = install_project_specs(&root, specs, args.dry_run)?;
    Ok(HarnessInstallReport {
        schema: "prog.harness.install",
        root: root.to_string_lossy().to_string(),
        dry_run: args.dry_run,
        mode: if args.auto || args.hosts.is_empty() {
            "auto"
        } else {
            "explicit"
        },
        hosts: selected.iter().map(|target| target.agent.clone()).collect(),
        files,
        next_steps: vec![
            "Run prog harness doctor to verify the installed extension files".to_string(),
            "Let the harness invoke prog for noisy tool results; use prog evidence for exact cached paths"
                .to_string(),
        ],
        warnings,
    })
}

pub(crate) fn doctor_harness(args: &HarnessDoctorArgs) -> Result<HarnessDoctorReport> {
    let root = project_root(&args.root)?;
    let targets = integration_targets(args.manifest_dir.as_deref())?;
    let selected = select_harness_targets(&root, &targets, &args.hosts)?;
    let specs = merged_target_specs(&selected)?;
    let mut checks = vec![HarnessDoctorCheck {
        name: "binary".to_string(),
        status: "ok",
        detail: format!(
            "running {}",
            std::env::current_exe()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| "prog".to_string())
        ),
    }];
    let mut blockers = Vec::new();

    for spec in specs {
        let full_path = root.join(&spec.relative_path);
        if !full_path.exists() {
            blockers.push(format!("missing {}", spec.relative_path));
            checks.push(HarnessDoctorCheck {
                name: spec.relative_path,
                status: "missing",
                detail: "run prog harness install to create it".to_string(),
            });
            continue;
        }
        let content = std::fs::read_to_string(&full_path)?;
        let content_matches = if spec.append_marker {
            content.contains(AGENTS_MARKER_START) && content.contains(AGENTS_MARKER_END)
        } else {
            content == spec.content
        };
        if !content_matches {
            blockers.push(format!(
                "unverified integration content in {}",
                spec.relative_path
            ));
            checks.push(HarnessDoctorCheck {
                name: spec.relative_path,
                status: "mismatch",
                detail:
                    "the installed file differs from this prog version; review before regenerating"
                        .to_string(),
            });
            continue;
        }
        #[cfg(unix)]
        if spec.executable {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(&full_path)?.permissions().mode() & 0o111 == 0 {
                blockers.push(format!("{} is not executable", spec.relative_path));
                checks.push(HarnessDoctorCheck {
                    name: spec.relative_path,
                    status: "not_executable",
                    detail: "restore executable permissions before the harness uses this adapter"
                        .to_string(),
                });
                continue;
            }
        }
        checks.push(HarnessDoctorCheck {
            name: spec.relative_path,
            status: "ok",
            detail: "content and permissions match this prog version".to_string(),
        });
    }

    Ok(HarnessDoctorReport {
        schema: "prog.harness.doctor",
        root: root.to_string_lossy().to_string(),
        ready: blockers.is_empty(),
        hosts: selected.iter().map(|target| target.agent.clone()).collect(),
        checks,
        blockers,
        warnings: Vec::new(),
    })
}

fn select_harness_targets<'a>(
    root: &Path,
    targets: &'a [IntegrationTarget],
    requested: &[String],
) -> Result<Vec<&'a IntegrationTarget>> {
    let names = if requested.is_empty() {
        auto_harness_target_names(root, targets)
    } else {
        requested
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let mut selected = Vec::new();
    for name in names {
        let target = targets
            .iter()
            .find(|target| target.agent == name)
            .ok_or_else(|| CoreError::BadArgs {
                operation: "harness".to_string(),
                reason: format!(
                    "unknown harness host '{name}'; available hosts: {}",
                    targets
                        .iter()
                        .map(|target| target.agent.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })?;
        selected.push(target);
    }
    Ok(selected)
}

fn auto_harness_target_names(root: &Path, targets: &[IntegrationTarget]) -> Vec<String> {
    let available = targets
        .iter()
        .map(|target| target.agent.as_str())
        .collect::<BTreeSet<_>>();
    let mut names = BTreeSet::new();
    if available.contains("agent-skills") {
        names.insert("agent-skills".to_string());
    }
    for (target, dirs, commands) in [
        ("codex", &[".codex"][..], &["codex"][..]),
        ("claude-code", &[".claude"][..], &["claude"][..]),
        ("cursor", &[".cursor"][..], &["cursor", "cursor-agent"][..]),
        ("gemini-cli", &[".gemini"][..], &["gemini"][..]),
    ] {
        if available.contains(target)
            && (dirs.iter().any(|dir| root.join(dir).exists())
                || commands.iter().any(|command| command_on_path(command)))
        {
            names.insert(target.to_string());
        }
    }
    names.into_iter().collect()
}

fn command_on_path(command: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(command);
            candidate.is_file()
        })
    })
}

fn merged_target_specs(targets: &[&IntegrationTarget]) -> Result<Vec<InitFileSpec>> {
    let mut specs = BTreeMap::<String, InitFileSpec>::new();
    for target in targets {
        for spec in agent_project_init_files(target) {
            if let Some(existing) = specs.get(&spec.relative_path) {
                if existing.content != spec.content
                    || existing.executable != spec.executable
                    || existing.append_marker != spec.append_marker
                {
                    return Err(CoreError::BadArgs {
                        operation: "harness".to_string(),
                        reason: format!(
                            "selected harness manifests disagree about generated path '{}'",
                            spec.relative_path
                        ),
                    });
                }
            } else {
                specs.insert(spec.relative_path.clone(), spec);
            }
        }
    }
    Ok(specs.into_values().collect())
}

fn install_project_specs(
    root: &Path,
    specs: Vec<InitFileSpec>,
    dry_run: bool,
) -> Result<(Vec<InitFileReport>, Vec<String>)> {
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for spec in specs {
        let full_path = root.join(&spec.relative_path);
        let exists = full_path.exists();
        let (action, reason) = if spec.append_marker {
            if exists && std::fs::read_to_string(&full_path)?.contains(AGENTS_MARKER_START) {
                skipped = skipped.saturating_add(1);
                (
                    "exists",
                    Some("existing prog marker section was left unchanged".to_string()),
                )
            } else if dry_run {
                (
                    if exists {
                        "would_append"
                    } else {
                        "would_create"
                    },
                    None,
                )
            } else {
                append_marked_init_file(&full_path, &spec.content)?;
                (if exists { "appended" } else { "created" }, None)
            }
        } else if exists {
            skipped = skipped.saturating_add(1);
            (
                "exists",
                Some("left existing file unchanged; remove it first to regenerate".to_string()),
            )
        } else if dry_run {
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
    if dry_run {
        warnings.push("dry-run only; no files were written".to_string());
    }
    Ok((files, warnings))
}

pub(crate) fn print_skill_content(frontmatter: FrontmatterFlavor) -> String {
    agent_skill_content(frontmatter)
}

fn integration_targets(manifest_dir: Option<&Path>) -> Result<Vec<IntegrationTarget>> {
    let mut targets = BUILTIN_INTEGRATION_MANIFESTS
        .iter()
        .map(|content| parse_target(content, "built-in manifest"))
        .collect::<Result<Vec<_>>>()?;

    if let Some(manifest_dir) = manifest_dir {
        if !manifest_dir.is_dir() {
            return Err(CoreError::BadArgs {
                operation: "init".to_string(),
                reason: format!(
                    "integration manifest directory '{}' does not exist or is not a directory",
                    manifest_dir.display()
                ),
            });
        }
        let mut paths = std::fs::read_dir(manifest_dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        for path in paths.into_iter().filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        }) {
            let content = std::fs::read_to_string(&path)?;
            let target = parse_target(&content, &path.display().to_string())?;
            if targets
                .iter()
                .any(|existing| existing.agent == target.agent)
            {
                return Err(CoreError::BadArgs {
                    operation: "init".to_string(),
                    reason: format!(
                        "duplicate integration manifest name '{}' in {}",
                        target.agent,
                        path.display()
                    ),
                });
            }
            targets.push(target);
        }
    }
    targets.sort_by(|left, right| left.agent.cmp(&right.agent));
    Ok(targets)
}

fn parse_target(content: &str, origin: &str) -> Result<IntegrationTarget> {
    let target: IntegrationTarget =
        serde_json::from_str(content).map_err(|error| CoreError::BadArgs {
            operation: "init".to_string(),
            reason: format!("invalid integration target in {origin}: {error}"),
        })?;
    if target.schema != INTEGRATION_TARGET_SCHEMA {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: format!(
                "invalid integration target in {origin}: schema must be '{INTEGRATION_TARGET_SCHEMA}'"
            ),
        });
    }
    if target.agent.is_empty()
        || !target
            .agent
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: format!(
                "invalid integration target in {origin}: agent must use lowercase letters, digits, and hyphens"
            ),
        });
    }
    validate_relative_target_path(&target.skill_path, "skill_path", origin)?;
    if let Some(hook_dir) = target.hook_dir.as_deref() {
        validate_relative_target_path(hook_dir, "hook_dir", origin)?;
    }
    if target.write_mode == IntegrationWriteMode::AppendMarker && target.hook_dir.is_some() {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: format!(
                "invalid integration target in {origin}: append_marker targets cannot install hook files"
            ),
        });
    }
    if target.write_mode == IntegrationWriteMode::Create && target.hook_dir.is_none() {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: format!(
                "invalid integration target in {origin}: create targets require hook_dir"
            ),
        });
    }
    Ok(target)
}

fn validate_relative_target_path(value: &str, field: &str, origin: &str) -> Result<()> {
    let path = Path::new(value);
    let safe = !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !safe {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: format!(
                "invalid integration target in {origin}: {field} must be a normalized project-relative path"
            ),
        });
    }
    Ok(())
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

fn agent_project_init_files(target: &IntegrationTarget) -> Vec<InitFileSpec> {
    if target.write_mode == IntegrationWriteMode::AppendMarker {
        return vec![InitFileSpec {
            relative_path: target.skill_path.clone(),
            content: marked_agents_skill(),
            executable: false,
            append_marker: true,
        }];
    }

    let hook_dir = target
        .hook_dir
        .as_deref()
        .expect("validated create targets have a hook directory");
    let hook_path = format!("{hook_dir}/prog-run.sh");
    let readme_path = format!("{hook_dir}/README.md");
    let manifest_path = format!("{hook_dir}/manifest.json");
    let uninstall_path = format!("{hook_dir}/uninstall.sh");
    let manifest_files = vec![
        target.skill_path.clone(),
        hook_path.clone(),
        readme_path.clone(),
        manifest_path.clone(),
        uninstall_path.clone(),
    ];
    let mut manifest = json!({
        "schema": "prog.integration",
        "agent": target.agent,
        "scope": "project",
        "mcp": {
            "status": "optional",
            "reason": "CLI, skill, and hooks are the durable V1 contract"
        },
        "files": manifest_files,
        "facade": {
            "observe": {
                "command": format!("{hook_path} <command...>"),
                "file": "prog observe --file <path>",
                "source": "prog call <source> <operation> --args <json>"
            },
            "evidence": {
                "exact": "prog evidence <cursor> --path <json-pointer>",
                "search": "prog search <cursor> <query>"
            },
            "status": {
                "readiness": "prog status",
                "comparison": "prog status --baseline <observation> --subject <observation>"
            }
        },
        "routing": {
            "classifier": "prog route -- <command...>",
            "semantic_substitution": false,
            "fallback": "execute the authored argv directly only when routing fails before execution"
        },
        "uninstall": format!("sh {uninstall_path}")
    });
    manifest["host_contract"] = json!({
        "host": target.agent,
        "instruction_discovery": target.capabilities.instruction_discovery.as_deref().unwrap_or("manifest_declared"),
        "command_input": target.capabilities.command_input.as_deref().unwrap_or("unknown"),
        "post_result": target.capabilities.post_result.as_deref().unwrap_or("unproven"),
        "can_replace_result": target.capabilities.can_replace_result,
        "native_package": target.capabilities.native_package.as_deref(),
        "integration": "skill_and_explicit_argv_wrapper",
        "semantic_substitution_allowed": false
    });
    if target.agent == "codex" {
        manifest["host_contract"]["native_pre_tool_rewrite_installed"] = json!(false);
        manifest["host_contract"]["reason"] = json!(
            "Codex PreToolUse exposes Bash input as a shell command string; prog does not reparse it into argv"
        );
        manifest["host_contract"]["official_contract"] =
            json!("https://learn.chatgpt.com/docs/hooks");
    }
    vec![
        InitFileSpec {
            relative_path: target.skill_path.clone(),
            content: agent_skill_content(target.frontmatter),
            executable: false,
            append_marker: false,
        },
        InitFileSpec {
            relative_path: hook_path,
            content: prog_run_hook(hook_dir),
            executable: true,
            append_marker: false,
        },
        InitFileSpec {
            relative_path: readme_path,
            content: hook_readme(&target.agent, hook_dir),
            executable: false,
            append_marker: false,
        },
        InitFileSpec {
            relative_path: manifest_path,
            content: format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
            executable: false,
            append_marker: false,
        },
        InitFileSpec {
            relative_path: uninstall_path,
            content: uninstall_hook(&manifest_files),
            executable: true,
            append_marker: false,
        },
    ]
}

fn canonical_skill_body() -> &'static str {
    PROG_AGENT_SKILL
        .strip_prefix("---\n")
        .and_then(|value| value.split_once("\n---\n"))
        .map_or(PROG_AGENT_SKILL, |(_, body)| body)
}

fn agent_skill_content(frontmatter: FrontmatterFlavor) -> String {
    match frontmatter {
        FrontmatterFlavor::Yaml => PROG_AGENT_SKILL.to_string(),
        FrontmatterFlavor::Mdc => format!(
            "---\ndescription: Use prog for bounded, cached evidence navigation over noisy commands, APIs, and files.\nglobs:\nalwaysApply: false\n---\n{}",
            canonical_skill_body()
        ),
        FrontmatterFlavor::None => canonical_skill_body().trim_start_matches('\n').to_string(),
    }
}

fn marked_agents_skill() -> String {
    format!(
        "{AGENTS_MARKER_START}\n{}{AGENTS_MARKER_END}\n",
        agent_skill_content(FrontmatterFlavor::None)
    )
}

fn agent_init_next_steps(target: &IntegrationTarget) -> Vec<String> {
    if let Some(hook_dir) = target.hook_dir.as_deref() {
        vec![
            format!(
                "Review {} before relying on the generated integration",
                target.skill_path
            ),
            format!("Route noisy commands through {hook_dir}/prog-run.sh <command...>"),
            "Use prog inspect <cursor> --goal <goal>, then prog evidence <cursor> --path <path>"
                .to_string(),
        ]
    } else {
        vec![
            format!("Review the marked prog section in {}", target.skill_path),
            "Run prog explicitly for noisy commands, then retrieve exact cached evidence"
                .to_string(),
        ]
    }
}

fn prog_run_hook(hook_dir: &str) -> String {
    format!(
        r#"#!/usr/bin/env sh
set -eu

if [ "$#" -eq 0 ]; then
  echo "usage: {hook_dir}/prog-run.sh <command...>" >&2
  exit 64
fi

if ! command -v prog >/dev/null 2>&1; then
  printf '%s\n' '{{"schema":"prog.integration_fallback","reason":"prog_unavailable","action":"execute_authored_argv"}}' >&2
  exec "$@"
fi

if ! route_output=$(prog route -- "$@" 2>/dev/null); then
  printf '%s\n' '{{"schema":"prog.integration_fallback","reason":"route_failed","action":"execute_authored_argv"}}' >&2
  exec "$@"
fi

case "$route_output" in
  *'"guidance":"progressive"'*)
    timeout_ms=${{PROG_HOOK_TIMEOUT_MS:-30000}}
    case "$timeout_ms" in
      ''|*[!0-9]*)
        printf '%s\n' '{{"schema":"prog.integration_error","reason":"PROG_HOOK_TIMEOUT_MS_must_be_an_unsigned_integer"}}' >&2
        exit 64
        ;;
    esac
    exec prog run --preserve-exit-code --timeout-ms "$timeout_ms" -- "$@"
    ;;
  *)
    exec "$@"
    ;;
esac
"#
    )
}

fn hook_readme(agent: &str, hook_dir: &str) -> String {
    format!(
        r#"# prog {agent} hooks

This project-local integration keeps `prog` usable without MCP server mode.

Use the wrapper for noisy commands:

```bash
    {hook_dir}/prog-run.sh cargo test
```

The wrapper calls `prog route` with the exact argv. Progressive guidance runs
the same argv through `prog run`; raw, passthrough, and unknown guidance execute
it directly. It never performs semantic substitution or reparses a shell
string. `PROG_HOOK_TIMEOUT_MS` sets the progressive capture timeout.

If `prog` is unavailable or classification fails before the command starts, the
wrapper emits a `prog.integration_fallback` record on stderr and executes the
authored argv directly. It never retries after `prog run` starts because doing
so could duplicate side effects. Progressive execution returns a bounded
`DisclosureEnvelope`; use `prog evidence` for exact retrieval and `prog status`
for readiness or comparison.

TTY, interactive, following/streaming, nested `prog`, and shell-structural
invocations pass through. Invoke pipelines, redirects, substitutions, heredocs,
and stateful shell compounds outside the wrapper; they are not parsed or
rewritten.

For shell aliases or editor tasks, wire the command directly rather than
rewriting user commands globally:

```sh
prog_run() {{
  {hook_dir}/prog-run.sh "$@"
}}
```

MCP is optional compatibility. Prefer the CLI, this skill, and explicit wrappers
unless the host agent already has a reliable MCP client.
"#
    )
}

fn uninstall_hook(files: &[String]) -> String {
    let mut script = "#!/usr/bin/env sh\nset -eu\n\n".to_string();
    for file in files {
        script.push_str(&format!("rm -f {}\n", shell_quote(file)));
    }
    let mut dirs = files
        .iter()
        .filter_map(|file| Path::new(file).parent())
        .flat_map(|path| path.ancestors().take_while(|path| *path != Path::new("")))
        .map(|path| path.to_string_lossy().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    dirs.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
    for dir in dirs {
        script.push_str(&format!(
            "rmdir {} 2>/dev/null || true\n",
            shell_quote(&dir)
        ));
    }
    script
}

fn write_init_file(path: &Path, content: &str, executable: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

fn append_marked_init_file(path: &Path, content: &str) -> Result<()> {
    let existing = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    let separator = if existing.is_empty() || existing.ends_with("\n\n") {
        ""
    } else if existing.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    write_init_file(path, &format!("{existing}{separator}{content}"), false)
}

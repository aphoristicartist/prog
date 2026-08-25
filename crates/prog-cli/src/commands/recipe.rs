//! Deterministic task recipe orchestration.

use crate::*;

pub(crate) async fn run_recipe(
    store: &Store,
    lens_dir: &Path,
    args: &RecipeArgs,
    ctx: &mut InvocationContext,
) -> Result<DisclosureEnvelope> {
    let goal = args
        .goal
        .clone()
        .unwrap_or_else(|| args.recipe.default_goal().to_string());
    if goal.trim().is_empty() {
        return Err(CoreError::BadArgs {
            operation: "recipe".to_string(),
            reason: "--goal must not be empty".to_string(),
        });
    }

    let (mut envelope, expanded_commands, command_result) = match args.recipe {
        RecipeKind::DiffReview | RecipeKind::LogsRootCause => {
            if !args.command.is_empty() {
                return Err(CoreError::BadArgs {
                    operation: format!("recipe {}", args.recipe.as_str()),
                    reason: "file recipes accept --file, not a trailing command".to_string(),
                });
            }
            let file = args.file.clone().ok_or_else(|| CoreError::BadArgs {
                operation: format!("recipe {}", args.recipe.as_str()),
                reason: "pass --file <path>".to_string(),
            })?;
            let (mime, lens) = match args.recipe {
                RecipeKind::DiffReview => ("text/x-diff", "unified-diff"),
                RecipeKind::LogsRootCause => ("text/plain", "logs"),
                _ => unreachable!(),
            };
            let observe = ObserveArgs {
                file: Some(file.clone()),
                stdin: false,
                mime: Some(mime.to_string()),
                name: Some(args.recipe.as_str().to_string()),
                lens: Some(lens.to_string()),
                comparison_family: args.comparison_family.clone(),
                selection_scopes: args.selection_scopes.clone(),
                selection_exhaustive: args.selection_exhaustive,
                ttl_seconds: args.ttl_seconds,
                invocation_identity: None,
            };
            (
                observe_artifact(store, lens_dir, &observe, ctx)?,
                vec![json!([
                    "prog",
                    "observe",
                    "--file",
                    file.to_string_lossy(),
                    "--mime",
                    mime,
                    "--lens",
                    lens
                ])],
                None,
            )
        }
        recipe if is_report_recipe(recipe) => {
            if args.file.is_some() {
                return Err(CoreError::BadArgs {
                    operation: format!("recipe {}", recipe.as_str()),
                    reason: "command recipes accept a trailing command, not --file".to_string(),
                });
            }
            let source_command = if args.command.is_empty() {
                default_recipe_command(recipe)
            } else {
                args.command.clone()
            };
            reject_conflicting_report_options(recipe, &source_command)?;
            let report = report_recipe_spec(recipe);
            let report_dir = tempfile::Builder::new()
                .prefix("prog-recipe-")
                .tempdir()
                .map_err(|error| CoreError::BadArgs {
                    operation: format!("recipe {}", recipe.as_str()),
                    reason: format!("temporary report directory could not be created: {error}"),
                })?;
            let report_path = report_dir.path().join(report.file_name);
            let command = configure_report_command(recipe, source_command.clone(), &report_path);
            let run = RunArgs {
                timeout_ms: args.timeout_ms,
                max_stdout_bytes: 1024 * 1024,
                max_stderr_bytes: 1024 * 1024,
                ttl_seconds: args.ttl_seconds,
                preserve_exit_code: false,
                out: None,
                lens: None,
                // The generated report, not this acquisition subprocess, is
                // the recipe's comparable observation. Keeping the internal
                // run outside the caller's family prevents it from becoming
                // the report's immediate delta baseline.
                comparison_family: None,
                selection_scopes: Vec::new(),
                selection_exhaustive: false,
                command: command.clone(),
            };
            let RunEnvelopeResult {
                mut envelope,
                exit_code,
            } = run_command(store, lens_dir, &run, ctx).await?;
            let mut expanded_commands = vec![json!(redact_run_argv(&command))];
            let report_observed = report_path
                .metadata()
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false);
            if report_observed {
                let cwd = std::env::current_dir()?;
                let observe = ObserveArgs {
                    file: Some(report_path.clone()),
                    stdin: false,
                    mime: Some(report.mime.to_string()),
                    name: Some(format!("{}-report", recipe.as_str())),
                    lens: Some(report.lens.to_string()),
                    comparison_family: args.comparison_family.clone(),
                    selection_scopes: args.selection_scopes.clone(),
                    selection_exhaustive: args.selection_exhaustive,
                    ttl_seconds: args.ttl_seconds,
                    invocation_identity: Some(json!({
                        "kind": "recipe_report",
                        "recipe": recipe.as_str(),
                        "argv": source_command,
                        "cwd": cwd.to_string_lossy()
                    })),
                };
                expanded_commands.push(json!([
                    "prog",
                    "observe",
                    "--file",
                    report_path.to_string_lossy(),
                    "--mime",
                    report.mime,
                    "--lens",
                    report.lens
                ]));
                envelope = observe_artifact(store, lens_dir, &observe, ctx)?;
            } else {
                envelope.warnings.push(format!(
                    "{} command produced no {} report; returning its captured process evidence",
                    recipe.as_str(),
                    report.format
                ));
            }
            (
                envelope,
                expanded_commands,
                Some(json!({
                    "exit": run_exit_metadata(exit_code),
                    "report_format": report.format,
                    "report_observed": report_observed
                })),
            )
        }
        recipe => {
            if args.file.is_some() {
                return Err(CoreError::BadArgs {
                    operation: format!("recipe {}", recipe.as_str()),
                    reason: "command recipes accept a trailing command, not --file".to_string(),
                });
            }
            let command = if args.command.is_empty() {
                default_recipe_command(recipe)
            } else {
                args.command.clone()
            };
            let command = configure_recipe_command(recipe, command);
            let lens = match recipe {
                RecipeKind::CargoTest => "cargo-test",
                RecipeKind::Pytest => "pytest",
                RecipeKind::NpmTest => "npm-test",
                RecipeKind::GoTest => "go-test",
                RecipeKind::GhIssues => "github-issues",
                RecipeKind::Vitest
                | RecipeKind::Playwright
                | RecipeKind::BunTest
                | RecipeKind::DenoTest
                | RecipeKind::Ruff
                | RecipeKind::Biome
                | RecipeKind::Semgrep
                | RecipeKind::DiffReview
                | RecipeKind::LogsRootCause => unreachable!(),
            };
            let run = RunArgs {
                timeout_ms: args.timeout_ms,
                max_stdout_bytes: 1024 * 1024,
                max_stderr_bytes: 1024 * 1024,
                ttl_seconds: args.ttl_seconds,
                preserve_exit_code: false,
                out: None,
                lens: Some(lens.to_string()),
                comparison_family: args.comparison_family.clone(),
                selection_scopes: args.selection_scopes.clone(),
                selection_exhaustive: args.selection_exhaustive,
                command: command.clone(),
            };
            (
                run_command(store, lens_dir, &run, ctx).await?.envelope,
                vec![json!(redact_run_argv(&command))],
                None,
            )
        }
    };

    if let Some(cursor) = envelope.cursor.clone() {
        let inspect = inspect_cursor(
            store,
            lens_dir,
            &InspectArgs {
                cursor,
                goal: goal.clone(),
                limit: 5,
                kind: None,
                path: String::new(),
            },
            ctx,
        )?;
        envelope.findings = inspect.findings;
    }
    let recommended_next = envelope.findings.first().and_then(|finding| {
        finding
            .commands
            .available
            .iter()
            .find(|command| {
                matches!(
                    command,
                    prog_core::NavigationCommand::Evidence | prog_core::NavigationCommand::Expand
                )
            })
            .map(|command| {
                json!({
                    "command": command,
                    "path": finding.path,
                    "kind": finding.kind,
                    "cursor": "{cursor}"
                })
            })
    });
    let mut recipe_details = json!({
        "id": args.recipe.as_str(),
        "goal": goal,
        "expanded_commands": expanded_commands,
        "recommended_next": recommended_next,
        "deterministic": true
    });
    if let Some(command_result) = command_result {
        recipe_details
            .as_object_mut()
            .expect("recipe details are an object")
            .insert("command_result".to_string(), command_result);
    }
    envelope.extra.insert("recipe".to_string(), recipe_details);
    compact_envelope_to_budget(&mut envelope, ctx.max_envelope_bytes())?;
    Ok(envelope)
}

fn default_recipe_command(recipe: RecipeKind) -> Vec<String> {
    match recipe {
        RecipeKind::CargoTest => vec!["cargo".to_string(), "test".to_string()],
        RecipeKind::Pytest => vec!["pytest".to_string()],
        RecipeKind::NpmTest => vec!["npm".to_string(), "test".to_string()],
        RecipeKind::GoTest => vec!["go".to_string(), "test".to_string(), "./...".to_string()],
        RecipeKind::Vitest => vec!["vitest".to_string(), "run".to_string()],
        RecipeKind::Playwright => vec!["playwright".to_string(), "test".to_string()],
        RecipeKind::BunTest => vec!["bun".to_string(), "test".to_string()],
        RecipeKind::DenoTest => vec!["deno".to_string(), "test".to_string()],
        RecipeKind::Ruff => vec!["ruff".to_string(), "check".to_string()],
        RecipeKind::Biome => vec!["biome".to_string(), "check".to_string()],
        RecipeKind::Semgrep => vec!["semgrep".to_string(), "scan".to_string()],
        RecipeKind::GhIssues => vec![
            "gh".to_string(),
            "issue".to_string(),
            "list".to_string(),
            "--json".to_string(),
            "number,title,state,labels,updatedAt,url".to_string(),
        ],
        RecipeKind::DiffReview | RecipeKind::LogsRootCause => Vec::new(),
    }
}

#[derive(Clone, Copy)]
struct ReportRecipeSpec {
    format: &'static str,
    file_name: &'static str,
    mime: &'static str,
    lens: &'static str,
}

fn is_report_recipe(recipe: RecipeKind) -> bool {
    matches!(
        recipe,
        RecipeKind::Vitest
            | RecipeKind::Playwright
            | RecipeKind::BunTest
            | RecipeKind::DenoTest
            | RecipeKind::Ruff
            | RecipeKind::Biome
            | RecipeKind::Semgrep
    )
}

fn report_recipe_spec(recipe: RecipeKind) -> ReportRecipeSpec {
    match recipe {
        RecipeKind::Vitest
        | RecipeKind::Playwright
        | RecipeKind::BunTest
        | RecipeKind::DenoTest => ReportRecipeSpec {
            format: "JUnit XML",
            file_name: "report.xml",
            mime: "application/junit+xml",
            lens: "junit",
        },
        RecipeKind::Ruff | RecipeKind::Biome | RecipeKind::Semgrep => ReportRecipeSpec {
            format: "SARIF",
            file_name: "report.sarif",
            mime: "application/sarif+json",
            lens: "sarif",
        },
        RecipeKind::CargoTest
        | RecipeKind::Pytest
        | RecipeKind::NpmTest
        | RecipeKind::GoTest
        | RecipeKind::GhIssues
        | RecipeKind::DiffReview
        | RecipeKind::LogsRootCause => unreachable!(),
    }
}

fn configure_report_command(
    recipe: RecipeKind,
    mut command: Vec<String>,
    report_path: &Path,
) -> Vec<String> {
    let report_path = report_path.to_string_lossy();
    let options = match recipe {
        RecipeKind::Vitest => vec![
            "--reporter=junit".to_string(),
            format!("--outputFile={report_path}"),
        ],
        RecipeKind::Playwright => vec!["--reporter=junit".to_string()],
        RecipeKind::BunTest => vec![
            "--reporter=junit".to_string(),
            format!("--reporter-outfile={report_path}"),
        ],
        RecipeKind::DenoTest => vec![format!("--junit-path={report_path}")],
        RecipeKind::Ruff => vec![
            "--output-format=sarif".to_string(),
            format!("--output-file={report_path}"),
        ],
        RecipeKind::Biome => vec![
            "--reporter=sarif".to_string(),
            format!("--reporter-file={report_path}"),
        ],
        RecipeKind::Semgrep => vec![format!("--sarif-output={report_path}")],
        RecipeKind::CargoTest
        | RecipeKind::Pytest
        | RecipeKind::NpmTest
        | RecipeKind::GoTest
        | RecipeKind::GhIssues
        | RecipeKind::DiffReview
        | RecipeKind::LogsRootCause => unreachable!(),
    };
    let insertion = command
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(command.len());
    command.splice(insertion..insertion, options);
    if recipe == RecipeKind::Playwright {
        let mut with_environment = vec![
            "env".to_string(),
            format!("PLAYWRIGHT_JUNIT_OUTPUT_FILE={report_path}"),
        ];
        with_environment.extend(command);
        with_environment
    } else {
        command
    }
}

fn reject_conflicting_report_options(recipe: RecipeKind, command: &[String]) -> Result<()> {
    let conflicts: &[&str] = match recipe {
        RecipeKind::Vitest => &["--reporter", "--outputFile"],
        RecipeKind::Playwright => &["--reporter"],
        RecipeKind::BunTest => &["--reporter", "--reporter-outfile"],
        RecipeKind::DenoTest => &["--junit-path"],
        RecipeKind::Ruff => &["--output-format", "--output-file"],
        RecipeKind::Biome => &["--reporter", "--reporter-file"],
        RecipeKind::Semgrep => &["--sarif", "--sarif-output", "--output", "-o"],
        RecipeKind::CargoTest
        | RecipeKind::Pytest
        | RecipeKind::NpmTest
        | RecipeKind::GoTest
        | RecipeKind::GhIssues
        | RecipeKind::DiffReview
        | RecipeKind::LogsRootCause => &[],
    };
    if let Some(conflict) = command
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .find(|argument| {
            conflicts.iter().any(|option| {
                *argument == *option
                    || argument
                        .strip_prefix(option)
                        .is_some_and(|suffix| suffix.starts_with('='))
            })
        })
    {
        return Err(CoreError::BadArgs {
            operation: format!("recipe {}", recipe.as_str()),
            reason: format!(
                "report recipes manage their own output file; remove conflicting option '{conflict}'"
            ),
        });
    }
    Ok(())
}

fn run_exit_metadata(exit: RunExitCode) -> Value {
    match exit {
        RunExitCode::Success => json!({"kind": "success", "code": 0}),
        RunExitCode::Code(code) => json!({"kind": "code", "code": code}),
        RunExitCode::Signal(signal) => json!({"kind": "signal", "signal": signal}),
        RunExitCode::Timeout => json!({"kind": "timeout"}),
        RunExitCode::SpawnError => json!({"kind": "spawn_error"}),
    }
}

fn configure_recipe_command(recipe: RecipeKind, mut command: Vec<String>) -> Vec<String> {
    if recipe != RecipeKind::CargoTest
        || command
            .first()
            .and_then(|program| std::path::Path::new(program).file_name())
            .and_then(|program| program.to_str())
            != Some("cargo")
    {
        return command;
    }
    let subcommand_index = 1 + usize::from(
        command
            .get(1)
            .is_some_and(|argument| argument.starts_with('+')),
    );
    if command.get(subcommand_index).map(String::as_str) != Some("test")
        || command.iter().any(|argument| {
            argument == "--message-format" || argument.starts_with("--message-format=")
        })
    {
        return command;
    }
    let insertion = command
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(command.len());
    command.insert(insertion, "--message-format=json".to_string());
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn cargo_recipe_adds_one_visible_structured_output_option() {
        assert_eq!(
            configure_recipe_command(RecipeKind::CargoTest, argv(&["cargo", "test"])),
            argv(&["cargo", "test", "--message-format=json"])
        );
        assert_eq!(
            configure_recipe_command(
                RecipeKind::CargoTest,
                argv(&["cargo", "+stable", "test", "--", "one_case"]),
            ),
            argv(&[
                "cargo",
                "+stable",
                "test",
                "--message-format=json",
                "--",
                "one_case",
            ])
        );
        assert_eq!(
            configure_recipe_command(
                RecipeKind::CargoTest,
                argv(&["cargo", "test", "--message-format=short"]),
            ),
            argv(&["cargo", "test", "--message-format=short"])
        );
    }

    #[test]
    fn generated_report_options_are_exact_argv_and_precede_harness_arguments() {
        let report = Path::new("/tmp/prog report.xml");
        assert_eq!(
            configure_report_command(
                RecipeKind::Vitest,
                argv(&["vitest", "run", "--", "case name"]),
                report,
            ),
            argv(&[
                "vitest",
                "run",
                "--reporter=junit",
                "--outputFile=/tmp/prog report.xml",
                "--",
                "case name",
            ])
        );
        assert_eq!(
            configure_report_command(
                RecipeKind::Playwright,
                argv(&["playwright", "test"]),
                report,
            ),
            argv(&[
                "env",
                "PLAYWRIGHT_JUNIT_OUTPUT_FILE=/tmp/prog report.xml",
                "playwright",
                "test",
                "--reporter=junit",
            ])
        );
        assert_eq!(
            configure_report_command(RecipeKind::Biome, argv(&["biome", "check"]), report),
            argv(&[
                "biome",
                "check",
                "--reporter=sarif",
                "--reporter-file=/tmp/prog report.xml",
            ])
        );
    }

    #[test]
    fn report_recipes_reject_user_owned_report_destinations() {
        let command = argv(&["ruff", "check", "--output-file=mine.sarif"]);
        let error = reject_conflicting_report_options(RecipeKind::Ruff, &command).unwrap_err();
        assert!(error.to_string().contains("remove conflicting option"));

        let harness_argument = argv(&["vitest", "run", "--", "--reporter=fixture"]);
        assert!(reject_conflicting_report_options(RecipeKind::Vitest, &harness_argument).is_ok());
    }
}

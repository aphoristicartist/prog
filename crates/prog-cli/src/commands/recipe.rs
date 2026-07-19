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

    let (mut envelope, expanded_commands) = match args.recipe {
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
            let lens = match recipe {
                RecipeKind::CargoTest => "cargo-test",
                RecipeKind::Pytest => "pytest",
                RecipeKind::NpmTest => "npm-test",
                RecipeKind::GoTest => "go-test",
                RecipeKind::GhIssues => "github-issues",
                RecipeKind::DiffReview | RecipeKind::LogsRootCause => unreachable!(),
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
            .evidence
            .clone()
            .or(finding.commands.expand.clone())
    });
    envelope.extra.insert(
        "recipe".to_string(),
        json!({
            "id": args.recipe.as_str(),
            "goal": goal,
            "expanded_commands": expanded_commands,
            "recommended_next": recommended_next,
            "deterministic": true
        }),
    );
    compact_envelope_to_budget(&mut envelope, ctx.max_envelope_bytes())?;
    Ok(envelope)
}

fn default_recipe_command(recipe: RecipeKind) -> Vec<String> {
    match recipe {
        RecipeKind::CargoTest => vec!["cargo".to_string(), "test".to_string()],
        RecipeKind::Pytest => vec!["pytest".to_string()],
        RecipeKind::NpmTest => vec!["npm".to_string(), "test".to_string()],
        RecipeKind::GoTest => vec!["go".to_string(), "test".to_string(), "./...".to_string()],
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

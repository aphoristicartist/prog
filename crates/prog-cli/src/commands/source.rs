//! Source onboarding commands.

use crate::*;

pub(crate) async fn source_command(
    store: &Store,
    dir: &Path,
    command: &SourceCommand,
) -> Result<SourceAddReport> {
    match command {
        SourceCommand::AddHttp(args) => source_add_http(store, dir, args).await,
        SourceCommand::AddCli(args) => source_add_cli(store, dir, args).await,
    }
}

async fn source_add_http(
    store: &Store,
    dir: &Path,
    args: &SourceAddHttpArgs,
) -> Result<SourceAddReport> {
    let operation = source_add_operation(&args.operation, "source add-http")?;
    let method = normalize_http_method(&args.method)?;
    let url = split_http_url(&args.url)?;
    let read_only = method == "GET";
    let mut warnings = Vec::new();
    if !read_only {
        warnings.push(format!(
            "generated HTTP operation '{}' is confirmation-gated because method '{}' is not GET",
            operation, method
        ));
    }
    let seed = json!({
        "kind": "http",
        "base_url": url.base_url,
        "operations": [{
            "name": operation,
            "method": method,
            "path": url.path,
            "query": url.query,
            "effect": generated_effect(read_only, true, false)
        }]
    });
    let discovery = discover_from_seed(
        store,
        &args.source_id,
        SourceKind::Http,
        seed.clone(),
        args.probe,
    )
    .await?;
    warnings.extend(discovery.warnings.clone());
    let next_steps = source_add_next_steps(dir, &args.source_id, &operation, !read_only);
    Ok(SourceAddReport {
        schema: DISCLOSURE_SCHEMA,
        source_id: args.source_id.clone(),
        kind: prog_core::SourceKind::Http,
        operation,
        generated_seed: seed,
        next_steps,
        structured_output: Vec::new(),
        warnings,
        discovery,
    })
}

async fn source_add_cli(
    store: &Store,
    dir: &Path,
    args: &SourceAddCliArgs,
) -> Result<SourceAddReport> {
    let operation = source_add_operation(&args.operation, "source add-cli")?;
    let Some((command, original_command_args)) = args.command.split_first() else {
        return Err(CoreError::BadArgs {
            operation: "source add-cli".to_string(),
            reason: "pass a command after --".to_string(),
        });
    };
    let read_only = args.read_only;
    let mut warnings = Vec::new();
    if !read_only {
        warnings.push(format!(
            "generated CLI operation '{}' is confirmation-gated; pass --read-only only for commands safe to invoke automatically",
            operation
        ));
    }
    let mut command_args = original_command_args.to_vec();
    let mut structured_output = cli_structured_output_hints(command, &command_args);
    if args.prefer_json {
        let applicable = structured_output
            .iter()
            .find(|hint| hint.status == "suggested" && hint.confidence == "high")
            .cloned()
            .ok_or_else(|| CoreError::BadArgs {
                operation: "source add-cli --prefer-json".to_string(),
                reason: "no high-confidence structured-output flag is known for this invocation; add the CLI's JSON flag explicitly after --".to_string(),
            })?;
        command_args.extend(applicable.flag.clone());
        structured_output = cli_structured_output_hints(command, &command_args);
        warnings.push(format!(
            "applied structured-output flag {} after explicit --prefer-json",
            applicable.flag.join(" ")
        ));
    } else if let Some(hint) = structured_output
        .iter()
        .find(|hint| hint.status == "suggested")
    {
        warnings.push(format!(
            "structured output available: add {} after --, or pass --prefer-json when the suggestion is high-confidence",
            hint.flag.join(" ")
        ));
    }
    let seed = json!({
        "kind": "cli",
        "operations": [{
            "name": operation,
            "command": command,
            "args": command_args,
            "effect": generated_effect(read_only, false, false)
        }]
    });
    let discovery = discover_from_seed(
        store,
        &args.source_id,
        SourceKind::Cli,
        seed.clone(),
        args.probe,
    )
    .await?;
    warnings.extend(discovery.warnings.clone());
    let next_steps = source_add_next_steps(dir, &args.source_id, &operation, !read_only);
    Ok(SourceAddReport {
        schema: DISCLOSURE_SCHEMA,
        source_id: args.source_id.clone(),
        kind: prog_core::SourceKind::Cli,
        operation,
        generated_seed: seed,
        next_steps,
        structured_output,
        warnings,
        discovery,
    })
}

fn cli_structured_output_hints(command: &str, args: &[String]) -> Vec<StructuredOutputHint> {
    let program = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();
    if let Some(flag) = detected_json_flag(args) {
        return vec![StructuredOutputHint {
            status: "detected",
            flag,
            confidence: "high",
            reason: "the authored command already requests structured output".to_string(),
        }];
    }

    let words = args.iter().map(String::as_str).collect::<Vec<_>>();
    let hint = match program.as_str() {
        "kubectl" if words.first().is_some_and(|word| *word == "get") => {
            Some(StructuredOutputHint {
                status: "suggested",
                flag: vec!["-o".to_string(), "json".to_string()],
                confidence: "high",
                reason: "kubectl get has a stable JSON output mode".to_string(),
            })
        }
        "gh" if words.starts_with(&["issue", "list"]) => Some(StructuredOutputHint {
            status: "suggested",
            flag: vec![
                "--json".to_string(),
                "number,title,state,labels,updatedAt,url".to_string(),
            ],
            confidence: "high",
            reason: "gh issue list supports an explicit JSON field set".to_string(),
        }),
        "gh" if words.starts_with(&["pr", "list"]) => Some(StructuredOutputHint {
            status: "suggested",
            flag: vec![
                "--json".to_string(),
                "number,title,state,author,labels,updatedAt,url".to_string(),
            ],
            confidence: "high",
            reason: "gh pr list supports an explicit JSON field set".to_string(),
        }),
        "gh" if words.starts_with(&["repo", "list"]) => Some(StructuredOutputHint {
            status: "suggested",
            flag: vec![
                "--json".to_string(),
                "nameWithOwner,description,isPrivate,updatedAt,url".to_string(),
            ],
            confidence: "high",
            reason: "gh repo list supports an explicit JSON field set".to_string(),
        }),
        "cargo"
            if !words.contains(&"--")
                && words
                    .first()
                    .is_some_and(|word| matches!(*word, "build" | "check" | "clippy" | "test")) =>
        {
            Some(StructuredOutputHint {
                status: "suggested",
                flag: vec!["--message-format=json".to_string()],
                confidence: "high",
                reason: "cargo supports newline-delimited JSON compiler messages".to_string(),
            })
        }
        "npm"
            if words.first().is_some_and(|word| {
                matches!(*word, "audit" | "list" | "ls" | "outdated" | "view")
            }) =>
        {
            Some(StructuredOutputHint {
                status: "suggested",
                flag: vec!["--json".to_string()],
                confidence: "high",
                reason: "this npm command supports JSON output".to_string(),
            })
        }
        _ => None,
    };
    hint.into_iter().collect()
}

fn detected_json_flag(args: &[String]) -> Option<Vec<String>> {
    for (index, arg) in args.iter().enumerate() {
        let normalized = arg.to_ascii_lowercase();
        if matches!(normalized.as_str(), "--json" | "--json=true")
            || normalized.starts_with("--message-format=json")
            || normalized.starts_with("--format=json")
            || normalized.starts_with("--output=json")
        {
            return Some(vec![arg.clone()]);
        }
        if matches!(normalized.as_str(), "--format" | "--output" | "-o")
            && args
                .get(index + 1)
                .is_some_and(|value| value.eq_ignore_ascii_case("json"))
        {
            return Some(vec![arg.clone(), args[index + 1].clone()]);
        }
    }
    None
}

fn source_add_operation(operation: &str, context: &str) -> Result<String> {
    let operation = operation.trim();
    if operation.is_empty() {
        return Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: "--operation must not be empty".to_string(),
        });
    }
    Ok(operation.to_string())
}

fn source_add_next_steps(
    dir: &Path,
    source_id: &str,
    operation: &str,
    needs_yes: bool,
) -> Vec<String> {
    let confirmation = if needs_yes { " --yes" } else { "" };
    let dir = shell_quote(&dir.to_string_lossy());
    vec![
        format!("prog --dir {dir} hints {source_id} {operation}"),
        format!("prog --dir {dir} call {source_id} {operation} --args '{{}}'{confirmation}"),
    ]
}

pub(crate) fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn generated_effect(read_only: bool, network: bool, shell: bool) -> Value {
    json!({
        "read_only": read_only,
        "mutating": !read_only,
        "network": network,
        "shell": shell,
        "sensitive": !read_only,
        "cacheable": read_only,
        "requires_confirmation": !read_only
    })
}

fn normalize_http_method(method: &str) -> Result<String> {
    let method = method.trim().to_ascii_uppercase();
    if method.is_empty() || !method.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return Err(CoreError::BadArgs {
            operation: "source add-http".to_string(),
            reason: "--method must be an HTTP token such as GET or POST".to_string(),
        });
    }
    Ok(method)
}

#[derive(Debug, PartialEq, Eq)]
struct HttpUrlParts {
    base_url: String,
    path: String,
    query: BTreeMap<String, String>,
}

fn split_http_url(raw: &str) -> Result<HttpUrlParts> {
    let raw = raw.trim();
    let Some((scheme, rest)) = raw.split_once("://") else {
        return Err(CoreError::BadArgs {
            operation: "source add-http".to_string(),
            reason: "--url must include http:// or https://".to_string(),
        });
    };
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return Err(CoreError::BadArgs {
            operation: "source add-http".to_string(),
            reason: "--url scheme must be http or https".to_string(),
        });
    }
    if rest.contains('#') {
        return Err(CoreError::BadArgs {
            operation: "source add-http".to_string(),
            reason: "--url fragments are not part of HTTP requests; remove the fragment"
                .to_string(),
        });
    }
    let split_at = rest
        .find(|ch| ['/', '?'].contains(&ch))
        .unwrap_or(rest.len());
    let authority = &rest[..split_at];
    if authority.is_empty() || authority.contains('@') {
        return Err(CoreError::BadArgs {
            operation: "source add-http".to_string(),
            reason: "--url must include a host and must not embed credentials".to_string(),
        });
    }
    let tail = &rest[split_at..];
    let (path, query_raw) = if tail.is_empty() {
        ("/", "")
    } else if let Some(query) = tail.strip_prefix('?') {
        ("/", query)
    } else if let Some((path, query)) = tail.split_once('?') {
        (if path.is_empty() { "/" } else { path }, query)
    } else {
        (tail, "")
    };
    let query = split_http_query(query_raw)?;
    Ok(HttpUrlParts {
        base_url: format!("{scheme}://{authority}"),
        path: path.to_string(),
        query,
    })
}

fn split_http_query(raw: &str) -> Result<BTreeMap<String, String>> {
    let mut query = BTreeMap::new();
    if raw.is_empty() {
        return Ok(query);
    }
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(CoreError::BadArgs {
                operation: "source add-http".to_string(),
                reason: format!("query parameter '{pair}' must use key=value form"),
            });
        };
        if key.is_empty() {
            return Err(CoreError::BadArgs {
                operation: "source add-http".to_string(),
                reason: "query parameter names must not be empty".to_string(),
            });
        }
        if query.insert(key.to_string(), value.to_string()).is_some() {
            return Err(CoreError::BadArgs {
                operation: "source add-http".to_string(),
                reason: format!("query parameter '{key}' appears more than once"),
            });
        }
    }
    Ok(query)
}

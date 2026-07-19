//! Source discovery command orchestration.

use crate::*;

fn read_seed(seed: &str) -> Result<Value> {
    let trimmed = seed.trim_start();
    let raw = if trimmed.starts_with('{') || trimmed.starts_with('[') {
        seed.to_string()
    } else {
        std::fs::read_to_string(seed).map_err(|error| CoreError::BadArgs {
            operation: "discover".to_string(),
            reason: format!("seed path '{seed}' could not be read: {error}"),
        })?
    };
    serde_json::from_str(&raw).map_err(|error| CoreError::BadArgs {
        operation: "discover".to_string(),
        reason: format!("seed must be valid JSON: {error}"),
    })
}

fn read_import_raw(seed: &str) -> Result<String> {
    let path = Path::new(seed);
    if path.exists() {
        std::fs::read_to_string(path).map_err(|error| CoreError::BadArgs {
            operation: "discover --import".to_string(),
            reason: format!("import path '{seed}' could not be read: {error}"),
        })
    } else {
        Ok(seed.to_string())
    }
}

fn import_profile_from_raw(
    args: &DiscoverArgs,
    format: ImportFormat,
    raw: &str,
    ctx: &ImportContext,
) -> Result<(SourceProfile, ImportReport, &'static str)> {
    match format {
        ImportFormat::Openapi => {
            require_import_kind(args.kind, SourceKind::Http, format)?;
            let value = parse_import_json(raw, format)?;
            let (profile, report) = import_openapi(args.source_id.clone(), &value, ctx)?;
            Ok((profile, report, format.as_str()))
        }
        ImportFormat::JsonSchema => {
            require_import_kind(args.kind, SourceKind::Http, format)?;
            let value = parse_import_json(raw, format)?;
            let (profile, report) = import_json_schema(args.source_id.clone(), &value, ctx)?;
            Ok((profile, report, format.as_str()))
        }
        ImportFormat::CliHelp => {
            require_import_kind(args.kind, SourceKind::Cli, format)?;
            let command_base = args.command_base.as_deref().ok_or_else(|| CoreError::BadArgs {
                operation: "discover --import cli-help".to_string(),
                reason: "pass --command-base <command> so the generated profile has an explicit executable".to_string(),
            })?;
            let (profile, report) =
                import_cli_help(args.source_id.clone(), raw, command_base, ctx)?;
            Ok((profile, report, format.as_str()))
        }
        ImportFormat::Auto => import_profile_auto(args, raw, ctx),
    }
}

fn import_profile_auto(
    args: &DiscoverArgs,
    raw: &str,
    ctx: &ImportContext,
) -> Result<(SourceProfile, ImportReport, &'static str)> {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        if value
            .get("openapi")
            .and_then(Value::as_str)
            .is_some_and(|version| version.starts_with("3."))
        {
            require_import_kind(args.kind, SourceKind::Http, ImportFormat::Auto)?;
            let (profile, report) = import_openapi(args.source_id.clone(), &value, ctx)?;
            return Ok((profile, report, ImportFormat::Openapi.as_str()));
        }
        if value.get("$schema").is_some() || value.get("type").is_some() {
            require_import_kind(args.kind, SourceKind::Http, ImportFormat::Auto)?;
            let (profile, report) = import_json_schema(args.source_id.clone(), &value, ctx)?;
            return Ok((profile, report, ImportFormat::JsonSchema.as_str()));
        }
    }

    if args.kind == SourceKind::Cli {
        let command_base = args
            .command_base
            .as_deref()
            .ok_or_else(|| CoreError::BadArgs {
                operation: "discover --import auto".to_string(),
                reason: "CLI help auto-import requires --command-base <command>".to_string(),
            })?;
        let (profile, report) = import_cli_help(args.source_id.clone(), raw, command_base, ctx)?;
        return Ok((profile, report, ImportFormat::CliHelp.as_str()));
    }

    Err(CoreError::BadArgs {
        operation: "discover --import auto".to_string(),
        reason: "could not detect OpenAPI 3.x, JSON Schema, or CLI help import".to_string(),
    })
}

fn parse_import_json(raw: &str, format: ImportFormat) -> Result<Value> {
    serde_json::from_str(raw).map_err(|error| CoreError::BadArgs {
        operation: format!("discover --import {}", format.as_str()),
        reason: format!("import input must be valid JSON: {error}"),
    })
}

fn require_import_kind(
    actual: SourceKind,
    expected: SourceKind,
    format: ImportFormat,
) -> Result<()> {
    if actual == expected {
        return Ok(());
    }
    Err(CoreError::BadArgs {
        operation: format!("discover --import {}", format.as_str()),
        reason: format!("--kind must be {expected:?} for this import format"),
    })
}

fn validate_seed_kind(kind: SourceKind, seed: &Value) -> Result<()> {
    let generic: GenericSeed =
        serde_json::from_value(seed.clone()).map_err(|error| CoreError::BadArgs {
            operation: "discover".to_string(),
            reason: format!("seed.kind is malformed: {error}"),
        })?;
    let Some(seed_kind) = generic.kind else {
        return Ok(());
    };
    let expected = match kind {
        SourceKind::Http => "http",
        SourceKind::Cli => "cli",
        SourceKind::Mcp => "mcp",
    };
    if seed_kind != expected {
        return Err(CoreError::BadArgs {
            operation: "discover".to_string(),
            reason: format!("seed.kind must be '{expected}', got '{seed_kind}'"),
        });
    }
    Ok(())
}

async fn prepare_discovery(
    source_id: &str,
    kind: SourceKind,
    seed: Value,
) -> Result<PreparedDiscovery> {
    if seed.get("schema_version").is_some() {
        return Err(CoreError::BadArgs {
            operation: "source discovery seed".to_string(),
            reason: "schema_version is unsupported; regenerate this pre-release profile"
                .to_string(),
        });
    }
    if seed.get("schema").is_some() {
        let mut profile: SourceProfile = serde_json::from_value(seed)?;
        profile.id = source_id.to_string();
        profile.kind = core_kind(kind);
        return Ok(PreparedDiscovery {
            profile,
            probe: None,
            warnings: Vec::new(),
            effects_assumed: Vec::new(),
        });
    }

    match kind {
        SourceKind::Http => prepare_http_seed(source_id, &seed),
        SourceKind::Cli => prepare_cli_seed(source_id, &seed),
        SourceKind::Mcp => prepare_mcp_seed(source_id, seed).await,
    }
}

fn prepare_http_seed(source_id: &str, seed: &Value) -> Result<PreparedDiscovery> {
    let base_url = required_string(seed, "base_url")?;
    let auth = auth_refs(seed)?;
    let operations_value = required_array(seed, "operations")?;
    let mut operations = Vec::new();
    let mut http_operations = Vec::new();
    let mut effects_assumed = Vec::new();

    for operation_value in operations_value {
        let id = operation_id(operation_value)?;
        let method =
            optional_string(operation_value, "method")?.unwrap_or_else(|| "GET".to_string());
        let path = required_string(operation_value, "path")?;
        let input_schema = input_schema(operation_value)?;
        let (effects, assumed) = effects_from_seed(
            operation_value
                .get("effect")
                .or_else(|| operation_value.get("effects")),
            http_adapter_effects(&method),
            http_hardening_effects(&method),
            "operations[].effects",
        )?;
        if assumed {
            effects_assumed.push(format!("{id}: no effect metadata, assumed unsafe"));
        }
        let query = string_map(operation_value.get("query"), "operations[].query")?;
        let headers = string_map(operation_value.get("headers"), "operations[].headers")?;
        let json_body = operation_value
            .get("json_body")
            .or_else(|| operation_value.get("body"))
            .cloned();
        let sensitive_args = string_vec(
            operation_value.get("sensitive_args"),
            "operations[].sensitive_args",
        )?;
        let mut extra = Extra::new();
        extra.insert(
            "invocation".to_string(),
            json!({"http": {
                "method": method.clone(),
                "path": path.clone(),
                "query": query.clone(),
                "headers": headers.clone(),
                "json_body": json_body.clone(),
                "sensitive_args": sensitive_args.clone()
            }}),
        );
        operations.push(OperationProfile {
            id: id.clone(),
            description: optional_string(operation_value, "description")?,
            input_schema,
            output_shape: None,
            declared_output_schema: operation_value.get("declared_output_schema").cloned(),
            effects,
            cache: CachePolicy::default(),
            pagination: None,
            extra,
        });
        http_operations.push(HttpOperation {
            id,
            method,
            path,
            query,
            headers,
            json_body,
            timeout_ms: None,
            max_response_bytes: None,
            sensitive_args,
        });
    }

    Ok(PreparedDiscovery {
        profile: SourceProfile {
            schema: SOURCE_PROFILE_SCHEMA.to_string(),
            id: source_id.to_string(),
            kind: prog_core::SourceKind::Http,
            revision: 1,
            description: optional_string(seed, "description")?,
            operations,
            auth: auth.clone(),
            cache: CachePolicy::default(),
            trust: TrustSettings {
                allow_network: true,
                ..TrustSettings::default()
            },
            effect_defaults: EffectSet::default(),
            redaction: prog_core::RedactionConfig::default(),
            disclosure_budget: None,
            extra: adapter_seed_extra(
                "http",
                seed,
                json!({"http": {
                    "base_url": base_url.clone(),
                    "timeout_ms": 30_000,
                    "max_response_bytes": DEFAULT_MAX_RESPONSE_BYTES,
                    "default_headers": {},
                    "response_header_allowlist": []
                }}),
            ),
        },
        probe: Some(ProbeSource::Http(HttpSource {
            id: source_id.to_string(),
            base_url,
            timeout_ms: 30_000,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            default_headers: BTreeMap::new(),
            response_header_allowlist: Vec::new(),
            auth,
            operations: http_operations,
        })),
        warnings: Vec::new(),
        effects_assumed,
    })
}

fn prepare_cli_seed(source_id: &str, seed: &Value) -> Result<PreparedDiscovery> {
    let operations_value = required_array(seed, "operations")?;
    let trust: TrustSettings = seed
        .get("trust")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let mut operations = Vec::new();
    let mut cli_operations = Vec::new();
    let mut effects_assumed = Vec::new();

    for operation_value in operations_value {
        let id = operation_id(operation_value)?;
        let command = required_string(operation_value, "command")?;
        let args = string_vec(operation_value.get("args"), "operations[].args")?;
        let input_schema = input_schema(operation_value)?;
        let shell = optional_bool(operation_value, "shell")?.unwrap_or(false);
        let (effects, assumed) = effects_from_seed(
            operation_value
                .get("effect")
                .or_else(|| operation_value.get("effects")),
            cli_adapter_effects(shell),
            cli_hardening_effects(shell),
            "operations[].effects",
        )?;
        if assumed {
            effects_assumed.push(format!("{id}: no effect metadata, assumed unsafe"));
        }
        let env = string_map(operation_value.get("env"), "operations[].env")?;
        let working_dir = operation_value
            .get("working_dir")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let sensitive_args = string_vec(
            operation_value.get("sensitive_args"),
            "operations[].sensitive_args",
        )?;
        let mut extra = Extra::new();
        extra.insert(
            "invocation".to_string(),
            json!({"cli": {
                "command": command.clone(),
                "args": args.clone(),
                "env": env.clone(),
                "working_dir": working_dir.clone(),
                "shell": shell,
                "sensitive_args": sensitive_args.clone()
            }}),
        );
        operations.push(OperationProfile {
            id: id.clone(),
            description: optional_string(operation_value, "description")?,
            input_schema: input_schema.clone(),
            output_shape: None,
            declared_output_schema: operation_value.get("declared_output_schema").cloned(),
            effects,
            cache: CachePolicy::default(),
            pagination: None,
            extra,
        });
        cli_operations.push(CliOperation {
            id,
            input_schema,
            command,
            args,
            env,
            working_dir,
            shell,
            timeout_ms: None,
            max_stdout_bytes: None,
            max_stderr_bytes: None,
            sensitive_args,
        });
    }

    Ok(PreparedDiscovery {
        profile: SourceProfile {
            schema: SOURCE_PROFILE_SCHEMA.to_string(),
            id: source_id.to_string(),
            kind: prog_core::SourceKind::Cli,
            revision: 1,
            description: optional_string(seed, "description")?,
            operations,
            auth: Vec::new(),
            cache: CachePolicy::default(),
            trust: trust.clone(),
            effect_defaults: EffectSet::default(),
            redaction: prog_core::RedactionConfig::default(),
            disclosure_budget: None,
            extra: adapter_seed_extra(
                "cli",
                seed,
                json!({"cli": {
                    "timeout_ms": 30_000,
                    "max_stdout_bytes": 1024 * 1024,
                    "max_stderr_bytes": 1024 * 1024
                }}),
            ),
        },
        probe: Some(ProbeSource::Cli(CliSource {
            id: source_id.to_string(),
            timeout_ms: 30_000,
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 1024 * 1024,
            trust,
            operations: cli_operations,
        })),
        warnings: Vec::new(),
        effects_assumed,
    })
}

async fn prepare_mcp_seed(source_id: &str, mut seed: Value) -> Result<PreparedDiscovery> {
    let object = seed.as_object_mut().ok_or_else(|| CoreError::BadArgs {
        operation: "discover".to_string(),
        reason: "MCP seed must be a JSON object".to_string(),
    })?;
    object.insert("id".to_string(), json!(source_id));
    let source: McpSource = serde_json::from_value(seed).map_err(|error| CoreError::BadArgs {
        operation: "discover".to_string(),
        reason: format!("MCP seed is malformed: {error}"),
    })?;
    let discovery = source.discover().await?;
    let mut profile = discovery.profile;
    profile.extra.insert(
        "adapter".to_string(),
        json!({"mcp": {
            "command": source.command.clone(),
            "args": source.args.clone(),
            "env": source.env.clone(),
            "timeout_ms": source.timeout_ms,
            "max_content_bytes": source.max_content_bytes,
            "max_stderr_bytes": source.max_stderr_bytes,
            "max_schema_depth": source.max_schema_depth
        }}),
    );
    Ok(PreparedDiscovery {
        profile,
        probe: Some(ProbeSource::Mcp(source)),
        warnings: discovery.warnings,
        effects_assumed: Vec::new(),
    })
}

pub(crate) fn profile_operation<'a>(
    profile: &'a SourceProfile,
    operation: &str,
) -> Result<&'a OperationProfile> {
    profile
        .operations
        .iter()
        .find(|candidate| candidate.id == operation)
        .ok_or_else(|| CoreError::UnknownOperation {
            source_id: profile.id.clone(),
            operation: operation.to_string(),
        })
}

async fn probe_profile(
    profile: &mut SourceProfile,
    probe: &ProbeSource,
    warnings: &mut Vec<String>,
    operations_probed: &mut usize,
    shapes_learned: &mut usize,
) {
    for index in 0..profile.operations.len() {
        let operation = &profile.operations[index];
        // Discovery now evaluates the EFFECTIVE effect set: under default
        // trust a *proven* read-only op is probeable (its confirmation is
        // relaxed); flipping trust.auto_upgrade=false re-gates it and the I6
        // skip fires (strict-when-disabled).
        if let Err(error) = check_discovery(operation, &profile.trust) {
            warnings.push(format!("I6: skipped probe for '{}': {error}", operation.id));
            continue;
        }
        let args = example_args(&operation.input_schema);
        let result = match probe {
            ProbeSource::Http(source) => source
                .execute_with_env(&operation.id, &args, &|name| std::env::var(name).ok())
                .await
                .map(|result| result.data),
            ProbeSource::Cli(source) => source
                .execute(&operation.id, &args)
                .await
                .map(|result| result.data),
            ProbeSource::Mcp(source) => {
                let mcp_invocation = operation
                    .extra
                    .get("invocation")
                    .and_then(|value| value.get("mcp"))
                    .and_then(Value::as_object);
                if mcp_invocation
                    .and_then(|value| value.get("kind"))
                    .and_then(Value::as_str)
                    == Some("tool")
                    && let Some(tool_name) = mcp_invocation
                        .and_then(|value| value.get("name"))
                        .and_then(Value::as_str)
                {
                    source
                        .call_tool(tool_name, &args)
                        .await
                        .map(|result| result.data)
                } else {
                    warnings.push(format!(
                        "I6: skipped probe for '{}' because no MCP tool invocation was derivable",
                        operation.id
                    ));
                    continue;
                }
            }
        };

        match result {
            Ok(data) => {
                *operations_probed += 1;
                *shapes_learned += 1;
                learn_from_probe(&mut profile.operations[index], &args, &data);
            }
            Err(error) => warnings.push(format!("probe for '{}' failed: {}", operation.id, error)),
        }
    }
}

fn learn_from_probe(operation: &mut OperationProfile, args: &Value, data: &Value) {
    let redacted = RawPayload::new(data.clone()).redact(&RedactionPolicy::default());
    let redacted = redacted.payload;
    let observed = infer(redacted.as_value());
    operation.output_shape = Some(match &operation.output_shape {
        Some(current) => join(current, &observed),
        None => observed,
    });
    // Infer the pagination shape from the probe response body and record it as
    // a capability hint on the operation (discover never auto-fetches, per I6).
    // `call` reads live hints first and falls back to this stored hint.
    if operation.pagination.is_none()
        && let Some(hint) = prog_core::extract_pagination_hints(redacted.as_value(), None)
    {
        operation.pagination = Some(hint);
    }
    let projection = project(redacted.as_value(), &PreviewPolicy::default(), "");
    let redacted_args = redacted_profile_args(operation, args);
    let examples = operation
        .extra
        .entry("examples".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(examples) = examples {
        examples.push(json!({
            "args": redacted_args,
            "projection": projection
        }));
    }
}

pub(crate) async fn discover_source(store: &Store, args: &DiscoverArgs) -> Result<DiscoverReport> {
    if let Some(format) = args.import {
        return discover_from_import(store, args, format).await;
    }
    let seed = read_seed(&args.seed)?;
    discover_from_seed(store, &args.source_id, args.kind, seed, args.probe).await
}

async fn discover_from_import(
    store: &Store,
    args: &DiscoverArgs,
    format: ImportFormat,
) -> Result<DiscoverReport> {
    let raw = read_import_raw(&args.seed)?;
    let ctx = ImportContext {
        max_schema_depth: args.max_schema_depth,
        ..ImportContext::default()
    };
    let (profile, report, import_format) = import_profile_from_raw(args, format, &raw, &ctx)?;
    let expected = core_kind(args.kind);
    if profile.kind != expected {
        return Err(CoreError::BadArgs {
            operation: "discover --import".to_string(),
            reason: format!(
                "--kind {:?} does not match imported profile kind {:?}",
                expected, profile.kind
            ),
        });
    }
    let mut warnings = report.warnings.clone();
    warnings.extend(
        report
            .errors
            .iter()
            .map(|error| format!("import warning: {error}")),
    );
    if args.probe {
        warnings.push(
            "probe is skipped for imported profiles; import never executes upstream calls"
                .to_string(),
        );
    }
    let source_id = args.source_id.clone();
    let profile = store.update_profile(&source_id, |current| {
        merge_profiles(current, profile.clone())
    })?;
    Ok(DiscoverReport {
        schema: DISCLOSURE_SCHEMA,
        source_id,
        kind: profile.kind,
        profile_revision: profile.revision,
        operations_found: report.operations_imported,
        operations_probed: 0,
        shapes_learned: 0,
        import_format: Some(import_format.to_string()),
        schemas_imported: report.schemas_imported,
        examples_inferred: report.examples_inferred,
        warnings,
        effects_assumed: Vec::new(),
    })
}

pub(crate) async fn discover_from_seed(
    store: &Store,
    source_id: &str,
    kind: SourceKind,
    seed: Value,
    probe: bool,
) -> Result<DiscoverReport> {
    validate_seed_kind(kind, &seed)?;
    let mut prepared = prepare_discovery(source_id, kind, seed).await?;
    let operations_found = prepared.profile.operations.len();
    let mut operations_probed = 0usize;
    let mut shapes_learned = 0usize;

    if probe {
        let probe = prepared.probe.take();
        if let Some(probe) = &probe {
            probe_profile(
                &mut prepared.profile,
                probe,
                &mut prepared.warnings,
                &mut operations_probed,
                &mut shapes_learned,
            )
            .await;
        } else {
            prepared.warnings.push(
                "probe requested, but this seed cannot be executed by the V1 probe path"
                    .to_string(),
            );
        }
    }

    let profile = store.update_profile(source_id, |current| {
        merge_profiles(current, prepared.profile.clone())
    })?;

    Ok(DiscoverReport {
        schema: DISCLOSURE_SCHEMA,
        source_id: source_id.to_string(),
        kind: profile.kind,
        profile_revision: profile.revision,
        operations_found,
        operations_probed,
        shapes_learned,
        import_format: None,
        schemas_imported: 0,
        examples_inferred: 0,
        warnings: prepared.warnings,
        effects_assumed: prepared.effects_assumed,
    })
}

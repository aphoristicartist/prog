//! Source adapter construction, execution, and call policy helpers.

use crate::*;

pub(crate) fn validate_call_args(operation: &OperationProfile, args: &Value) -> Result<()> {
    let args = args.as_object().ok_or_else(|| CoreError::BadArgs {
        operation: operation.id.clone(),
        reason: "args must be a JSON object".to_string(),
    })?;
    let Some(schema) = operation.input_schema.as_object() else {
        if args.is_empty() || operation.input_schema.is_null() {
            return Ok(());
        }
        return Err(CoreError::BadArgs {
            operation: operation.id.clone(),
            reason: "input_schema must be an object when args are supplied".to_string(),
        });
    };
    if let Some(schema_type) = schema.get("type").and_then(Value::as_str)
        && schema_type != "object"
    {
        return Err(CoreError::BadArgs {
            operation: operation.id.clone(),
            reason: "input_schema.type must be 'object'".to_string(),
        });
    }

    let required = schema_string_set(
        schema.get("required"),
        &operation.id,
        "input_schema.required",
    )?;
    let properties = schema
        .get("properties")
        .map(|value| {
            value
                .as_object()
                .map(|properties| properties.keys().cloned().collect::<BTreeSet<_>>())
                .ok_or_else(|| CoreError::BadArgs {
                    operation: operation.id.clone(),
                    reason: "input_schema.properties must be an object".to_string(),
                })
        })
        .transpose()?
        .unwrap_or_default();
    let mut allowed = properties;
    allowed.extend(required.iter().cloned());
    let allow_unknown = schema
        .get("additional_properties")
        .or_else(|| schema.get("additionalProperties"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let missing = required
        .iter()
        .filter(|name| !args.contains_key(*name))
        .cloned()
        .collect::<Vec<_>>();
    let unknown = if allow_unknown {
        Vec::new()
    } else {
        args.keys()
            .filter(|name| !allowed.contains(*name))
            .cloned()
            .collect::<Vec<_>>()
    };
    if missing.is_empty() && unknown.is_empty() {
        return Ok(());
    }

    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("missing parameters: {}", missing.join(", ")));
    }
    if !unknown.is_empty() {
        parts.push(format!("unknown parameters: {}", unknown.join(", ")));
    }
    Err(CoreError::BadArgs {
        operation: operation.id.clone(),
        reason: parts.join("; "),
    })
}

fn schema_string_set(
    value: Option<&Value>,
    operation: &str,
    field: &str,
) -> Result<BTreeSet<String>> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    let values = value.as_array().ok_or_else(|| CoreError::BadArgs {
        operation: operation.to_string(),
        reason: format!("{field} must be an array"),
    })?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| CoreError::BadArgs {
                    operation: operation.to_string(),
                    reason: format!("{field} entries must be strings"),
                })
        })
        .collect()
}

pub(crate) fn callable_source_from_profile(profile: &SourceProfile) -> Result<CallableSource> {
    match profile.kind {
        prog_core::SourceKind::Http => Ok(CallableSource::Http(http_source_from_profile(profile)?)),
        prog_core::SourceKind::Cli => Ok(CallableSource::Cli(cli_source_from_profile(profile)?)),
        prog_core::SourceKind::Mcp => Ok(CallableSource::Mcp(mcp_source_from_profile(profile)?)),
    }
}

pub(crate) fn http_source_from_profile(profile: &SourceProfile) -> Result<HttpSource> {
    let adapter = adapter_config(profile, "http");
    let base_url = adapter
        .and_then(|config| config.get("base_url"))
        .or_else(|| profile.extra.get("seed_origin"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| profile_adapter_error(profile, "http.base_url"))?;
    let mut operations = Vec::new();
    for operation in &profile.operations {
        let invocation = invocation_config(operation, "http")?;
        operations.push(HttpOperation {
            id: operation.id.clone(),
            method: optional_profile_string(invocation, "method")?
                .unwrap_or_else(|| "GET".to_string()),
            path: required_profile_string(invocation, "path")?,
            query: profile_string_map(invocation.get("query"), "http.query")?,
            headers: profile_string_map(invocation.get("headers"), "http.headers")?,
            json_body: invocation
                .get("json_body")
                .cloned()
                .filter(|value| !value.is_null()),
            timeout_ms: None,
            max_response_bytes: None,
            sensitive_args: profile_string_vec(
                invocation.get("sensitive_args"),
                "http.sensitive_args",
            )?,
        });
    }
    Ok(HttpSource {
        id: profile.id.clone(),
        base_url,
        timeout_ms: adapter_u64(adapter, "timeout_ms", 30_000),
        max_response_bytes: adapter_usize(
            adapter,
            "max_response_bytes",
            DEFAULT_MAX_RESPONSE_BYTES,
        ),
        default_headers: profile_string_map(
            adapter.and_then(|config| config.get("default_headers")),
            "http.default_headers",
        )?,
        response_header_allowlist: profile_string_vec(
            adapter.and_then(|config| config.get("response_header_allowlist")),
            "http.response_header_allowlist",
        )?,
        auth: profile.auth.clone(),
        operations,
    })
}

pub(crate) fn cli_source_from_profile(profile: &SourceProfile) -> Result<CliSource> {
    let adapter = adapter_config(profile, "cli");
    let mut operations = Vec::new();
    for operation in &profile.operations {
        let invocation = invocation_config(operation, "cli")?;
        operations.push(CliOperation {
            id: operation.id.clone(),
            input_schema: operation.input_schema.clone(),
            command: required_profile_string(invocation, "command")?,
            args: profile_string_vec(invocation.get("args"), "cli.args")?,
            env: profile_string_map(invocation.get("env"), "cli.env")?,
            working_dir: invocation
                .get("working_dir")
                .and_then(Value::as_str)
                .map(PathBuf::from),
            shell: invocation
                .get("shell")
                .and_then(Value::as_bool)
                .unwrap_or(operation.effects.shell),
            timeout_ms: None,
            max_stdout_bytes: None,
            max_stderr_bytes: None,
            sensitive_args: profile_string_vec(
                invocation.get("sensitive_args"),
                "cli.sensitive_args",
            )?,
        });
    }
    Ok(CliSource {
        id: profile.id.clone(),
        timeout_ms: adapter_u64(adapter, "timeout_ms", 30_000),
        max_stdout_bytes: adapter_usize(adapter, "max_stdout_bytes", 1024 * 1024),
        max_stderr_bytes: adapter_usize(adapter, "max_stderr_bytes", 1024 * 1024),
        trust: profile.trust.clone(),
        operations,
    })
}

pub(crate) fn mcp_source_from_profile(profile: &SourceProfile) -> Result<McpSource> {
    let adapter =
        adapter_config(profile, "mcp").ok_or_else(|| profile_adapter_error(profile, "mcp"))?;
    Ok(McpSource {
        id: profile.id.clone(),
        command: required_profile_string(adapter, "command")?,
        args: profile_string_vec(adapter.get("args"), "mcp.args")?,
        env: profile_string_map(adapter.get("env"), "mcp.env")?,
        timeout_ms: adapter_u64(Some(adapter), "timeout_ms", 30_000),
        max_content_bytes: adapter_usize(Some(adapter), "max_content_bytes", 1024 * 1024),
        max_stderr_bytes: adapter_usize(Some(adapter), "max_stderr_bytes", 64 * 1024),
        max_schema_depth: adapter_usize(Some(adapter), "max_schema_depth", 32),
    })
}

pub(crate) async fn execute_callable(
    source: &CallableSource,
    operation: &OperationProfile,
    args: &Value,
) -> Result<AdapterCall> {
    match source {
        CallableSource::Http(source) => {
            let result = source
                .execute_with_env(&operation.id, args, &|name| std::env::var(name).ok())
                .await?;
            Ok(AdapterCall {
                data: result.data,
                provenance: serde_json::to_value(result.provenance.clone())?,
                status: Some(result.provenance.status.to_string()),
                duration_ms: Some(result.provenance.duration_ms),
                pagination: result.pagination,
                warnings: result.warnings,
                received_error: result.received_error,
                not_modified: result.not_modified,
            })
        }
        CallableSource::Cli(source) => {
            let result = source.execute(&operation.id, args).await?;
            let mut provenance = serde_json::to_value(result.provenance.clone())?;
            if let Value::Object(map) = &mut provenance {
                map.insert(
                    "diagnostics".to_string(),
                    serde_json::to_value(result.diagnostics)?,
                );
            }
            Ok(AdapterCall {
                data: result.data,
                provenance,
                status: result.provenance.exit_code.map(|code| code.to_string()),
                duration_ms: Some(result.provenance.duration_ms),
                pagination: None,
                warnings: result.warnings,
                received_error: result.received_error,
                not_modified: false,
            })
        }
        CallableSource::Mcp(source) => {
            let invocation = invocation_config(operation, "mcp")?;
            let kind = required_profile_string(invocation, "kind")?;
            let result = match kind.as_str() {
                "tool" => {
                    let name = required_profile_string(invocation, "name")?;
                    source
                        .call_tool_with_schema(
                            &name,
                            args,
                            operation.declared_output_schema.as_ref(),
                        )
                        .await?
                }
                "resource" => {
                    let uri = args
                        .get("uri")
                        .and_then(Value::as_str)
                        .or_else(|| invocation.get("uri").and_then(Value::as_str))
                        .ok_or_else(|| CoreError::BadArgs {
                            operation: operation.id.clone(),
                            reason: "resource calls require args.uri".to_string(),
                        })?;
                    source.read_resource(uri).await?
                }
                _ => {
                    return Err(CoreError::BadArgs {
                        operation: operation.id.clone(),
                        reason: format!("MCP invocation kind '{kind}' is not callable in V1"),
                    });
                }
            };
            let mut provenance = serde_json::to_value(result.provenance.clone())?;
            if let Value::Object(map) = &mut provenance {
                map.insert(
                    "diagnostics".to_string(),
                    serde_json::to_value(result.diagnostics)?,
                );
                if let Some(valid) = result.output_schema_valid {
                    map.insert("output_schema_valid".to_string(), json!(valid));
                }
            }
            Ok(AdapterCall {
                data: result.data,
                provenance,
                status: None,
                duration_ms: Some(result.provenance.duration_ms),
                pagination: None,
                warnings: result.warnings,
                received_error: result.received_error,
                not_modified: false,
            })
        }
    }
}

pub(crate) async fn execute_callable_conditional(
    source: &CallableSource,
    operation: &OperationProfile,
    args: &Value,
    source_state: Option<&SourceStateToken>,
) -> Result<AdapterCall> {
    match source {
        CallableSource::Http(source) => {
            let result = source
                .execute_with_env_conditional(
                    &operation.id,
                    args,
                    &|name| std::env::var(name).ok(),
                    source_state,
                )
                .await?;
            Ok(AdapterCall {
                data: result.data,
                provenance: serde_json::to_value(result.provenance.clone())?,
                status: Some(result.provenance.status.to_string()),
                duration_ms: Some(result.provenance.duration_ms),
                pagination: result.pagination,
                warnings: result.warnings,
                received_error: result.received_error,
                not_modified: result.not_modified,
            })
        }
        CallableSource::Cli(_) | CallableSource::Mcp(_) => {
            execute_callable(source, operation, args).await
        }
    }
}

/// Follow a literal next-page URL (Link `rel="next"`). Returns `Ok(None)` for
/// source kinds with no URL model (CLI/MCP) so the caller can fall back to
/// warn-and-stop; returns `Ok(Some(_))` only for HTTP sources. The HTTP path
/// enforces the same-origin SSRF guard (see `HttpSource::execute_url`).
pub(crate) async fn execute_callable_url(
    source: &CallableSource,
    operation: &OperationProfile,
    url: &str,
    args: &Value,
) -> Result<Option<AdapterCall>> {
    match source {
        CallableSource::Http(http) => {
            let result = http.execute_url(&operation.id, url, args).await?;
            Ok(Some(AdapterCall {
                data: result.data,
                provenance: serde_json::to_value(result.provenance.clone())?,
                status: Some(result.provenance.status.to_string()),
                duration_ms: Some(result.provenance.duration_ms),
                pagination: result.pagination,
                warnings: result.warnings,
                received_error: result.received_error,
                not_modified: result.not_modified,
            }))
        }
        // CLI and MCP sources have no URL continuation model.
        CallableSource::Cli(_) | CallableSource::Mcp(_) => Ok(None),
    }
}

fn adapter_config<'a>(profile: &'a SourceProfile, kind: &str) -> Option<&'a Map<String, Value>> {
    profile
        .extra
        .get("adapter")
        .and_then(|value| value.get(kind))
        .and_then(Value::as_object)
}

pub(crate) fn capture_budget_for_call(
    profile: &SourceProfile,
    operation: &OperationProfile,
) -> CaptureBudget {
    let (kind, byte_fields, defaults): (&str, &[&str], &[u64]) = match profile.kind {
        prog_core::SourceKind::Http => (
            "http",
            &["max_response_bytes"],
            &[DEFAULT_MAX_RESPONSE_BYTES as u64],
        ),
        prog_core::SourceKind::Cli => (
            "cli",
            &["max_stdout_bytes", "max_stderr_bytes"],
            &[1024 * 1024, 1024 * 1024],
        ),
        prog_core::SourceKind::Mcp => (
            "mcp",
            &["max_content_bytes", "max_stderr_bytes"],
            &[1024 * 1024, 64 * 1024],
        ),
    };
    let adapter = adapter_config(profile, kind);
    let invocation = operation
        .extra
        .get("invocation")
        .and_then(|value| value.get(kind))
        .and_then(Value::as_object);
    let operation_overrides = invocation.is_some_and(|config| {
        byte_fields
            .iter()
            .chain(std::iter::once(&"timeout_ms"))
            .any(|field| config.contains_key(*field))
    });
    let source = if operation_overrides {
        BudgetSource::Operation
    } else if adapter.is_some() {
        BudgetSource::Profile
    } else {
        BudgetSource::Default
    };
    let timeout_ms = invocation
        .and_then(|config| config.get("timeout_ms"))
        .and_then(Value::as_u64)
        .or_else(|| {
            adapter
                .and_then(|config| config.get("timeout_ms"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(30_000);
    let scopes: &[&str] = match profile.kind {
        prog_core::SourceKind::Http => &["body"],
        prog_core::SourceKind::Cli => &["stdout", "stderr"],
        prog_core::SourceKind::Mcp => &["content", "stderr"],
    };
    let limits = scopes
        .iter()
        .zip(byte_fields.iter().zip(defaults.iter()))
        .map(|(scope, (field, default))| CaptureLimit {
            scope: (*scope).to_string(),
            max_bytes: Some(
                invocation
                    .and_then(|config| config.get(*field))
                    .and_then(Value::as_u64)
                    .or_else(|| {
                        adapter
                            .and_then(|config| config.get(*field))
                            .and_then(Value::as_u64)
                    })
                    .unwrap_or(*default),
            ),
            max_duration_ms: Some(timeout_ms),
            max_work_units: None,
            extra: Extra::new(),
        })
        .collect();
    CaptureBudget {
        source,
        limits,
        extra: Extra::new(),
    }
}

pub(crate) fn profile_adapter_error(profile: &SourceProfile, field: &str) -> CoreError {
    CoreError::BadArgs {
        operation: "call".to_string(),
        reason: format!(
            "profile '{}' is missing adapter.{field}; re-run `prog discover` for this source",
            profile.id
        ),
    }
}

pub(crate) fn required_profile_string(map: &Map<String, Value>, field: &str) -> Result<String> {
    map.get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CoreError::BadArgs {
            operation: "call".to_string(),
            reason: format!("profile field '{field}' must be a string"),
        })
}

pub(crate) fn optional_profile_string(
    map: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>> {
    map.get(field)
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| CoreError::BadArgs {
                    operation: "call".to_string(),
                    reason: format!("profile field '{field}' must be a string"),
                })
        })
        .transpose()
}

pub(crate) fn profile_string_map(
    value: Option<&Value>,
    field: &str,
) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    serde_json::from_value(value.clone()).map_err(|error| CoreError::BadArgs {
        operation: "call".to_string(),
        reason: format!("profile field '{field}' must be an object of strings: {error}"),
    })
}

pub(crate) fn profile_string_vec(value: Option<&Value>, field: &str) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    serde_json::from_value(value.clone()).map_err(|error| CoreError::BadArgs {
        operation: "call".to_string(),
        reason: format!("profile field '{field}' must be an array of strings: {error}"),
    })
}

pub(crate) fn adapter_u64(adapter: Option<&Map<String, Value>>, field: &str, default: u64) -> u64 {
    adapter
        .and_then(|config| config.get(field))
        .and_then(Value::as_u64)
        .unwrap_or(default)
}

pub(crate) fn adapter_usize(
    adapter: Option<&Map<String, Value>>,
    field: &str,
    default: usize,
) -> usize {
    adapter_u64(adapter, field, default.try_into().unwrap_or(u64::MAX))
        .try_into()
        .unwrap_or(default)
}

pub(crate) fn effective_cache_policy(
    profile: &SourceProfile,
    operation: &OperationProfile,
) -> CachePolicy {
    let mut policy = if operation.cache.enabled {
        operation.cache.clone()
    } else if profile.cache.enabled {
        profile.cache.clone()
    } else {
        CachePolicy::default()
    };
    if !policy.enabled && operation.effects.cacheable && !operation.effects.sensitive {
        policy.enabled = true;
        policy.ttl_seconds = Some(86_400);
    }
    policy
}

pub(crate) fn ttl_seconds(policy: &CachePolicy) -> i64 {
    policy
        .ttl_seconds
        .unwrap_or(86_400)
        .try_into()
        .unwrap_or(i64::MAX)
}

pub(crate) fn cache_skip_warning(no_cache: bool, operation: &OperationProfile) -> String {
    if no_cache {
        "cache persistence skipped by --no-cache".to_string()
    } else if operation.effects.sensitive {
        "cache persistence skipped because the operation may handle sensitive data".to_string()
    } else if !operation.effects.cacheable {
        "cache persistence skipped because the operation is not cacheable".to_string()
    } else {
        "cache persistence skipped by cache policy".to_string()
    }
}

pub(crate) fn profile_source_kind_name(kind: prog_core::SourceKind) -> &'static str {
    match kind {
        prog_core::SourceKind::Http => "http",
        prog_core::SourceKind::Cli => "cli",
        prog_core::SourceKind::Mcp => "mcp",
    }
}

pub(crate) fn source_kind_for_source_id(source_id: &str) -> Option<String> {
    match source_id {
        "observe" => Some("artifact".to_string()),
        "prog" => Some("internal".to_string()),
        _ => None,
    }
}

pub(crate) fn cache_info(
    status: CacheStatus,
    entry: &prog_core::CacheEntryMeta,
    age_seconds: Option<u64>,
) -> CacheInfo {
    CacheInfo {
        status,
        ttl_seconds: ttl_between(&entry.created_at, &entry.expires_at).ok(),
        expires_at: Some(entry.expires_at.clone()),
        age_seconds,
    }
}

pub(crate) fn cache_is_stale(cache: Option<&CacheInfo>) -> bool {
    cache.is_some_and(|cache| {
        matches!((cache.age_seconds, cache.ttl_seconds), (Some(age), Some(ttl)) if age >= ttl)
    })
}

pub(crate) fn cached_pagination_satisfies(pagination: &Value, requested_pages: usize) -> bool {
    let pages_fetched = pagination
        .get("pages_fetched")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let requested_pages = requested_pages.min(50) as u64;
    pagination.get("stop_reason").and_then(Value::as_str) == Some("no_more")
        || pages_fetched >= requested_pages
}

pub(crate) fn call_provenance(
    cache_key: &str,
    status: Option<String>,
    duration_ms: Option<u64>,
    adapter_provenance: Value,
) -> CallProvenance {
    let mut extra = Extra::new();
    extra.insert("adapter".to_string(), adapter_provenance);
    CallProvenance {
        source_call_id: format!(
            "call_{}",
            Utc::now()
                .timestamp_nanos_opt()
                .unwrap_or_else(|| Utc::now().timestamp_micros())
        ),
        cache_key: Some(cache_key.to_string()),
        captured_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        status,
        duration_ms,
        extra,
    }
}

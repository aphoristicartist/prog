//! Source profile importers for external schema formats.
//!
//! Importers preserve declared schemas as priors and keep observed shapes
//! separate, so later calls can refine profiles monotonically.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    contracts::{
        AuthRef, CachePolicy, EffectSet, OperationProfile, SourceKind, SourceProfile, TrustSettings,
    },
    error::{CoreError, Result},
    policy::{EvidenceGrade, mcp_tool_annotation_effects, stamp_evidence_grade},
    redaction::RedactionPolicy,
    shape::{infer, join},
};

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_BYTES: usize = 1024 * 1024;
const MAX_SCHEMA_NODES: usize = 512;
const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "patch", "delete", "head", "options", "trace",
];

/// Import context configuration.
#[derive(Debug, Clone)]
pub struct ImportContext {
    /// Whether to import all operations or only read-only ones.
    pub read_only_only: bool,
    /// Maximum depth retained while copying declared schemas.
    pub max_schema_depth: usize,
    /// Whether checked-in examples may refine observed output shapes.
    pub infer_from_examples: bool,
}

impl Default for ImportContext {
    fn default() -> Self {
        Self {
            read_only_only: false,
            max_schema_depth: 10,
            infer_from_examples: true,
        }
    }
}

/// Import result with metadata.
#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub schema_version: String,
    pub source_id: String,
    pub kind: SourceKind,
    pub operations_imported: usize,
    pub schemas_imported: usize,
    pub examples_inferred: usize,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

/// Checked-in example/fixture output for monotone profile refinement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportExample {
    pub operation: String,
    #[serde(default)]
    pub args: Value,
    pub output: Value,
}

/// Import a source profile from an OpenAPI 3.x document.
pub fn import_openapi(
    source_id: String,
    openapi_spec: &Value,
    ctx: &ImportContext,
) -> Result<(SourceProfile, ImportReport)> {
    let spec = object(openapi_spec, "openapi", "document")?;
    let openapi_version = required_str(spec, "openapi", "openapi")?.to_string();
    if !openapi_version.starts_with("3.") {
        return Err(import_error(
            "openapi",
            format!("only OpenAPI 3.x is supported, got '{openapi_version}'"),
        ));
    }

    let info = spec
        .get("info")
        .and_then(Value::as_object)
        .ok_or_else(|| import_error("openapi", "missing info object"))?;
    let title = required_str(info, "title", "openapi.info")?.to_string();
    let info_version = info
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let description = info
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(Some(title));

    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let base_url = first_server_url(spec, &mut warnings);
    let auth = import_openapi_security(spec.get("components"), &mut warnings);
    let paths = spec
        .get("paths")
        .and_then(Value::as_object)
        .ok_or_else(|| import_error("openapi", "missing paths object"))?;

    let mut operations = Vec::new();
    let mut schemas_imported = 0usize;
    let mut seen_ids = BTreeSet::new();
    for (path, path_item) in paths {
        let Some(path_item) = path_item.as_object() else {
            push_warning(
                &mut warnings,
                format!("skipped OpenAPI path '{path}' because it is not an object"),
            );
            continue;
        };
        let path_parameters = parse_parameters(
            path_item.get("parameters"),
            ctx,
            &mut warnings,
            &format!("paths.{path}.parameters"),
        );

        for method in HTTP_METHODS {
            let Some(operation_value) = path_item.get(*method) else {
                continue;
            };
            if ctx.read_only_only && !is_read_only_method(method) {
                push_warning(
                    &mut warnings,
                    format!("skipped {method} {path} because it is not read-only"),
                );
                continue;
            }

            match import_openapi_operation(
                path,
                method,
                operation_value,
                &path_parameters,
                ctx,
                &mut warnings,
                &mut seen_ids,
            ) {
                Ok((operation, imported_schema)) => {
                    schemas_imported += usize::from(imported_schema);
                    operations.push(operation);
                }
                Err(error) => errors.push(format!("failed to import {method} {path}: {error}")),
            }
        }
    }

    let operations_imported = operations.len();
    let profile = SourceProfile {
        schema_version: crate::contracts::SOURCE_PROFILE_VERSION.to_string(),
        id: source_id.clone(),
        kind: SourceKind::Http,
        version: 1,
        description,
        operations,
        auth,
        cache: CachePolicy::default(),
        trust: TrustSettings {
            allow_shell: false,
            allow_network: true,
            auto_upgrade: true,
            extra: Map::new(),
        },
        effect_defaults: EffectSet::default(),
        redaction: crate::redaction::RedactionConfig::default(),
        extra: {
            let mut extra = Map::new();
            extra.insert("import_source".to_string(), json!("openapi"));
            extra.insert("openapi_version".to_string(), json!(openapi_version));
            extra.insert("openapi_info_version".to_string(), json!(info_version));
            extra.insert(
                "adapter".to_string(),
                json!({"http": {
                    "base_url": base_url,
                    "timeout_ms": DEFAULT_TIMEOUT_MS,
                    "max_response_bytes": DEFAULT_MAX_BYTES,
                    "default_headers": {},
                    "response_header_allowlist": []
                }}),
            );
            extra
        },
    };

    Ok((
        profile,
        ImportReport {
            schema_version: crate::contracts::DISCLOSURE_VERSION.to_string(),
            source_id,
            kind: SourceKind::Http,
            operations_imported,
            schemas_imported,
            examples_inferred: 0,
            warnings,
            errors,
        },
    ))
}

/// Import from a JSON Schema document.
pub fn import_json_schema(
    source_id: String,
    schema: &Value,
    ctx: &ImportContext,
) -> Result<(SourceProfile, ImportReport)> {
    let mut warnings = Vec::new();
    let declared_output_schema = bounded_schema(schema, ctx, &mut warnings);
    let operation_id = format!("{}_get", sanitize_id(&source_id));
    let operation = OperationProfile {
        id: operation_id,
        description: schema
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        input_schema: json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Resource identifier"}
            },
            "additionalProperties": false
        }),
        output_shape: None,
        declared_output_schema: Some(declared_output_schema),
        effects: {
            // The synthesized op is read-only by INFERENCE from the JSON Schema
            // shape, not by an explicit descriptor declaration, so it is graded
            // Assumed. Assumed evidence NEVER relaxes confirmation (hard fence),
            // and the op is stored gated regardless of trust.auto_upgrade.
            let mut effects = EffectSet {
                read_only: true,
                mutating: false,
                network: false,
                shell: false,
                sensitive: false,
                cacheable: true,
                requires_confirmation: true,
                extra: Map::new(),
            };
            stamp_evidence_grade(&mut effects, EvidenceGrade::Assumed);
            effects
        },
        cache: CachePolicy {
            enabled: true,
            ttl_seconds: Some(3600),
            refresh_after_seconds: None,
            extra: Map::new(),
        },
        pagination: None,
        extra: schema_prior("json_schema", false),
    };

    let profile = SourceProfile {
        schema_version: crate::contracts::SOURCE_PROFILE_VERSION.to_string(),
        id: source_id.clone(),
        kind: SourceKind::Http,
        version: 1,
        description: schema
            .get("title")
            .and_then(Value::as_str)
            .or_else(|| schema.get("description").and_then(Value::as_str))
            .map(str::to_string),
        operations: vec![operation],
        auth: Vec::new(),
        cache: CachePolicy::default(),
        trust: TrustSettings::default(),
        effect_defaults: EffectSet::default(),
        redaction: crate::redaction::RedactionConfig::default(),
        extra: {
            let mut extra = Map::new();
            extra.insert("import_source".to_string(), json!("json_schema"));
            extra
        },
    };

    Ok((
        profile,
        ImportReport {
            schema_version: crate::contracts::DISCLOSURE_VERSION.to_string(),
            source_id,
            kind: SourceKind::Http,
            operations_imported: 1,
            schemas_imported: 1,
            examples_inferred: 0,
            warnings,
            errors: Vec::new(),
        },
    ))
}

/// Import from an MCP server's declared schemas.
pub fn import_mcp_schemas(
    source_id: String,
    tools: &[McpTool],
    resources: &[McpResource],
    ctx: &ImportContext,
) -> Result<(SourceProfile, ImportReport)> {
    let mut warnings = Vec::new();
    let mut operations = Vec::new();
    let mut schemas_imported = 0usize;

    for tool in tools {
        let read_only = tool.read_only_hint == Some(true);
        if ctx.read_only_only && !read_only {
            push_warning(
                &mut warnings,
                format!(
                    "skipped MCP tool '{}' because readOnlyHint is not true",
                    tool.name
                ),
            );
            continue;
        }
        if tool.read_only_hint.is_none() {
            push_warning(
                &mut warnings,
                format!(
                    "MCP tool '{}' has no readOnlyHint; imported as confirmation-gated",
                    tool.name
                ),
            );
        }

        let declared_output_schema = tool
            .output_schema
            .as_ref()
            .map(|schema| bounded_schema(schema, ctx, &mut warnings));
        schemas_imported += usize::from(declared_output_schema.is_some());

        let mut extra = schema_prior("mcp.output_schema", declared_output_schema.is_some());
        extra.insert("mcp_tool".to_string(), json!(tool.name));
        if let Some(annotations) = &tool.annotations {
            extra.insert("mcp_annotations".to_string(), json!(annotations));
        }

        let effects = mcp_tool_annotation_effects(tool.read_only_hint, tool.destructive_hint);
        // Cache eligibility follows the contradiction-aware read-only fact so a
        // contradictory destructiveHint never produces a cacheable mutating op.
        let cacheable_read_only = effects.read_only;
        operations.push(OperationProfile {
            id: sanitize_id(&tool.name),
            description: tool.description.clone(),
            input_schema: bounded_schema(&tool.input_schema, ctx, &mut warnings),
            output_shape: None,
            declared_output_schema,
            effects,
            cache: CachePolicy {
                enabled: cacheable_read_only,
                ttl_seconds: cacheable_read_only.then_some(3600),
                refresh_after_seconds: None,
                extra: Map::new(),
            },
            pagination: None,
            extra,
        });
    }

    for resource in resources {
        let operation_id = format!("resource_{}", sanitize_id(&resource.name));
        let mut extra = Map::new();
        extra.insert("mcp_resource".to_string(), json!(resource.name));
        if let Some(mime_type) = &resource.mime_type {
            extra.insert("mime_type".to_string(), json!(mime_type));
        }
        operations.push(OperationProfile {
            id: operation_id,
            description: resource
                .description
                .clone()
                .or_else(|| Some(format!("Read MCP resource: {}", resource.name))),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "uri": {"type": "string", "description": "Resource URI"}
                },
                "required": ["uri"],
                "additionalProperties": false
            }),
            output_shape: None,
            declared_output_schema: None,
            effects: {
                // An MCP resource is spec-defined read-only, so it is graded
                // Proven; stored gated and relaxed at call time under
                // trust.auto_upgrade.
                let mut effects = EffectSet {
                    read_only: true,
                    mutating: false,
                    network: false,
                    shell: false,
                    sensitive: false,
                    cacheable: true,
                    requires_confirmation: true,
                    extra: Map::new(),
                };
                stamp_evidence_grade(&mut effects, EvidenceGrade::Proven);
                effects
            },
            cache: CachePolicy {
                enabled: true,
                ttl_seconds: Some(3600),
                refresh_after_seconds: None,
                extra: Map::new(),
            },
            pagination: None,
            extra,
        });
    }

    let operations_imported = operations.len();
    let profile = SourceProfile {
        schema_version: crate::contracts::SOURCE_PROFILE_VERSION.to_string(),
        id: source_id.clone(),
        kind: SourceKind::Mcp,
        version: 1,
        description: Some("MCP schema import".to_string()),
        operations,
        auth: Vec::new(),
        cache: CachePolicy::default(),
        trust: TrustSettings::default(),
        effect_defaults: EffectSet::default(),
        redaction: crate::redaction::RedactionConfig::default(),
        extra: {
            let mut extra = Map::new();
            extra.insert("import_source".to_string(), json!("mcp"));
            extra.insert("tools_count".to_string(), json!(tools.len()));
            extra.insert("resources_count".to_string(), json!(resources.len()));
            extra
        },
    };

    Ok((
        profile,
        ImportReport {
            schema_version: crate::contracts::DISCLOSURE_VERSION.to_string(),
            source_id,
            kind: SourceKind::Mcp,
            operations_imported,
            schemas_imported,
            examples_inferred: 0,
            warnings,
            errors: Vec::new(),
        },
    ))
}

/// Parse CLI help text into operations with fail-closed effects.
pub fn import_cli_help(
    source_id: String,
    help_text: &str,
    command_base: &str,
    _ctx: &ImportContext,
) -> Result<(SourceProfile, ImportReport)> {
    let (command, base_args) = split_command_base(command_base)?;
    let mut warnings = vec![
        "CLI help import is conservative: operations are confirmation-gated and not marked read-only"
            .to_string(),
    ];
    let mut operations = Vec::new();

    for subcommand in parse_cli_subcommands(help_text) {
        operations.push(cli_operation(
            &source_id,
            &command,
            base_args.iter().chain([&subcommand]).cloned().collect(),
            Some(format!("CLI subcommand: {subcommand}")),
        ));
    }

    if operations.is_empty() {
        warnings
            .push("no unambiguous subcommands detected; imported base command only".to_string());
        operations.push(cli_operation(
            &source_id,
            &command,
            base_args,
            Some(format!("Execute CLI: {command_base}")),
        ));
    }

    let operations_imported = operations.len();
    let profile = SourceProfile {
        schema_version: crate::contracts::SOURCE_PROFILE_VERSION.to_string(),
        id: source_id.clone(),
        kind: SourceKind::Cli,
        version: 1,
        description: Some(format!("CLI help import: {command_base}")),
        operations,
        auth: Vec::new(),
        cache: CachePolicy::default(),
        trust: TrustSettings {
            allow_shell: false,
            allow_network: false,
            auto_upgrade: true,
            extra: Map::new(),
        },
        effect_defaults: EffectSet::default(),
        redaction: crate::redaction::RedactionConfig::default(),
        extra: {
            let mut extra = Map::new();
            extra.insert("import_source".to_string(), json!("cli_help"));
            extra.insert("cli_base".to_string(), json!(command_base));
            extra.insert(
                "adapter".to_string(),
                json!({"cli": {
                    "timeout_ms": DEFAULT_TIMEOUT_MS,
                    "max_stdout_bytes": DEFAULT_MAX_BYTES,
                    "max_stderr_bytes": DEFAULT_MAX_BYTES
                }}),
            );
            extra
        },
    };

    Ok((
        profile,
        ImportReport {
            schema_version: crate::contracts::DISCLOSURE_VERSION.to_string(),
            source_id,
            kind: SourceKind::Cli,
            operations_imported,
            schemas_imported: 0,
            examples_inferred: 0,
            warnings,
            errors: Vec::new(),
        },
    ))
}

/// Refine imported priors with checked-in examples without storing raw example bodies.
pub fn refine_with_examples(
    profile: &mut SourceProfile,
    examples: &[ImportExample],
    ctx: &ImportContext,
) -> ImportReport {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let mut examples_inferred = 0usize;
    if !ctx.infer_from_examples {
        push_warning(
            &mut warnings,
            "example inference disabled by ImportContext".to_string(),
        );
    } else {
        for example in examples {
            let Some(operation) = profile
                .operations
                .iter_mut()
                .find(|operation| operation.id == example.operation)
            else {
                errors.push(format!("unknown operation '{}'", example.operation));
                continue;
            };
            let redacted =
                crate::RawPayload::new(example.output.clone()).redact(&RedactionPolicy::default());
            let redacted_paths = redacted.redacted_paths;
            let redacted = redacted.payload;
            let observed = infer(redacted.as_value());
            operation.output_shape = Some(match &operation.output_shape {
                Some(current) => join(current, &observed),
                None => observed,
            });
            operation.extra.insert(
                "example_observations".to_string(),
                json!({
                    "count": operation.extra
                        .get("example_observations")
                        .and_then(|value| value.get("count"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                        + 1,
                    "redacted_paths": redacted_paths.len()
                }),
            );
            examples_inferred += 1;
        }
    }

    ImportReport {
        schema_version: crate::contracts::DISCLOSURE_VERSION.to_string(),
        source_id: profile.id.clone(),
        kind: profile.kind,
        operations_imported: 0,
        schemas_imported: 0,
        examples_inferred,
        warnings,
        errors,
    }
}

/// Load and import from a JSON file by auto-detecting schema format.
pub fn import_from_file(
    source_id: String,
    path: &Path,
    ctx: &ImportContext,
) -> Result<(SourceProfile, ImportReport)> {
    let content = fs::read_to_string(path).map_err(|error| {
        import_error("file", format!("cannot read {}: {}", path.display(), error))
    })?;
    let value: Value = serde_json::from_str(&content).map_err(|error| {
        import_error(
            "json",
            format!("invalid JSON in {}: {}", path.display(), error),
        )
    })?;

    if value
        .get("openapi")
        .and_then(Value::as_str)
        .is_some_and(|version| version.starts_with("3."))
    {
        return import_openapi(source_id, &value, ctx);
    }
    if value.get("$schema").is_some() || value.get("type").is_some() {
        return import_json_schema(source_id, &value, ctx);
    }
    Err(import_error(
        "auto",
        "could not detect OpenAPI 3.x or JSON Schema document",
    ))
}

fn import_openapi_operation(
    path: &str,
    method: &str,
    operation_value: &Value,
    path_parameters: &[OpenApiParameter],
    ctx: &ImportContext,
    warnings: &mut Vec<String>,
    seen_ids: &mut BTreeSet<String>,
) -> Result<(OperationProfile, bool)> {
    let operation: OpenApiOperation =
        serde_json::from_value(operation_value.clone()).map_err(|error| {
            import_error("openapi", format!("operation object is malformed: {error}"))
        })?;
    if operation.deprecated {
        push_warning(
            warnings,
            format!("{method} {path} is deprecated but was imported"),
        );
    }
    let operation_id = unique_operation_id(
        operation
            .operation_id
            .clone()
            .unwrap_or_else(|| format!("{method}_{path}")),
        seen_ids,
    );
    let mut parameters = path_parameters.to_vec();
    parameters.extend(operation.parameters.clone());
    let (input_schema, query, headers, json_body) =
        build_request_contract(&parameters, operation.request_body.as_ref(), ctx, warnings);
    let declared_output_schema = response_schema(&operation.responses, ctx, warnings);
    let read_only = is_read_only_method(method);
    // HTTP method is a normative, machine-readable read-declaration: GET/HEAD/
    // OPTIONS explicitly declare read-only, so read-only ops are graded Proven
    // and stored confirmation-gated (relaxed at call time under
    // trust.auto_upgrade). Non-read-only methods are Unproven and stay gated.
    let grade = if read_only {
        EvidenceGrade::Proven
    } else {
        EvidenceGrade::Unproven
    };
    let mut extra = Map::new();
    extra.insert(
        "invocation".to_string(),
        json!({"http": {
            "method": method.to_ascii_uppercase(),
            "path": path,
            "query": query,
            "headers": headers,
            "json_body": json_body,
            "sensitive_args": []
        }}),
    );
    extra.insert("import_source".to_string(), json!("openapi"));
    if declared_output_schema.is_some() {
        extra.insert(
            "schema_prior".to_string(),
            json!({"source": "openapi.response_schema", "observed": false}),
        );
    }

    Ok((
        OperationProfile {
            id: operation_id,
            description: operation.description.or(operation.summary),
            input_schema,
            output_shape: None,
            declared_output_schema: declared_output_schema.clone(),
            effects: {
                // Stored gated for read-only (Proven) ops so trust.auto_upgrade
                // is a live runtime knob; non-read-only ops are already gated.
                let mut effects = EffectSet {
                    read_only,
                    mutating: !read_only,
                    network: true,
                    shell: false,
                    sensitive: false,
                    cacheable: read_only,
                    requires_confirmation: true,
                    extra: Map::new(),
                };
                stamp_evidence_grade(&mut effects, grade);
                effects
            },
            cache: CachePolicy {
                enabled: read_only,
                ttl_seconds: read_only.then_some(86_400),
                refresh_after_seconds: None,
                extra: Map::new(),
            },
            pagination: None,
            extra,
        },
        declared_output_schema.is_some(),
    ))
}

fn build_request_contract(
    parameters: &[OpenApiParameter],
    request_body: Option<&OpenApiRequestBody>,
    ctx: &ImportContext,
    warnings: &mut Vec<String>,
) -> (
    Value,
    BTreeMap<String, String>,
    BTreeMap<String, String>,
    Value,
) {
    let mut properties = Map::new();
    let mut required = BTreeSet::new();
    let mut query = BTreeMap::new();
    let mut headers = BTreeMap::new();

    for parameter in parameters {
        let Some(location) = &parameter.location else {
            push_warning(
                warnings,
                format!(
                    "parameter '{}' has no location and was skipped",
                    parameter.name
                ),
            );
            continue;
        };
        let mut schema = parameter
            .schema
            .as_ref()
            .map(|schema| bounded_schema(schema, ctx, warnings))
            .unwrap_or_else(|| json!({"type": "string"}));
        if let Some(description) = &parameter.description
            && let Some(object) = schema.as_object_mut()
        {
            object
                .entry("description".to_string())
                .or_insert_with(|| json!(description));
        }
        properties.insert(parameter.name.clone(), schema);
        if parameter.required || location == "path" {
            required.insert(parameter.name.clone());
        }
        match location.as_str() {
            "query" => {
                query.insert(parameter.name.clone(), format!("{{{}}}", parameter.name));
            }
            "header" => {
                headers.insert(parameter.name.clone(), format!("{{{}}}", parameter.name));
            }
            "path" | "cookie" => {}
            other => push_warning(
                warnings,
                format!(
                    "parameter '{}' has unsupported location '{}' and is not wired into invocation",
                    parameter.name, other
                ),
            ),
        }
    }

    let mut json_body = Value::Null;
    if let Some(request_body) = request_body
        && let Some(schema) = json_media_schema(&request_body.content)
    {
        properties.insert("body".to_string(), bounded_schema(schema, ctx, warnings));
        if request_body.required {
            required.insert("body".to_string());
        }
        json_body = json!("{body}");
    }

    (
        json!({
            "type": "object",
            "properties": properties,
            "required": required.into_iter().collect::<Vec<_>>(),
            "additionalProperties": false
        }),
        query,
        headers,
        json_body,
    )
}

fn response_schema(
    responses: &BTreeMap<String, OpenApiResponse>,
    ctx: &ImportContext,
    warnings: &mut Vec<String>,
) -> Option<Value> {
    let mut keys = ["200", "201", "202", "default"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    keys.extend(
        responses
            .keys()
            .filter(|status| status.starts_with('2'))
            .cloned(),
    );
    for key in keys {
        if let Some(response) = responses.get(&key)
            && let Some(schema) = json_media_schema(&response.content)
        {
            return Some(bounded_schema(schema, ctx, warnings));
        }
    }
    None
}

fn json_media_schema(content: &BTreeMap<String, OpenApiMediaType>) -> Option<&Value> {
    content
        .iter()
        .find(|(mime, _)| mime.to_ascii_lowercase().contains("json"))
        .and_then(|(_, media)| media.schema.as_ref())
}

fn parse_parameters(
    value: Option<&Value>,
    ctx: &ImportContext,
    warnings: &mut Vec<String>,
    context: &str,
) -> Vec<OpenApiParameter> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(values) = value.as_array() else {
        push_warning(
            warnings,
            format!("{context} is not an array and was skipped"),
        );
        return Vec::new();
    };
    values
        .iter()
        .filter_map(
            |value| match serde_json::from_value::<OpenApiParameter>(value.clone()) {
                Ok(mut parameter) => {
                    if let Some(schema) = &parameter.schema {
                        parameter.schema = Some(bounded_schema(schema, ctx, warnings));
                    }
                    Some(parameter)
                }
                Err(error) => {
                    push_warning(
                        warnings,
                        format!("{context} contains a malformed parameter: {error}"),
                    );
                    None
                }
            },
        )
        .collect()
}

fn first_server_url(spec: &Map<String, Value>, warnings: &mut Vec<String>) -> String {
    let Some(server) = spec
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(Value::as_object)
    else {
        push_warning(
            warnings,
            "OpenAPI document has no servers; using '/' as base_url".to_string(),
        );
        return "/".to_string();
    };
    let url = server
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("/")
        .to_string();
    if url.contains('{') {
        push_warning(
            warnings,
            "OpenAPI server variables are preserved literally in base_url".to_string(),
        );
    }
    url
}

fn import_openapi_security(components: Option<&Value>, warnings: &mut Vec<String>) -> Vec<AuthRef> {
    let Some(schemes) = components
        .and_then(|value| value.get("securitySchemes"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };

    let mut auth = Vec::new();
    for (name, value) in schemes {
        let Some(scheme) = value.as_object() else {
            push_warning(
                warnings,
                format!("security scheme '{name}' is not an object and was skipped"),
            );
            continue;
        };
        let scheme_type = scheme.get("type").and_then(Value::as_str).unwrap_or("");
        match scheme_type {
            "http" if scheme.get("scheme").and_then(Value::as_str) == Some("bearer") => {
                auth.push(AuthRef {
                    name: name.clone(),
                    env: format!("{}_TOKEN", env_key(name)),
                    header: Some("Authorization".to_string()),
                    format: Some("Bearer {value}".to_string()),
                    extra: Map::new(),
                });
            }
            "http" if scheme.get("scheme").and_then(Value::as_str) == Some("basic") => {
                auth.push(AuthRef {
                    name: name.clone(),
                    env: format!("{}_BASIC", env_key(name)),
                    header: Some("Authorization".to_string()),
                    format: Some("Basic {value}".to_string()),
                    extra: Map::new(),
                });
            }
            "apiKey" if scheme.get("in").and_then(Value::as_str) == Some("header") => {
                let header = scheme
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(name)
                    .to_string();
                auth.push(AuthRef {
                    name: name.clone(),
                    env: format!("{}_KEY", env_key(name)),
                    header: Some(header),
                    format: Some("{value}".to_string()),
                    extra: Map::new(),
                });
            }
            "apiKey" => push_warning(
                warnings,
                format!("apiKey security scheme '{name}' is not header-based and was skipped"),
            ),
            "oauth2" | "openIdConnect" => push_warning(
                warnings,
                format!("security scheme '{name}' requires manual token configuration"),
            ),
            other => push_warning(
                warnings,
                format!("unsupported security scheme '{name}' of type '{other}'"),
            ),
        }
    }
    auth
}

fn bounded_schema(schema: &Value, ctx: &ImportContext, warnings: &mut Vec<String>) -> Value {
    let mut nodes = 0usize;
    bounded_schema_at(schema, 0, ctx.max_schema_depth, &mut nodes, warnings)
}

fn bounded_schema_at(
    value: &Value,
    depth: usize,
    max_depth: usize,
    nodes: &mut usize,
    warnings: &mut Vec<String>,
) -> Value {
    *nodes += 1;
    if depth > max_depth || *nodes > MAX_SCHEMA_NODES {
        push_warning(
            warnings,
            format!("schema import truncated at depth {depth} after {nodes} nodes"),
        );
        return json!({"x-prog-truncated_schema": true});
    }
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                if reference.starts_with("http://")
                    || reference.starts_with("https://")
                    || reference.starts_with("file:")
                    || !reference.starts_with("#/")
                {
                    push_warning(
                        warnings,
                        format!(
                            "preserved external or ambiguous $ref '{reference}' without dereferencing"
                        ),
                    );
                }
                return json!({
                    "$ref": reference,
                    "x-prog-ref_status": "preserved_not_dereferenced"
                });
            }
            let mut output = Map::new();
            for (key, child) in map {
                output.insert(
                    key.clone(),
                    bounded_schema_at(child, depth + 1, max_depth, nodes, warnings),
                );
            }
            Value::Object(output)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|child| bounded_schema_at(child, depth + 1, max_depth, nodes, warnings))
                .collect(),
        ),
        scalar => scalar.clone(),
    }
}

fn cli_operation(
    source_id: &str,
    command: &str,
    args: Vec<String>,
    description: Option<String>,
) -> OperationProfile {
    let id = if let Some(last) = args.last() {
        format!("{}_{}", sanitize_id(source_id), sanitize_id(last))
    } else {
        format!("{}_run", sanitize_id(source_id))
    };
    let mut extra = Map::new();
    extra.insert(
        "invocation".to_string(),
        json!({"cli": {
            "command": command,
            "args": args,
            "env": {},
            "working_dir": null,
            "shell": false,
            "sensitive_args": []
        }}),
    );
    extra.insert("import_source".to_string(), json!("cli_help"));

    OperationProfile {
        id,
        description,
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        output_shape: None,
        declared_output_schema: None,
        effects: {
            // CLI help text is ambiguous: "list" could mutate, so parsed
            // commands are graded Unproven and stay confirmation-gated.
            let mut effects = EffectSet {
                read_only: false,
                mutating: true,
                network: false,
                shell: false,
                sensitive: false,
                cacheable: false,
                requires_confirmation: true,
                extra: Map::new(),
            };
            stamp_evidence_grade(&mut effects, EvidenceGrade::Unproven);
            effects
        },
        cache: CachePolicy::default(),
        pagination: None,
        extra,
    }
}

fn parse_cli_subcommands(help_text: &str) -> Vec<String> {
    let mut in_commands = false;
    let mut subcommands = BTreeSet::new();
    for raw in help_text.lines() {
        let trimmed = raw.trim();
        let lower = trimmed.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "commands:" | "subcommands:" | "available commands:"
        ) {
            in_commands = true;
            continue;
        }
        if in_commands
            && (lower.ends_with(':') || lower.starts_with("options") || lower.starts_with("flags"))
        {
            in_commands = false;
        }
        if !in_commands || trimmed.is_empty() || trimmed.starts_with('-') {
            continue;
        }
        let leading_spaces = raw.len().saturating_sub(raw.trim_start().len());
        if leading_spaces < 2 {
            continue;
        }
        let Some(token) = trimmed.split_whitespace().next() else {
            continue;
        };
        let token = token.trim_matches(',');
        if is_safe_cli_token(token) {
            subcommands.insert(token.to_string());
        }
    }
    subcommands.into_iter().collect()
}

fn split_command_base(command_base: &str) -> Result<(String, Vec<String>)> {
    let mut parts = command_base.split_whitespace();
    let command = parts
        .next()
        .ok_or_else(|| import_error("cli_help", "command base must not be empty"))?
        .to_string();
    Ok((command, parts.map(str::to_string).collect()))
}

fn is_safe_cli_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 64
        && !token.starts_with('-')
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':'))
}

fn is_read_only_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "get" | "head" | "options"
    )
}

fn schema_prior(source: &str, present: bool) -> Map<String, Value> {
    let mut extra = Map::new();
    extra.insert("import_source".to_string(), json!(source));
    extra.insert(
        "schema_prior".to_string(),
        json!({"source": source, "present": present, "observed": false}),
    );
    extra
}

fn unique_operation_id(raw: String, seen_ids: &mut BTreeSet<String>) -> String {
    let base = sanitize_id(&raw);
    if seen_ids.insert(base.clone()) {
        return base;
    }
    for suffix in 2usize.. {
        let candidate = format!("{base}_{suffix}");
        if seen_ids.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search must eventually find a unique id")
}

fn sanitize_id(raw: &str) -> String {
    let mut output = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    while output.contains("__") {
        output = output.replace("__", "_");
    }
    let output = output.trim_matches('_').to_string();
    if output.is_empty() {
        "operation".to_string()
    } else {
        output
    }
}

fn env_key(raw: &str) -> String {
    sanitize_id(raw).to_ascii_uppercase()
}

fn object<'a>(value: &'a Value, format: &str, field: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| import_error(format, format!("{field} must be an object")))
}

fn required_str<'a>(map: &'a Map<String, Value>, field: &str, context: &str) -> Result<&'a str> {
    map.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| import_error("openapi", format!("{context}.{field} must be a string")))
}

fn import_error(format: impl Into<String>, reason: impl Into<String>) -> CoreError {
    CoreError::ImportError {
        format: format.into(),
        reason: reason.into(),
    }
}

fn push_warning(warnings: &mut Vec<String>, warning: String) {
    if warnings.len() < 32 && !warnings.contains(&warning) {
        warnings.push(warning);
    }
}

#[derive(Debug, Clone, Deserialize)]
struct OpenApiOperation {
    #[serde(rename = "operationId", default)]
    operation_id: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Vec<OpenApiParameter>,
    #[serde(rename = "requestBody", default)]
    request_body: Option<OpenApiRequestBody>,
    #[serde(default)]
    responses: BTreeMap<String, OpenApiResponse>,
    #[serde(default)]
    deprecated: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenApiParameter {
    name: String,
    #[serde(rename = "in", default)]
    location: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    schema: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenApiRequestBody {
    #[serde(default)]
    required: bool,
    #[serde(default)]
    content: BTreeMap<String, OpenApiMediaType>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenApiResponse {
    #[serde(default)]
    content: BTreeMap<String, OpenApiMediaType>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenApiMediaType {
    #[serde(default)]
    schema: Option<Value>,
}

/// MCP tool schema declaration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default = "default_object_schema")]
    pub input_schema: Value,
    #[serde(rename = "outputSchema", default)]
    pub output_schema: Option<Value>,
    #[serde(rename = "readOnlyHint", default)]
    pub read_only_hint: Option<bool>,
    /// Destructive hint. When `Some(true)` it contradicts a `readOnlyHint` of
    /// `true` and tightens the tool to *unproven*/mutating (monotone tightening)
    /// so a contradictory annotation can never silently relax confirmation.
    #[serde(rename = "destructiveHint", default)]
    pub destructive_hint: Option<bool>,
    #[serde(default)]
    pub annotations: Option<BTreeMap<String, Value>>,
}

/// MCP resource schema declaration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpResource {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default)]
    pub mime_type: Option<String>,
}

fn default_object_schema() -> Value {
    json!({"type": "object", "properties": {}, "additionalProperties": false})
}

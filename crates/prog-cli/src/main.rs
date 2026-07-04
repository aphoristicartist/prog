use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use prog_adapters::{
    cli::{CliOperation, CliSource},
    http::{HttpOperation, HttpSource},
    mcp::McpSource,
};
use prog_core::{
    AuthRef, CachePolicy, CoreError, DISCLOSURE_VERSION, EffectSet, Extra, NextAction,
    OmittedRegion, OperationProfile, PreviewPolicy, RedactionPolicy, Result,
    SOURCE_PROFILE_VERSION, SliceRequest, SourceProfile, Store, TrustSettings, expand, infer, join,
    new_cache_entry, project, render_hints,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tracing_subscriber::{EnvFilter, fmt::writer::MakeWriterExt};

#[derive(Debug, Parser)]
#[command(
    name = "prog",
    version,
    about = "Progressive-disclosure gateway for APIs, CLIs, and MCP servers"
)]
struct Cli {
    #[arg(long, env = "PROG_DIR", default_value = "./.prog", global = true)]
    dir: PathBuf,

    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Discover(DiscoverArgs),
    Hints(HintsArgs),
    Call(CallArgs),
    Expand(ExpandArgs),
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    Meta(MetaArgs),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SourceKind {
    Http,
    Cli,
    Mcp,
}

#[derive(Debug, Args)]
struct DiscoverArgs {
    source_id: String,

    #[arg(long)]
    kind: SourceKind,

    #[arg(long)]
    seed: String,

    #[arg(long)]
    probe: bool,
}

#[derive(Debug, Args)]
struct HintsArgs {
    source_id: String,
    operation: Option<String>,
}

#[derive(Debug, Args)]
struct CallArgs {
    source_id: String,
    operation: String,

    #[arg(long)]
    args: String,

    #[arg(long)]
    view: Option<String>,

    #[arg(long)]
    yes: bool,

    #[arg(long)]
    no_cache: bool,

    #[arg(long)]
    refresh: bool,
}

#[derive(Debug, Args)]
struct ExpandArgs {
    cursor: String,

    #[arg(long, default_value = "")]
    path: String,

    #[arg(long)]
    limit: Option<usize>,

    #[arg(long)]
    depth: Option<usize>,

    #[arg(long, value_delimiter = ',')]
    fields: Vec<String>,

    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    List,
    Get(CacheGetArgs),
    Purge(CachePurgeArgs),
}

#[derive(Debug, Args)]
struct CacheGetArgs {
    key: String,
}

#[derive(Debug, Args)]
struct CachePurgeArgs {
    #[arg(long)]
    source: Option<String>,

    #[arg(long)]
    expired: bool,

    #[arg(long)]
    all: bool,
}

#[derive(Debug, Args)]
struct MetaArgs {
    contract: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = err.print();
            return ExitCode::from(err.exit_code() as u8);
        }
        Err(err) => {
            let error = CoreError::CliUsage(err.to_string());
            return write_error(&error, false);
        }
    };

    match run(&cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => write_error(&error, cli.pretty),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr.with_max_level(tracing::Level::TRACE))
        .try_init();
}

async fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Discover(args) => {
            let store = Store::open(&cli.dir)?;
            let report = discover_source(&store, args).await?;
            write_success(&report, cli.pretty)
        }
        Command::Hints(args) => {
            let store = Store::open(&cli.dir)?;
            let response = hints_source(&store, args)?;
            write_success(&response, cli.pretty)
        }
        Command::Call(args) => {
            let _ = (
                &args.source_id,
                &args.operation,
                &args.args,
                &args.view,
                args.yes,
                args.no_cache,
                args.refresh,
            );
            not_implemented("call")
        }
        Command::Expand(args) => {
            let _ = (
                &args.cursor,
                &args.path,
                &args.limit,
                &args.depth,
                &args.fields,
                &args.out,
            );
            not_implemented("expand")
        }
        Command::Cache { command } => match command {
            CacheCommand::List => {
                let store = Store::open(&cli.dir)?;
                write_success(&store.list_entries(100)?, cli.pretty)
            }
            CacheCommand::Get(args) => {
                let store = Store::open(&cli.dir)?;
                let entry = store
                    .get_entry(&args.key)?
                    .ok_or_else(|| CoreError::CacheMiss(args.key.clone()))?;
                let payload = store
                    .get_payload(&entry.payload_hash)?
                    .ok_or_else(|| CoreError::CacheMiss(args.key.clone()))?;
                let projection = expand(
                    &payload,
                    "",
                    &SliceRequest {
                        path: None,
                        limit: None,
                        depth: None,
                        fields: Vec::new(),
                        omit: Vec::new(),
                        extra: serde_json::Map::new(),
                    },
                    &PreviewPolicy::default(),
                )?;
                write_success(&CacheGetOutput { entry, projection }, cli.pretty)
            }
            CacheCommand::Purge(args) => {
                let store = Store::open(&cli.dir)?;
                let summary = if args.all {
                    store.purge_all()?
                } else if args.expired {
                    store.purge_expired(chrono::Utc::now())?
                } else if let Some(source) = &args.source {
                    store.purge_source(source)?
                } else {
                    return Err(CoreError::BadArgs {
                        operation: "cache purge".to_string(),
                        reason: "pass --all, --expired, or --source <id>".to_string(),
                    });
                };
                write_success(&summary, cli.pretty)
            }
        },
        Command::Meta(args) => {
            let _ = &args.contract;
            not_implemented("meta")
        }
    }
}

#[derive(Serialize)]
struct DiscoverReport {
    schema_version: &'static str,
    source_id: String,
    kind: prog_core::SourceKind,
    profile_version: u64,
    operations_found: usize,
    operations_probed: usize,
    shapes_learned: usize,
    warnings: Vec<String>,
    effects_assumed: Vec<String>,
}

#[derive(Serialize)]
struct HintsResponse {
    schema_version: &'static str,
    source_id: String,
    profile_version: u64,
    hints: Value,
    omitted: Vec<OmittedRegion>,
    cursor: Option<String>,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct PreparedDiscovery {
    profile: SourceProfile,
    probe: Option<ProbeSource>,
    warnings: Vec<String>,
    effects_assumed: Vec<String>,
}

#[derive(Debug)]
enum ProbeSource {
    Http(HttpSource),
    Cli(CliSource),
    Mcp(McpSource),
}

#[derive(Debug, Deserialize)]
struct GenericSeed {
    #[serde(default)]
    kind: Option<String>,
}

async fn discover_source(store: &Store, args: &DiscoverArgs) -> Result<DiscoverReport> {
    let seed = read_seed(&args.seed)?;
    validate_seed_kind(args.kind, &seed)?;
    let mut prepared = prepare_discovery(&args.source_id, args.kind, seed).await?;
    let operations_found = prepared.profile.operations.len();
    let mut operations_probed = 0usize;
    let mut shapes_learned = 0usize;

    if args.probe {
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

    let profile = store.update_profile(&args.source_id, |current| {
        merge_profiles(current, prepared.profile.clone())
    })?;

    Ok(DiscoverReport {
        schema_version: DISCLOSURE_VERSION,
        source_id: args.source_id.clone(),
        kind: profile.kind,
        profile_version: profile.version,
        operations_found,
        operations_probed,
        shapes_learned,
        warnings: prepared.warnings,
        effects_assumed: prepared.effects_assumed,
    })
}

fn hints_source(store: &Store, args: &HintsArgs) -> Result<HintsResponse> {
    let profile = store
        .read_profile(&args.source_id)?
        .ok_or_else(|| CoreError::UnknownSource(args.source_id.clone()))?;
    let hints = build_hints_document(&profile, args.operation.as_deref())?;
    let payload_hash = store.put_payload(&hints)?;
    let projection = project(&hints, &PreviewPolicy::default(), "");
    let cache_key = Store::cache_key(
        &args.source_id,
        "hints",
        &json!({"operation": args.operation}),
    )?;
    let entry = new_cache_entry(
        cache_key.clone(),
        payload_hash,
        args.source_id.clone(),
        "hints".to_string(),
        serde_json::to_vec(&hints)?
            .len()
            .try_into()
            .unwrap_or(u64::MAX),
        86_400,
    );
    store.put_entry(&cache_key, &entry)?;
    let cursor = if projection.omitted.is_empty() {
        None
    } else {
        Some(store.create_cursor(&cache_key, &args.source_id, "hints", "", 1, 86_400)?)
    };

    Ok(HintsResponse {
        schema_version: DISCLOSURE_VERSION,
        source_id: args.source_id.clone(),
        profile_version: profile.version,
        hints: projection.preview,
        omitted: projection.omitted,
        cursor,
        warnings: Vec::new(),
    })
}

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
            true,
            false,
        );
        if assumed {
            effects_assumed.push(format!("{id}: no effect metadata, assumed unsafe"));
        }
        let query = string_map(operation_value.get("query"), "operations[].query")?;
        let headers = string_map(operation_value.get("headers"), "operations[].headers")?;
        let json_body = operation_value
            .get("json_body")
            .or_else(|| operation_value.get("body"))
            .cloned();
        let mut extra = Extra::new();
        extra.insert(
            "invocation".to_string(),
            json!({"http": {"method": method, "path": path, "query": query}}),
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
            sensitive_args: string_vec(
                operation_value.get("sensitive_args"),
                "operations[].sensitive_args",
            )?,
        });
    }

    Ok(PreparedDiscovery {
        profile: SourceProfile {
            schema_version: SOURCE_PROFILE_VERSION.to_string(),
            id: source_id.to_string(),
            kind: prog_core::SourceKind::Http,
            version: 1,
            description: optional_string(seed, "description")?,
            operations,
            auth: auth.clone(),
            cache: CachePolicy::default(),
            trust: TrustSettings {
                allow_network: true,
                ..TrustSettings::default()
            },
            effect_defaults: EffectSet::default(),
            extra: seed_extra("http", seed),
        },
        probe: Some(ProbeSource::Http(HttpSource {
            id: source_id.to_string(),
            base_url,
            timeout_ms: 30_000,
            max_response_bytes: 1024 * 1024,
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
            false,
            shell,
        );
        if assumed {
            effects_assumed.push(format!("{id}: no effect metadata, assumed unsafe"));
        }
        let env = string_map(operation_value.get("env"), "operations[].env")?;
        let mut extra = Extra::new();
        extra.insert(
            "invocation".to_string(),
            json!({"cli": {"command": command, "args": args, "env": env.keys().cloned().collect::<Vec<_>>()}}),
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
            working_dir: operation_value
                .get("working_dir")
                .and_then(Value::as_str)
                .map(PathBuf::from),
            shell,
            timeout_ms: None,
            max_stdout_bytes: None,
            max_stderr_bytes: None,
            sensitive_args: string_vec(
                operation_value.get("sensitive_args"),
                "operations[].sensitive_args",
            )?,
        });
    }

    Ok(PreparedDiscovery {
        profile: SourceProfile {
            schema_version: SOURCE_PROFILE_VERSION.to_string(),
            id: source_id.to_string(),
            kind: prog_core::SourceKind::Cli,
            version: 1,
            description: optional_string(seed, "description")?,
            operations,
            auth: Vec::new(),
            cache: CachePolicy::default(),
            trust: trust.clone(),
            effect_defaults: EffectSet::default(),
            extra: seed_extra("cli", seed),
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
    Ok(PreparedDiscovery {
        profile: discovery.profile,
        probe: Some(ProbeSource::Mcp(source)),
        warnings: discovery.warnings,
        effects_assumed: Vec::new(),
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
        if !operation.effects.read_only {
            warnings.push(format!(
                "I6: skipped probe for '{}' because effect.read_only is not explicitly true",
                operation.id
            ));
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
                if let Some(tool_name) = operation
                    .extra
                    .get("invocation")
                    .and_then(|value| value.get("mcp"))
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
    let redacted = RedactionPolicy::default().apply_persistence(data).0;
    let observed = infer(&redacted);
    operation.output_shape = Some(match &operation.output_shape {
        Some(current) => join(current, &observed),
        None => observed,
    });
    let projection = project(&redacted, &PreviewPolicy::default(), "");
    let examples = operation
        .extra
        .entry("examples".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(examples) = examples {
        examples.push(json!({
            "args": args,
            "projection": projection
        }));
    }
}

fn merge_profiles(current: Option<SourceProfile>, mut authored: SourceProfile) -> SourceProfile {
    let Some(current) = current else {
        return authored;
    };

    for operation in &mut authored.operations {
        if let Some(existing) = current
            .operations
            .iter()
            .find(|candidate| candidate.id == operation.id)
        {
            operation.output_shape = match (&operation.output_shape, &existing.output_shape) {
                (Some(left), Some(right)) => Some(join(left, right)),
                (None, Some(shape)) => Some(shape.clone()),
                (shape, None) => shape.clone(),
            };
            if operation.declared_output_schema.is_none() {
                operation.declared_output_schema = existing.declared_output_schema.clone();
            }
            if operation.pagination.is_none() {
                operation.pagination = existing.pagination.clone();
            }
            for key in ["examples"] {
                if !operation.extra.contains_key(key)
                    && let Some(value) = existing.extra.get(key)
                {
                    operation.extra.insert(key.to_string(), value.clone());
                }
            }
        }
    }

    for existing in current.operations {
        if !authored
            .operations
            .iter()
            .any(|operation| operation.id == existing.id)
        {
            authored.operations.push(existing);
        }
    }
    for (key, value) in current.extra {
        authored.extra.entry(key).or_insert(value);
    }
    authored
}

fn build_hints_document(profile: &SourceProfile, operation_filter: Option<&str>) -> Result<Value> {
    let mut operations = Vec::new();
    let selected: Vec<&OperationProfile> = match operation_filter {
        Some(operation) => {
            let operation = profile
                .operations
                .iter()
                .find(|candidate| candidate.id == operation)
                .ok_or_else(|| CoreError::UnknownOperation {
                    source_id: profile.id.clone(),
                    operation: operation.to_string(),
                })?;
            vec![operation]
        }
        None => profile.operations.iter().collect(),
    };

    for operation in selected {
        operations.push(operation_hint(operation));
    }

    Ok(json!({
        "source_id": profile.id,
        "kind": profile.kind,
        "version": profile.version,
        "operation_count": profile.operations.len(),
        "operations": operations,
        "suggested_next_calls": profile.operations.iter().take(10).map(|operation| {
            json!({"kind": "call", "operation": operation.id, "reason": "operation is available in the source profile"})
        }).collect::<Vec<_>>()
    }))
}

fn operation_hint(operation: &OperationProfile) -> Value {
    let (required_inputs, optional_inputs) = schema_inputs(&operation.input_schema);
    let declared_fields = operation
        .declared_output_schema
        .as_ref()
        .map(declared_schema_fields)
        .unwrap_or_default();
    let observed_fields = operation
        .output_shape
        .as_ref()
        .map(|shape| render_hints(shape, ""))
        .unwrap_or_default();
    let expandable_regions = operation
        .extra
        .get("examples")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|example| {
            example
                .get("projection")
                .and_then(|projection| projection.get("omitted"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|omitted| omitted.get("path").and_then(Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    json!({
        "id": operation.id,
        "description": operation.description,
        "inputs": {
            "required": required_inputs,
            "optional": optional_inputs
        },
        "output_fields": {
            "declared": declared_fields,
            "observed": observed_fields
        },
        "expandable_regions": expandable_regions,
        "effects": operation.effects,
        "cache": operation.cache,
        "risk_notes": risk_notes(&operation.effects),
        "next_actions": [
            NextAction {
                kind: "call".to_string(),
                operation: Some(operation.id.clone()),
                path: None,
                reason: Some("run this operation with JSON args".to_string()),
                extra: Extra::new(),
            }
        ],
    })
}

fn schema_inputs(schema: &Value) -> (Vec<String>, Vec<String>) {
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let optional = properties
        .difference(&required)
        .cloned()
        .collect::<Vec<_>>();
    (required.into_iter().collect(), optional)
}

fn declared_schema_fields(schema: &Value) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    collect_declared_fields(schema, "", &mut fields);
    fields
}

fn collect_declared_fields(schema: &Value, path: &str, fields: &mut BTreeMap<String, String>) {
    if let Some(schema_type) = schema.get("type").and_then(Value::as_str)
        && !path.is_empty()
    {
        fields.insert(path.to_string(), format!("{schema_type} (declared)"));
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, value) in properties {
            collect_declared_fields(value, &json_pointer_child(path, name), fields);
        }
    }
    if let Some(items) = schema.get("items") {
        collect_declared_fields(items, &json_pointer_child(path, "*"), fields);
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        fields.insert(path.to_string(), format!("$ref {reference} (declared)"));
    }
}

fn json_pointer_child(path: &str, child: &str) -> String {
    let escaped = child.replace('~', "~0").replace('/', "~1");
    if path.is_empty() {
        format!("/{escaped}")
    } else {
        format!("{path}/{escaped}")
    }
}

fn risk_notes(effects: &EffectSet) -> Vec<String> {
    let mut notes = Vec::new();
    if !effects.read_only {
        notes.push("not explicitly read-only; mutation risk fails closed".to_string());
    }
    if effects.requires_confirmation {
        notes.push("requires confirmation before call execution".to_string());
    }
    if effects.shell {
        notes.push("shell-backed operation requires trusted profile settings".to_string());
    }
    if effects.sensitive {
        notes.push("may handle sensitive data".to_string());
    }
    notes
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CoreError::BadArgs {
            operation: "discover".to_string(),
            reason: format!("seed.{field} must be a string"),
        })
}

fn optional_string(value: &Value, field: &str) -> Result<Option<String>> {
    value
        .get(field)
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| CoreError::BadArgs {
                    operation: "discover".to_string(),
                    reason: format!("seed.{field} must be a string"),
                })
        })
        .transpose()
}

fn optional_bool(value: &Value, field: &str) -> Result<Option<bool>> {
    value
        .get(field)
        .map(|value| {
            value.as_bool().ok_or_else(|| CoreError::BadArgs {
                operation: "discover".to_string(),
                reason: format!("seed.{field} must be a boolean"),
            })
        })
        .transpose()
}

fn required_array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| CoreError::BadArgs {
            operation: "discover".to_string(),
            reason: format!("seed.{field} must be an array"),
        })
}

fn operation_id(value: &Value) -> Result<String> {
    value
        .get("id")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CoreError::BadArgs {
            operation: "discover".to_string(),
            reason: "seed.operations[].name must be a string".to_string(),
        })
}

fn input_schema(value: &Value) -> Result<Value> {
    if let Some(schema) = value
        .get("input_schema")
        .or_else(|| value.get("inputSchema"))
    {
        return Ok(schema.clone());
    }
    let Some(args) = value.get("args").and_then(Value::as_object) else {
        return Ok(json!({"type": "object", "properties": {}}));
    };
    let mut properties = Map::new();
    for (name, value) in args {
        let schema_type = value.as_str().ok_or_else(|| CoreError::BadArgs {
            operation: "discover".to_string(),
            reason: format!("seed.operations[].args.{name} must be a type string"),
        })?;
        properties.insert(name.clone(), json!({"type": schema_type}));
    }
    Ok(json!({
        "type": "object",
        "required": args.keys().cloned().collect::<Vec<_>>(),
        "properties": properties
    }))
}

fn auth_refs(seed: &Value) -> Result<Vec<AuthRef>> {
    let values = seed
        .get("auth_refs")
        .or_else(|| seed.get("auth"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    values
        .into_iter()
        .map(|value| serde_json::from_value(value).map_err(CoreError::from))
        .collect()
}

fn string_map(value: Option<&Value>, field: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value.as_object().ok_or_else(|| CoreError::BadArgs {
        operation: "discover".to_string(),
        reason: format!("seed.{field} must be an object"),
    })?;
    object
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| CoreError::BadArgs {
                    operation: "discover".to_string(),
                    reason: format!("seed.{field}.{key} must be a string"),
                })
        })
        .collect()
}

fn string_vec(value: Option<&Value>, field: &str) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or_else(|| CoreError::BadArgs {
        operation: "discover".to_string(),
        reason: format!("seed.{field} must be an array"),
    })?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| CoreError::BadArgs {
                    operation: "discover".to_string(),
                    reason: format!("seed.{field} entries must be strings"),
                })
        })
        .collect()
}

fn effects_from_seed(
    value: Option<&Value>,
    network_default: bool,
    shell_default: bool,
) -> (EffectSet, bool) {
    let mut effects = EffectSet {
        read_only: false,
        mutating: true,
        network: network_default,
        shell: shell_default,
        sensitive: false,
        cacheable: false,
        requires_confirmation: true,
        extra: Extra::new(),
    };
    let Some(value) = value.and_then(Value::as_object) else {
        return (effects, true);
    };
    if let Some(read_only) = value.get("read_only").and_then(Value::as_bool) {
        effects.read_only = read_only;
        if read_only {
            effects.mutating = false;
            effects.requires_confirmation = false;
            effects.cacheable = true;
        }
    }
    if let Some(mutating) = value.get("mutating").and_then(Value::as_bool) {
        effects.mutating = mutating;
    }
    if let Some(network) = value.get("network").and_then(Value::as_bool) {
        effects.network = network;
    }
    if let Some(shell) = value.get("shell").and_then(Value::as_bool) {
        effects.shell = shell;
    }
    if let Some(sensitive) = value.get("sensitive").and_then(Value::as_bool) {
        effects.sensitive = sensitive;
    }
    if let Some(cacheable) = value.get("cacheable").and_then(Value::as_bool) {
        effects.cacheable = cacheable;
    }
    if let Some(requires_confirmation) = value.get("requires_confirmation").and_then(Value::as_bool)
    {
        effects.requires_confirmation = requires_confirmation;
    }
    (effects, false)
}

fn example_args(schema: &Value) -> Value {
    let mut args = Map::new();
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for name in required {
        let schema = properties.get(name).unwrap_or(&Value::Null);
        args.insert(name.to_string(), example_value(schema));
    }
    Value::Object(args)
}

fn example_value(schema: &Value) -> Value {
    if let Some(value) = schema.get("default") {
        return value.clone();
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("integer") => json!(0),
        Some("number") => json!(0.0),
        Some("boolean") => json!(false),
        Some("array") => json!([]),
        Some("object") => json!({}),
        _ => json!(""),
    }
}

fn seed_extra(kind: &str, seed: &Value) -> Extra {
    let mut extra = Extra::new();
    extra.insert("seed_kind".to_string(), json!(kind));
    if let Some(value) = seed.get("base_url").or_else(|| seed.get("command")) {
        extra.insert("seed_origin".to_string(), value.clone());
    }
    extra
}

fn core_kind(kind: SourceKind) -> prog_core::SourceKind {
    match kind {
        SourceKind::Http => prog_core::SourceKind::Http,
        SourceKind::Cli => prog_core::SourceKind::Cli,
        SourceKind::Mcp => prog_core::SourceKind::Mcp,
    }
}

#[derive(Serialize)]
struct CacheGetOutput {
    entry: prog_core::CacheEntryMeta,
    projection: prog_core::Projection,
}

fn not_implemented(command: &'static str) -> Result<()> {
    Err(CoreError::NotImplemented { command })
}

fn write_success<T: Serialize>(value: &T, pretty: bool) -> Result<()> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
    }
    Ok(())
}

fn write_error(error: &CoreError, pretty: bool) -> ExitCode {
    let envelope = error.envelope();
    let rendered = if pretty {
        serde_json::to_string_pretty(&envelope)
    } else {
        serde_json::to_string(&envelope)
    };

    match rendered {
        Ok(json) => {
            println!("{json}");
            ExitCode::FAILURE
        }
        Err(json_error) => {
            let fallback = CoreError::Json(json_error);
            println!(
                "{}",
                serde_json::to_string(&fallback.envelope()).unwrap_or_else(|_| {
                    "{\"error\":{\"kind\":\"json\",\"message\":\"failed to render error\",\"hint\":\"Report this bug.\"}}".to_string()
                })
            );
            ExitCode::FAILURE
        }
    }
}

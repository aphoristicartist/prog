use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    process::{ExitCode, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use chrono::{SecondsFormat, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use prog_adapters::{
    cli::{CliOperation, CliSource},
    http::{HttpOperation, HttpSource},
    mcp::McpSource,
};
use prog_core::{
    AuthRef, CacheInfo, CachePolicy, CacheStatus, CallFlags, CallProvenance, CoreError,
    DISCLOSURE_VERSION, DisclosureEnvelope, EffectSet, Extra, LensManifest, NextAction,
    ObservationCompleteness, ObservationFreshness, ObservationMetadata, ObservationPayloadStatus,
    ObservationSafety, ObservationTrust, OmissionReason, OmittedRegion, OperationProfile,
    PreviewPolicy, RedactionPolicy, Result, SOURCE_PROFILE_VERSION, SliceRequest, SourceProfile,
    Store, Summary, TrustSettings, cache_allowed, call_effect_warnings, check_call,
    check_discovery, cli_adapter_effects, cli_hardening_effects, expand, http_adapter_effects,
    http_hardening_effects, infer, join, lens_slice_request, new_cache_entry, project,
    project_with_lens, public_contract_schemas, render_hints, slice_value, tighten_effects,
    validate_lens_manifest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command as TokioCommand,
    sync::mpsc,
};
use tracing_subscriber::{EnvFilter, fmt::writer::MakeWriterExt};

static RUN_CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const PROG_AGENT_SKILL: &str = include_str!("../../../skills/prog/SKILL.md");

#[derive(Debug, Parser)]
#[command(
    name = "prog",
    version,
    about = "Progressive-disclosure gateway for APIs, CLIs, and MCP servers"
)]
struct Cli {
    #[arg(long, env = "PROG_DIR", default_value = "./.prog", global = true)]
    dir: PathBuf,

    #[arg(long, env = "PROG_LENS_DIR", default_value = "./lenses", global = true)]
    lens_dir: PathBuf,

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
    Observe(ObserveArgs),
    Run(RunArgs),
    Init(InitArgs),
    Paths(PathsArgs),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum AgentKind {
    Codex,
    ClaudeCode,
    Cursor,
    GeminiCli,
}

impl AgentKind {
    fn as_str(self) -> &'static str {
        match self {
            AgentKind::Codex => "codex",
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Cursor => "cursor",
            AgentKind::GeminiCli => "gemini-cli",
        }
    }
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
    lens: Option<String>,

    #[arg(long)]
    yes: bool,

    #[arg(long)]
    no_cache: bool,

    #[arg(long)]
    refresh: bool,
}

#[derive(Debug, Args)]
struct ObserveArgs {
    #[arg(long, conflicts_with = "stdin")]
    file: Option<PathBuf>,

    #[arg(long, conflicts_with = "file")]
    stdin: bool,

    #[arg(long)]
    mime: Option<String>,

    #[arg(long)]
    name: Option<String>,

    #[arg(long, default_value_t = 86_400)]
    ttl_seconds: u64,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,

    #[arg(long, default_value_t = 1024 * 1024)]
    max_stdout_bytes: usize,

    #[arg(long, default_value_t = 1024 * 1024)]
    max_stderr_bytes: usize,

    #[arg(long, default_value_t = 86_400)]
    ttl_seconds: u64,

    #[arg(long)]
    preserve_exit_code: bool,

    #[arg(long)]
    out: Option<PathBuf>,

    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct InitArgs {
    #[arg(long, value_enum)]
    agent: AgentKind,

    #[arg(long)]
    project: bool,

    #[arg(long)]
    dry_run: bool,

    #[arg(long, default_value = ".")]
    root: PathBuf,
}

#[derive(Debug, Args)]
struct PathsArgs {
    cursor: String,

    #[arg(long, default_value = "")]
    prefix: String,

    #[arg(long)]
    reason: Option<String>,

    #[arg(long, value_delimiter = ',')]
    field: Vec<String>,

    #[arg(long)]
    omitted_only: bool,

    #[arg(long)]
    expandable_only: bool,

    #[arg(long, default_value_t = 200)]
    limit: usize,

    #[arg(long, default_value_t = 6)]
    depth: usize,
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
        Ok(exit_code) => exit_code,
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

async fn run(cli: &Cli) -> Result<ExitCode> {
    match &cli.command {
        Command::Discover(args) => {
            let store = Store::open(&cli.dir)?;
            let report = discover_source(&store, args).await?;
            write_success(&report, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Hints(args) => {
            let store = Store::open(&cli.dir)?;
            let response = hints_source(&store, args)?;
            write_success(&response, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Call(args) => {
            let store = Store::open(&cli.dir)?;
            let envelope = call_source(&store, &cli.lens_dir, args).await?;
            write_success(&envelope, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Observe(args) => {
            let store = Store::open(&cli.dir)?;
            let envelope = observe_artifact(&store, args)?;
            write_success(&envelope, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Run(args) => {
            let store = Store::open(&cli.dir)?;
            let result = run_command(&store, args).await?;
            write_success(&result.envelope, cli.pretty)?;
            Ok(if args.preserve_exit_code {
                child_exit_code(result.exit_code)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Init(args) => {
            let report = init_integration(args)?;
            write_success(&report, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Paths(args) => {
            let store = Store::open(&cli.dir)?;
            let response = paths_cursor(&store, args)?;
            write_success(&response, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Expand(args) => {
            let store = Store::open(&cli.dir)?;
            let envelope = expand_cursor(&store, args)?;
            write_success(&envelope, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Cache { command } => match command {
            CacheCommand::List => {
                let store = Store::open(&cli.dir)?;
                write_success(&store.list_entries(100)?, cli.pretty)?;
                Ok(ExitCode::SUCCESS)
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
                write_success(&CacheGetOutput { entry, projection }, cli.pretty)?;
                Ok(ExitCode::SUCCESS)
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
                write_success(&summary, cli.pretty)?;
                Ok(ExitCode::SUCCESS)
            }
        },
        Command::Meta(args) => {
            let store = Store::open(&cli.dir)?;
            let envelope = meta_contracts(&store, args)?;
            write_success(&envelope, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
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

#[derive(Debug)]
enum CallableSource {
    Http(HttpSource),
    Cli(CliSource),
    Mcp(McpSource),
}

#[derive(Debug)]
struct AdapterCall {
    data: Value,
    provenance: Value,
    status: Option<String>,
    duration_ms: Option<u64>,
    pagination: Option<Value>,
    warnings: Vec<String>,
}

struct EnvelopeInput {
    source_id: String,
    operation: String,
    source_kind: Option<String>,
    payload: Value,
    root_path: String,
    slice: SliceRequest,
    payload_bytes: u64,
    provenance: Option<CallProvenance>,
    cache: Option<CacheInfo>,
    effects: Option<EffectSet>,
    redacted_paths: usize,
    cache_disabled_reason: Option<String>,
    warnings: Vec<String>,
    schema_hints: BTreeMap<String, String>,
    next_action_operation: Option<String>,
    additional_next_actions: Vec<NextAction>,
    lens: Option<LensManifest>,
}

struct CursorInput<'a> {
    cache_key: &'a str,
    source_id: &'a str,
    operation: &'a str,
    root_path: &'a str,
    payload: &'a Value,
    slice: &'a SliceRequest,
    cache: &'a CachePolicy,
    may_cache: bool,
    lens: Option<&'a LensManifest>,
}

struct ObservationInput {
    name: String,
    input: Value,
    mime: String,
    bytes: Vec<u8>,
}

struct NormalizedObservation {
    kind: String,
    payload: Value,
    warnings: Vec<String>,
}

struct RunEnvelopeResult {
    envelope: DisclosureEnvelope,
    exit_code: RunExitCode,
}

#[derive(Clone, Copy)]
enum RunExitCode {
    Success,
    Code(i32),
    Signal(i32),
    Timeout,
    SpawnError,
}

struct RunCapture {
    stream: &'static str,
    bytes: Vec<u8>,
    total_bytes: usize,
    truncated: bool,
}

struct RunChunk {
    stream: &'static str,
    bytes: Vec<u8>,
}

struct RunText {
    text: String,
    head: Vec<String>,
    tail: Vec<String>,
    line_count: usize,
    byte_count: usize,
    captured_bytes: usize,
    truncated: bool,
    utf8_valid: bool,
    redactions: usize,
}

#[derive(Clone)]
struct RunFailureSection {
    kind: &'static str,
    stream: &'static str,
    line_start: usize,
    line_end: usize,
    lines: Vec<String>,
    reason: String,
    priority: u8,
}

struct RunPayloadInput<'a> {
    run_id: &'a str,
    argv: &'a [String],
    redacted_argv: &'a [String],
    cwd: &'a Path,
    started_at: chrono::DateTime<Utc>,
    ended_at: chrono::DateTime<Utc>,
    duration_ms: u64,
    status: &'a RunProcessStatus,
    stdout: &'a RunText,
    stderr: &'a RunText,
    combined: Vec<Value>,
    failure_sections: &'a [RunFailureSection],
    out: Option<&'a PathBuf>,
}

struct InitFileSpec {
    relative_path: &'static str,
    content: String,
    executable: bool,
}

#[derive(Debug, Serialize)]
struct InitReport {
    schema_version: &'static str,
    agent: &'static str,
    scope: &'static str,
    root: String,
    dry_run: bool,
    files: Vec<InitFileReport>,
    next_steps: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct InitFileReport {
    path: String,
    full_path: String,
    action: &'static str,
    executable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct PathsResponse {
    schema_version: &'static str,
    cursor: String,
    source_id: String,
    operation: String,
    root_path: String,
    prefix: String,
    paths: Vec<PathEntry>,
    omitted: Vec<OmittedRegion>,
    next_actions: Vec<NextAction>,
    cache: CacheInfo,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PathEntry {
    path: String,
    kind: String,
    expandable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    omitted_reason: Option<OmissionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

struct PathFilters {
    reason: Option<OmissionReason>,
    fields: BTreeSet<String>,
    omitted_only: bool,
    expandable_only: bool,
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

async fn call_source(
    store: &Store,
    lens_dir: &Path,
    args: &CallArgs,
) -> Result<DisclosureEnvelope> {
    let profile = store
        .read_profile(&args.source_id)?
        .ok_or_else(|| CoreError::UnknownSource(args.source_id.clone()))?;
    let operation = profile_operation(&profile, &args.operation)?.clone();
    let call_args = parse_json_argument(&args.args, "call --args")?;
    validate_call_args(&operation, &call_args)?;
    check_call(&operation, CallFlags { yes: args.yes }, &profile.trust)?;
    let requested_view = parse_view(args.view.as_deref())?;
    let lens = match &args.lens {
        Some(id) => {
            let lens = load_lens(lens_dir, id)?;
            validate_lens_matches_call(&lens, &profile, &operation)?;
            Some(lens)
        }
        None => None,
    };
    let view = match &lens {
        Some(lens) => lens_slice_request(lens, &requested_view)?,
        None => requested_view,
    };
    let root_path = view.path.clone().unwrap_or_default();
    let effective_cache = effective_cache_policy(&profile, &operation);
    let may_cache = !args.no_cache && cache_allowed(&operation, &effective_cache);
    let cache_key = Store::cache_key(&args.source_id, &args.operation, &call_args)?;

    if may_cache
        && !args.refresh
        && let Some(entry) = store.get_entry(&cache_key)?
    {
        let payload = store
            .get_payload(&entry.payload_hash)?
            .ok_or_else(|| CoreError::CacheMiss(cache_key.clone()))?;
        let cache_info = cache_info(
            CacheStatus::Hit,
            &entry,
            Some(age_seconds(&entry.created_at)?),
        );
        let cursor = cursor_for_projection(
            store,
            CursorInput {
                cache_key: &cache_key,
                source_id: &args.source_id,
                operation: &args.operation,
                root_path: &root_path,
                payload: &payload,
                slice: &view,
                cache: &effective_cache,
                may_cache,
                lens: lens.as_ref(),
            },
        )?;
        return envelope_for_payload(
            store,
            EnvelopeInput {
                source_id: args.source_id.clone(),
                operation: args.operation.clone(),
                source_kind: Some(profile_source_kind_name(profile.kind).to_string()),
                payload,
                root_path: root_path.clone(),
                slice: view,
                payload_bytes: entry.payload_bytes,
                provenance: entry.provenance.clone(),
                cache: Some(cache_info),
                effects: Some(operation.effects.clone()),
                redacted_paths: 0,
                cache_disabled_reason: None,
                warnings: Vec::new(),
                schema_hints: operation
                    .output_shape
                    .as_ref()
                    .map(|shape| render_hints(shape, ""))
                    .unwrap_or_default(),
                next_action_operation: Some(args.operation.clone()),
                additional_next_actions: Vec::new(),
                lens,
            },
            cursor,
        );
    }

    let source = callable_source_from_profile(&profile)?;
    let adapter_call = execute_callable(&source, &operation, &call_args).await?;
    let redaction = RedactionPolicy::default();
    let (redacted, redacted_paths) = redaction.apply_persistence(&adapter_call.data);
    let payload_bytes = json_len_u64(&redacted)?;
    let observed = infer(&redacted);
    update_profile_from_call(
        store,
        &profile,
        &operation.id,
        &call_args,
        &redacted,
        &observed,
    )?;

    let mut provenance = call_provenance(
        &cache_key,
        adapter_call.status,
        adapter_call.duration_ms,
        adapter_call.provenance,
    );
    let mut warnings = adapter_call.warnings;
    warnings.extend(call_effect_warnings(&operation));
    if !redacted_paths.is_empty() {
        warnings.push(format!(
            "redacted {} sensitive path(s) before inference and persistence",
            redacted_paths.len()
        ));
    }
    if let Some(pagination) = adapter_call.pagination {
        warnings.push(format!(
            "pagination hints available: {}",
            compact_json(&pagination)?
        ));
    }

    let mut cache_disabled_reason = None;
    let cache_status = if may_cache {
        let payload_hash = store.put_payload(&redacted)?;
        let ttl = ttl_seconds(&effective_cache);
        let mut entry = new_cache_entry(
            cache_key.clone(),
            payload_hash,
            args.source_id.clone(),
            args.operation.clone(),
            payload_bytes,
            ttl,
        );
        provenance.cache_key = Some(cache_key.clone());
        entry.provenance = Some(provenance.clone());
        store.put_entry(&cache_key, &entry)?;
        Some(cache_info(CacheStatus::Stored, &entry, Some(0)))
    } else {
        provenance.cache_key = None;
        let reason = cache_skip_warning(args.no_cache, &operation);
        warnings.push(reason.clone());
        cache_disabled_reason = Some(reason);
        Some(CacheInfo {
            status: CacheStatus::Skipped,
            ttl_seconds: None,
            expires_at: None,
            age_seconds: None,
            extra: Extra::new(),
        })
    };

    let cursor = cursor_for_projection(
        store,
        CursorInput {
            cache_key: &cache_key,
            source_id: &args.source_id,
            operation: &args.operation,
            root_path: &root_path,
            payload: &redacted,
            slice: &view,
            cache: &effective_cache,
            may_cache,
            lens: lens.as_ref(),
        },
    )?;
    envelope_for_payload(
        store,
        EnvelopeInput {
            source_id: args.source_id.clone(),
            operation: args.operation.clone(),
            source_kind: Some(profile_source_kind_name(profile.kind).to_string()),
            payload: redacted,
            root_path,
            slice: view,
            payload_bytes,
            provenance: Some(provenance),
            cache: cache_status,
            effects: Some(operation.effects.clone()),
            redacted_paths: redacted_paths.len(),
            cache_disabled_reason,
            warnings,
            schema_hints: render_hints(&observed, ""),
            next_action_operation: Some(args.operation.clone()),
            additional_next_actions: Vec::new(),
            lens,
        },
        cursor,
    )
}

fn observe_artifact(store: &Store, args: &ObserveArgs) -> Result<DisclosureEnvelope> {
    let input = read_observation_input(args)?;
    let normalized = normalize_observation(&input.bytes, &input.mime)?;
    let redaction = RedactionPolicy::default();
    let (redacted, redacted_paths) = redaction.apply_persistence(&normalized.payload);
    let redacted_bytes = serde_json::to_vec(&redacted)?;
    let payload_bytes = redacted_bytes.len().try_into().unwrap_or(u64::MAX);
    let cache_key = Store::cache_key(
        "observe",
        &input.name,
        &json!({
            "kind": normalized.kind,
            "mime": input.mime,
            "redacted_sha256": hex_sha256(&redacted_bytes)
        }),
    )?;
    let payload_hash = store.put_payload(&redacted)?;
    let ttl: i64 = args
        .ttl_seconds
        .try_into()
        .map_err(|_| CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "ttl_seconds is too large".to_string(),
        })?;
    let mut entry = new_cache_entry(
        cache_key.clone(),
        payload_hash,
        "observe".to_string(),
        input.name.clone(),
        payload_bytes,
        ttl,
    );
    entry.provenance = Some(observation_provenance(
        &cache_key,
        &input,
        &normalized.kind,
        redacted_paths.len(),
    ));
    store.put_entry(&cache_key, &entry)?;

    let slice = SliceRequest {
        path: None,
        limit: None,
        depth: None,
        fields: Vec::new(),
        omit: Vec::new(),
        extra: Extra::new(),
    };
    let cursor = Some(store.create_cursor(
        &cache_key,
        "observe",
        &input.name,
        "",
        RedactionPolicy::default().version,
        ttl,
    )?);
    let mut warnings = normalized.warnings;
    if !redacted_paths.is_empty() {
        warnings.push(format!(
            "redacted {} sensitive path(s) before persistence",
            redacted_paths.len()
        ));
    }
    envelope_for_payload(
        store,
        EnvelopeInput {
            source_id: "observe".to_string(),
            operation: input.name,
            source_kind: Some("artifact".to_string()),
            payload: redacted,
            root_path: "".to_string(),
            slice,
            payload_bytes,
            provenance: entry.provenance.clone(),
            cache: Some(cache_info(CacheStatus::Stored, &entry, Some(0))),
            effects: None,
            redacted_paths: redacted_paths.len(),
            cache_disabled_reason: None,
            warnings,
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: Vec::new(),
            lens: None,
        },
        cursor,
    )
}

async fn run_command(store: &Store, args: &RunArgs) -> Result<RunEnvelopeResult> {
    let cwd = std::env::current_dir()?;
    let started_at = Utc::now();
    let started_instant = Instant::now();
    let argv = args.command.clone();
    let run_sequence = RUN_CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let started_at_nanos = started_at
        .timestamp_nanos_opt()
        .map(|value| value.to_string())
        .unwrap_or_else(|| started_at.timestamp_micros().to_string());
    let run_id = format!(
        "run_{}_{}_{}",
        std::process::id(),
        started_at_nanos,
        run_sequence
    );
    let operation = run_operation_name(&argv);
    let redacted_argv = redact_run_argv(&argv);
    let cache_args = json!({
        "run_id": &run_id,
        "argv": argv,
        "cwd": cwd.to_string_lossy(),
        "started_at": started_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
    });
    let cache_key = Store::cache_key("run", &operation, &cache_args)?;

    let mut command = TokioCommand::new(&args.command[0]);
    command
        .args(&args.command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_run_process_group(&mut command);

    let run = match command.spawn() {
        Ok(child) => {
            run_spawned_child(
                child,
                args.timeout_ms,
                args.max_stdout_bytes,
                args.max_stderr_bytes,
            )
            .await?
        }
        Err(error) => RunProcessResult {
            stdout: empty_run_capture("stdout"),
            stderr: empty_run_capture("stderr"),
            combined: Vec::new(),
            status: RunProcessStatus::SpawnError(error.to_string()),
        },
    };

    let ended_at = Utc::now();
    let duration_ms = started_instant
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    let stdout_text = run_text_from_capture(&run.stdout);
    let stderr_text = run_text_from_capture(&run.stderr);
    let combined = run
        .combined
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let text = redact_run_output_bytes(&chunk.bytes).text;
            json!({
                "index": index,
                "stream": chunk.stream,
                "text": text,
                "byte_count": chunk.bytes.len()
            })
        })
        .collect::<Vec<_>>();
    let failure_sections = detect_run_failure_sections(&run.status, &stdout_text, &stderr_text);
    let payload = run_payload(RunPayloadInput {
        run_id: &run_id,
        argv: &args.command,
        redacted_argv: &redacted_argv,
        cwd: &cwd,
        started_at,
        ended_at,
        duration_ms,
        status: &run.status,
        stdout: &stdout_text,
        stderr: &stderr_text,
        combined,
        failure_sections: &failure_sections,
        out: args.out.as_ref(),
    });
    let redaction = RedactionPolicy::default();
    let (redacted_payload, policy_redactions) = redaction.apply_persistence(&payload);
    if let Some(path) = &args.out {
        write_private_file(path, &serde_json::to_vec_pretty(&redacted_payload)?)?;
    }
    let payload_hash = store.put_payload(&redacted_payload)?;
    let payload_bytes = json_len_u64(&redacted_payload)?;
    let ttl: i64 = args
        .ttl_seconds
        .try_into()
        .map_err(|_| CoreError::BadArgs {
            operation: "run".to_string(),
            reason: "ttl_seconds is too large".to_string(),
        })?;
    let mut provenance = run_provenance(
        &run_id,
        &cache_key,
        &redacted_argv,
        &cwd,
        duration_ms,
        &run.status,
        args,
    );
    let mut entry = new_cache_entry(
        cache_key.clone(),
        payload_hash,
        "run".to_string(),
        operation.clone(),
        payload_bytes,
        ttl,
    );
    entry.provenance = Some(provenance.clone());
    store.put_entry(&cache_key, &entry)?;
    let cursor = Some(store.create_cursor(
        &cache_key,
        "run",
        &operation,
        "",
        RedactionPolicy::default().version,
        ttl,
    )?);
    provenance.cache_key = Some(cache_key.clone());

    let mut warnings = run_warnings(&run.status, args, &run.stdout, &run.stderr);
    let text_redactions = stdout_text
        .redactions
        .saturating_add(stderr_text.redactions)
        .saturating_add(
            redacted_argv
                .iter()
                .filter(|arg| arg.contains("[REDACTED"))
                .count(),
        );
    let redacted_paths = policy_redactions.len().saturating_add(text_redactions);
    if redacted_paths > 0 {
        warnings.push(format!(
            "redacted {redacted_paths} sensitive value(s) before persistence"
        ));
    }
    if args.out.is_some() {
        warnings.push("wrote redacted structured run capture to --out path".to_string());
    }
    let next_actions = run_next_actions(cursor.as_deref(), &failure_sections);
    let envelope = envelope_for_payload(
        store,
        EnvelopeInput {
            source_id: "run".to_string(),
            operation,
            source_kind: Some("cli".to_string()),
            payload: redacted_payload,
            root_path: "".to_string(),
            slice: SliceRequest {
                path: None,
                limit: None,
                depth: None,
                fields: Vec::new(),
                omit: Vec::new(),
                extra: Extra::new(),
            },
            payload_bytes,
            provenance: Some(provenance),
            cache: Some(cache_info(CacheStatus::Stored, &entry, Some(0))),
            effects: Some(run_effects()),
            redacted_paths,
            cache_disabled_reason: None,
            warnings,
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: next_actions,
            lens: None,
        },
        cursor,
    )?;

    Ok(RunEnvelopeResult {
        envelope,
        exit_code: run_exit_code(&run.status),
    })
}

struct RunProcessResult {
    stdout: RunCapture,
    stderr: RunCapture,
    combined: Vec<RunChunk>,
    status: RunProcessStatus,
}

enum RunProcessStatus {
    Exited {
        success: bool,
        code: Option<i32>,
        signal: Option<i32>,
    },
    TimedOut,
    SpawnError(String),
}

async fn run_spawned_child(
    mut child: tokio::process::Child,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<RunProcessResult> {
    let stdout = child.stdout.take().ok_or_else(|| CoreError::CliTransport {
        operation: "run".to_string(),
        message: "failed to capture stdout".to_string(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| CoreError::CliTransport {
        operation: "run".to_string(),
        message: "failed to capture stderr".to_string(),
    })?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let stdout_task = tokio::spawn(read_run_stream(
        "stdout",
        stdout,
        max_stdout_bytes,
        tx.clone(),
    ));
    let stderr_task = tokio::spawn(read_run_stream(
        "stderr",
        stderr,
        max_stderr_bytes,
        tx.clone(),
    ));
    drop(tx);

    let wait = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await;
    let status = match wait {
        Ok(result) => {
            let status = result.map_err(|error| CoreError::CliTransport {
                operation: "run".to_string(),
                message: error.to_string(),
            })?;
            RunProcessStatus::Exited {
                success: status.success(),
                code: status.code(),
                signal: exit_signal(&status),
            }
        }
        Err(_) => {
            kill_run_process_group(&mut child).await;
            RunProcessStatus::TimedOut
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|error| CoreError::CliTransport {
            operation: "run".to_string(),
            message: error.to_string(),
        })?
        .map_err(|error| CoreError::CliTransport {
            operation: "run".to_string(),
            message: error.to_string(),
        })?;
    let stderr = stderr_task
        .await
        .map_err(|error| CoreError::CliTransport {
            operation: "run".to_string(),
            message: error.to_string(),
        })?
        .map_err(|error| CoreError::CliTransport {
            operation: "run".to_string(),
            message: error.to_string(),
        })?;
    let mut combined = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        combined.push(chunk);
    }
    Ok(RunProcessResult {
        stdout,
        stderr,
        combined,
        status,
    })
}

#[cfg(unix)]
fn configure_run_process_group(command: &mut TokioCommand) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_run_process_group(_command: &mut TokioCommand) {}

async fn kill_run_process_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
            let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

async fn read_run_stream<R: AsyncRead + Unpin>(
    stream: &'static str,
    mut reader: R,
    cap: usize,
    tx: mpsc::UnboundedSender<RunChunk>,
) -> std::io::Result<RunCapture> {
    let mut output = Vec::new();
    let mut total_bytes = 0usize;
    let mut truncated = false;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read);
        let remaining = cap.saturating_sub(output.len());
        if remaining > 0 {
            let stored = read.min(remaining);
            let bytes = buffer[..stored].to_vec();
            output.extend_from_slice(&bytes);
            let _ = tx.send(RunChunk { stream, bytes });
        }
        if read > remaining || total_bytes > cap {
            truncated = true;
        }
    }
    Ok(RunCapture {
        stream,
        bytes: output,
        total_bytes,
        truncated,
    })
}

fn empty_run_capture(stream: &'static str) -> RunCapture {
    RunCapture {
        stream,
        bytes: Vec::new(),
        total_bytes: 0,
        truncated: false,
    }
}

fn run_operation_name(argv: &[String]) -> String {
    Path::new(&argv[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&argv[0])
        .to_string()
}

fn run_text_from_capture(capture: &RunCapture) -> RunText {
    let mut text = redact_run_output_bytes(&capture.bytes);
    text.byte_count = capture.total_bytes;
    text.captured_bytes = capture.bytes.len();
    text.truncated = capture.truncated;
    text
}

fn redact_run_output_bytes(bytes: &[u8]) -> RunText {
    let utf8_valid = std::str::from_utf8(bytes).is_ok();
    let text = String::from_utf8_lossy(bytes);
    let mut redactions = 0usize;
    let lines = text
        .lines()
        .map(|line| {
            let redacted = redact_observed_text(line);
            if redacted != line {
                redactions += 1;
            }
            redacted
        })
        .collect::<Vec<_>>();
    let line_count = lines.len();
    let head = lines.iter().take(10).cloned().collect::<Vec<_>>();
    let tail_start = lines.len().saturating_sub(10).max(head.len());
    let tail = lines.iter().skip(tail_start).cloned().collect::<Vec<_>>();
    RunText {
        text: lines.join("\n"),
        head,
        tail,
        line_count,
        byte_count: bytes.len(),
        captured_bytes: bytes.len(),
        truncated: false,
        utf8_valid,
        redactions,
    }
}

fn run_payload(input: RunPayloadInput<'_>) -> Value {
    json!({
        "format": "run",
        "command": {
            "capture_id": input.run_id,
            "argv": input.redacted_argv,
            "argv_count": input.argv.len(),
            "cwd": input.cwd.to_string_lossy(),
            "started_at": input.started_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "ended_at": input.ended_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "duration_ms": input.duration_ms,
            "success": matches!(input.status, RunProcessStatus::Exited { success: true, .. }),
            "exit_code": match input.status {
                RunProcessStatus::Exited { code, .. } => json!(code),
                _ => Value::Null,
            },
            "signal": match input.status {
                RunProcessStatus::Exited { signal, .. } => json!(signal),
                _ => Value::Null,
            },
            "timed_out": matches!(input.status, RunProcessStatus::TimedOut),
            "spawn_error": match input.status {
                RunProcessStatus::SpawnError(message) => json!(message),
                _ => Value::Null,
            },
            "out": input.out.map(|path| path.to_string_lossy().to_string())
        },
        "stdout": run_stream_value(input.stdout),
        "stderr": run_stream_value(input.stderr),
        "combined": input.combined,
        "failure_sections": input.failure_sections
            .iter()
            .enumerate()
            .map(|(index, section)| {
                json!({
                    "index": index,
                    "kind": section.kind,
                    "stream": section.stream,
                    "line_start": section.line_start,
                    "line_end": section.line_end,
                    "reason": section.reason,
                    "priority": section.priority,
                    "lines": section.lines
                })
            })
            .collect::<Vec<_>>()
    })
}

fn run_stream_value(text: &RunText) -> Value {
    json!({
        "format": "text",
        "text": text.text,
        "head": text.head,
        "tail": text.tail,
        "line_count": text.line_count,
        "byte_count": text.byte_count,
        "captured_bytes": text.captured_bytes,
        "truncated": text.truncated,
        "utf8_valid": text.utf8_valid
    })
}

fn detect_run_failure_sections(
    status: &RunProcessStatus,
    stdout: &RunText,
    stderr: &RunText,
) -> Vec<RunFailureSection> {
    let mut sections = Vec::new();
    collect_failure_sections("stderr", &stderr.text, &mut sections);
    collect_failure_sections("stdout", &stdout.text, &mut sections);
    if sections.is_empty() {
        match status {
            RunProcessStatus::Exited { success: false, .. } => {
                let lines = stderr
                    .text
                    .lines()
                    .chain(stdout.text.lines())
                    .rev()
                    .take(8)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if !lines.is_empty() {
                    sections.push(RunFailureSection {
                        kind: "generic",
                        stream: "stderr",
                        line_start: 1,
                        line_end: lines.len(),
                        lines: lines.into_iter().rev().collect(),
                        reason: "command exited unsuccessfully; inspect captured diagnostics"
                            .to_string(),
                        priority: 50,
                    });
                }
            }
            RunProcessStatus::TimedOut => sections.push(RunFailureSection {
                kind: "timeout",
                stream: "stderr",
                line_start: 1,
                line_end: 1,
                lines: vec!["command timed out".to_string()],
                reason: "command exceeded --timeout-ms".to_string(),
                priority: 95,
            }),
            RunProcessStatus::SpawnError(message) => sections.push(RunFailureSection {
                kind: "spawn_error",
                stream: "stderr",
                line_start: 1,
                line_end: 1,
                lines: vec![message.clone()],
                reason: "command could not be started".to_string(),
                priority: 100,
            }),
            RunProcessStatus::Exited { success: true, .. } => {}
        }
    }
    sections.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.stream.cmp(right.stream))
            .then_with(|| left.line_start.cmp(&right.line_start))
    });
    sections.truncate(10);
    sections
}

fn collect_failure_sections(
    stream: &'static str,
    text: &str,
    sections: &mut Vec<RunFailureSection>,
) {
    let lines = text.lines().map(str::to_string).collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        let detected = if line.contains("error[") || line.contains("panicked at") {
            Some(("rust", 90, "Rust compiler or test failure"))
        } else if line.contains("Traceback (most recent call last):") {
            Some(("python", 90, "Python traceback"))
        } else if line.contains("npm ERR!")
            || line.starts_with("Error:")
            || line.starts_with("node:")
        {
            Some(("node", 85, "Node.js or npm error"))
        } else if lower.contains("error")
            || lower.contains("failed")
            || lower.contains("exception")
            || lower.contains("not found")
        {
            Some(("generic", 60, "generic failure diagnostic"))
        } else {
            None
        };
        if let Some((kind, priority, reason)) = detected {
            let start = index.saturating_sub(2);
            let end = (index + 6).min(lines.len());
            sections.push(RunFailureSection {
                kind,
                stream,
                line_start: start + 1,
                line_end: end,
                lines: lines[start..end].to_vec(),
                reason: reason.to_string(),
                priority,
            });
        }
    }
}

fn run_next_actions(cursor: Option<&str>, sections: &[RunFailureSection]) -> Vec<NextAction> {
    let Some(cursor) = cursor else {
        return Vec::new();
    };
    sections
        .iter()
        .take(6)
        .enumerate()
        .map(|(index, section)| {
            let path = format!("/failure_sections/{index}");
            let mut extra = Extra::new();
            extra.insert("priority".to_string(), json!(section.priority));
            extra.insert("stream".to_string(), json!(section.stream));
            extra.insert("kind".to_string(), json!(section.kind));
            extra.insert(
                "argv".to_string(),
                json!(["prog", "expand", cursor, "--path", path]),
            );
            NextAction {
                kind: "expand".to_string(),
                operation: None,
                path: Some(path),
                reason: Some(section.reason.clone()),
                extra,
            }
        })
        .collect()
}

fn run_provenance(
    run_id: &str,
    cache_key: &str,
    redacted_argv: &[String],
    cwd: &Path,
    duration_ms: u64,
    status: &RunProcessStatus,
    args: &RunArgs,
) -> CallProvenance {
    let mut extra = Extra::new();
    extra.insert(
        "run".to_string(),
        json!({
            "argv": redacted_argv,
            "cwd": cwd.to_string_lossy(),
            "timeout_ms": args.timeout_ms,
            "max_stdout_bytes": args.max_stdout_bytes,
            "max_stderr_bytes": args.max_stderr_bytes,
            "preserve_exit_code": args.preserve_exit_code,
            "exit_code": match status {
                RunProcessStatus::Exited { code, .. } => json!(code),
                _ => Value::Null,
            },
            "signal": match status {
                RunProcessStatus::Exited { signal, .. } => json!(signal),
                _ => Value::Null,
            },
            "timed_out": matches!(status, RunProcessStatus::TimedOut)
        }),
    );
    CallProvenance {
        source_call_id: run_id.to_string(),
        cache_key: Some(cache_key.to_string()),
        captured_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        status: Some(run_status_name(status).to_string()),
        duration_ms: Some(duration_ms),
        extra,
    }
}

fn run_warnings(
    status: &RunProcessStatus,
    args: &RunArgs,
    stdout: &RunCapture,
    stderr: &RunCapture,
) -> Vec<String> {
    let mut warnings = Vec::new();
    match status {
        RunProcessStatus::Exited {
            success: false,
            code,
            signal,
        } => {
            warnings.push(format!(
                "child command exited unsuccessfully: exit_code={code:?}, signal={signal:?}; envelope still returned successfully"
            ));
        }
        RunProcessStatus::TimedOut => warnings.push(format!(
            "child command timed out after {} ms and was killed",
            args.timeout_ms
        )),
        RunProcessStatus::SpawnError(message) => {
            warnings.push(format!("child command could not be started: {message}"));
        }
        RunProcessStatus::Exited { success: true, .. } => {}
    }
    if stdout.truncated {
        warnings.push(format!(
            "{} exceeded max_stdout_bytes ({}); captured output was truncated",
            stdout.stream, args.max_stdout_bytes
        ));
    }
    if stderr.truncated {
        warnings.push(format!(
            "{} exceeded max_stderr_bytes ({}); captured diagnostics were truncated",
            stderr.stream, args.max_stderr_bytes
        ));
    }
    warnings
}

fn run_status_name(status: &RunProcessStatus) -> &'static str {
    match status {
        RunProcessStatus::Exited { success: true, .. } => "success",
        RunProcessStatus::Exited { success: false, .. } => "exit_nonzero",
        RunProcessStatus::TimedOut => "timeout",
        RunProcessStatus::SpawnError(_) => "spawn_error",
    }
}

fn run_exit_code(status: &RunProcessStatus) -> RunExitCode {
    match status {
        RunProcessStatus::Exited { success: true, .. } => RunExitCode::Success,
        RunProcessStatus::Exited {
            code: Some(code), ..
        } => RunExitCode::Code(*code),
        RunProcessStatus::Exited {
            signal: Some(signal),
            ..
        } => RunExitCode::Signal(*signal),
        RunProcessStatus::Exited { .. } => RunExitCode::Code(1),
        RunProcessStatus::TimedOut => RunExitCode::Timeout,
        RunProcessStatus::SpawnError(_) => RunExitCode::SpawnError,
    }
}

fn child_exit_code(code: RunExitCode) -> ExitCode {
    let raw = match code {
        RunExitCode::Success => 0,
        RunExitCode::Code(code) => code.clamp(1, 255),
        RunExitCode::Signal(signal) => (128 + signal).clamp(1, 255),
        RunExitCode::Timeout => 124,
        RunExitCode::SpawnError => 127,
    };
    ExitCode::from(raw as u8)
}

fn run_effects() -> EffectSet {
    EffectSet {
        read_only: false,
        mutating: true,
        network: true,
        shell: true,
        sensitive: false,
        cacheable: true,
        requires_confirmation: false,
        extra: Extra::new(),
    }
}

fn redact_run_argv(argv: &[String]) -> Vec<String> {
    let mut redact_next = false;
    argv.iter()
        .map(|arg| {
            if redact_next {
                redact_next = false;
                return "[REDACTED:run_arg_secret]".to_string();
            }
            if is_sensitive_flag(arg) {
                redact_next = true;
                return arg.clone();
            }
            redact_inline_secret(arg)
        })
        .collect()
}

fn is_sensitive_flag(arg: &str) -> bool {
    let trimmed = arg.trim_start_matches('-');
    is_sensitive_name(trimmed)
}

fn redact_inline_secret(arg: &str) -> String {
    for separator in ["=", ":"] {
        if let Some((name, _)) = arg.split_once(separator)
            && is_sensitive_name(name.trim_start_matches('-'))
        {
            return format!("{name}{separator}[REDACTED:run_arg_secret]");
        }
    }
    redact_observed_text(arg)
}

fn is_sensitive_name(name: &str) -> bool {
    let normalized = name
        .chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "authorization"
            | "apikey"
            | "bearer"
            | "cookie"
            | "credential"
            | "password"
            | "privatekey"
            | "secret"
            | "session"
            | "token"
    )
}

fn init_integration(args: &InitArgs) -> Result<InitReport> {
    if !args.project {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: "pass --project; global shell installation is not implemented in V1"
                .to_string(),
        });
    }
    if args.agent != AgentKind::Codex {
        return Err(CoreError::BadArgs {
            operation: "init".to_string(),
            reason: format!(
                "agent '{}' is documented in the integration matrix but not implemented yet; supported project agent: codex",
                args.agent.as_str()
            ),
        });
    }

    let root = project_root(&args.root)?;
    let specs = codex_project_init_files();
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for spec in specs {
        let full_path = root.join(spec.relative_path);
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
            path: spec.relative_path.to_string(),
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
        schema_version: "prog.init.v1",
        agent: args.agent.as_str(),
        scope: "project",
        root: root.to_string_lossy().to_string(),
        dry_run: args.dry_run,
        files,
        next_steps: vec![
            "Review .codex/skills/prog/SKILL.md before relying on the generated skill".to_string(),
            "Route noisy commands through .codex/prog-hooks/prog-run.sh <command...>".to_string(),
            "After a run/observe/call envelope returns a cursor, inspect with prog paths before expanding exact evidence".to_string(),
        ],
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

fn codex_project_init_files() -> Vec<InitFileSpec> {
    let manifest_files = [
        ".codex/skills/prog/SKILL.md",
        ".codex/prog-hooks/prog-run.sh",
        ".codex/prog-hooks/README.md",
        ".codex/prog-hooks/manifest.json",
        ".codex/prog-hooks/uninstall.sh",
    ];
    let manifest = json!({
        "schema_version": "prog.integration.v1",
        "agent": "codex",
        "scope": "project",
        "mcp": {
            "status": "optional",
            "reason": "CLI, skill, and hooks are the durable V1 contract"
        },
        "files": manifest_files,
        "commands": {
            "wrap_command": ".codex/prog-hooks/prog-run.sh <command...>",
            "observe_file": "prog observe --file <path>",
            "inspect_paths": "prog paths <cursor>",
            "expand_evidence": "prog expand <cursor> --path <json-pointer>"
        },
        "uninstall": "sh .codex/prog-hooks/uninstall.sh"
    });
    vec![
        InitFileSpec {
            relative_path: ".codex/skills/prog/SKILL.md",
            content: PROG_AGENT_SKILL.to_string(),
            executable: false,
        },
        InitFileSpec {
            relative_path: ".codex/prog-hooks/prog-run.sh",
            content: codex_prog_run_hook(),
            executable: true,
        },
        InitFileSpec {
            relative_path: ".codex/prog-hooks/README.md",
            content: codex_hook_readme(),
            executable: false,
        },
        InitFileSpec {
            relative_path: ".codex/prog-hooks/manifest.json",
            content: format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
            executable: false,
        },
        InitFileSpec {
            relative_path: ".codex/prog-hooks/uninstall.sh",
            content: codex_uninstall_hook(),
            executable: true,
        },
    ]
}

fn codex_prog_run_hook() -> String {
    r#"#!/usr/bin/env sh
set -eu

if [ "$#" -eq 0 ]; then
  echo "usage: .codex/prog-hooks/prog-run.sh <command...>" >&2
  exit 64
fi

exec prog run -- "$@"
"#
    .to_string()
}

fn codex_hook_readme() -> String {
    r#"# prog Codex hooks

This project-local integration keeps `prog` usable without MCP server mode.

Use the wrapper for noisy commands:

```bash
.codex/prog-hooks/prog-run.sh cargo test
```

The wrapper returns a bounded `DisclosureEnvelope`. Inspect the returned
`cursor` with `prog paths <cursor>` before expanding exact evidence with
`prog expand <cursor> --path <json-pointer>`.

For shell aliases or editor tasks, wire the command directly rather than
rewriting user commands globally:

```sh
prog_run() {
  .codex/prog-hooks/prog-run.sh "$@"
}
```

MCP is optional compatibility. Prefer the CLI, this skill, and explicit hooks
unless the host agent already has a reliable MCP client.
"#
    .to_string()
}

fn codex_uninstall_hook() -> String {
    r#"#!/usr/bin/env sh
set -eu

rm -f .codex/skills/prog/SKILL.md
rm -f .codex/prog-hooks/prog-run.sh
rm -f .codex/prog-hooks/README.md
rm -f .codex/prog-hooks/manifest.json
rm -f .codex/prog-hooks/uninstall.sh
rmdir .codex/skills/prog 2>/dev/null || true
rmdir .codex/skills 2>/dev/null || true
rmdir .codex/prog-hooks 2>/dev/null || true
rmdir .codex 2>/dev/null || true
"#
    .to_string()
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

fn paths_cursor(store: &Store, args: &PathsArgs) -> Result<PathsResponse> {
    let filters = path_filters(args)?;
    let redaction_version = RedactionPolicy::default().version;
    let record = store.get_cursor(&args.cursor, redaction_version)?;
    let entry = store
        .get_entry(&record.cache_key)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let payload = store
        .get_payload(&entry.payload_hash)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let prefix = if args.prefix.is_empty() {
        record.root_path.clone()
    } else {
        args.prefix.clone()
    };
    if !prog_core::pointer::is_within(&record.root_path, &prefix)? {
        return Err(CoreError::PathOutsideBoundary {
            path: prefix,
            boundary: record.root_path,
        });
    }
    let target =
        prog_core::pointer::get(&payload, &prefix)?.ok_or_else(|| CoreError::PathNotFound {
            path: prefix.clone(),
            hint: prog_core::pointer::siblings_hint(&payload, &prefix),
        })?;
    let projection = project(target, &PreviewPolicy::default(), &prefix);
    let mut paths = Vec::new();
    let truncated = collect_paths(target, &prefix, args.depth, args.limit, &mut paths);
    annotate_path_omissions(&mut paths, &projection.omitted);
    append_missing_omitted_paths(&mut paths, &projection.omitted, args.limit);
    paths.retain(|path| path_matches_filters(path, &filters));
    let omitted = projection
        .omitted
        .into_iter()
        .filter(|region| omitted_matches_filters(region, &filters))
        .collect::<Vec<_>>();
    let next_actions = expansion_next_actions(
        Some(args.cursor.as_str()),
        Some(record.operation.as_str()),
        &omitted,
        args.limit.min(10),
    );
    let age = age_seconds(&entry.created_at)?;
    let mut warnings = Vec::new();
    if truncated {
        warnings.push(format!(
            "path listing reached --limit {}; use --prefix to narrow the result",
            args.limit
        ));
    }
    if age > 0 {
        warnings.push(format!(
            "cached payload age_seconds={age}; re-run the original observation or call to refresh"
        ));
    }

    Ok(PathsResponse {
        schema_version: DISCLOSURE_VERSION,
        cursor: args.cursor.clone(),
        source_id: record.source_id,
        operation: record.operation,
        root_path: record.root_path,
        prefix,
        paths,
        omitted,
        next_actions,
        cache: cache_info(CacheStatus::Hit, &entry, Some(age)),
        warnings,
    })
}

fn path_filters(args: &PathsArgs) -> Result<PathFilters> {
    let reason = args
        .reason
        .as_deref()
        .map(parse_omission_reason)
        .transpose()?;
    Ok(PathFilters {
        reason,
        fields: args.field.iter().cloned().collect(),
        omitted_only: args.omitted_only || reason.is_some(),
        expandable_only: args.expandable_only,
    })
}

fn parse_omission_reason(raw: &str) -> Result<OmissionReason> {
    let normalized = raw.replace('-', "_").to_ascii_lowercase();
    match normalized.as_str() {
        "large_string" => Ok(OmissionReason::LargeString),
        "long_array" => Ok(OmissionReason::LongArray),
        "many_fields" => Ok(OmissionReason::ManyFields),
        "deep_object" => Ok(OmissionReason::DeepObject),
        "node_budget" => Ok(OmissionReason::NodeBudget),
        "redacted" => Ok(OmissionReason::Redacted),
        _ => Err(CoreError::BadArgs {
            operation: "paths --reason".to_string(),
            reason: format!(
                "unknown omission reason '{raw}'; expected one of large_string, long_array, many_fields, deep_object, node_budget, redacted"
            ),
        }),
    }
}

fn path_matches_filters(path: &PathEntry, filters: &PathFilters) -> bool {
    if filters.expandable_only && !path.expandable {
        return false;
    }
    if filters.omitted_only && path.omitted_reason.is_none() {
        return false;
    }
    if let Some(reason) = filters.reason
        && path.omitted_reason != Some(reason)
    {
        return false;
    }
    if !filters.fields.is_empty() && !path_has_any_field(&path.path, &filters.fields) {
        return false;
    }
    true
}

fn omitted_matches_filters(region: &OmittedRegion, filters: &PathFilters) -> bool {
    if let Some(reason) = filters.reason
        && region.reason != reason
    {
        return false;
    }
    if !filters.fields.is_empty() && !path_has_any_field(&region.path, &filters.fields) {
        return false;
    }
    true
}

fn path_has_any_field(path: &str, fields: &BTreeSet<String>) -> bool {
    prog_core::pointer::parse(path)
        .map(|segments| segments.iter().any(|segment| fields.contains(segment)))
        .unwrap_or(false)
}

fn collect_paths(
    value: &Value,
    path: &str,
    depth: usize,
    limit: usize,
    out: &mut Vec<PathEntry>,
) -> bool {
    let mut truncated = false;
    collect_paths_inner(value, path, depth, limit, out, &mut truncated);
    truncated
}

fn collect_paths_inner(
    value: &Value,
    path: &str,
    depth: usize,
    limit: usize,
    out: &mut Vec<PathEntry>,
    truncated: &mut bool,
) {
    if out.len() >= limit {
        *truncated = true;
        return;
    }

    out.push(PathEntry {
        path: path.to_string(),
        kind: value_kind(value).to_string(),
        expandable: matches!(value, Value::Array(_) | Value::Object(_)),
        omitted_reason: None,
        detail: None,
    });

    if depth == 0 {
        if matches!(value, Value::Array(items) if !items.is_empty())
            || matches!(value, Value::Object(map) if !map.is_empty())
        {
            *truncated = true;
        }
        return;
    }

    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                if out.len() >= limit {
                    *truncated = true;
                    break;
                }
                let child_path = prog_core::pointer::push(path, &index.to_string());
                collect_paths_inner(item, &child_path, depth - 1, limit, out, truncated);
            }
        }
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if out.len() >= limit {
                    *truncated = true;
                    break;
                }
                let child_path = prog_core::pointer::push(path, key);
                collect_paths_inner(&map[key], &child_path, depth - 1, limit, out, truncated);
            }
        }
        _ => {}
    }
}

fn annotate_path_omissions(paths: &mut [PathEntry], omitted: &[OmittedRegion]) {
    let omitted_by_path = omitted
        .iter()
        .map(|region| (region.path.as_str(), region))
        .collect::<BTreeMap<_, _>>();

    for path in paths {
        if let Some(region) = omitted_by_path.get(path.path.as_str()) {
            path.expandable = true;
            path.omitted_reason = Some(region.reason);
            path.detail.clone_from(&region.detail);
        }
    }
}

fn append_missing_omitted_paths(
    paths: &mut Vec<PathEntry>,
    omitted: &[OmittedRegion],
    limit: usize,
) {
    let mut seen = paths
        .iter()
        .map(|path| path.path.clone())
        .collect::<BTreeSet<_>>();
    for region in omitted {
        if paths.len() >= limit {
            break;
        }
        if !seen.insert(region.path.clone()) {
            continue;
        }
        paths.push(PathEntry {
            path: region.path.clone(),
            kind: "omitted".to_string(),
            expandable: true,
            omitted_reason: Some(region.reason),
            detail: region.detail.clone(),
        });
    }
}

fn expansion_next_actions(
    cursor: Option<&str>,
    operation: Option<&str>,
    omitted: &[OmittedRegion],
    limit: usize,
) -> Vec<NextAction> {
    let Some(cursor) = cursor else {
        return Vec::new();
    };
    let mut ranked = omitted.iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        omission_priority(right.reason)
            .cmp(&omission_priority(left.reason))
            .then_with(|| left.path.cmp(&right.path))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|region| expansion_next_action(cursor, operation, region))
        .collect()
}

fn expansion_next_action(
    cursor: &str,
    operation: Option<&str>,
    region: &OmittedRegion,
) -> NextAction {
    let mut extra = Extra::new();
    extra.insert(
        "priority".to_string(),
        json!(omission_priority(region.reason)),
    );
    extra.insert(
        "omitted_reason".to_string(),
        json!(omission_reason_name(region.reason)),
    );
    if let Some(detail) = &region.detail {
        extra.insert("detail".to_string(), json!(detail));
    }
    extra.insert(
        "argv".to_string(),
        json!(["prog", "expand", cursor, "--path", region.path]),
    );
    extra.insert(
        "offline".to_string(),
        json!("uses cached redacted payload; does not contact upstream"),
    );
    NextAction {
        kind: "expand".to_string(),
        operation: operation.map(str::to_string),
        path: Some(region.path.clone()),
        reason: Some(omission_action_reason(region)),
        extra,
    }
}

fn omission_priority(reason: OmissionReason) -> u8 {
    match reason {
        OmissionReason::LargeString => 90,
        OmissionReason::DeepObject => 80,
        OmissionReason::ManyFields => 70,
        OmissionReason::LongArray => 60,
        OmissionReason::NodeBudget => 50,
        OmissionReason::Redacted => 10,
    }
}

fn omission_reason_name(reason: OmissionReason) -> &'static str {
    match reason {
        OmissionReason::LargeString => "large_string",
        OmissionReason::LongArray => "long_array",
        OmissionReason::ManyFields => "many_fields",
        OmissionReason::DeepObject => "deep_object",
        OmissionReason::NodeBudget => "node_budget",
        OmissionReason::Redacted => "redacted",
    }
}

fn omission_action_reason(region: &OmittedRegion) -> String {
    match region.reason {
        OmissionReason::LargeString => format!(
            "{} is a large string; expand to inspect a bounded view of the stored redacted value",
            region.path
        ),
        OmissionReason::LongArray => format!(
            "{} is a long array; expand with --limit to inspect selected items",
            region.path
        ),
        OmissionReason::ManyFields => format!(
            "{} has many fields; expand with --fields or --omit to inspect selected fields",
            region.path
        ),
        OmissionReason::DeepObject => format!(
            "{} was omitted by depth; expand with --depth to inspect nested structure",
            region.path
        ),
        OmissionReason::NodeBudget => format!(
            "{} was omitted by the global node budget; expand a narrower prefix",
            region.path
        ),
        OmissionReason::Redacted => format!(
            "{} is redacted before persistence; expansion will not reveal the original secret",
            region.path
        ),
    }
}

fn expand_cursor(store: &Store, args: &ExpandArgs) -> Result<DisclosureEnvelope> {
    let redaction_version = RedactionPolicy::default().version;
    let record = store.get_cursor(&args.cursor, redaction_version)?;
    let entry = store
        .get_entry(&record.cache_key)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let payload = store
        .get_payload(&entry.payload_hash)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let slice = SliceRequest {
        path: if args.path.is_empty() {
            None
        } else {
            Some(args.path.clone())
        },
        limit: args.limit,
        depth: args.depth,
        fields: args.fields.clone(),
        omit: Vec::new(),
        extra: Extra::new(),
    };
    let age = age_seconds(&entry.created_at)?;
    let mut warnings = Vec::new();
    if age > 0 {
        warnings.push(format!(
            "cached payload age_seconds={age}; re-run `prog call {} {} --refresh` to refresh",
            record.source_id, record.operation
        ));
    }

    if let Some(path) = &args.out {
        let (target_path, selected) = slice_value(&payload, &record.root_path, &slice)?;
        let bytes = serde_json::to_vec_pretty(&selected)?;
        write_private_file(path, &bytes)?;
        let receipt = json!({
            "path": path,
            "json_pointer": target_path,
            "bytes": bytes.len(),
            "sha256": hex_sha256(&bytes)
        });
        return envelope_for_payload(
            store,
            EnvelopeInput {
                source_id: record.source_id.clone(),
                operation: record.operation.clone(),
                source_kind: source_kind_for_source_id(&record.source_id),
                payload: receipt,
                root_path: "".to_string(),
                slice: SliceRequest {
                    path: None,
                    limit: Some(5),
                    depth: Some(2),
                    fields: Vec::new(),
                    omit: Vec::new(),
                    extra: Extra::new(),
                },
                payload_bytes: bytes.len().try_into().unwrap_or(u64::MAX),
                provenance: entry.provenance.clone(),
                cache: Some(cache_info(CacheStatus::Hit, &entry, Some(age))),
                effects: None,
                redacted_paths: 0,
                cache_disabled_reason: None,
                warnings,
                schema_hints: BTreeMap::new(),
                next_action_operation: None,
                additional_next_actions: Vec::new(),
                lens: None,
            },
            None,
        );
    }

    envelope_for_payload(
        store,
        EnvelopeInput {
            source_id: record.source_id.clone(),
            operation: record.operation.clone(),
            source_kind: source_kind_for_source_id(&record.source_id),
            payload,
            root_path: record.root_path,
            slice,
            payload_bytes: entry.payload_bytes,
            provenance: entry.provenance.clone(),
            cache: Some(cache_info(CacheStatus::Hit, &entry, Some(age))),
            effects: None,
            redacted_paths: 0,
            cache_disabled_reason: None,
            warnings,
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: Vec::new(),
            lens: None,
        },
        Some(args.cursor.clone()),
    )
}

fn meta_contracts(store: &Store, args: &MetaArgs) -> Result<DisclosureEnvelope> {
    let schemas = public_contract_schemas()?;
    let payload = match &args.contract {
        Some(contract) => schemas
            .get(contract)
            .cloned()
            .ok_or_else(|| CoreError::BadArgs {
                operation: "meta".to_string(),
                reason: format!(
                    "unknown contract '{contract}'; expected one of {}",
                    schemas.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            })?,
        None => json!({
            "contracts": schemas.keys().cloned().collect::<Vec<_>>()
        }),
    };
    let operation = args.contract.as_deref().unwrap_or("contracts").to_string();
    let cache_key = Store::cache_key("prog", "meta", &json!({"contract": args.contract}))?;
    let payload_hash = store.put_payload(&payload)?;
    let payload_bytes = json_len_u64(&payload)?;
    let entry = new_cache_entry(
        cache_key.clone(),
        payload_hash,
        "prog".to_string(),
        operation.clone(),
        payload_bytes,
        86_400,
    );
    store.put_entry(&cache_key, &entry)?;
    let slice = SliceRequest {
        path: None,
        limit: None,
        depth: None,
        fields: Vec::new(),
        omit: Vec::new(),
        extra: Extra::new(),
    };
    let projection = expand(&payload, "", &slice, &PreviewPolicy::default())?;
    let cursor = if projection.omitted.is_empty() {
        None
    } else {
        Some(store.create_cursor(&cache_key, "prog", &operation, "", 1, 86_400)?)
    };
    envelope_for_payload(
        store,
        EnvelopeInput {
            source_id: "prog".to_string(),
            operation,
            source_kind: Some("internal".to_string()),
            payload,
            root_path: "".to_string(),
            slice,
            payload_bytes,
            provenance: entry.provenance.clone(),
            cache: Some(cache_info(CacheStatus::Stored, &entry, Some(0))),
            effects: None,
            redacted_paths: 0,
            cache_disabled_reason: None,
            warnings: Vec::new(),
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: Vec::new(),
            lens: None,
        },
        cursor,
    )
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
            extra: adapter_seed_extra(
                "http",
                seed,
                json!({"http": {
                    "base_url": base_url.clone(),
                    "timeout_ms": 30_000,
                    "max_response_bytes": 1024 * 1024,
                    "default_headers": {},
                    "response_header_allowlist": []
                }}),
            ),
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

fn profile_operation<'a>(
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

fn parse_json_argument(raw: &str, operation: &str) -> Result<Value> {
    serde_json::from_str(raw).map_err(|error| CoreError::BadArgs {
        operation: operation.to_string(),
        reason: format!("must be valid JSON: {error}"),
    })
}

fn parse_view(raw: Option<&str>) -> Result<SliceRequest> {
    match raw {
        Some(raw) => serde_json::from_str(raw).map_err(|error| CoreError::BadArgs {
            operation: "call --view".to_string(),
            reason: format!("must be a SliceRequest JSON object: {error}"),
        }),
        None => Ok(SliceRequest {
            path: None,
            limit: None,
            depth: None,
            fields: Vec::new(),
            omit: Vec::new(),
            extra: Extra::new(),
        }),
    }
}

fn read_observation_input(args: &ObserveArgs) -> Result<ObservationInput> {
    let (bytes, name, input) = if let Some(path) = &args.file {
        let bytes = std::fs::read(path).map_err(|error| CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: format!(
                "file '{}' could not be read: {error}",
                path.to_string_lossy()
            ),
        })?;
        let name = args.name.clone().unwrap_or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("file")
                .to_string()
        });
        (
            bytes,
            name,
            json!({
                "kind": "file",
                "path": path.to_string_lossy()
            }),
        )
    } else if args.stdin {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|error| CoreError::BadArgs {
                operation: "observe".to_string(),
                reason: format!("stdin could not be read: {error}"),
            })?;
        (
            bytes,
            args.name.clone().unwrap_or_else(|| "stdin".to_string()),
            json!({"kind": "stdin"}),
        )
    } else {
        return Err(CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "pass --file <path> or --stdin".to_string(),
        });
    };

    let mime = args
        .mime
        .clone()
        .unwrap_or_else(|| sniff_mime_from_bytes(&bytes).to_string());
    Ok(ObservationInput {
        name,
        input,
        mime,
        bytes,
    })
}

fn normalize_observation(bytes: &[u8], mime: &str) -> Result<NormalizedObservation> {
    let normalized_mime = mime.to_ascii_lowercase();
    if normalized_mime.contains("ndjson") || normalized_mime.contains("jsonlines") {
        return normalize_ndjson_observation(bytes);
    }
    if normalized_mime.contains("json") || sniff_mime_from_bytes(bytes) == "application/json" {
        return normalize_json_observation(bytes, mime);
    }
    normalize_text_observation(bytes)
}

fn normalize_json_observation(bytes: &[u8], mime: &str) -> Result<NormalizedObservation> {
    let payload = serde_json::from_slice(bytes).map_err(|error| CoreError::BadArgs {
        operation: "observe".to_string(),
        reason: format!("input with mime '{mime}' must be valid JSON: {error}"),
    })?;
    Ok(NormalizedObservation {
        kind: "json".to_string(),
        payload,
        warnings: Vec::new(),
    })
}

fn normalize_ndjson_observation(bytes: &[u8]) -> Result<NormalizedObservation> {
    let text = std::str::from_utf8(bytes).map_err(|error| CoreError::BadArgs {
        operation: "observe".to_string(),
        reason: format!("NDJSON input must be valid UTF-8: {error}"),
    })?;
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str::<Value>(line).map_err(|error| CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: format!("NDJSON line {} is not valid JSON: {error}", index + 1),
        })?;
        records.push(record);
    }
    let record_count = records.len();
    let line_count = text.lines().count();
    Ok(NormalizedObservation {
        kind: "ndjson".to_string(),
        payload: json!({
            "format": "ndjson",
            "records": records,
            "record_count": record_count,
            "line_count": line_count,
            "byte_count": bytes.len()
        }),
        warnings: Vec::new(),
    })
}

fn normalize_text_observation(bytes: &[u8]) -> Result<NormalizedObservation> {
    if is_binaryish(bytes) {
        return Err(CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "input appears to be binary; pass a text, JSON, or NDJSON artifact".to_string(),
        });
    }

    let mut warnings = Vec::new();
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            warnings
                .push("input was not valid UTF-8; replacement characters were used".to_string());
            String::from_utf8_lossy(bytes).to_string()
        }
    };
    let lines = text
        .lines()
        .enumerate()
        .map(|(index, line)| {
            json!({
                "number": index + 1,
                "text": redact_observed_text(line)
            })
        })
        .collect::<Vec<_>>();
    let line_count = lines.len();
    let head = lines
        .iter()
        .take(10)
        .map(|line| line["text"].clone())
        .collect::<Vec<_>>();
    let tail_start = lines.len().saturating_sub(10).max(head.len());
    let tail = lines
        .iter()
        .skip(tail_start)
        .map(|line| line["text"].clone())
        .collect::<Vec<_>>();

    Ok(NormalizedObservation {
        kind: "text".to_string(),
        payload: json!({
            "format": "text",
            "head": head,
            "tail": tail,
            "lines": lines,
            "line_count": line_count,
            "byte_count": bytes.len(),
            "utf8_valid": warnings.is_empty()
        }),
        warnings,
    })
}

fn sniff_mime_from_bytes(bytes: &[u8]) -> &'static str {
    if bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| byte == b'{' || byte == b'[')
    {
        "application/json"
    } else {
        "text/plain"
    }
}

fn is_binaryish(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if bytes.contains(&0) {
        return true;
    }
    let suspicious = bytes
        .iter()
        .filter(|byte| byte.is_ascii_control() && !matches!(byte, b'\n' | b'\r' | b'\t'))
        .count();
    suspicious.saturating_mul(10) > bytes.len()
}

fn redact_observed_text(line: &str) -> String {
    let separators = ["=", ":", " "];
    for key in [
        "authorization",
        "api_key",
        "apikey",
        "password",
        "secret",
        "token",
    ] {
        let lower = line.to_ascii_lowercase();
        for separator in separators {
            let marker = format!("{key}{separator}");
            if let Some(start) = lower.find(&marker) {
                let marker_end = start + marker.len();
                let value_start = marker_end
                    + line[marker_end..]
                        .chars()
                        .take_while(|ch| ch.is_whitespace())
                        .map(char::len_utf8)
                        .sum::<usize>();
                let value_end = line[value_start..]
                    .find(char::is_whitespace)
                    .map(|offset| value_start + offset)
                    .unwrap_or(line.len());
                let mut redacted = String::new();
                redacted.push_str(&line[..value_start]);
                redacted.push_str("[REDACTED:observed_text_secret]");
                redacted.push_str(&line[value_end..]);
                return redacted;
            }
        }
    }
    line.to_string()
}

fn observation_provenance(
    cache_key: &str,
    input: &ObservationInput,
    kind: &str,
    redacted_paths: usize,
) -> CallProvenance {
    let mut extra = Extra::new();
    extra.insert(
        "observe".to_string(),
        json!({
            "name": &input.name,
            "input": &input.input,
            "mime": &input.mime,
            "kind": kind,
            "input_bytes": input.bytes.len(),
            "redacted_paths": redacted_paths
        }),
    );
    CallProvenance {
        source_call_id: format!(
            "observe_{}",
            Utc::now()
                .timestamp_nanos_opt()
                .unwrap_or_else(|| Utc::now().timestamp_micros())
        ),
        cache_key: Some(cache_key.to_string()),
        captured_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        status: Some("observed".to_string()),
        duration_ms: None,
        extra,
    }
}

fn load_lens(lens_dir: &Path, id: &str) -> Result<LensManifest> {
    let manifests = load_lens_manifests(lens_dir)?;
    let mut matches = manifests
        .into_iter()
        .filter(|manifest| manifest.id == id)
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(CoreError::BadArgs {
            operation: "call --lens".to_string(),
            reason: format!("lens '{id}' not found in '{}'", lens_dir.to_string_lossy()),
        }),
        1 => Ok(matches.remove(0)),
        _ => Err(CoreError::BadArgs {
            operation: "call --lens".to_string(),
            reason: format!(
                "lens '{id}' is defined more than once in '{}'",
                lens_dir.to_string_lossy()
            ),
        }),
    }
}

fn load_lens_manifests(lens_dir: &Path) -> Result<Vec<LensManifest>> {
    if !lens_dir.exists() {
        return Err(CoreError::BadArgs {
            operation: "call --lens".to_string(),
            reason: format!(
                "lens directory '{}' does not exist",
                lens_dir.to_string_lossy()
            ),
        });
    }

    let mut manifests = Vec::new();
    for entry in std::fs::read_dir(lens_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || !is_lens_manifest_path(&path) {
            continue;
        }
        let raw = std::fs::read_to_string(&path).map_err(|error| CoreError::BadArgs {
            operation: "call --lens".to_string(),
            reason: format!("could not read lens '{}': {error}", path.to_string_lossy()),
        })?;
        let manifest = parse_lens_manifest(&path, &raw)?;
        validate_lens_manifest(&manifest)?;
        manifests.push(manifest);
    }
    Ok(manifests)
}

fn is_lens_manifest_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("json" | "yaml" | "yml")
    )
}

fn parse_lens_manifest(path: &Path, raw: &str) -> Result<LensManifest> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => serde_json::from_str(raw).map_err(|error| CoreError::BadArgs {
            operation: "call --lens".to_string(),
            reason: format!(
                "lens '{}' must be valid JSON: {error}",
                path.to_string_lossy()
            ),
        }),
        Some("yaml" | "yml") => serde_yaml_ng::from_str(raw).map_err(|error| CoreError::BadArgs {
            operation: "call --lens".to_string(),
            reason: format!(
                "lens '{}' must be valid YAML: {error}",
                path.to_string_lossy()
            ),
        }),
        _ => Err(CoreError::BadArgs {
            operation: "call --lens".to_string(),
            reason: format!(
                "lens '{}' must use .json, .yaml, or .yml",
                path.to_string_lossy()
            ),
        }),
    }
}

fn validate_lens_matches_call(
    lens: &LensManifest,
    profile: &SourceProfile,
    operation: &OperationProfile,
) -> Result<()> {
    if let Some(source_id) = &lens.match_rules.source_id
        && source_id != &profile.id
    {
        return Err(CoreError::BadArgs {
            operation: "call --lens".to_string(),
            reason: format!(
                "lens '{}' matches source_id '{}', not '{}'",
                lens.id, source_id, profile.id
            ),
        });
    }
    if let Some(source_kind) = lens.match_rules.source_kind
        && source_kind != profile.kind
    {
        return Err(CoreError::BadArgs {
            operation: "call --lens".to_string(),
            reason: format!(
                "lens '{}' matches source_kind '{:?}', not '{:?}'",
                lens.id, source_kind, profile.kind
            ),
        });
    }
    if let Some(expected_operation) = &lens.match_rules.operation
        && expected_operation != &operation.id
    {
        return Err(CoreError::BadArgs {
            operation: "call --lens".to_string(),
            reason: format!(
                "lens '{}' matches operation '{}', not '{}'",
                lens.id, expected_operation, operation.id
            ),
        });
    }
    Ok(())
}

fn validate_call_args(operation: &OperationProfile, args: &Value) -> Result<()> {
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

fn callable_source_from_profile(profile: &SourceProfile) -> Result<CallableSource> {
    match profile.kind {
        prog_core::SourceKind::Http => Ok(CallableSource::Http(http_source_from_profile(profile)?)),
        prog_core::SourceKind::Cli => Ok(CallableSource::Cli(cli_source_from_profile(profile)?)),
        prog_core::SourceKind::Mcp => Ok(CallableSource::Mcp(mcp_source_from_profile(profile)?)),
    }
}

fn http_source_from_profile(profile: &SourceProfile) -> Result<HttpSource> {
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
        max_response_bytes: adapter_usize(adapter, "max_response_bytes", 1024 * 1024),
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

fn cli_source_from_profile(profile: &SourceProfile) -> Result<CliSource> {
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

fn mcp_source_from_profile(profile: &SourceProfile) -> Result<McpSource> {
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

async fn execute_callable(
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
            })
        }
        CallableSource::Mcp(source) => {
            let invocation = invocation_config(operation, "mcp")?;
            let kind = required_profile_string(invocation, "kind")?;
            let result = match kind.as_str() {
                "tool" => {
                    let name = required_profile_string(invocation, "name")?;
                    source.call_tool(&name, args).await?
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
            }
            Ok(AdapterCall {
                data: result.data,
                provenance,
                status: None,
                duration_ms: Some(result.provenance.duration_ms),
                pagination: None,
                warnings: result.warnings,
            })
        }
    }
}

fn adapter_config<'a>(profile: &'a SourceProfile, kind: &str) -> Option<&'a Map<String, Value>> {
    profile
        .extra
        .get("adapter")
        .and_then(|value| value.get(kind))
        .and_then(Value::as_object)
}

fn invocation_config<'a>(
    operation: &'a OperationProfile,
    kind: &str,
) -> Result<&'a Map<String, Value>> {
    operation
        .extra
        .get("invocation")
        .and_then(|value| value.get(kind))
        .and_then(Value::as_object)
        .ok_or_else(|| CoreError::BadArgs {
            operation: operation.id.clone(),
            reason: format!(
                "profile is missing invocation.{kind}; re-run `prog discover` for this source"
            ),
        })
}

fn profile_adapter_error(profile: &SourceProfile, field: &str) -> CoreError {
    CoreError::BadArgs {
        operation: "call".to_string(),
        reason: format!(
            "profile '{}' is missing adapter.{field}; re-run `prog discover` for this source",
            profile.id
        ),
    }
}

fn required_profile_string(map: &Map<String, Value>, field: &str) -> Result<String> {
    map.get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CoreError::BadArgs {
            operation: "call".to_string(),
            reason: format!("profile field '{field}' must be a string"),
        })
}

fn optional_profile_string(map: &Map<String, Value>, field: &str) -> Result<Option<String>> {
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

fn profile_string_map(value: Option<&Value>, field: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    serde_json::from_value(value.clone()).map_err(|error| CoreError::BadArgs {
        operation: "call".to_string(),
        reason: format!("profile field '{field}' must be an object of strings: {error}"),
    })
}

fn profile_string_vec(value: Option<&Value>, field: &str) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    serde_json::from_value(value.clone()).map_err(|error| CoreError::BadArgs {
        operation: "call".to_string(),
        reason: format!("profile field '{field}' must be an array of strings: {error}"),
    })
}

fn adapter_u64(adapter: Option<&Map<String, Value>>, field: &str, default: u64) -> u64 {
    adapter
        .and_then(|config| config.get(field))
        .and_then(Value::as_u64)
        .unwrap_or(default)
}

fn adapter_usize(adapter: Option<&Map<String, Value>>, field: &str, default: usize) -> usize {
    adapter_u64(adapter, field, default.try_into().unwrap_or(u64::MAX))
        .try_into()
        .unwrap_or(default)
}

fn effective_cache_policy(profile: &SourceProfile, operation: &OperationProfile) -> CachePolicy {
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

fn ttl_seconds(policy: &CachePolicy) -> i64 {
    policy
        .ttl_seconds
        .unwrap_or(86_400)
        .try_into()
        .unwrap_or(i64::MAX)
}

fn cache_skip_warning(no_cache: bool, operation: &OperationProfile) -> String {
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

fn profile_source_kind_name(kind: prog_core::SourceKind) -> &'static str {
    match kind {
        prog_core::SourceKind::Http => "http",
        prog_core::SourceKind::Cli => "cli",
        prog_core::SourceKind::Mcp => "mcp",
    }
}

fn source_kind_for_source_id(source_id: &str) -> Option<String> {
    match source_id {
        "observe" => Some("artifact".to_string()),
        "prog" => Some("internal".to_string()),
        _ => None,
    }
}

fn cache_info(
    status: CacheStatus,
    entry: &prog_core::CacheEntryMeta,
    age_seconds: Option<u64>,
) -> CacheInfo {
    CacheInfo {
        status,
        ttl_seconds: ttl_between(&entry.created_at, &entry.expires_at).ok(),
        expires_at: Some(entry.expires_at.clone()),
        age_seconds,
        extra: Extra::new(),
    }
}

fn call_provenance(
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

fn cursor_for_projection(store: &Store, input: CursorInput<'_>) -> Result<Option<String>> {
    if !input.may_cache {
        return Ok(None);
    }
    let lens_projection = project_with_lens(
        input.payload,
        input.root_path,
        input.slice,
        &PreviewPolicy::default(),
        input.lens,
    )?;
    let has_expand_action = lens_projection
        .next_actions
        .iter()
        .any(|action| action.kind == "expand" && action.path.is_some());
    if lens_projection.projection.omitted.is_empty() && !has_expand_action {
        return Ok(None);
    }
    Ok(Some(store.create_cursor(
        input.cache_key,
        input.source_id,
        input.operation,
        input.root_path,
        RedactionPolicy::default().version,
        ttl_seconds(input.cache),
    )?))
}

fn envelope_for_payload(
    _store: &Store,
    input: EnvelopeInput,
    cursor: Option<String>,
) -> Result<DisclosureEnvelope> {
    let mut policy = PreviewPolicy::default();
    let mut last = None;
    for _ in 0..16 {
        let lens_projection = project_with_lens(
            &input.payload,
            &input.root_path,
            &input.slice,
            &policy,
            input.lens.as_ref(),
        )?;
        let mut envelope = make_envelope(&input, lens_projection, cursor.clone());
        let bytes = finalize_envelope_bytes(&mut envelope)?;
        if bytes <= policy.max_envelope_bytes {
            return Ok(envelope);
        }
        last = Some(envelope);
        let next = shrink_policy(&policy);
        if next == policy {
            break;
        }
        policy = next;
    }
    let mut envelope = last.expect("envelope loop always builds at least once");
    if serde_json::to_vec(&envelope)?.len() > PreviewPolicy::default().max_envelope_bytes {
        envelope.schema_hints.clear();
        envelope.provenance = None;
        envelope.next_actions.truncate(4);
        envelope.omitted.truncate(8);
        envelope.warnings.truncate(4);
        envelope
            .warnings
            .push("envelope metadata compacted to enforce max_envelope_bytes".to_string());
        finalize_envelope_bytes(&mut envelope)?;
    }
    if serde_json::to_vec(&envelope)?.len() > PreviewPolicy::default().max_envelope_bytes {
        envelope.data_preview =
            Value::String("«preview omitted to enforce envelope budget»".to_string());
        envelope.omitted.clear();
        envelope.next_actions.clear();
        envelope.warnings.truncate(1);
        finalize_envelope_bytes(&mut envelope)?;
    }
    Ok(envelope)
}

fn make_envelope(
    input: &EnvelopeInput,
    lens_projection: prog_core::LensProjection,
    cursor: Option<String>,
) -> DisclosureEnvelope {
    let projection = lens_projection.projection;
    let observation = observation_metadata(input, &projection.omitted, cursor.as_deref());
    let mut next_actions = lens_projection
        .next_actions
        .into_iter()
        .filter(|action| cursor.is_some() || action.kind != "expand")
        .collect::<Vec<_>>();
    let generated_next_actions = cursor
        .as_ref()
        .map(|cursor| {
            expansion_next_actions(
                Some(cursor.as_str()),
                input.next_action_operation.as_deref(),
                &projection.omitted,
                10,
            )
        })
        .unwrap_or_default();
    for action in generated_next_actions {
        let duplicate = next_actions
            .iter()
            .any(|existing| existing.kind == action.kind && existing.path == action.path);
        if !duplicate {
            next_actions.push(action);
        }
    }
    for action in input.additional_next_actions.clone() {
        let duplicate = next_actions
            .iter()
            .any(|existing| existing.kind == action.kind && existing.path == action.path);
        if !duplicate {
            next_actions.push(action);
        }
    }
    next_actions.truncate(10);
    let mut extra = Extra::new();
    if let Some(lens) = &input.lens {
        extra.insert(
            "lens".to_string(),
            json!({
                "id": lens.id,
                "version": lens.version
            }),
        );
    }
    DisclosureEnvelope {
        schema_version: DISCLOSURE_VERSION.to_string(),
        source_id: Some(input.source_id.clone()),
        operation: Some(input.operation.clone()),
        summary: Summary {
            kind: value_kind(&input.payload).to_string(),
            item_count: item_count(&input.payload),
            preview_count: item_count(&projection.preview),
            payload_bytes: input.payload_bytes,
            approx_tokens: input.payload_bytes.saturating_add(3) / 4,
            envelope_bytes: None,
            extra: Extra::new(),
        },
        data_preview: projection.preview,
        schema_hints: input.schema_hints.clone(),
        omitted: projection.omitted,
        cursor,
        next_actions,
        provenance: input.provenance.clone(),
        cache: input.cache.clone(),
        observation: Some(observation),
        warnings: input.warnings.clone(),
        extra,
    }
}

fn observation_metadata(
    input: &EnvelopeInput,
    omitted: &[OmittedRegion],
    cursor: Option<&str>,
) -> ObservationMetadata {
    let redacted_omissions = omitted
        .iter()
        .filter(|region| region.reason == OmissionReason::Redacted)
        .count();
    let redacted_count = input.redacted_paths.max(redacted_omissions);
    let truncated = omitted
        .iter()
        .any(|region| region.reason != OmissionReason::Redacted);
    let path_scoped = !input.root_path.is_empty()
        || input.slice.path.is_some()
        || !input.slice.fields.is_empty()
        || !input.slice.omit.is_empty();
    let preview_complete = omitted.is_empty() && !path_scoped;
    let completeness_status = if path_scoped {
        "path_scoped"
    } else if truncated {
        "truncated"
    } else if redacted_count > 0 {
        "redacted"
    } else if !omitted.is_empty() {
        "partial"
    } else {
        "complete"
    };
    let cache_status = input.cache.as_ref().map(|cache| cache.status);
    let cached = matches!(cache_status, Some(CacheStatus::Stored | CacheStatus::Hit));
    let age_seconds = input.cache.as_ref().and_then(|cache| cache.age_seconds);
    let stale = matches!(cache_status, Some(CacheStatus::Hit)) && age_seconds.unwrap_or(0) > 0;
    let sensitive_cache_disabled = matches!(cache_status, Some(CacheStatus::Skipped))
        && input
            .effects
            .as_ref()
            .is_some_and(|effects| effects.sensitive);
    ObservationMetadata {
        completeness: ObservationCompleteness {
            status: completeness_status.to_string(),
            preview_complete,
            path_scoped,
            truncated,
            redacted: redacted_count > 0,
            omitted_count: omitted.len().try_into().unwrap_or(u64::MAX),
            redacted_count: redacted_count.try_into().unwrap_or(u64::MAX),
            root_path: input.root_path.clone(),
            extra: Extra::new(),
        },
        freshness: ObservationFreshness {
            captured_at: input
                .provenance
                .as_ref()
                .map(|provenance| provenance.captured_at.clone()),
            age_seconds,
            expires_at: input
                .cache
                .as_ref()
                .and_then(|cache| cache.expires_at.clone()),
            stale_after_seconds: match cache_status {
                Some(CacheStatus::Hit) => Some(0),
                _ => input.cache.as_ref().and_then(|cache| cache.ttl_seconds),
            },
            stale,
            refresh_recommended: stale,
            extra: Extra::new(),
        },
        trust: ObservationTrust {
            profile_backed: !matches!(input.source_id.as_str(), "observe" | "prog"),
            source_kind: input.source_kind.clone(),
            adapter_provenance: input
                .provenance
                .as_ref()
                .is_some_and(|provenance| provenance.extra.contains_key("adapter")),
            provenance_status: input
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.status.clone()),
            extra: Extra::new(),
        },
        safety: ObservationSafety {
            redacted_before_persistence: redacted_count > 0,
            redacted_paths: redacted_count.try_into().unwrap_or(u64::MAX),
            sensitive_cache_disabled,
            cache_disabled_reason: input.cache_disabled_reason.clone(),
            effects: input.effects.clone(),
            extra: Extra::new(),
        },
        payload: ObservationPayloadStatus {
            cache_status,
            cached,
            expandable: cursor.is_some(),
            payload_bytes: input.payload_bytes,
            extra: Extra::new(),
        },
        extra: Extra::new(),
    }
}

fn finalize_envelope_bytes(envelope: &mut DisclosureEnvelope) -> Result<usize> {
    envelope.summary.envelope_bytes = None;
    let first = serde_json::to_vec(envelope)?.len();
    envelope.summary.envelope_bytes = Some(first.try_into().unwrap_or(u64::MAX));
    let second = serde_json::to_vec(envelope)?.len();
    envelope.summary.envelope_bytes = Some(second.try_into().unwrap_or(u64::MAX));
    Ok(second)
}

fn shrink_policy(policy: &PreviewPolicy) -> PreviewPolicy {
    PreviewPolicy {
        array_items: halve_to_zero(policy.array_items),
        object_fields: halve_to_zero(policy.object_fields),
        string_chars: halve_to_zero(policy.string_chars).max(16),
        depth: policy.depth.saturating_sub(1),
        node_budget: halve_to_zero(policy.node_budget).max(1),
        max_envelope_bytes: policy.max_envelope_bytes,
    }
}

fn update_profile_from_call(
    store: &Store,
    profile: &SourceProfile,
    operation_id: &str,
    args: &Value,
    redacted: &Value,
    observed: &prog_core::Shape,
) -> Result<()> {
    let profile_seed = profile.clone();
    let operation_id = operation_id.to_string();
    let args = args.clone();
    let redacted = redacted.clone();
    let observed = observed.clone();
    store.update_profile(&profile.id, |current| {
        let mut next = current.unwrap_or_else(|| profile_seed.clone());
        if let Some(operation) = next
            .operations
            .iter_mut()
            .find(|operation| operation.id == operation_id)
        {
            operation.output_shape = Some(match &operation.output_shape {
                Some(current) => join(current, &observed),
                None => observed.clone(),
            });
            push_bounded_example(operation, &args, &redacted);
        }
        next
    })?;
    Ok(())
}

fn push_bounded_example(operation: &mut OperationProfile, args: &Value, redacted: &Value) {
    let projection = project(redacted, &PreviewPolicy::default(), "");
    let example = json!({
        "args": args,
        "projection": projection
    });
    let examples = operation
        .extra
        .entry("examples".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(examples) = examples {
        examples.push(example);
        while examples.len() > 5 {
            examples.remove(0);
        }
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn age_seconds(created_at: &str) -> Result<u64> {
    let created = chrono::DateTime::parse_from_rfc3339(created_at)
        .map_err(CoreError::storage)?
        .with_timezone(&Utc);
    Ok((Utc::now() - created)
        .num_seconds()
        .max(0)
        .try_into()
        .unwrap_or(u64::MAX))
}

fn ttl_between(created_at: &str, expires_at: &str) -> Result<u64> {
    let created = chrono::DateTime::parse_from_rfc3339(created_at)
        .map_err(CoreError::storage)?
        .with_timezone(&Utc);
    let expires = chrono::DateTime::parse_from_rfc3339(expires_at)
        .map_err(CoreError::storage)?
        .with_timezone(&Utc);
    Ok((expires - created)
        .num_seconds()
        .max(0)
        .try_into()
        .unwrap_or(u64::MAX))
}

fn json_len_u64(value: &Value) -> Result<u64> {
    Ok(serde_json::to_vec(value)?
        .len()
        .try_into()
        .unwrap_or(u64::MAX))
}

fn compact_json(value: &Value) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn item_count(value: &Value) -> Option<u64> {
    match value {
        Value::Array(items) => Some(items.len().try_into().unwrap_or(u64::MAX)),
        Value::Object(map) => Some(map.len().try_into().unwrap_or(u64::MAX)),
        _ => None,
    }
}

fn halve_to_zero(value: usize) -> usize {
    if value <= 1 { 0 } else { value / 2 }
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
        if let Err(error) = check_discovery(operation) {
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
    if effects.mutating {
        notes.push("mutating operation; --yes is required for calls".to_string());
    }
    if effects.network {
        notes.push("network-backed operation may contact an upstream service".to_string());
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
    if !effects.cacheable {
        notes.push("result is not cacheable under the effect policy".to_string());
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
    adapter_default: EffectSet,
    hardening: EffectSet,
    field: &str,
) -> Result<(EffectSet, bool)> {
    let Some(value) = value else {
        return Ok((adapter_default, true));
    };
    let seed: EffectSet =
        serde_json::from_value(value.clone()).map_err(|error| CoreError::BadArgs {
            operation: "discover".to_string(),
            reason: format!("seed.{field} must be an effect object: {error}"),
        })?;
    Ok((tighten_effects(&seed, &hardening), false))
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

fn adapter_seed_extra(kind: &str, seed: &Value, adapter: Value) -> Extra {
    let mut extra = Extra::new();
    extra.insert("seed_kind".to_string(), json!(kind));
    if let Some(value) = seed.get("base_url").or_else(|| seed.get("command")) {
        extra.insert("seed_origin".to_string(), value.clone());
    }
    extra.insert("adapter".to_string(), adapter);
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

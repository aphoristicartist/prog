use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    process::{ExitCode, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use chrono::{SecondsFormat, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use prog_adapters::{
    cli::{CliOperation, CliSource},
    http::{DEFAULT_MAX_RESPONSE_BYTES, HttpOperation, HttpSource},
    mcp::McpSource,
};
use prog_core::importers::{
    ImportContext, ImportReport, import_cli_help, import_json_schema, import_openapi,
};
use prog_core::{
    AuthRef, BudgetSource, CacheEntryMeta, CacheInfo, CachePolicy, CacheStatus, CallFlags,
    CallProvenance, CaptureBudget, CaptureCompleteness, CaptureLimit, CaptureScope,
    CaptureStopReason, CommandHintConfig, CoreError, DISCLOSURE_SCHEMA, DisclosureBudget,
    DisclosureEnvelope, EffectSet, EvidenceAvailability, EvidenceBlock, EvidenceGrade, EvidenceRef,
    ExpansionScope, Extra, FindingOptions, InspectRequest, InspectResponse, LensManifest,
    NewObservation, NewSessionEvent, NextAction, ObligationEvaluation, ObservationCompleteness,
    ObservationFreshness, ObservationMetadata, ObservationPayloadStatus, ObservationSafety,
    ObservationTrust, OmissionReason, OmittedRegion, OperationProfile, PersistedPayload,
    PreviewPolicy, RawPayload, ReadinessReport, RedactedPayload, RedactionPolicy, Result,
    SOURCE_PROFILE_SCHEMA, ScopedSlice, SearchOptions, SearchResponse, SliceRequest, SourceProfile,
    SourceStateToken, StorageBudget, Store, Summary, TrustSettings, VERIFICATION_SCHEMA,
    ValidatedCursor, ValueScanReport, VerificationObligation, VerificationStatus,
    build_inspect_response, cache_allowed, call_effect_warnings, canonical_json, check_call,
    check_discovery, cli_adapter_effects, cli_hardening_effects, effective_effects, evidence_block,
    expand, http_adapter_effects, http_hardening_effects, http_source_state, infer, join,
    lens_slice_request, new_cache_entry, project, project_with_lens, public_contract_schemas,
    ranked_findings_with_lens, render_hints, search_payload_with_lens, slice_value,
    tighten_effects, validate_lens_manifest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command as TokioCommand,
    sync::mpsc,
    task::JoinHandle,
};
use tracing_subscriber::{EnvFilter, fmt::writer::MakeWriterExt};

static RUN_CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static DISCLOSURE_BUDGET: OnceLock<Mutex<EffectiveDisclosureBudget>> = OnceLock::new();
static RESPONSE_STORAGE_BUDGET: OnceLock<Mutex<StorageBudget>> = OnceLock::new();
static RESPONSE_CAPTURE_BUDGET: OnceLock<Mutex<CaptureBudget>> = OnceLock::new();
const PROG_AGENT_SKILL: &str = include_str!("../../../skills/prog/SKILL.md");
const DEFAULT_DISCLOSURE_BUDGET_BYTES: usize = 16 * 1024;
const MAX_DISCLOSURE_BUDGET_BYTES: usize = 64 * 1024;
const MIN_DISCLOSURE_BUDGET_BYTES: usize = 512;
const BUDGET_METADATA_RESERVE_BYTES: usize = 384;
const TOKEN_ESTIMATOR: &str = "bytes_div_4_approximate";

#[derive(Clone, Debug)]
struct EffectiveDisclosureBudget {
    requested_bytes: Option<u64>,
    requested_tokens: Option<u64>,
    source: &'static str,
    effective_bytes: usize,
}

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

    /// Hard maximum number of bytes written in one model-visible JSON response.
    #[arg(long, global = true)]
    budget_bytes: Option<u64>,

    /// Approximate token convenience input, converted by the named bytes/4 estimator.
    #[arg(long, global = true)]
    budget_tokens: Option<u64>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Discover(DiscoverArgs),
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    Hints(HintsArgs),
    Call(CallArgs),
    Observe(ObserveArgs),
    Run(RunArgs),
    Recipe(RecipeArgs),
    Init(InitArgs),
    Cost(CostArgs),
    Paths(PathsArgs),
    Inspect(InspectArgs),
    Evidence(EvidenceArgs),
    Search(SearchArgs),
    Find(FindArgs),
    Delta(DeltaArgs),
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
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
enum ImportFormat {
    Auto,
    Openapi,
    JsonSchema,
    CliHelp,
}

impl ImportFormat {
    fn as_str(self) -> &'static str {
        match self {
            ImportFormat::Auto => "auto",
            ImportFormat::Openapi => "openapi",
            ImportFormat::JsonSchema => "json-schema",
            ImportFormat::CliHelp => "cli-help",
        }
    }
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

    #[arg(long = "import", value_enum)]
    import: Option<ImportFormat>,

    #[arg(long)]
    command_base: Option<String>,

    #[arg(long, default_value_t = 10)]
    max_schema_depth: usize,

    #[arg(long)]
    probe: bool,
}

#[derive(Debug, Subcommand)]
enum SourceCommand {
    AddHttp(SourceAddHttpArgs),
    AddCli(SourceAddCliArgs),
}

#[derive(Debug, Args)]
struct SourceAddHttpArgs {
    source_id: String,

    #[arg(long)]
    operation: String,

    #[arg(long)]
    url: String,

    #[arg(long, default_value = "GET")]
    method: String,

    #[arg(long)]
    probe: bool,
}

#[derive(Debug, Args)]
struct SourceAddCliArgs {
    source_id: String,

    #[arg(long)]
    operation: String,

    #[arg(long)]
    read_only: bool,

    #[arg(long)]
    probe: bool,

    /// Apply a conservatively detected structured-output flag when one is
    /// known to be valid for this CLI invocation.
    #[arg(long)]
    prefer_json: bool,

    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    command: Vec<String>,
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

    /// Follow pagination links for read-only operations, prefetching up to N
    /// pages into the local cache under hard page/byte/time caps.
    #[arg(long, default_value_t = 1)]
    pages: usize,
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

    #[arg(long)]
    lens: Option<String>,

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

    #[arg(long)]
    lens: Option<String>,

    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    command: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RecipeKind {
    CargoTest,
    Pytest,
    NpmTest,
    GoTest,
    GhIssues,
    DiffReview,
    LogsRootCause,
}

impl RecipeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::CargoTest => "cargo-test",
            Self::Pytest => "pytest",
            Self::NpmTest => "npm-test",
            Self::GoTest => "go-test",
            Self::GhIssues => "gh-issues",
            Self::DiffReview => "diff-review",
            Self::LogsRootCause => "logs-root-cause",
        }
    }

    fn default_goal(self) -> &'static str {
        match self {
            Self::CargoTest | Self::Pytest | Self::NpmTest | Self::GoTest => {
                "find the first causal test failure"
            }
            Self::GhIssues => "triage the most important issue evidence",
            Self::DiffReview => "find risky changed hunks",
            Self::LogsRootCause => "find the root cause in the logs",
        }
    }
}

#[derive(Debug, Args)]
struct RecipeArgs {
    #[arg(value_enum)]
    recipe: RecipeKind,

    #[arg(long)]
    goal: Option<String>,

    #[arg(long)]
    file: Option<PathBuf>,

    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u64,

    #[arg(long, default_value_t = 86_400)]
    ttl_seconds: u64,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
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
struct CostArgs {
    #[arg(long)]
    model_profile: PathBuf,

    #[arg(long)]
    raw_file: PathBuf,

    #[arg(long)]
    mime: Option<String>,

    #[arg(long = "expand-path")]
    expand_paths: Vec<String>,

    #[arg(long, default_value_t = 0)]
    estimated_output_tokens: u64,

    #[arg(long, default_value_t = 3)]
    repeated_inspections: u64,
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
struct InspectArgs {
    cursor: String,

    #[arg(long)]
    goal: String,

    #[arg(long, default_value_t = 10)]
    limit: usize,

    #[arg(long)]
    kind: Option<String>,

    #[arg(long, default_value = "")]
    path: String,
}

#[derive(Debug, Args)]
struct EvidenceArgs {
    cursor: String,

    #[arg(long, default_value = "")]
    path: String,
}

#[derive(Debug, Args)]
struct SearchArgs {
    cursor: String,
    query: String,

    #[arg(long)]
    kind: Option<String>,

    #[arg(long, default_value = "")]
    path: String,

    #[arg(long, default_value_t = 20)]
    limit: usize,

    #[arg(long)]
    case_sensitive: bool,

    #[arg(long)]
    regex: bool,
}

#[derive(Debug, Args)]
struct FindArgs {
    cursor: String,

    #[arg(long)]
    kind: String,

    #[arg(long, default_value = "")]
    path: String,

    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Debug, Args)]
struct DeltaArgs {
    baseline: String,
    subject: String,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    Start(SessionStartArgs),
    Show(SessionShowArgs),
    Note(SessionNoteArgs),
    ObligationAdd(ObligationAddArgs),
    ObligationList(ObligationListArgs),
}

#[derive(Debug, Args)]
struct SessionStartArgs {
    #[arg(long)]
    goal: Option<String>,
}

#[derive(Debug, Args)]
struct SessionShowArgs {
    session_id: Option<String>,

    /// Evaluate declared verification obligations instead of returning the session trail.
    #[arg(long)]
    readiness: bool,
}

#[derive(Debug, Args)]
struct SessionNoteArgs {
    note: String,
}

#[derive(Debug, Args)]
struct ObligationAddArgs {
    /// Stable identifier, unique within the session.
    id: String,

    /// Human-readable check the agent intends to run or evaluate.
    #[arg(long = "check")]
    intended_check: String,

    /// Scope that this check covers, such as target, affected-suite, or regression-suite.
    #[arg(long)]
    scope: String,

    /// Canonical invocation family expected for evidence.
    #[arg(long)]
    comparison_family: Option<String>,

    /// Earlier observation containing the finding that must disappear.
    #[arg(long)]
    origin_observation: Option<String>,

    /// Stable finding fingerprint that must be absent from the evidence observation.
    #[arg(long)]
    expected_absent_fingerprint: Option<String>,

    /// Observation used to evaluate this obligation.
    #[arg(long)]
    evidence_observation: Option<String>,

    /// Record an advisory obligation that does not block readiness.
    #[arg(long)]
    optional: bool,
}

#[derive(Debug, Args)]
struct ObligationListArgs {
    /// Session to evaluate. Defaults to the active session.
    session_id: Option<String>,
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
    Observations(CacheObservationsArgs),
    Get(CacheGetArgs),
    Purge(CachePurgeArgs),
    Retention(CacheRetentionArgs),
}

#[derive(Debug, Args)]
struct CacheObservationsArgs {
    #[arg(long, default_value_t = 20)]
    limit: usize,
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

    /// Retain at most this many bytes of redacted payload blobs, evicting
    /// oldest payload-reference groups while preserving metadata lineage.
    #[arg(long)]
    payload_budget_bytes: Option<u64>,
}

#[derive(Debug, Args)]
struct CacheRetentionArgs {
    /// Persist a maximum number of redacted payload bytes. Omit to keep the
    /// current value; use --clear-max-payload-bytes to remove the cap.
    #[arg(long, conflicts_with = "clear_max_payload_bytes")]
    max_payload_bytes: Option<u64>,

    /// Persist a maximum cache-entry age in seconds. Omit to keep the current
    /// value; use --clear-max-age-seconds to remove the cap.
    #[arg(long, conflicts_with = "clear_max_age_seconds")]
    max_age_seconds: Option<u64>,

    #[arg(long)]
    clear_max_payload_bytes: bool,

    #[arg(long)]
    clear_max_age_seconds: bool,
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

    let budget = match resolve_disclosure_budget(&cli) {
        Ok(budget) => budget,
        Err(error) => return write_error(&error, cli.pretty),
    };
    set_disclosure_budget(budget);

    match run(&cli).await {
        Ok(exit_code) => exit_code,
        Err(error) => write_error(&error, cli.pretty),
    }
}

fn resolve_disclosure_budget(cli: &Cli) -> Result<EffectiveDisclosureBudget> {
    let ((requested_bytes, requested_tokens), source) = if let Some(bytes) = cli.budget_bytes {
        ((Some(bytes), None), "flag")
    } else if let Some(tokens) = cli.budget_tokens {
        ((None, Some(tokens)), "flag")
    } else if let Some(bytes) = budget_env("PROG_BUDGET_BYTES")? {
        ((Some(bytes), None), "environment")
    } else if let Some(tokens) = budget_env("PROG_BUDGET_TOKENS")? {
        ((None, Some(tokens)), "environment")
    } else {
        ((None, None), "default")
    };
    effective_disclosure_budget(requested_bytes, requested_tokens, source)
}

fn effective_disclosure_budget(
    requested_bytes: Option<u64>,
    requested_tokens: Option<u64>,
    source: &'static str,
) -> Result<EffectiveDisclosureBudget> {
    let requested = match (requested_bytes, requested_tokens) {
        (Some(bytes), None) => bytes,
        (None, Some(tokens)) => tokens.checked_mul(4).ok_or_else(|| CoreError::BadArgs {
            operation: "disclosure budget".to_string(),
            reason: "token budget overflows the bytes/4 approximation".to_string(),
        })?,
        (None, None) => DEFAULT_DISCLOSURE_BUDGET_BYTES as u64,
        (Some(_), Some(_)) => unreachable!("budget source has one authoritative value"),
    };
    if requested == 0 {
        return Err(CoreError::BadArgs {
            operation: "disclosure budget".to_string(),
            reason: "budget values must be greater than zero".to_string(),
        });
    }
    let effective_bytes = requested.min(MAX_DISCLOSURE_BUDGET_BYTES as u64) as usize;
    if effective_bytes < MIN_DISCLOSURE_BUDGET_BYTES {
        return Err(CoreError::BudgetTooSmall {
            requested_bytes: effective_bytes,
            minimum_bytes: MIN_DISCLOSURE_BUDGET_BYTES,
        });
    }
    Ok(EffectiveDisclosureBudget {
        requested_bytes: requested_bytes.map(|_| requested),
        requested_tokens,
        source,
        effective_bytes,
    })
}

fn budget_env(name: &str) -> Result<Option<u64>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = value.into_string().map_err(|_| CoreError::BadArgs {
        operation: "disclosure budget".to_string(),
        reason: format!("{name} must be valid UTF-8 digits"),
    })?;
    let parsed = value.parse::<u64>().map_err(|_| CoreError::BadArgs {
        operation: "disclosure budget".to_string(),
        reason: format!("{name} must be an unsigned integer"),
    })?;
    Ok(Some(parsed))
}

fn disclosure_budget() -> EffectiveDisclosureBudget {
    DISCLOSURE_BUDGET
        .get_or_init(|| {
            Mutex::new(EffectiveDisclosureBudget {
                requested_bytes: None,
                requested_tokens: None,
                source: "default",
                effective_bytes: DEFAULT_DISCLOSURE_BUDGET_BYTES,
            })
        })
        .lock()
        .expect("disclosure budget mutex is not poisoned")
        .clone()
}

fn set_disclosure_budget(budget: EffectiveDisclosureBudget) {
    *DISCLOSURE_BUDGET
        .get_or_init(|| {
            Mutex::new(EffectiveDisclosureBudget {
                requested_bytes: None,
                requested_tokens: None,
                source: "default",
                effective_bytes: DEFAULT_DISCLOSURE_BUDGET_BYTES,
            })
        })
        .lock()
        .expect("disclosure budget mutex is not poisoned") = budget;
}

fn apply_profile_disclosure_budget(profile: &SourceProfile) -> Result<()> {
    let Some(DisclosureBudget { max_bytes, .. }) = &profile.disclosure_budget else {
        return Ok(());
    };
    let current = disclosure_budget();
    if current.source != "default" {
        return Ok(());
    }
    set_disclosure_budget(effective_disclosure_budget(
        Some(*max_bytes),
        None,
        "profile",
    )?);
    Ok(())
}

fn response_budget_bytes() -> usize {
    disclosure_budget()
        .effective_bytes
        .saturating_sub(BUDGET_METADATA_RESERVE_BYTES)
}

fn response_storage_budget() -> StorageBudget {
    RESPONSE_STORAGE_BUDGET
        .get_or_init(|| Mutex::new(StorageBudget::default()))
        .lock()
        .expect("response storage budget mutex is not poisoned")
        .clone()
}

fn set_response_storage_budget(budget: StorageBudget) {
    *RESPONSE_STORAGE_BUDGET
        .get_or_init(|| Mutex::new(StorageBudget::default()))
        .lock()
        .expect("response storage budget mutex is not poisoned") = budget;
}

fn response_capture_budget() -> CaptureBudget {
    RESPONSE_CAPTURE_BUDGET
        .get_or_init(|| Mutex::new(CaptureBudget::unavailable()))
        .lock()
        .expect("response capture budget mutex is not poisoned")
        .clone()
}

fn set_response_capture_budget(budget: CaptureBudget) {
    *RESPONSE_CAPTURE_BUDGET
        .get_or_init(|| Mutex::new(CaptureBudget::unavailable()))
        .lock()
        .expect("response capture budget mutex is not poisoned") = budget;
}

fn open_store(dir: &Path) -> Result<Store> {
    let store = Store::open(dir)?;
    set_response_storage_budget(store.storage_budget()?);
    Ok(store)
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
            let store = open_store(&cli.dir)?;
            let report = discover_source(&store, args).await?;
            write_success(&report, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Source { command } => {
            let store = open_store(&cli.dir)?;
            let report = source_command(&store, &cli.dir, command).await?;
            write_success(&report, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Hints(args) => {
            let store = open_store(&cli.dir)?;
            let response = hints_source(&store, args)?;
            write_success(&response, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Call(args) => {
            let store = open_store(&cli.dir)?;
            let mut result = call_source(&store, &cli.lens_dir, args).await?;
            record_envelope_event(&store, &mut result.envelope, "call");
            write_success(&result.envelope, cli.pretty)?;
            Ok(if result.received_error {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Observe(args) => {
            let store = open_store(&cli.dir)?;
            let mut envelope = observe_artifact(&store, &cli.lens_dir, args)?;
            record_envelope_event(&store, &mut envelope, "observe");
            write_success(&envelope, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Run(args) => {
            let store = open_store(&cli.dir)?;
            let mut result = run_command(&store, &cli.lens_dir, args).await?;
            record_envelope_event(&store, &mut result.envelope, "run");
            write_success(&result.envelope, cli.pretty)?;
            Ok(if args.preserve_exit_code {
                child_exit_code(result.exit_code)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Recipe(args) => {
            let store = open_store(&cli.dir)?;
            let mut envelope = run_recipe(&store, &cli.lens_dir, args).await?;
            record_envelope_event(&store, &mut envelope, "recipe");
            write_success(&envelope, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Init(args) => {
            let report = init_integration(args)?;
            write_success(&report, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Cost(args) => {
            let report = cost_report(args)?;
            write_success(&report, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Paths(args) => {
            let store = open_store(&cli.dir)?;
            let response = paths_cursor(&store, args)?;
            record_navigation_event(
                &store,
                "paths",
                Some(&args.cursor),
                Some(&response.prefix),
                None,
                Some(format!("listed {} cached path(s)", response.paths.len())),
            );
            write_success(&response, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Inspect(args) => {
            let store = open_store(&cli.dir)?;
            let response = inspect_cursor(&store, &cli.lens_dir, args)?;
            record_navigation_event(
                &store,
                "inspect",
                Some(&args.cursor),
                response.scope_path.as_deref(),
                None,
                Some(format!(
                    "ranked {} finding(s) for {}",
                    response.findings.len(),
                    response.goal
                )),
            );
            write_success(&response, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Evidence(args) => {
            let store = open_store(&cli.dir)?;
            let response = evidence_cursor(&store, &cli.lens_dir, args)?;
            record_navigation_event(
                &store,
                "evidence",
                Some(&args.cursor),
                Some(&response.path),
                response
                    .evidence_ref
                    .as_ref()
                    .and_then(|reference| reference.uri.as_deref()),
                Some(response.summary.clone()),
            );
            write_success(&response, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Search(args) => {
            let store = open_store(&cli.dir)?;
            let response = search_cursor(
                &store,
                &cli.lens_dir,
                &args.cursor,
                Some(args.query.clone()),
                args.kind.clone(),
                &args.path,
                args.limit,
                args.case_sensitive,
                args.regex,
            )?;
            record_navigation_event(
                &store,
                "search",
                Some(&args.cursor),
                response.scope_path.as_deref(),
                None,
                Some(format!("found {} cached match(es)", response.hits.len())),
            );
            write_success(&response, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Find(args) => {
            let store = open_store(&cli.dir)?;
            let response = search_cursor(
                &store,
                &cli.lens_dir,
                &args.cursor,
                None,
                Some(args.kind.clone()),
                &args.path,
                args.limit,
                false,
                false,
            )?;
            record_navigation_event(
                &store,
                "find",
                Some(&args.cursor),
                response.scope_path.as_deref(),
                None,
                Some(format!(
                    "found {} {} match(es)",
                    response.hits.len(),
                    args.kind
                )),
            );
            write_success(&response, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Delta(args) => {
            let store = open_store(&cli.dir)?;
            let delta = delta_observations(&store, args)?;
            write_success(&delta, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Session { command } => {
            let store = open_store(&cli.dir)?;
            match command {
                SessionCommand::Start(args) => {
                    let trail = store.start_session(args.goal.clone())?;
                    write_success(&trail, cli.pretty)?;
                }
                SessionCommand::Show(args) => {
                    if args.readiness {
                        let session_id = args.session_id.as_deref();
                        let report = readiness_report(&store, session_id)?;
                        write_success(&report, cli.pretty)?;
                    } else {
                        let trail = session_show(&store, args)?;
                        write_success(&trail, cli.pretty)?;
                    }
                }
                SessionCommand::Note(args) => {
                    let event = store.record_session_event(NewSessionEvent {
                        kind: "conclusion".to_string(),
                        summary: Some(args.note.clone()),
                        ..NewSessionEvent::default()
                    })?;
                    write_success(&event, cli.pretty)?;
                }
                SessionCommand::ObligationAdd(args) => {
                    let session = match store.get_session(None)? {
                        Some(session) => session,
                        None => store.start_session(None)?,
                    };
                    let obligation = VerificationObligation {
                        schema: VERIFICATION_SCHEMA.to_string(),
                        id: args.id.clone(),
                        session_id: session.session_id,
                        required: !args.optional,
                        intended_check: args.intended_check.clone(),
                        required_scope: args.scope.clone(),
                        comparison_family: args.comparison_family.clone(),
                        origin_observation_id: args.origin_observation.clone(),
                        expected_absent_fingerprint: args.expected_absent_fingerprint.clone(),
                        evidence_observation_id: args.evidence_observation.clone(),
                        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                        extra: Extra::new(),
                    };
                    store.put_obligation(&obligation)?;
                    write_success(&obligation, cli.pretty)?;
                }
                SessionCommand::ObligationList(args) => {
                    let report = readiness_report(&store, args.session_id.as_deref())?;
                    write_success(&report, cli.pretty)?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Expand(args) => {
            let store = open_store(&cli.dir)?;
            let mut envelope = expand_cursor(&store, args)?;
            record_envelope_event(&store, &mut envelope, "expand");
            write_success(&envelope, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Cache { command } => match command {
            CacheCommand::List => {
                let store = open_store(&cli.dir)?;
                write_success(&store.list_entries(100)?, cli.pretty)?;
                Ok(ExitCode::SUCCESS)
            }
            CacheCommand::Observations(args) => {
                let store = open_store(&cli.dir)?;
                write_success(&store.list_observations(args.limit)?, cli.pretty)?;
                Ok(ExitCode::SUCCESS)
            }
            CacheCommand::Get(args) => {
                let store = open_store(&cli.dir)?;
                let entry = store
                    .get_entry(&args.key)?
                    .ok_or_else(|| CoreError::CacheMiss(args.key.clone()))?;
                let payload = store
                    .get_payload(&entry.payload_hash)?
                    .ok_or_else(|| CoreError::CacheMiss(args.key.clone()))?;
                let scoped = ScopedSlice::root(SliceRequest {
                    path: None,
                    limit: None,
                    depth: None,
                    fields: Vec::new(),
                    omit: Vec::new(),
                    extra: serde_json::Map::new(),
                })?;
                let projection = expand(&payload, &scoped, &PreviewPolicy::default())?;
                write_success(&CacheGetOutput { entry, projection }, cli.pretty)?;
                Ok(ExitCode::SUCCESS)
            }
            CacheCommand::Purge(args) => {
                let store = open_store(&cli.dir)?;
                let selected = usize::from(args.all)
                    + usize::from(args.expired)
                    + usize::from(args.source.is_some())
                    + usize::from(args.payload_budget_bytes.is_some());
                if selected != 1 {
                    return Err(CoreError::BadArgs {
                        operation: "cache purge".to_string(),
                        reason: "pass exactly one of --all, --expired, --source <id>, or --payload-budget-bytes <bytes>".to_string(),
                    });
                }
                let summary = if args.all {
                    store.purge_all()?
                } else if args.expired {
                    store.purge_expired(chrono::Utc::now())?
                } else if let Some(source) = &args.source {
                    store.purge_source(source)?
                } else if let Some(max_payload_bytes) = args.payload_budget_bytes {
                    write_success(&store.enforce_payload_quota(max_payload_bytes)?, cli.pretty)?;
                    return Ok(ExitCode::SUCCESS);
                } else {
                    unreachable!("validated one cache purge selector")
                };
                write_success(&summary, cli.pretty)?;
                Ok(ExitCode::SUCCESS)
            }
            CacheCommand::Retention(args) => {
                let store = open_store(&cli.dir)?;
                let changes = usize::from(args.max_payload_bytes.is_some())
                    + usize::from(args.max_age_seconds.is_some())
                    + usize::from(args.clear_max_payload_bytes)
                    + usize::from(args.clear_max_age_seconds);
                if changes == 0 {
                    write_success(&store.storage_budget()?, cli.pretty)?;
                    return Ok(ExitCode::SUCCESS);
                }
                let mut budget = store.storage_budget()?;
                budget.source = BudgetSource::StorePolicy;
                if let Some(max_payload_bytes) = args.max_payload_bytes {
                    budget.max_payload_bytes = Some(max_payload_bytes);
                } else if args.clear_max_payload_bytes {
                    budget.max_payload_bytes = None;
                }
                if let Some(max_age_seconds) = args.max_age_seconds {
                    budget.max_age_seconds = Some(max_age_seconds);
                } else if args.clear_max_age_seconds {
                    budget.max_age_seconds = None;
                }
                let summary = store.set_storage_budget(&budget)?;
                set_response_storage_budget(summary.budget.clone());
                write_success(&summary, cli.pretty)?;
                Ok(ExitCode::SUCCESS)
            }
        },
        Command::Meta(args) => {
            let store = open_store(&cli.dir)?;
            let envelope = meta_contracts(&store, args)?;
            write_success(&envelope, cli.pretty)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[derive(Clone, Serialize)]
struct DiscoverReport {
    schema: &'static str,
    source_id: String,
    kind: prog_core::SourceKind,
    profile_revision: u64,
    operations_found: usize,
    operations_probed: usize,
    shapes_learned: usize,
    import_format: Option<String>,
    schemas_imported: usize,
    examples_inferred: usize,
    warnings: Vec<String>,
    effects_assumed: Vec<String>,
}

#[derive(Serialize)]
struct SourceAddReport {
    schema: &'static str,
    source_id: String,
    kind: prog_core::SourceKind,
    operation: String,
    generated_seed: Value,
    discovery: DiscoverReport,
    next_steps: Vec<String>,
    structured_output: Vec<StructuredOutputHint>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct StructuredOutputHint {
    status: &'static str,
    flag: Vec<String>,
    confidence: &'static str,
    reason: String,
}

#[derive(Serialize)]
struct HintsResponse {
    schema: &'static str,
    source_id: String,
    profile_revision: u64,
    observation_id: String,
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
    received_error: bool,
    not_modified: bool,
}

struct CallSourceResult {
    envelope: DisclosureEnvelope,
    received_error: bool,
}

struct EnvelopeInput {
    source_id: String,
    operation: String,
    source_kind: Option<String>,
    payload: RedactedPayload,
    root_path: String,
    slice: SliceRequest,
    payload_bytes: u64,
    observation_id: Option<String>,
    provenance: Option<CallProvenance>,
    cache: Option<CacheInfo>,
    effects: Option<EffectSet>,
    /// Audit note recorded when trust auto-upgrade relaxed a *proven* read-only
    /// op's `requires_confirmation` for this call. When `Some`, the observation
    /// metadata surfaces the evidence chain (grade + reason) under
    /// `observation.trust.extra["auto_upgrade"]`.
    auto_upgrade_audit: Option<String>,
    redacted_paths: usize,
    cache_disabled_reason: Option<String>,
    warnings: Vec<String>,
    schema_hints: BTreeMap<String, String>,
    next_action_operation: Option<String>,
    additional_next_actions: Vec<NextAction>,
    observation_parser: Option<Value>,
    lens: Option<LensManifest>,
    value_scan: Option<ValueScanReport>,
}

struct CursorInput<'a> {
    cache_key: &'a str,
    source_id: &'a str,
    operation: &'a str,
    root_path: &'a str,
    payload: &'a RedactedPayload,
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
    parser: ObservationParserInfo,
    warnings: Vec<String>,
}

#[derive(Clone)]
struct ObservationParserInfo {
    id: &'static str,
    label: &'static str,
    confidence: f64,
    lossy: bool,
    fallback: bool,
    reason: &'static str,
    path_semantics: &'static str,
    range_semantics: &'static str,
}

struct ParserMatch {
    confidence: f64,
    reason: &'static str,
}

struct ObservationParser {
    id: &'static str,
    detect: fn(&[u8], &str) -> Option<ParserMatch>,
    parse: fn(&[u8], &str, ParserMatch) -> Result<NormalizedObservation>,
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
    relative_path: String,
    content: String,
    executable: bool,
}

struct EvidenceRefInput<'a> {
    source_id: &'a str,
    operation: &'a str,
    cursor: Option<&'a str>,
    path: &'a str,
    value: &'a Value,
    observation: Option<&'a prog_core::ObservationRecord>,
    provenance: Option<&'a CallProvenance>,
    cache: Option<&'a CacheInfo>,
    omitted: &'a [OmittedRegion],
    redacted_paths: usize,
}

struct PathEvidenceContext<'a> {
    record: &'a prog_core::CursorRecord,
    entry: &'a prog_core::CacheEntryMeta,
    observation: Option<&'a prog_core::ObservationRecord>,
    cache: &'a CacheInfo,
    omitted: &'a [OmittedRegion],
    cursor: &'a str,
}

#[derive(Debug, Deserialize)]
struct ModelCostProfile {
    #[serde(default)]
    schema: Option<String>,
    model: String,
    #[serde(default)]
    input_price_per_million_tokens: Option<f64>,
    #[serde(default)]
    output_price_per_million_tokens: Option<f64>,
    context_window_tokens: u64,
    #[serde(default)]
    cache_read_price_per_million_tokens: Option<f64>,
    #[serde(default)]
    cache_write_price_per_million_tokens: Option<f64>,
    #[serde(default)]
    pricing_source: Option<String>,
    #[serde(default)]
    priced_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct CostReport {
    schema: &'static str,
    model: CostModelSummary,
    input: CostInputSummary,
    scenarios: Vec<CostScenario>,
    warnings: Vec<String>,
    counterexamples: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CostModelSummary {
    model: String,
    input_price_per_million_tokens: f64,
    output_price_per_million_tokens: f64,
    context_window_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_read_price_per_million_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_write_price_per_million_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pricing_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    priced_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct CostInputSummary {
    raw_file: String,
    raw_bytes: u64,
    raw_tokens: u64,
    mime: String,
    expand_paths: Vec<String>,
    estimated_output_tokens: u64,
    repeated_inspections: u64,
}

#[derive(Debug, Serialize)]
struct CostScenario {
    name: &'static str,
    input_tokens: u64,
    output_tokens: u64,
    total_estimated_cost_usd: f64,
    baseline_input_tokens: u64,
    baseline_estimated_cost_usd: f64,
    savings_ratio: f64,
    fits_context: bool,
    lossless: bool,
    notes: Vec<String>,
}

struct CostFlowEstimate {
    observe_tokens: u64,
    paths_tokens: u64,
    expansion_tokens: u64,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct InitReport {
    schema: &'static str,
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
    schema: &'static str,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence_ref: Option<EvidenceRef>,
}

struct PathFilters {
    reason: Option<OmissionReason>,
    fields: BTreeSet<String>,
    omitted_only: bool,
    expandable_only: bool,
}

async fn discover_source(store: &Store, args: &DiscoverArgs) -> Result<DiscoverReport> {
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

async fn discover_from_seed(
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

async fn source_command(
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

fn shell_quote(value: &str) -> String {
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

/// Resolve the persistence redaction policy for an optional source profile,
/// honoring per-source `RedactionConfig` and the `PROG_REDACTION_ALLOWLIST` /
/// `PROG_REDACTION_EXTRA_KEYWORDS` env overrides (comma-separated). The
/// built-in allowlist (e.g. `max_tokens`, `session_timeout`) is always present
/// so benign token-count fields survive by default.
fn resolve_redaction(profile: Option<&SourceProfile>) -> RedactionPolicy {
    let mut policy = match profile {
        Some(profile) => RedactionPolicy::from_config(&profile.redaction),
        None => RedactionPolicy::default(),
    };
    if let Ok(raw) = std::env::var("PROG_REDACTION_ALLOWLIST") {
        policy.allowlist.extend(
            raw.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
        );
    }
    if let Ok(raw) = std::env::var("PROG_REDACTION_EXTRA_KEYWORDS") {
        let names: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .map(str::to_string)
            .filter(|name| !name.is_empty())
            .collect();
        if !names.is_empty() {
            policy.rules.push(prog_core::RedactionRule {
                name: "env_extra".to_string(),
                class: prog_core::RedactionClass::Persistence,
                field_names: names,
            });
        }
    }
    policy
}

fn hints_source(store: &Store, args: &HintsArgs) -> Result<HintsResponse> {
    let profile = store
        .read_profile(&args.source_id)?
        .ok_or_else(|| CoreError::UnknownSource(args.source_id.clone()))?;
    apply_profile_disclosure_budget(&profile)?;
    let hints = build_hints_document(&profile, args.operation.as_deref())?;
    let redacted = RawPayload::new(hints).redact(&resolve_redaction(Some(&profile)));
    let payload = redacted.payload;
    let payload_hash = store.put_payload(&payload)?;
    let projection = project(payload.as_value(), &PreviewPolicy::default(), "");
    let cache_key = Store::cache_key(
        &args.source_id,
        "hints",
        &json!({"operation": args.operation}),
    )?;
    let mut entry = new_cache_entry(
        cache_key.clone(),
        payload_hash.clone(),
        args.source_id.clone(),
        "hints".to_string(),
        serde_json::to_vec(payload.as_value())?
            .len()
            .try_into()
            .unwrap_or(u64::MAX),
        86_400,
    );
    let (availability, capture) = complete_capture(entry.payload_bytes, true, false);
    set_response_capture_budget(capture.budget.clone());
    let observation_id = record_capture(
        store,
        payload_hash,
        availability,
        capture,
        cache_key.clone(),
        args.source_id.clone(),
        "hints".to_string(),
        entry.provenance.clone(),
        Some(cache_key.clone()),
        false,
        None,
        None,
        None,
    )?;
    entry.observation_id = Some(observation_id.clone());
    let cache_retained = store.put_entry(&cache_key, &entry)?;
    let cursor = if projection.omitted.is_empty() || !cache_retained {
        None
    } else {
        Some(store.create_cursor(&cache_key, &args.source_id, "hints", "", 1, 86_400)?)
    };

    Ok(HintsResponse {
        schema: DISCLOSURE_SCHEMA,
        source_id: args.source_id.clone(),
        profile_revision: profile.revision,
        observation_id,
        hints: projection.preview,
        omitted: projection.omitted,
        cursor,
        warnings: Vec::new(),
    })
}

async fn call_source(store: &Store, lens_dir: &Path, args: &CallArgs) -> Result<CallSourceResult> {
    let profile = store
        .read_profile(&args.source_id)?
        .ok_or_else(|| CoreError::UnknownSource(args.source_id.clone()))?;
    apply_profile_disclosure_budget(&profile)?;
    let operation = profile_operation(&profile, &args.operation)?.clone();
    let call_args = parse_json_argument(&args.args, "call --args")?;
    validate_call_args(&operation, &call_args)?;
    // check_call runs trust auto-upgrade internally and returns the EFFECTIVE
    // (possibly relaxed) effect set plus the audit note; both flow into the
    // envelope so the recorded observation reflects the policy actually applied
    // and the upgrade is inspectable.
    let (effective_effects, auto_upgrade_audit) =
        check_call(&operation, CallFlags { yes: args.yes }, &profile.trust)?;
    let requested_view = parse_view(args.view.as_deref())?;
    let lens = match &args.lens {
        Some(id) => {
            let lens = load_lens(lens_dir, id, "call --lens")?;
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

    let cached_entry = if may_cache {
        store.get_entry(&cache_key)?
    } else {
        None
    };
    if !args.refresh
        && let Some(entry) = cached_entry.as_ref()
    {
        let cached_pagination = entry.extra.get("pagination").cloned();
        let cache_satisfies_request = args.pages <= 1
            || cached_pagination
                .as_ref()
                .is_some_and(|value| cached_pagination_satisfies(value, args.pages));
        if cache_satisfies_request {
            if let Some(observation_id) = &entry.observation_id
                && let Some(observation) = store.get_observation(observation_id)?
            {
                set_response_capture_budget(observation.capture.budget);
            }
            let payload = store
                .get_payload(&entry.payload_hash)?
                .ok_or_else(|| CoreError::CacheMiss(cache_key.clone()))?
                .into_redacted();
            let cache_info = cache_info(
                CacheStatus::Hit,
                entry,
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
            let mut envelope = envelope_for_payload(
                store,
                EnvelopeInput {
                    value_scan: None,
                    source_id: args.source_id.clone(),
                    operation: args.operation.clone(),
                    source_kind: Some(profile_source_kind_name(profile.kind).to_string()),
                    payload,
                    root_path: root_path.clone(),
                    slice: view,
                    payload_bytes: entry.payload_bytes,
                    observation_id: entry.observation_id.clone(),
                    provenance: entry.provenance.clone(),
                    cache: Some(cache_info),
                    effects: Some(effective_effects.clone()),
                    auto_upgrade_audit: auto_upgrade_audit.clone(),
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
                    observation_parser: None,
                    lens,
                },
                cursor,
            )?;
            if let Some(pagination) = cached_pagination {
                envelope.extra.insert("pagination".to_string(), pagination);
                if let Some(actions) = entry.extra.get("pagination_next_actions") {
                    let actions: Vec<NextAction> = serde_json::from_value(actions.clone())?;
                    envelope.next_actions.extend(actions);
                }
                compact_pagination_extra_to_budget(&mut envelope)?;
            }
            let received_error = entry.provenance.as_ref().is_some_and(|provenance| {
                provenance.extra.get("received_error") == Some(&Value::Bool(true))
            });
            return Ok(CallSourceResult {
                envelope,
                received_error,
            });
        }
    }

    let source = callable_source_from_profile(&profile)?;
    let revalidation = if args.refresh {
        match cached_entry
            .as_ref()
            .and_then(|entry| entry.observation_id.as_deref())
        {
            Some(observation_id) => store
                .get_observation(observation_id)?
                .and_then(|observation| observation.source_state),
            None => None,
        }
    } else {
        None
    };
    let adapter_call =
        execute_callable_conditional(&source, &operation, &call_args, revalidation.as_ref())
            .await?;
    if adapter_call.not_modified {
        let prior = cached_entry.as_ref().ok_or_else(|| CoreError::BadArgs {
            operation: "call --refresh".to_string(),
            reason: "received HTTP 304 without a reusable cached observation".to_string(),
        })?;
        let prior_id = prior
            .observation_id
            .as_ref()
            .ok_or_else(|| CoreError::BadArgs {
                operation: "call --refresh".to_string(),
                reason: "received HTTP 304 but cached evidence has no observation identity"
                    .to_string(),
            })?;
        let prior_observation =
            store
                .get_observation(prior_id)?
                .ok_or_else(|| CoreError::BadArgs {
                    operation: "call --refresh".to_string(),
                    reason: "received HTTP 304 but cached observation metadata is unavailable"
                        .to_string(),
                })?;
        set_response_capture_budget(prior_observation.capture.budget.clone());
        let payload = store
            .get_payload(&prior.payload_hash)?
            .ok_or_else(|| CoreError::CacheMiss(cache_key.clone()))?
            .into_redacted();
        let provenance = call_provenance(
            &cache_key,
            adapter_call.status.clone(),
            adapter_call.duration_ms,
            adapter_call.provenance,
        );
        let observation_id = store
            .record_observation(NewObservation {
                payload_hash: prior.payload_hash.clone(),
                availability: prior_observation.availability,
                invocation_fingerprint: cache_key.clone(),
                source_id: args.source_id.clone(),
                operation: args.operation.clone(),
                captured_at: Some(provenance.captured_at.clone()),
                duration_ms: provenance.duration_ms,
                status: provenance.status.clone(),
                capture: prior_observation.capture.clone(),
                redacted: prior_observation.redacted,
                source_state: prior_observation.source_state.clone(),
                lineage: prog_core::ObservationLineage {
                    revalidates_id: Some(prior_id.clone()),
                    ..prog_core::ObservationLineage::default()
                },
                provenance: Some(provenance.clone()),
                cache_key: Some(cache_key.clone()),
                ..NewObservation::default()
            })?
            .observation_id;
        let mut entry = prior.clone();
        entry.observation_id = Some(observation_id.clone());
        entry.provenance = Some(provenance.clone());
        let cache_retained = store.put_entry(&cache_key, &entry)?;
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
                may_cache: cache_retained,
                lens: lens.as_ref(),
            },
        )?;
        let retention_warning =
            "cache retention policy evicted this payload before it could be reused".to_string();
        let mut envelope = envelope_for_payload(
            store,
            EnvelopeInput {
                value_scan: None,
                source_id: args.source_id.clone(),
                operation: args.operation.clone(),
                source_kind: Some(profile_source_kind_name(profile.kind).to_string()),
                payload,
                root_path,
                slice: view,
                payload_bytes: entry.payload_bytes,
                observation_id: Some(observation_id),
                provenance: Some(provenance),
                cache: Some(if cache_retained {
                    cache_info(
                        CacheStatus::Hit,
                        &entry,
                        Some(age_seconds(&entry.created_at)?),
                    )
                } else {
                    CacheInfo {
                        status: CacheStatus::Skipped,
                        ttl_seconds: None,
                        expires_at: None,
                        age_seconds: None,
                    }
                }),
                effects: Some(effective_effects),
                auto_upgrade_audit,
                redacted_paths: 0,
                cache_disabled_reason: (!cache_retained).then_some(retention_warning.clone()),
                warnings: {
                    let mut warnings = vec![
                        "HTTP validator confirmed the source is unchanged (304 Not Modified)"
                            .to_string(),
                    ];
                    if !cache_retained {
                        warnings.push(retention_warning);
                    }
                    warnings
                },
                schema_hints: operation
                    .output_shape
                    .as_ref()
                    .map(|shape| render_hints(shape, ""))
                    .unwrap_or_default(),
                next_action_operation: Some(args.operation.clone()),
                additional_next_actions: Vec::new(),
                observation_parser: None,
                lens,
            },
            cursor,
        )?;
        envelope
            .extra
            .insert("source_validity".to_string(), json!("confirmed_unchanged"));
        compact_envelope_to_budget(&mut envelope)?;
        return Ok(CallSourceResult {
            envelope,
            received_error: false,
        });
    }
    let received_error = adapter_call.received_error;
    let first_pagination = adapter_call.pagination.clone();
    let redaction = resolve_redaction(Some(&profile));
    let redacted = RawPayload::new(adapter_call.data).redact(&redaction);
    let redacted_paths = redacted.redacted_paths;
    let value_scan = redacted.value_scan;
    let payload = redacted.payload;
    let payload_bytes = json_len_u64(payload.as_value())?;
    let observed = infer(payload.as_value());

    let mut provenance = call_provenance(
        &cache_key,
        adapter_call.status,
        adapter_call.duration_ms,
        adapter_call.provenance,
    );
    provenance
        .extra
        .insert("received_error".to_string(), Value::Bool(received_error));
    let mut warnings = adapter_call.warnings;
    warnings.extend(call_effect_warnings(&operation));
    if args.no_cache {
        warnings.push("profile learning skipped because --no-cache was requested".to_string());
    } else if operation.effects.sensitive {
        warnings.push(
            "profile learning skipped because the operation may handle sensitive data".to_string(),
        );
    } else {
        update_profile_from_call(
            store,
            &profile,
            &operation.id,
            &call_args,
            payload.as_value(),
            &observed,
        )?;
    }
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

    let payload_hash = if may_cache {
        store.put_payload(&payload)?
    } else {
        Store::payload_hash(&payload)?
    };
    if may_cache {
        provenance.cache_key = Some(cache_key.clone());
    } else {
        provenance.cache_key = None;
    }
    let (availability, mut capture) = adapter_capture(
        Some(&provenance),
        payload.as_value(),
        payload_bytes,
        may_cache,
        !redacted_paths.is_empty(),
    );
    capture.budget = capture_budget_for_call(&profile, &operation);
    set_response_capture_budget(capture.budget.clone());
    let observation_id = record_capture(
        store,
        payload_hash.clone(),
        availability,
        capture,
        cache_key.clone(),
        args.source_id.clone(),
        args.operation.clone(),
        Some(provenance.clone()),
        may_cache.then(|| cache_key.clone()),
        !redacted_paths.is_empty(),
        None,
        lens.as_ref(),
        source_state_from_provenance(
            profile.kind,
            &args.source_id,
            &args.operation,
            &call_args,
            &provenance,
        )?,
    )?;

    let mut cache_disabled_reason = None;
    let cache_retained = if may_cache {
        let ttl = ttl_seconds(&effective_cache);
        let mut entry = new_cache_entry(
            cache_key.clone(),
            payload_hash,
            args.source_id.clone(),
            args.operation.clone(),
            payload_bytes,
            ttl,
        );
        entry.observation_id = Some(observation_id.clone());
        entry.provenance = Some(provenance.clone());
        let retained = store.put_entry(&cache_key, &entry)?;
        if !retained {
            let reason =
                "cache retention policy evicted this payload before it could be reused".to_string();
            warnings.push(reason.clone());
            cache_disabled_reason = Some(reason);
        }
        retained
    } else {
        false
    };
    let cache_status = if cache_retained {
        let entry = store
            .get_entry(&cache_key)?
            .ok_or_else(|| CoreError::CacheMiss(cache_key.clone()))?;
        Some(cache_info(CacheStatus::Stored, &entry, Some(0)))
    } else if !may_cache {
        let reason = cache_skip_warning(args.no_cache, &operation);
        warnings.push(reason.clone());
        cache_disabled_reason = Some(reason);
        Some(CacheInfo {
            status: CacheStatus::Skipped,
            ttl_seconds: None,
            expires_at: None,
            age_seconds: None,
        })
    } else {
        Some(CacheInfo {
            status: CacheStatus::Skipped,
            ttl_seconds: None,
            expires_at: None,
            age_seconds: None,
        })
    };

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
            may_cache: cache_retained,
            lens: lens.as_ref(),
        },
    )?;
    let mut envelope = envelope_for_payload(
        store,
        EnvelopeInput {
            value_scan: Some(value_scan),
            source_id: args.source_id.clone(),
            operation: args.operation.clone(),
            source_kind: Some(profile_source_kind_name(profile.kind).to_string()),
            payload,
            root_path: root_path.clone(),
            slice: view,
            payload_bytes,
            observation_id: Some(observation_id),
            provenance: Some(provenance),
            cache: cache_status,
            effects: Some(effective_effects),
            auto_upgrade_audit,
            redacted_paths: redacted_paths.len(),
            cache_disabled_reason,
            warnings,
            schema_hints: render_hints(&observed, ""),
            next_action_operation: Some(args.operation.clone()),
            additional_next_actions: Vec::new(),
            observation_parser: None,
            lens: lens.clone(),
        },
        cursor,
    )?;
    if args.refresh {
        let validity = if received_error {
            "refresh_failed"
        } else if revalidation.is_some() {
            "source_changed"
        } else {
            "validator_unavailable"
        };
        envelope
            .extra
            .insert("source_validity".to_string(), json!(validity));
    }

    // Auto-pagination: when --pages > 1 on a read-only operation, prefetch up
    // to N pages into the cache under hard page/byte/time caps (I10). The
    // envelope stays the bounded view of page 1; additional pages are each
    // redacted -> inferred -> stored -> projected (I2/I8), their shapes merged
    // monotonically (I5), and each is reachable via its own pc1_ page cursor
    // (I9) or the surfaced continuation NextAction.
    if args.pages > 1 && !received_error {
        if prog_core::pagination_allowed(&operation.effects) {
            let caps = prog_core::PageCaps {
                max_pages: args.pages.min(50),
                ..prog_core::PageCaps::default()
            };
            let mut current_args = call_args.clone();
            // Live hints win; fall back to the discover-time pagination shape
            // stored on the operation profile when the live response carries none.
            let mut hints = first_pagination
                .clone()
                .or_else(|| operation.pagination.clone());
            let mut pages_fetched = 1usize;
            let mut total_bytes = payload_bytes;
            let mut stop = prog_core::StopReason::NoMore;
            let started = std::time::Instant::now();
            let mut prefetch_warnings: Vec<String> = Vec::new();
            // Per-page shape accumulation (I5) seeded with page 1.
            let mut merged_shape = observed.clone();
            // Page summaries (page 1 first). `envelope.omitted` stays page-1
            // scoped so an expand against the page-1 cursor can never reach a
            // page-2 path (I3 containment / I9 fail-closed).
            let mut page_summaries: Vec<Value> = Vec::new();
            page_summaries.push(json!({
                "page": 1,
                "cache_key": cache_key.clone(),
                "cursor": envelope.cursor.clone(),
                "bytes": payload_bytes,
                "omitted_count": envelope.omitted.len(),
                "omitted_paths": envelope.omitted.iter().take(8)
                    .map(|region| region.path.clone()).collect::<Vec<_>>(),
            }));
            while pages_fetched < caps.max_pages {
                let Some(target) = hints
                    .as_ref()
                    .and_then(|value| prog_core::next_args_from_hints(value, &current_args))
                else {
                    stop = prog_core::StopReason::NoMore;
                    break;
                };
                if started.elapsed().as_millis() as u64 > caps.max_wall_ms {
                    stop = prog_core::StopReason::TimeCap;
                    break;
                }
                // Resolve the target into a fetched page + the args used for
                // the cache key. URL continuation (Link rel="next") now follows
                // the same-host guard inside HttpSource::execute_url.
                let (page_call, page_key_args) = match target {
                    prog_core::PageTarget::Args(page_args) => {
                        let call = match execute_callable(&source, &operation, &page_args).await {
                            Ok(call) => call,
                            Err(error) => {
                                prefetch_warnings.push(format!(
                                    "pagination prefetch stopped at page {}: {error}",
                                    pages_fetched + 1
                                ));
                                stop = prog_core::StopReason::NoMore;
                                break;
                            }
                        };
                        let key_args = page_args.clone();
                        (call, key_args)
                    }
                    prog_core::PageTarget::Url(url) => {
                        match execute_callable_url(&source, &operation, &url, &current_args).await {
                            Ok(Some(call)) => {
                                // Distinct, deterministic cache key per URL page.
                                (call, json!({ "__url__": url }))
                            }
                            Ok(None) => {
                                prefetch_warnings.push(
                                    "pagination prefetch stopped: the next page is a URL \
                                     continuation (Link rel=\"next\") but this source kind has no \
                                     URL model"
                                        .to_string(),
                                );
                                stop = prog_core::StopReason::NoMore;
                                break;
                            }
                            Err(error) => {
                                prefetch_warnings.push(format!(
                                    "pagination prefetch stopped at page {}: {error}",
                                    pages_fetched + 1
                                ));
                                stop = prog_core::StopReason::NoMore;
                                break;
                            }
                        }
                    }
                };
                // redact -> infer -> store -> project, per page (I2/I8).
                let page_payload = RawPayload::new(page_call.data).redact(&redaction).payload;
                let page_bytes = json_len_u64(page_payload.as_value())?;
                if total_bytes + page_bytes > caps.max_total_bytes {
                    stop = prog_core::StopReason::ByteCap;
                    break;
                }
                total_bytes += page_bytes;
                prefetch_warnings.extend(page_call.warnings);

                let page_shape = infer(page_payload.as_value());
                merged_shape = prog_core::merge_page_shapes(Some(&merged_shape), &page_shape);
                // Project with a coarsened policy to obtain THIS page's omitted
                // regions; previews for N>=2 never enter envelope.data_preview
                // (page 1 stays the bounded view), only counts + top-K paths.
                let page_projection = project(
                    page_payload.as_value(),
                    &shrink_policy(&PreviewPolicy::default()),
                    "",
                );
                let omitted_paths: Vec<String> = page_projection
                    .omitted
                    .iter()
                    .take(8)
                    .map(|region| region.path.clone())
                    .collect();

                let page_cache_key =
                    Store::cache_key(&args.source_id, &args.operation, &page_key_args)?;
                let page_hash = if may_cache {
                    store.put_payload(&page_payload)?
                } else {
                    Store::payload_hash(&page_payload)?
                };
                let page_provenance = call_provenance(
                    &page_cache_key,
                    page_call.status.clone(),
                    page_call.duration_ms,
                    page_call.provenance.clone(),
                );
                let (availability, mut capture) = adapter_capture(
                    Some(&page_provenance),
                    page_payload.as_value(),
                    page_bytes,
                    may_cache,
                    false,
                );
                capture.budget = capture_budget_for_call(&profile, &operation);
                set_response_capture_budget(capture.budget.clone());
                let page_observation_id = record_capture(
                    store,
                    page_hash.clone(),
                    availability,
                    capture,
                    page_cache_key.clone(),
                    args.source_id.clone(),
                    args.operation.clone(),
                    Some(page_provenance.clone()),
                    may_cache.then(|| page_cache_key.clone()),
                    false,
                    None,
                    lens.as_ref(),
                    source_state_from_provenance(
                        profile.kind,
                        &args.source_id,
                        &args.operation,
                        &page_key_args,
                        &page_provenance,
                    )?,
                )?;
                let page_cursor = if may_cache {
                    let ttl = ttl_seconds(&effective_cache);
                    let mut entry = new_cache_entry(
                        page_cache_key.clone(),
                        page_hash,
                        args.source_id.clone(),
                        args.operation.clone(),
                        page_bytes,
                        ttl,
                    );
                    entry.observation_id = Some(page_observation_id.clone());
                    entry.provenance = Some(page_provenance);
                    let page_retained = store.put_entry(&page_cache_key, &entry)?;
                    if !page_retained {
                        prefetch_warnings.push(format!(
                            "page {} was not retained because the cache retention policy evicted it",
                            pages_fetched + 1
                        ));
                    }
                    // Mint a pc1_ cursor carrying page metadata (I9 fail-closed
                    // reuse; extra is observability only).
                    let mut cursor_extra = Map::new();
                    cursor_extra.insert("kind".to_string(), json!("page"));
                    cursor_extra.insert("page".to_string(), json!(pages_fetched + 1));
                    cursor_extra.insert(
                        "args".to_string(),
                        redacted_profile_args(&operation, &page_key_args),
                    );
                    page_retained
                        .then(|| {
                            store.create_cursor_with_extra(
                                &page_cache_key,
                                &args.source_id,
                                &args.operation,
                                &root_path,
                                RedactionPolicy::default().version,
                                ttl,
                                cursor_extra,
                            )
                        })
                        .transpose()?
                } else {
                    None
                };

                // Profile learning: each page's shape joins the operation's
                // output_shape (monotonic via the store, same as across calls).
                if !args.no_cache && !operation.effects.sensitive {
                    update_profile_from_call(
                        store,
                        &profile,
                        &args.operation,
                        &page_key_args,
                        page_payload.as_value(),
                        &page_shape,
                    )?;
                }

                page_summaries.push(json!({
                    "page": pages_fetched + 1,
                    "cache_key": page_cache_key,
                    "cursor": page_cursor,
                    "bytes": page_bytes,
                    "omitted_count": page_projection.omitted.len(),
                    "omitted_paths": omitted_paths,
                }));

                pages_fetched += 1;
                current_args = page_key_args;
                hints = page_call.pagination.clone();
            }
            if pages_fetched >= caps.max_pages {
                stop = prog_core::StopReason::PageCap;
            }

            // Reconcile the stop reason with reality: the next-page target is
            // computed from the LAST fetched page's hints. If no next page
            // remains, the chain ended naturally (NoMore) regardless of which
            // exit path the loop took (a page cap reached exactly at the end of
            // a finite chain is NoMore, not PageCap). This target is also the
            // resume point surfaced below when paused at a real cap.
            let resume_target = hints
                .as_ref()
                .and_then(|value| prog_core::next_args_from_hints(value, &current_args));
            if resume_target.is_none() {
                stop = prog_core::StopReason::NoMore;
            }

            // Continuation: when paused at a cap (not NoMore) with a concrete
            // next target, surface a resume NextAction. NoMore never surfaces one.
            if !stop.is_terminal()
                && let Some(resume) = resume_target
            {
                let reason = format!(
                    "pagination paused at {}; {} page(s) fetched; resume with the next page",
                    stop.as_str(),
                    pages_fetched
                );
                let next_action = match resume {
                    prog_core::PageTarget::Args(resume_args) => NextAction {
                        kind: "call".to_string(),
                        operation: Some(args.operation.clone()),
                        path: None,
                        reason: Some(reason),
                        extra: {
                            let mut map = Map::new();
                            map.insert("args".to_string(), resume_args);
                            map.insert(
                                "source_id".to_string(),
                                Value::String(args.source_id.clone()),
                            );
                            map
                        },
                        ..NextAction::default()
                    },
                    prog_core::PageTarget::Url(url) => NextAction {
                        kind: "call_url".to_string(),
                        operation: Some(args.operation.clone()),
                        path: None,
                        reason: Some(reason),
                        extra: {
                            let mut map = Map::new();
                            map.insert("url".to_string(), Value::String(url));
                            map.insert(
                                "source_id".to_string(),
                                Value::String(args.source_id.clone()),
                            );
                            map
                        },
                        ..NextAction::default()
                    },
                };
                envelope.next_actions.push(next_action);
            }

            envelope.warnings.extend(prefetch_warnings);
            envelope.extra.insert(
                "pagination".to_string(),
                json!({
                    "pages_fetched": pages_fetched,
                    "total_bytes": total_bytes,
                    "stop_reason": stop.as_str(),
                    "max_pages": caps.max_pages,
                    "merged_shape": serde_json::to_value(&merged_shape)?,
                    "pages": page_summaries,
                }),
            );
            // The pagination extra (uncapped `merged_shape` + per-page `pages[]`)
            // is appended AFTER `envelope_for_payload`'s budget loop, so re-enforce
            // `max_envelope_bytes` here: compact the pagination metadata if the
            // final envelope would otherwise exceed the budget (invariant I11).
            compact_pagination_extra_to_budget(&mut envelope)?;
            if may_cache
                && let Some(pagination) = envelope.extra.get("pagination").cloned()
                && let Some(mut entry) = store.get_entry(&cache_key)?
            {
                entry.extra.insert("pagination".to_string(), pagination);
                let pagination_next_actions = envelope
                    .next_actions
                    .iter()
                    .filter(|action| matches!(action.kind.as_str(), "call" | "call_url"))
                    .cloned()
                    .collect::<Vec<_>>();
                entry.extra.insert(
                    "pagination_next_actions".to_string(),
                    serde_json::to_value(pagination_next_actions)?,
                );
                store.put_entry(&cache_key, &entry)?;
            }
        } else {
            envelope.warnings.push(
                "--pages requested but the operation is not auto-pagination-safe \
                 (it is not read-only); fetched a single page"
                    .to_string(),
            );
        }
    }

    if received_error {
        envelope
            .extra
            .insert("received_error".to_string(), Value::Bool(true));
    }
    Ok(CallSourceResult {
        envelope,
        received_error,
    })
}

fn observe_artifact(
    store: &Store,
    lens_dir: &Path,
    args: &ObserveArgs,
) -> Result<DisclosureEnvelope> {
    let input = read_observation_input(args)?;
    let normalized = normalize_observation(&input.bytes, &input.mime)?;
    let lens = match &args.lens {
        Some(id) => {
            let lens = load_lens(lens_dir, id, "observe --lens")?;
            validate_lens_matches_observe(&lens, &input, &normalized)?;
            Some(lens)
        }
        None => None,
    };
    let redaction = RedactionPolicy::default();
    let redacted = RawPayload::new(normalized.payload).redact(&redaction);
    let redacted_paths = redacted.redacted_paths;
    let value_scan = redacted.value_scan;
    let payload = redacted.payload;
    let redacted_bytes = serde_json::to_vec(payload.as_value())?;
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
    let payload_hash = store.put_payload(&payload)?;
    let ttl: i64 = args
        .ttl_seconds
        .try_into()
        .map_err(|_| CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "ttl_seconds is too large".to_string(),
        })?;
    let mut entry = new_cache_entry(
        cache_key.clone(),
        payload_hash.clone(),
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
    let (availability, capture) = complete_capture(payload_bytes, true, !redacted_paths.is_empty());
    set_response_capture_budget(capture.budget.clone());
    let observation_id = record_capture(
        store,
        payload_hash.clone(),
        availability,
        capture,
        cache_key.clone(),
        "observe".to_string(),
        input.name.clone(),
        entry.provenance.clone(),
        Some(cache_key.clone()),
        !redacted_paths.is_empty(),
        Some(normalized.parser.id.to_string()),
        lens.as_ref(),
        None,
    )?;
    entry.observation_id = Some(observation_id.clone());
    let cache_retained = store.put_entry(&cache_key, &entry)?;

    let requested_view = SliceRequest {
        path: None,
        limit: None,
        depth: None,
        fields: Vec::new(),
        omit: Vec::new(),
        extra: Extra::new(),
    };
    let slice = match &lens {
        Some(lens) => lens_slice_request(lens, &requested_view)?,
        None => requested_view,
    };
    let root_path = slice.path.clone().unwrap_or_default();
    let cursor = cache_retained
        .then(|| {
            store.create_cursor_with_extra(
                &cache_key,
                "observe",
                &input.name,
                &root_path,
                RedactionPolicy::default().version,
                ttl,
                cursor_lens_extra(lens.as_ref()),
            )
        })
        .transpose()?;
    let mut warnings = normalized.warnings;
    if !redacted_paths.is_empty() {
        warnings.push(format!(
            "redacted {} sensitive path(s) before persistence",
            redacted_paths.len()
        ));
    }
    if !cache_retained {
        warnings.push(
            "cache retention policy evicted this payload before it could be reused".to_string(),
        );
    }
    envelope_for_payload(
        store,
        EnvelopeInput {
            value_scan: Some(value_scan),
            source_id: "observe".to_string(),
            operation: input.name.clone(),
            source_kind: Some("artifact".to_string()),
            payload,
            root_path,
            slice,
            payload_bytes,
            observation_id: Some(observation_id),
            provenance: entry.provenance.clone(),
            cache: Some(if cache_retained {
                cache_info(CacheStatus::Stored, &entry, Some(0))
            } else {
                CacheInfo {
                    status: CacheStatus::Skipped,
                    ttl_seconds: None,
                    expires_at: None,
                    age_seconds: None,
                }
            }),
            effects: None,
            auto_upgrade_audit: None,
            redacted_paths: redacted_paths.len(),
            cache_disabled_reason: None,
            warnings,
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: Vec::new(),
            observation_parser: Some(parser_metadata(&normalized.parser)),
            lens,
        },
        cursor,
    )
}

async fn run_command(store: &Store, lens_dir: &Path, args: &RunArgs) -> Result<RunEnvelopeResult> {
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
    let lens = match &args.lens {
        Some(id) => {
            let lens = load_lens(lens_dir, id, "run --lens")?;
            validate_lens_matches_run(&lens, &operation)?;
            Some(lens)
        }
        None => None,
    };
    let requested_view = SliceRequest {
        path: None,
        limit: None,
        depth: None,
        fields: Vec::new(),
        omit: Vec::new(),
        extra: Extra::new(),
    };
    let slice = match &lens {
        Some(lens) => lens_slice_request(lens, &requested_view)?,
        None => requested_view,
    };
    let root_path = slice.path.clone().unwrap_or_default();
    let redacted_argv = redact_run_argv(&argv);
    let cache_args = json!({
        "run_id": &run_id,
        "argv": argv,
        "cwd": cwd.to_string_lossy(),
        "started_at": started_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
    });
    let cache_key = Store::cache_key("run", &operation, &cache_args)?;
    let invocation_fingerprint = Store::cache_key(
        "run",
        &operation,
        &json!({
            "argv": argv,
            "cwd": cwd.to_string_lossy(),
        }),
    )?;

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
    let redacted = RawPayload::new(payload).redact(&redaction);
    let policy_redactions = redacted.redacted_paths;
    let value_scan = redacted.value_scan;
    let redacted_payload = redacted.payload;
    if let Some(path) = &args.out {
        write_private_file(
            path,
            &serde_json::to_vec_pretty(redacted_payload.as_value())?,
        )?;
    }
    let payload_hash = store.put_payload(&redacted_payload)?;
    let payload_bytes = json_len_u64(redacted_payload.as_value())?;
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
        payload_hash.clone(),
        "run".to_string(),
        operation.clone(),
        payload_bytes,
        ttl,
    );
    entry.provenance = Some(provenance.clone());
    let (availability, mut capture) = run_capture_completeness(
        &run.stdout,
        &run.stderr,
        payload_bytes,
        !policy_redactions.is_empty(),
    );
    capture.budget = capture_budget_for_run(args);
    set_response_capture_budget(capture.budget.clone());
    let observation_id = record_capture(
        store,
        payload_hash.clone(),
        availability,
        capture,
        invocation_fingerprint,
        "run".to_string(),
        operation.clone(),
        Some(provenance.clone()),
        Some(cache_key.clone()),
        !policy_redactions.is_empty(),
        None,
        lens.as_ref(),
        None,
    )?;
    entry.observation_id = Some(observation_id.clone());
    let cache_retained = store.put_entry(&cache_key, &entry)?;
    let cursor = cache_retained
        .then(|| {
            store.create_cursor_with_extra(
                &cache_key,
                "run",
                &operation,
                &root_path,
                RedactionPolicy::default().version,
                ttl,
                cursor_lens_extra(lens.as_ref()),
            )
        })
        .transpose()?;
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
    if !cache_retained {
        warnings.push(
            "cache retention policy evicted this payload before it could be reused".to_string(),
        );
    }
    let mut next_actions = run_next_actions(cursor.as_deref(), &failure_sections);
    next_actions.extend(pytest_target_actions(&args.command, &failure_sections));
    let envelope = envelope_for_payload(
        store,
        EnvelopeInput {
            value_scan: Some(value_scan),
            source_id: "run".to_string(),
            operation,
            source_kind: Some("cli".to_string()),
            payload: redacted_payload,
            root_path,
            slice,
            payload_bytes,
            observation_id: Some(observation_id),
            provenance: Some(provenance),
            cache: Some(if cache_retained {
                cache_info(CacheStatus::Stored, &entry, Some(0))
            } else {
                CacheInfo {
                    status: CacheStatus::Skipped,
                    ttl_seconds: None,
                    expires_at: None,
                    age_seconds: None,
                }
            }),
            effects: Some(run_effects()),
            auto_upgrade_audit: None,
            redacted_paths,
            cache_disabled_reason: None,
            warnings,
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: next_actions,
            observation_parser: None,
            lens,
        },
        cursor,
    )?;

    Ok(RunEnvelopeResult {
        envelope,
        exit_code: run_exit_code(&run.status),
    })
}

async fn run_recipe(
    store: &Store,
    lens_dir: &Path,
    args: &RecipeArgs,
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
                ttl_seconds: args.ttl_seconds,
            };
            (
                observe_artifact(store, lens_dir, &observe)?,
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
                command: command.clone(),
            };
            (
                run_command(store, lens_dir, &run).await?.envelope,
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
    compact_envelope_to_budget(&mut envelope)?;
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
            let _ = tokio::join!(
                finish_run_reader_or_abort(stdout_task),
                finish_run_reader_or_abort(stderr_task)
            );
            let mut combined = Vec::new();
            while let Ok(chunk) = rx.try_recv() {
                combined.push(chunk);
            }
            return Ok(RunProcessResult {
                stdout: empty_run_capture("stdout"),
                stderr: empty_run_capture("stderr"),
                combined,
                status: RunProcessStatus::TimedOut,
            });
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
    let _ = tokio::time::timeout(Duration::from_millis(100), child.wait()).await;
}

async fn finish_run_reader_or_abort(mut task: JoinHandle<std::io::Result<RunCapture>>) {
    tokio::select! {
        _ = &mut task => {}
        _ = tokio::time::sleep(Duration::from_millis(25)) => {
            task.abort();
            let _ = task.await;
        }
    }
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
            let (redacted, count) = prog_core::redact_sensitive_text(line);
            redactions = redactions.saturating_add(count);
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
    let allow_generic = !matches!(status, RunProcessStatus::Exited { success: true, .. });
    let mut sections = Vec::new();
    collect_failure_sections("stderr", &stderr.text, allow_generic, &mut sections);
    collect_failure_sections("stdout", &stdout.text, allow_generic, &mut sections);
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
    allow_generic: bool,
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
        } else if allow_generic
            && (lower.contains("error")
                || lower.contains("failed")
                || lower.contains("exception")
                || lower.contains("not found"))
        {
            Some(("generic", 60, "generic failure diagnostic"))
        } else {
            None
        };
        if let Some((kind, priority, reason)) = detected {
            let start = index.saturating_sub(2);
            let end = (index + 6).min(lines.len());
            if let Some(existing) = sections
                .iter_mut()
                .rev()
                .find(|section| section.stream == stream && section.line_end > start)
            {
                if priority > existing.priority {
                    existing.kind = kind;
                    existing.reason = reason.to_string();
                    existing.priority = priority;
                    existing.line_start = start + 1;
                    existing.line_end = end;
                    existing.lines = lines[start..end].to_vec();
                    continue;
                }
                existing.line_end = existing.line_end.max(end);
                existing.lines = lines[existing.line_start - 1..existing.line_end].to_vec();
                continue;
            }
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
            // `extra` is flattened into NextAction; avoid colliding with the
            // typed `kind` field and producing duplicate JSON object keys.
            extra.insert("section_kind".to_string(), json!(section.kind));
            NextAction {
                kind: "expand".to_string(),
                operation: None,
                path: Some(path),
                reason: Some(section.reason.clone()),
                argv: Some(vec![
                    "prog".to_string(),
                    "expand".to_string(),
                    cursor.to_string(),
                    "--path".to_string(),
                    format!("/failure_sections/{index}"),
                ]),
                scope: Some("failure_section".to_string()),
                exactness: Some(prog_core::ActionExactness::Exact),
                derived_from: Some("run.failure_section".to_string()),
                extra,
                ..NextAction::default()
            }
        })
        .collect()
}

fn pytest_target_actions(command: &[String], sections: &[RunFailureSection]) -> Vec<NextAction> {
    // This is deliberately narrower than command equivalence: only the
    // literal pytest executable and a complete node ID printed by pytest are
    // evidence enough for an exact argv recommendation.
    if command.first().map(String::as_str) != Some("pytest") {
        return Vec::new();
    }
    let mut node_ids = BTreeSet::new();
    for section in sections {
        for line in &section.lines {
            let Some(candidate) = line.trim_start().strip_prefix("FAILED ") else {
                continue;
            };
            let Some(node_id) = candidate.split_whitespace().next() else {
                continue;
            };
            if node_id.contains("::")
                && !node_id.starts_with('-')
                && !node_id.contains('\0')
                && !node_id.contains(char::is_whitespace)
            {
                node_ids.insert(node_id.to_string());
            }
        }
    }
    node_ids
        .into_iter()
        .take(3)
        .map(|node_id| NextAction {
            kind: "rerun".to_string(),
            operation: None,
            path: None,
            reason: Some("rerun this exact pytest node ID for a focused diagnostic".to_string()),
            argv: Some(vec!["pytest".to_string(), node_id.clone()]),
            scope: Some("target_test".to_string()),
            exactness: Some(prog_core::ActionExactness::Exact),
            derived_from: Some("pytest.failed_node_id".to_string()),
            does_not_satisfy: vec!["affected_suite".to_string(), "regression_suite".to_string()],
            cwd: None,
            extra: Extra::new(),
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
            // Inline form first: `--access-token=JWT` or `--access-token:JWT`.
            // Checking this before the bare-flag path ensures the secret value
            // embedded in the same element is redacted rather than leaked.
            if let Some(redacted) = redact_inline_secret(arg) {
                return redacted;
            }
            // Bare flag form: `--access-token` marks the *next* element as the
            // value to redact.
            if is_sensitive_flag(arg) {
                redact_next = true;
                return arg.clone();
            }
            redact_observed_text(arg)
        })
        .collect()
}

fn is_sensitive_flag(arg: &str) -> bool {
    let trimmed = arg.trim_start_matches('-');
    prog_core::is_sensitive_name(trimmed)
}

/// If `arg` is an inline `name<sep>value` whose name is sensitive, return the
/// redacted form; otherwise `None`. Catches compound flag names like
/// `--access-token=...` and `--passwd=...` that the bare-flag path would miss.
fn redact_inline_secret(arg: &str) -> Option<String> {
    for separator in ['=', ':'] {
        if let Some((name, value)) = arg.split_once(separator)
            && !value.is_empty()
            && prog_core::is_sensitive_name(name.trim_start_matches('-'))
        {
            return Some(format!("{name}{separator}[REDACTED:run_arg_secret]"));
        }
    }
    None
}

fn init_integration(args: &InitArgs) -> Result<InitReport> {
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

fn prog_run_hook(hook_dir: &str) -> String {
    format!(
        r#"#!/usr/bin/env sh
set -eu

if [ "$#" -eq 0 ]; then
  echo "usage: {hook_dir}/prog-run.sh <command...>" >&2
  exit 64
fi

exec prog run -- "$@"
"#
    )
}

fn hook_readme(agent: AgentKind, hook_dir: &str) -> String {
    format!(
        r#"# prog {agent} hooks

This project-local integration keeps `prog` usable without MCP server mode.

Use the wrapper for noisy commands:

```bash
    {hook_dir}/prog-run.sh cargo test
```

The wrapper returns a bounded `DisclosureEnvelope`. Use its ranked findings or
run `prog inspect <cursor> --goal <goal>`, then cite exact evidence with
`prog evidence <cursor> --path <json-pointer>`.

For shell aliases or editor tasks, wire the command directly rather than
rewriting user commands globally:

```sh
prog_run() {{
  {hook_dir}/prog-run.sh "$@"
}}
```

MCP is optional compatibility. Prefer the CLI, this skill, and explicit hooks
unless the host agent already has a reliable MCP client.
"#,
        agent = agent.as_str()
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

fn agent_init_next_steps(agent: AgentKind) -> Vec<String> {
    let (skill_path, hook_dir) = agent_integration_paths(agent);
    vec![
        format!("Review {skill_path} before relying on the generated integration"),
        format!("Route noisy commands through {hook_dir}/prog-run.sh <command...>"),
        "Use prog inspect <cursor> --goal <goal>, then prog evidence <cursor> --path <path>"
            .to_string(),
    ]
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

fn cost_report(args: &CostArgs) -> Result<CostReport> {
    if args.repeated_inspections == 0 {
        return Err(CoreError::BadArgs {
            operation: "cost".to_string(),
            reason: "--repeated-inspections must be at least 1".to_string(),
        });
    }
    let (profile, profile_warnings) = read_model_cost_profile(&args.model_profile)?;
    let input_price = profile
        .input_price_per_million_tokens
        .ok_or_else(|| CoreError::BadArgs {
            operation: "cost".to_string(),
            reason: "model profile must include input_price_per_million_tokens".to_string(),
        })?;
    let output_price =
        profile
            .output_price_per_million_tokens
            .ok_or_else(|| CoreError::BadArgs {
                operation: "cost".to_string(),
                reason: "model profile must include output_price_per_million_tokens".to_string(),
            })?;
    validate_nonnegative_price(input_price, "input_price_per_million_tokens")?;
    validate_nonnegative_price(output_price, "output_price_per_million_tokens")?;
    if profile.context_window_tokens == 0 {
        return Err(CoreError::BadArgs {
            operation: "cost".to_string(),
            reason: "model profile context_window_tokens must be greater than 0".to_string(),
        });
    }

    let raw = std::fs::read(&args.raw_file).map_err(|error| CoreError::BadArgs {
        operation: "cost".to_string(),
        reason: format!(
            "raw file '{}' could not be read: {error}",
            args.raw_file.to_string_lossy()
        ),
    })?;
    let mime = args
        .mime
        .clone()
        .unwrap_or_else(|| sniff_mime_from_bytes(&raw).to_string());
    let raw_bytes = raw.len().try_into().unwrap_or(u64::MAX);
    let raw_tokens = approx_tokens_for_bytes(raw_bytes);
    let flow = estimate_prog_cost_flow(&raw, &mime, &args.expand_paths)?;
    let output_tokens = args.estimated_output_tokens;
    let raw_single_cost = token_cost(raw_tokens, output_tokens, input_price, output_price);
    let truncation_tokens = raw_tokens.min(profile.context_window_tokens);
    let observe_tokens = flow.observe_tokens;
    let targeted_tokens = flow
        .observe_tokens
        .saturating_add(flow.paths_tokens)
        .saturating_add(flow.expansion_tokens);
    let repeated_input_tokens = flow.observe_tokens.saturating_add(
        args.repeated_inspections
            .saturating_mul(flow.paths_tokens.saturating_add(flow.expansion_tokens)),
    );
    let repeated_raw_tokens = raw_tokens.saturating_mul(args.repeated_inspections);
    let repeated_raw_cost = token_cost(
        repeated_raw_tokens,
        output_tokens.saturating_mul(args.repeated_inspections),
        input_price,
        output_price,
    );

    let scenarios = vec![
        cost_scenario(CostScenarioInput {
            name: "raw_payload",
            input_tokens: raw_tokens,
            output_tokens,
            baseline_input_tokens: raw_tokens,
            baseline_cost: raw_single_cost,
            input_price,
            output_price,
            context_window_tokens: profile.context_window_tokens,
            lossless: true,
            notes: vec!["places the complete raw artifact in model context".to_string()],
        }),
        cost_scenario(CostScenarioInput {
            name: "simple_truncation",
            input_tokens: truncation_tokens,
            output_tokens,
            baseline_input_tokens: raw_tokens,
            baseline_cost: raw_single_cost,
            input_price,
            output_price,
            context_window_tokens: profile.context_window_tokens,
            lossless: raw_tokens <= profile.context_window_tokens,
            notes: vec![
                "baseline for clipping to the model context window; may drop needed evidence"
                    .to_string(),
            ],
        }),
        cost_scenario(CostScenarioInput {
            name: "prog_observe_only",
            input_tokens: observe_tokens,
            output_tokens,
            baseline_input_tokens: raw_tokens,
            baseline_cost: raw_single_cost,
            input_price,
            output_price,
            context_window_tokens: profile.context_window_tokens,
            lossless: false,
            notes: vec![
                "bounded first view only; full redacted artifact remains cursor-backed".to_string(),
            ],
        }),
        cost_scenario(CostScenarioInput {
            name: "prog_observe_paths_expand",
            input_tokens: targeted_tokens,
            output_tokens,
            baseline_input_tokens: raw_tokens,
            baseline_cost: raw_single_cost,
            input_price,
            output_price,
            context_window_tokens: profile.context_window_tokens,
            lossless: !args.expand_paths.is_empty(),
            notes: vec![
                "bounded observation plus path listing and requested exact expansions".to_string(),
            ],
        }),
        cost_scenario(CostScenarioInput {
            name: "repeated_cache_hits",
            input_tokens: repeated_input_tokens,
            output_tokens: output_tokens.saturating_mul(args.repeated_inspections),
            baseline_input_tokens: repeated_raw_tokens,
            baseline_cost: repeated_raw_cost,
            input_price,
            output_price,
            context_window_tokens: profile.context_window_tokens,
            lossless: !args.expand_paths.is_empty(),
            notes: vec![format!(
                "models {} repeated inspections as one capture plus cached paths/expansions",
                args.repeated_inspections
            )],
        }),
    ];

    let mut warnings = vec![
        "model pricing is profile-driven; refresh model_profile pricing before making budget decisions".to_string(),
    ];
    warnings.extend(profile_warnings);
    warnings.extend(flow.warnings);
    if raw_tokens < 512 {
        warnings.push(
            "tiny payload counterexample: prog overhead may exceed raw context cost".to_string(),
        );
    }
    if output_tokens > targeted_tokens.max(1) {
        warnings.push(
            "estimated output tokens dominate this scenario; input-token savings may not control total cost".to_string(),
        );
    }
    if args.expand_paths.is_empty() {
        warnings.push(
            "no --expand-path was provided; targeted expansion scenario includes path discovery only".to_string(),
        );
    }

    Ok(CostReport {
        schema: "prog.cost_report",
        model: CostModelSummary {
            model: profile.model,
            input_price_per_million_tokens: input_price,
            output_price_per_million_tokens: output_price,
            context_window_tokens: profile.context_window_tokens,
            cache_read_price_per_million_tokens: profile.cache_read_price_per_million_tokens,
            cache_write_price_per_million_tokens: profile.cache_write_price_per_million_tokens,
            pricing_source: profile.pricing_source,
            priced_at: profile.priced_at,
        },
        input: CostInputSummary {
            raw_file: args.raw_file.to_string_lossy().to_string(),
            raw_bytes,
            raw_tokens,
            mime,
            expand_paths: args.expand_paths.clone(),
            estimated_output_tokens: output_tokens,
            repeated_inspections: args.repeated_inspections,
        },
        scenarios,
        warnings,
        counterexamples: vec![
            "tiny payloads can be cheaper to send raw".to_string(),
            "one expansion can reveal nearly the entire artifact".to_string(),
            "large expected model outputs can dominate total cost".to_string(),
            "low-cost local models may make latency more important than token spend".to_string(),
        ],
    })
}

struct CostScenarioInput {
    name: &'static str,
    input_tokens: u64,
    output_tokens: u64,
    baseline_input_tokens: u64,
    baseline_cost: f64,
    input_price: f64,
    output_price: f64,
    context_window_tokens: u64,
    lossless: bool,
    notes: Vec<String>,
}

fn cost_scenario(input: CostScenarioInput) -> CostScenario {
    let total = token_cost(
        input.input_tokens,
        input.output_tokens,
        input.input_price,
        input.output_price,
    );
    CostScenario {
        name: input.name,
        input_tokens: input.input_tokens,
        output_tokens: input.output_tokens,
        total_estimated_cost_usd: total,
        baseline_input_tokens: input.baseline_input_tokens,
        baseline_estimated_cost_usd: input.baseline_cost,
        savings_ratio: ratio(input.baseline_cost, total),
        fits_context: input.input_tokens <= input.context_window_tokens,
        lossless: input.lossless,
        notes: input.notes,
    }
}

fn read_model_cost_profile(path: &Path) -> Result<(ModelCostProfile, Vec<String>)> {
    let raw = std::fs::read_to_string(path).map_err(|error| CoreError::BadArgs {
        operation: "cost".to_string(),
        reason: format!(
            "model profile '{}' could not be read: {error}",
            path.to_string_lossy()
        ),
    })?;
    let profile: ModelCostProfile =
        serde_json::from_str(&raw).map_err(|error| CoreError::BadArgs {
            operation: "cost".to_string(),
            reason: format!(
                "model profile '{}' must be valid JSON: {error}",
                path.to_string_lossy()
            ),
        })?;
    let mut warnings = Vec::new();
    if profile.schema.as_deref() != Some("prog.model_profile") {
        warnings.push("model profile schema should be prog.model_profile".to_string());
    }
    if profile.pricing_source.is_none() || profile.priced_at.is_none() {
        warnings
            .push("model profile should include pricing_source and priced_at metadata".to_string());
    }
    Ok((profile, warnings))
}

fn validate_nonnegative_price(price: f64, field: &str) -> Result<()> {
    if price.is_finite() && price >= 0.0 {
        Ok(())
    } else {
        Err(CoreError::BadArgs {
            operation: "cost".to_string(),
            reason: format!("model profile {field} must be a non-negative finite number"),
        })
    }
}

fn estimate_prog_cost_flow(
    raw: &[u8],
    mime: &str,
    expand_paths: &[String],
) -> Result<CostFlowEstimate> {
    let normalized = normalize_observation(raw, mime)?;
    let redacted = RawPayload::new(normalized.payload).redact(&RedactionPolicy::default());
    let redacted_paths = redacted.redacted_paths;
    let redacted = redacted.payload;
    let redacted_bytes = canonical_json(redacted.as_value())?;
    let payload_bytes = redacted_bytes.len().try_into().unwrap_or(u64::MAX);
    let projection = project(redacted.as_value(), &PreviewPolicy::default(), "");
    let observe_envelope = json!({
        "schema": DISCLOSURE_SCHEMA,
        "source_id": "observe",
        "operation": "cost",
        "summary": {
            "kind": value_kind(redacted.as_value()),
            "payload_bytes": payload_bytes,
            "approx_tokens": approx_tokens_for_bytes(payload_bytes)
        },
        "data_preview": projection.preview,
        "omitted": projection.omitted,
        "cursor": "pc1_cost_example",
        "warnings": normalized.warnings,
        "redacted_paths": redacted_paths.len()
    });
    let observe_tokens = approx_tokens_for_json(&observe_envelope)?;

    let mut paths = Vec::new();
    let truncated = collect_paths(redacted.as_value(), "", 6, 200, &mut paths);
    let root_projection = project(redacted.as_value(), &PreviewPolicy::default(), "");
    annotate_path_omissions(&mut paths, &root_projection.omitted);
    append_missing_omitted_paths(&mut paths, &root_projection.omitted, 200);
    let paths_doc = json!({
        "schema": DISCLOSURE_SCHEMA,
        "cursor": "pc1_cost_example",
        "prefix": "",
        "paths": paths,
        "omitted": root_projection.omitted,
        "truncated": truncated
    });
    let paths_tokens = approx_tokens_for_json(&paths_doc)?;

    let mut expansion_tokens = 0u64;
    for path in expand_paths {
        let slice = SliceRequest {
            path: Some(path.clone()),
            limit: None,
            depth: None,
            fields: Vec::new(),
            omit: Vec::new(),
            extra: Extra::new(),
        };
        let scoped = ScopedSlice::root(slice)?;
        let (target_path, selected) = slice_value(&redacted, &scoped)?;
        let expansion = project(&selected, &PreviewPolicy::default(), &target_path);
        let expansion_envelope = json!({
            "schema": DISCLOSURE_SCHEMA,
            "source_id": "observe",
            "operation": "cost",
            "data_preview": expansion.preview,
            "omitted": expansion.omitted,
            "cursor": "pc1_cost_example",
            "evidence_ref": {
                "schema": "prog.evidence_ref",
                "path": target_path
            }
        });
        expansion_tokens =
            expansion_tokens.saturating_add(approx_tokens_for_json(&expansion_envelope)?);
    }

    let mut warnings = Vec::new();
    if truncated {
        warnings.push("estimated path listing reached the default path limit".to_string());
    }
    if !redacted_paths.is_empty() {
        warnings.push(format!(
            "cost estimate uses redacted payload; {} sensitive path(s) were removed before estimates",
            redacted_paths.len()
        ));
    }

    Ok(CostFlowEstimate {
        observe_tokens,
        paths_tokens,
        expansion_tokens,
        warnings,
    })
}

fn approx_tokens_for_json(value: &Value) -> Result<u64> {
    let bytes = serde_json::to_vec(value)?
        .len()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(approx_tokens_for_bytes(bytes))
}

fn approx_tokens_for_bytes(bytes: u64) -> u64 {
    bytes.saturating_add(3) / 4
}

fn token_cost(input_tokens: u64, output_tokens: u64, input_price: f64, output_price: f64) -> f64 {
    round_usd(
        (input_tokens as f64 * input_price / 1_000_000.0)
            + (output_tokens as f64 * output_price / 1_000_000.0),
    )
}

fn ratio(baseline: f64, candidate: f64) -> f64 {
    if candidate <= 0.0 {
        return f64::INFINITY;
    }
    round_ratio(baseline / candidate)
}

fn round_usd(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn round_ratio(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn evidence_ref(input: EvidenceRefInput<'_>) -> EvidenceRef {
    let omitted_in_scope = input
        .omitted
        .iter()
        .filter(|region| omission_intersects_path(input.path, &region.path))
        .collect::<Vec<_>>();
    let availability = input
        .observation
        .map(|observation| observation.availability)
        .unwrap_or(EvidenceAvailability::Unavailable);
    let capture = input
        .observation
        .map(|observation| observation.capture.clone())
        .unwrap_or_else(|| CaptureCompleteness::unavailable(0));
    let redacted = input.redacted_paths > 0
        || value_contains_redaction(input.value)
        || omitted_in_scope
            .iter()
            .any(|region| region.reason == OmissionReason::Redacted);
    let lossy = omitted_in_scope
        .iter()
        .any(|region| region.reason != OmissionReason::Redacted);
    let redacted_slice_sha256 = canonical_json(input.value)
        .ok()
        .map(|bytes| hex_sha256(bytes.as_slice()));
    let cache_status = input.cache.map(|cache| cache.status);
    let age_seconds = input.cache.and_then(|cache| cache.age_seconds);
    let stale = cache_is_stale(input.cache);
    EvidenceRef {
        schema: "prog.evidence_ref".to_string(),
        source_id: input.source_id.to_string(),
        operation: input.operation.to_string(),
        cursor: input.cursor.map(str::to_string),
        path: input.path.to_string(),
        uri: input
            .cursor
            .map(|cursor| format!("prog://{cursor}#{}", input.path)),
        captured_at: input
            .provenance
            .map(|provenance| provenance.captured_at.clone()),
        cache_status,
        age_seconds,
        expires_at: input.cache.and_then(|cache| cache.expires_at.clone()),
        stale,
        availability,
        capture,
        redacted,
        lossy,
        redacted_slice_sha256,
        extra: Extra::new(),
    }
}

fn omission_intersects_path(path: &str, omitted_path: &str) -> bool {
    prog_core::pointer::is_within(path, omitted_path).unwrap_or(false)
        || prog_core::pointer::is_within(omitted_path, path).unwrap_or(false)
}

fn value_contains_redaction(value: &Value) -> bool {
    match value {
        Value::String(value) => value.contains("[REDACTED:"),
        Value::Array(values) => values.iter().any(value_contains_redaction),
        Value::Object(map) => map.values().any(value_contains_redaction),
        _ => false,
    }
}

struct CursorContext {
    record: ValidatedCursor,
    entry: CacheEntryMeta,
    observation: Option<prog_core::ObservationRecord>,
    payload: PersistedPayload,
    target_path: String,
    age_seconds: u64,
    cache: CacheInfo,
}

fn cursor_context(store: &Store, cursor: &str, requested_path: &str) -> Result<CursorContext> {
    let record = store.get_cursor(cursor, RedactionPolicy::default().version)?;
    let entry = store
        .get_entry(&record.cache_key)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let observation = record
        .observation_id
        .as_deref()
        .map(|observation_id| store.get_observation(observation_id))
        .transpose()?
        .flatten();
    let payload = store
        .get_payload(&entry.payload_hash)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let target_path = if requested_path.is_empty() {
        record.root_path.clone()
    } else {
        requested_path.to_string()
    };
    let scoped = ScopedSlice::new(
        ExpansionScope::from_cursor(&record)?,
        SliceRequest {
            path: Some(target_path.clone()),
            limit: None,
            depth: None,
            fields: Vec::new(),
            omit: Vec::new(),
            extra: Extra::new(),
        },
    )?;
    let target_path = scoped.target_path().as_str().to_string();
    if prog_core::pointer::get(payload.as_value(), &target_path)?.is_none() {
        return Err(CoreError::PathNotFound {
            path: target_path,
            hint: prog_core::pointer::siblings_hint(payload.as_value(), requested_path),
        });
    }
    let age_seconds = age_seconds(&entry.created_at)?;
    let cache = cache_info(CacheStatus::Hit, &entry, Some(age_seconds));
    Ok(CursorContext {
        record,
        entry,
        observation,
        payload,
        target_path,
        age_seconds,
        cache,
    })
}

fn inspect_cursor(store: &Store, lens_dir: &Path, args: &InspectArgs) -> Result<InspectResponse> {
    if args.goal.trim().is_empty() {
        return Err(CoreError::BadArgs {
            operation: "inspect".to_string(),
            reason: "--goal must not be empty".to_string(),
        });
    }
    if args.limit > 100 {
        return Err(CoreError::BadArgs {
            operation: "inspect".to_string(),
            reason: "--limit must be at most 100".to_string(),
        });
    }
    let context = cursor_context(store, &args.cursor, &args.path)?;
    let request = InspectRequest::builder(args.cursor.clone())
        .goal(args.goal.clone())
        .scope_path(context.target_path.clone())
        .limit(args.limit.saturating_mul(4).min(100))
        .hints(CommandHintConfig::NAV_ALL)
        .build();
    let mut response = build_inspect_response(context.payload.as_value(), &request)?;
    let lens = cursor_lens(lens_dir, context.record.record(), &mut response.warnings);
    let options = FindingOptions {
        goal: Some(args.goal.clone()),
        cursor: Some(args.cursor.clone()),
        scope_path: Some(context.target_path.clone()),
        limit: args.limit.saturating_mul(4).min(100),
        hints: CommandHintConfig::NAV_ALL,
    };
    response.findings =
        ranked_findings_with_lens(context.payload.as_value(), &options, lens.as_ref())?;
    if let Some(kind) = &args.kind {
        let kind = normalize_finding_kind(kind);
        response
            .findings
            .retain(|finding| finding_matches_kind(finding, &kind));
    }
    response.findings.truncate(args.limit);
    for (index, finding) in response.findings.iter_mut().enumerate() {
        finding.rank = index as u64 + 1;
        if let Some(value) = prog_core::pointer::get(context.payload.as_value(), &finding.path)? {
            finding.evidence_ref = Some(evidence_ref(EvidenceRefInput {
                source_id: &context.record.source_id,
                operation: &context.record.operation,
                cursor: Some(&args.cursor),
                path: &finding.path,
                value,
                observation: context.observation.as_ref(),
                provenance: context.entry.provenance.as_ref(),
                cache: Some(&context.cache),
                omitted: &[],
                redacted_paths: 0,
            }));
        }
    }
    response.cache = Some(context.cache);
    let scoped_value = prog_core::pointer::get(context.payload.as_value(), &context.target_path)?
        .expect("cursor_context validated the target path");
    if exceeds_node_budget(scoped_value, 10_000, 64) {
        response.omitted.push(OmittedRegion {
            path: context.target_path.clone(),
            reason: OmissionReason::NodeBudget,
            detail: Some("inspect finding traversal reached the 10000-node budget".to_string()),
            extra: Extra::new(),
        });
        response.warnings.push(
            "inspect findings are partial; narrow --path to traverse a smaller subtree".to_string(),
        );
    }
    if context.age_seconds > 0 {
        response.warnings.push(format!(
            "cached payload age_seconds={}; inspect did not contact upstream",
            context.age_seconds
        ));
    }
    bound_inspect_response(&mut response)?;
    Ok(response)
}

fn evidence_cursor(store: &Store, lens_dir: &Path, args: &EvidenceArgs) -> Result<EvidenceBlock> {
    let context = cursor_context(store, &args.cursor, &args.path)?;
    let value = prog_core::pointer::get(context.payload.as_value(), &context.target_path)?
        .expect("cursor_context validated the target path");
    let mut block = evidence_block(
        context.payload.as_value(),
        args.cursor.clone(),
        &context.target_path,
    )?;
    let mut lens_warnings = Vec::new();
    let lens = cursor_lens(lens_dir, context.record.record(), &mut lens_warnings);
    if let Some(finding) = ranked_findings_with_lens(
        context.payload.as_value(),
        &FindingOptions {
            cursor: Some(args.cursor.clone()),
            scope_path: Some(context.target_path.clone()),
            limit: 20,
            hints: CommandHintConfig::NAV_ALL,
            ..FindingOptions::default()
        },
        lens.as_ref(),
    )?
    .into_iter()
    .find(|finding| finding.path == context.target_path)
    {
        block.kind = finding.kind;
        if let Some(lens_id) = finding.lens_id {
            block.extra.insert("lens_id".to_string(), json!(lens_id));
        }
    }
    block.warnings.extend(lens_warnings);
    block.evidence_ref = Some(evidence_ref(EvidenceRefInput {
        source_id: &context.record.source_id,
        operation: &context.record.operation,
        cursor: Some(&args.cursor),
        path: &context.target_path,
        value,
        observation: context.observation.as_ref(),
        provenance: context.entry.provenance.as_ref(),
        cache: Some(&context.cache),
        omitted: &[],
        redacted_paths: 0,
    }));
    block.source_command = source_command_from_provenance(context.entry.provenance.as_ref());
    block.provenance = context.entry.provenance;
    block.cache = Some(context.cache);
    if context.age_seconds > 0 {
        block.warnings.push(format!(
            "cached payload age_seconds={}; evidence did not contact upstream",
            context.age_seconds
        ));
    }
    bound_evidence_block(&mut block)?;
    Ok(block)
}

#[allow(clippy::too_many_arguments)]
fn search_cursor(
    store: &Store,
    lens_dir: &Path,
    cursor: &str,
    query: Option<String>,
    kind: Option<String>,
    path: &str,
    limit: usize,
    case_sensitive: bool,
    regex: bool,
) -> Result<SearchResponse> {
    if limit > 200 {
        return Err(CoreError::BadArgs {
            operation: "search".to_string(),
            reason: "--limit must be at most 200".to_string(),
        });
    }
    let context = cursor_context(store, cursor, path)?;
    let mut lens_warnings = Vec::new();
    let lens = cursor_lens(lens_dir, context.record.record(), &mut lens_warnings);
    let mut response = search_payload_with_lens(
        context.payload.as_value(),
        cursor.to_string(),
        &SearchOptions {
            query,
            kind,
            scope_path: Some(context.target_path),
            limit,
            case_sensitive,
            regex,
            ..SearchOptions::default()
        },
        lens.as_ref(),
    )?;
    response.warnings.extend(lens_warnings);
    response.cache = Some(context.cache);
    if context.age_seconds > 0 {
        response.warnings.push(format!(
            "cached payload age_seconds={}; search did not contact upstream",
            context.age_seconds
        ));
    }
    bound_search_response(&mut response)?;
    Ok(response)
}

fn cursor_lens(
    lens_dir: &Path,
    record: &prog_core::CursorRecord,
    warnings: &mut Vec<String>,
) -> Option<LensManifest> {
    let id = record.extra.get("lens_id").and_then(Value::as_str)?;
    match load_lens(lens_dir, id, "inspect") {
        Ok(lens) => Some(lens),
        Err(error) => {
            warnings.push(format!(
                "lens '{id}' recorded on the cursor could not be loaded; used generic findings: {error}"
            ));
            None
        }
    }
}

fn normalize_finding_kind(kind: &str) -> String {
    kind.trim().to_ascii_lowercase().replace('-', "_")
}

fn finding_matches_kind(finding: &prog_core::Finding, kind: &str) -> bool {
    normalize_finding_kind(&finding.kind) == kind
        || (kind == "error"
            && (finding.severity.as_deref() == Some("error")
                || finding.kind.contains("error")
                || finding.kind.contains("failure")
                || finding.kind.contains("exception")
                || finding.kind.contains("panic")
                || finding.kind.contains("timeout")))
        || (kind == "warning" && finding.severity.as_deref() == Some("warning"))
}

fn source_command_from_provenance(provenance: Option<&CallProvenance>) -> Option<String> {
    let argv = provenance?
        .extra
        .get("run")?
        .get("argv")?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?;
    Some(
        argv.into_iter()
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn bound_inspect_response(response: &mut InspectResponse) -> Result<()> {
    let budget = response_budget_bytes();
    let original_len = response.findings.len();
    while serde_json::to_vec(response)?.len() > budget && !response.findings.is_empty() {
        response.findings.pop();
    }
    if response.findings.len() < original_len
        && response
            .omitted
            .iter()
            .all(|region| region.reason != OmissionReason::NodeBudget)
    {
        response.omitted.push(OmittedRegion {
            path: response.scope_path.clone().unwrap_or_default(),
            reason: OmissionReason::NodeBudget,
            detail: Some("inspect findings were compacted to the disclosure budget".to_string()),
            extra: Extra::new(),
        });
        response.warnings.push(format!(
            "inspect retained {} of {original_len} findings to enforce the disclosure budget",
            response.findings.len()
        ));
    }
    while serde_json::to_vec(response)?.len() > budget && !response.findings.is_empty() {
        response.findings.pop();
    }
    if serde_json::to_vec(response)?.len() > budget {
        response.warnings.truncate(1);
        response.omitted.truncate(1);
    }
    Ok(())
}

fn bound_search_response(response: &mut SearchResponse) -> Result<()> {
    let budget = response_budget_bytes();
    let original_len = response.hits.len();
    while serde_json::to_vec(response)?.len() > budget && !response.hits.is_empty() {
        response.hits.pop();
    }
    if response.hits.len() < original_len {
        response.omitted.push(OmittedRegion {
            path: response.scope_path.clone().unwrap_or_default(),
            reason: OmissionReason::NodeBudget,
            detail: Some("search hits were compacted to the disclosure budget".to_string()),
            extra: Extra::new(),
        });
        response.warnings.push(format!(
            "search retained {} of {original_len} hits to enforce the disclosure budget",
            response.hits.len()
        ));
    }
    while serde_json::to_vec(response)?.len() > budget && !response.hits.is_empty() {
        response.hits.pop();
    }
    for (index, hit) in response.hits.iter_mut().enumerate() {
        hit.rank = index as u64 + 1;
    }
    Ok(())
}

fn bound_evidence_block(block: &mut EvidenceBlock) -> Result<()> {
    let budget = response_budget_bytes();
    if serde_json::to_vec(block)?.len() > budget {
        block.citations.truncate(1);
        if let Some(citation) = block.citations.first_mut() {
            citation.excerpt = json!("excerpt compacted; use commands.expand");
        }
        block.excerpt = json!("excerpt compacted; use commands.expand");
        block.warnings.truncate(2);
        block
            .warnings
            .push("evidence excerpt compacted to the disclosure budget".to_string());
    }
    if serde_json::to_vec(block)?.len() > budget {
        block.provenance = None;
        block.citations.clear();
        block.extra.clear();
        block.warnings.truncate(1);
        block.summary = block.summary.chars().take(180).collect();
        block.commands.inspect = None;
        block.commands.search = None;
        block.commands.evidence = None;
    }
    Ok(())
}

fn exceeds_node_budget(value: &Value, max_nodes: usize, max_depth: usize) -> bool {
    fn visit(
        value: &Value,
        depth: usize,
        max_depth: usize,
        max_nodes: usize,
        visited: &mut usize,
    ) -> bool {
        if depth > max_depth || *visited >= max_nodes {
            return true;
        }
        *visited += 1;
        match value {
            Value::Array(items) => items
                .iter()
                .any(|item| visit(item, depth + 1, max_depth, max_nodes, visited)),
            Value::Object(map) => map
                .values()
                .any(|item| visit(item, depth + 1, max_depth, max_nodes, visited)),
            _ => false,
        }
    }
    let mut visited = 0usize;
    visit(value, 0, max_depth, max_nodes, &mut visited)
}

fn record_envelope_event(store: &Store, envelope: &mut DisclosureEnvelope, kind: &str) {
    if let Some(observation_id) = envelope
        .observation
        .as_ref()
        .and_then(|observation| observation.observation_id.as_deref())
        && let Ok(Some(subject)) = store.get_observation(observation_id)
        && let Ok(Some(baseline)) =
            store.latest_session_predecessor(&subject.invocation_fingerprint, observation_id)
        && let Ok(mut delta) =
            compare_observation_ids(store, &baseline.observation_id, &subject.observation_id)
    {
        delta.findings.truncate(10);
        envelope.extra.insert(
            "changes_since".to_string(),
            serde_json::to_value(delta).unwrap_or(Value::Null),
        );
        if let Err(error) = compact_envelope_to_budget(envelope) {
            envelope.extra.remove("changes_since");
            envelope.warnings.push(format!(
                "automatic changes_since was omitted because it could not fit the envelope budget: {error}"
            ));
        }
    }
    let evidence_ref = envelope
        .extra
        .get("evidence_ref")
        .and_then(|reference| reference.get("uri"))
        .and_then(Value::as_str);
    let mut extra = Extra::new();
    if let Some(observation_id) = envelope
        .observation
        .as_ref()
        .and_then(|observation| observation.observation_id.as_ref())
    {
        extra.insert("observation_id".to_string(), json!(observation_id));
    }
    let _ = store.record_session_event(NewSessionEvent {
        kind: kind.to_string(),
        cursor: envelope.cursor.clone(),
        evidence_ref: evidence_ref.map(str::to_string),
        summary: Some(format!(
            "{} {} byte payload; {} finding(s)",
            envelope.summary.kind,
            envelope.summary.payload_bytes,
            envelope.findings.len()
        )),
        extra,
        ..NewSessionEvent::default()
    });
}

fn record_navigation_event(
    store: &Store,
    kind: &str,
    cursor: Option<&str>,
    path: Option<&str>,
    evidence_ref: Option<&str>,
    summary: Option<String>,
) {
    let _ = store.record_session_event(NewSessionEvent {
        kind: kind.to_string(),
        cursor: cursor.map(str::to_string),
        path: path.map(str::to_string),
        evidence_ref: evidence_ref.map(str::to_string),
        summary,
        extra: Extra::new(),
    });
}

fn session_show(store: &Store, args: &SessionShowArgs) -> Result<prog_core::SessionTrail> {
    let mut trail = store
        .get_session(args.session_id.as_deref())?
        .ok_or_else(|| CoreError::BadArgs {
            operation: "session show".to_string(),
            reason: "no session exists; run `prog session start --goal <goal>`".to_string(),
        })?;
    let mut unavailable = 0usize;
    for event in &mut trail.events {
        let Some(cursor) = event.cursor.as_deref() else {
            continue;
        };
        match store.get_cursor(cursor, RedactionPolicy::default().version) {
            Ok(_) => {
                event
                    .extra
                    .insert("cursor_status".to_string(), json!("available"));
            }
            Err(error) => {
                unavailable += 1;
                event
                    .extra
                    .insert("cursor_status".to_string(), json!(error.kind()));
            }
        }
    }
    if unavailable > 0 {
        trail.warnings.push(format!(
            "{unavailable} event cursor(s) are expired, missing, or incompatible with the current redaction policy"
        ));
    }
    Ok(trail)
}

fn readiness_report(store: &Store, session_id: Option<&str>) -> Result<ReadinessReport> {
    let obligations = store.list_obligations(session_id)?.obligations;
    if obligations.is_empty() {
        return Ok(ReadinessReport {
            schema: VERIFICATION_SCHEMA.to_string(),
            configured: false,
            ready: false,
            evaluations: Vec::new(),
            blockers: vec!["no verification obligations are declared for this session".to_string()],
            extra: Extra::new(),
        });
    }

    let mut evaluations = Vec::with_capacity(obligations.len());
    let mut blockers = Vec::new();
    for obligation in obligations {
        let evaluation = evaluate_obligation(store, obligation)?;
        if evaluation.obligation.required && evaluation.status != VerificationStatus::Passed {
            blockers.push(format!(
                "{}: {}",
                evaluation.obligation.id,
                evaluation.reasons.join("; ")
            ));
        }
        evaluations.push(evaluation);
    }
    Ok(ReadinessReport {
        schema: VERIFICATION_SCHEMA.to_string(),
        configured: true,
        ready: blockers.is_empty(),
        evaluations,
        blockers,
        extra: Extra::new(),
    })
}

fn evaluate_obligation(
    store: &Store,
    obligation: VerificationObligation,
) -> Result<ObligationEvaluation> {
    let Some(evidence_id) = obligation.evidence_observation_id.clone() else {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Pending,
            vec!["no evidence observation has been attached".to_string()],
            None,
        ));
    };
    let Some(evidence) = store.get_observation(&evidence_id)? else {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Unverifiable,
            vec![format!(
                "evidence observation '{evidence_id}' is unavailable"
            )],
            None,
        ));
    };
    if evidence.availability != prog_core::EvidenceAvailability::Recoverable {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Unverifiable,
            vec!["the evidence payload is no longer available".to_string()],
            None,
        ));
    }
    if !evidence.capture.can_prove_absence {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Unverifiable,
            vec!["the evidence observation is incomplete or truncated".to_string()],
            None,
        ));
    }
    if let Some(family) = obligation.comparison_family.as_deref()
        && evidence.invocation_fingerprint != family
    {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Stale,
            vec!["evidence does not match the declared comparison family".to_string()],
            None,
        ));
    }

    match (
        obligation.origin_observation_id.clone(),
        obligation.expected_absent_fingerprint.clone(),
    ) {
        (Some(origin_id), Some(expected_fingerprint)) => {
            let delta = compare_observation_ids(store, &origin_id, &evidence_id)?;
            let status = delta
                .findings
                .iter()
                .find(|finding| finding.fingerprint == expected_fingerprint)
                .map(|finding| match finding.status {
                    prog_core::DeltaFindingStatus::Resolved => VerificationStatus::Passed,
                    prog_core::DeltaFindingStatus::Persisting => VerificationStatus::Persisting,
                    prog_core::DeltaFindingStatus::New => VerificationStatus::New,
                    prog_core::DeltaFindingStatus::NotObserved => VerificationStatus::NotObserved,
                    prog_core::DeltaFindingStatus::Unknown => VerificationStatus::Unknown,
                })
                .unwrap_or(VerificationStatus::Unknown);
            let reasons = match status {
                VerificationStatus::Passed => vec![
                    "the expected finding is absent under a comparable, complete observation"
                        .to_string(),
                ],
                VerificationStatus::Unknown => vec![
                    "the expected finding could not be evaluated from the comparable evidence"
                        .to_string(),
                ],
                _ => delta
                    .findings
                    .iter()
                    .find(|finding| finding.fingerprint == expected_fingerprint)
                    .map(|finding| finding.reasons.clone())
                    .filter(|reasons| !reasons.is_empty())
                    .unwrap_or_else(|| delta.assessment.reasons.clone()),
            };
            Ok(obligation_evaluation(
                obligation,
                status,
                reasons,
                Some(delta.assessment),
            ))
        }
        (None, None) => match command_success(store, &evidence)? {
            Some(true) => Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Passed,
                vec!["a complete command observation exited successfully".to_string()],
                None,
            )),
            Some(false) => Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Failed,
                vec!["the evidence command did not exit successfully".to_string()],
                None,
            )),
            None => Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Unknown,
                vec![
                    "evidence has no explicit finding comparison or successful command result"
                        .to_string(),
                ],
                None,
            )),
        },
        _ => Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Unknown,
            vec![
                "origin observation and expected finding fingerprint must be supplied together"
                    .to_string(),
            ],
            None,
        )),
    }
}

fn command_success(
    store: &Store,
    observation: &prog_core::ObservationRecord,
) -> Result<Option<bool>> {
    let Some(payload) = store.get_payload(&observation.payload_hash)? else {
        return Ok(None);
    };
    Ok(payload
        .as_value()
        .pointer("/command/success")
        .and_then(Value::as_bool))
}

fn obligation_evaluation(
    obligation: VerificationObligation,
    status: VerificationStatus,
    reasons: Vec<String>,
    assessment: Option<prog_core::ComparabilityAssessment>,
) -> ObligationEvaluation {
    ObligationEvaluation {
        obligation,
        status,
        reasons,
        assessment,
        extra: Extra::new(),
    }
}

fn paths_cursor(store: &Store, args: &PathsArgs) -> Result<PathsResponse> {
    let filters = path_filters(args)?;
    let redaction_version = RedactionPolicy::default().version;
    let record = store.get_cursor(&args.cursor, redaction_version)?;
    let entry = store
        .get_entry(&record.cache_key)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let observation = record
        .observation_id
        .as_deref()
        .map(|observation_id| store.get_observation(observation_id))
        .transpose()?
        .flatten();
    let payload = store
        .get_payload(&entry.payload_hash)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let requested_prefix = if args.prefix.is_empty() {
        record.root_path.clone()
    } else {
        args.prefix.clone()
    };
    let scoped = ScopedSlice::new(
        ExpansionScope::from_cursor(&record)?,
        SliceRequest {
            path: Some(requested_prefix),
            limit: None,
            depth: None,
            fields: Vec::new(),
            omit: Vec::new(),
            extra: Extra::new(),
        },
    )?;
    let prefix = scoped.target_path().as_str().to_string();
    let value = payload.as_value();
    let target =
        prog_core::pointer::get(value, &prefix)?.ok_or_else(|| CoreError::PathNotFound {
            path: prefix.clone(),
            hint: prog_core::pointer::siblings_hint(value, &prefix),
        })?;
    let projection = project(target, &PreviewPolicy::default(), &prefix);
    let projected_omitted = projection.omitted.clone();
    let mut paths = Vec::new();
    let truncated = collect_paths(target, &prefix, args.depth, args.limit, &mut paths);
    annotate_path_omissions(&mut paths, &projected_omitted);
    append_missing_omitted_paths(&mut paths, &projected_omitted, args.limit);
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
    let cache = cache_info(CacheStatus::Hit, &entry, Some(age));
    if cache_is_stale(Some(&cache)) {
        warnings.push(format!(
            "cached payload age_seconds={age}; re-run the original observation or call to refresh"
        ));
    }
    attach_path_evidence_refs(
        &mut paths,
        value,
        PathEvidenceContext {
            record: record.record(),
            entry: &entry,
            observation: observation.as_ref(),
            cache: &cache,
            omitted: &projected_omitted,
            cursor: &args.cursor,
        },
    )?;

    Ok(PathsResponse {
        schema: DISCLOSURE_SCHEMA,
        cursor: args.cursor.clone(),
        source_id: record.source_id.clone(),
        operation: record.operation.clone(),
        root_path: record.root_path.clone(),
        prefix,
        paths,
        omitted,
        next_actions,
        cache,
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
        evidence_ref: None,
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
            evidence_ref: None,
        });
    }
}

fn attach_path_evidence_refs(
    paths: &mut [PathEntry],
    payload: &Value,
    context: PathEvidenceContext<'_>,
) -> Result<()> {
    for path in paths {
        if !path.expandable && path.omitted_reason.is_none() {
            continue;
        }
        if let Some(value) = prog_core::pointer::get(payload, &path.path)? {
            path.evidence_ref = Some(evidence_ref(EvidenceRefInput {
                source_id: &context.record.source_id,
                operation: &context.record.operation,
                cursor: Some(context.cursor),
                path: &path.path,
                value,
                observation: context.observation,
                provenance: context.entry.provenance.as_ref(),
                cache: Some(context.cache),
                omitted: context.omitted,
                redacted_paths: 0,
            }));
        }
    }
    Ok(())
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
    let action_kind = if region.reason == OmissionReason::LargeString {
        "evidence"
    } else {
        "expand"
    };
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
        "offline".to_string(),
        json!("uses cached redacted payload; does not contact upstream"),
    );
    NextAction {
        kind: action_kind.to_string(),
        operation: operation.map(str::to_string),
        path: Some(region.path.clone()),
        reason: Some(omission_action_reason(region)),
        argv: Some(match region.reason {
            OmissionReason::LargeString => vec![
                "prog".to_string(),
                "evidence".to_string(),
                cursor.to_string(),
                "--path".to_string(),
                region.path.clone(),
            ],
            _ => vec![
                "prog".to_string(),
                "expand".to_string(),
                cursor.to_string(),
                "--path".to_string(),
                region.path.clone(),
            ],
        }),
        scope: Some("cached_evidence".to_string()),
        exactness: Some(prog_core::ActionExactness::Exact),
        derived_from: Some("omitted_region".to_string()),
        extra,
        ..NextAction::default()
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
            "{} is a large string; emit a bounded evidence excerpt, or use expand --out for the full stored redacted value",
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
    let observation = record
        .observation_id
        .as_deref()
        .map(|observation_id| store.get_observation(observation_id))
        .transpose()?
        .flatten();
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
    let scoped = ScopedSlice::new(ExpansionScope::from_cursor(&record)?, slice.clone())?;
    let age = age_seconds(&entry.created_at)?;
    let cache = cache_info(CacheStatus::Hit, &entry, Some(age));
    let mut warnings = Vec::new();
    if cache_is_stale(Some(&cache)) {
        warnings.push(format!(
            "cached payload age_seconds={age}; re-run `prog call {} {} --refresh` to refresh",
            record.source_id, record.operation
        ));
    }

    if let Some(path) = &args.out {
        let (target_path, selected) = slice_value(&payload, &scoped)?;
        let bytes = serde_json::to_vec_pretty(&selected)?;
        write_private_file(path, &bytes)?;
        let evidence_ref = evidence_ref(EvidenceRefInput {
            source_id: &record.source_id,
            operation: &record.operation,
            cursor: Some(&args.cursor),
            path: &target_path,
            value: &selected,
            observation: observation.as_ref(),
            provenance: entry.provenance.as_ref(),
            cache: Some(&cache),
            omitted: &[],
            redacted_paths: 0,
        });
        let receipt = json!({
            "path": path,
            "json_pointer": target_path,
            "bytes": bytes.len(),
            "sha256": hex_sha256(&bytes),
            "evidence_ref": evidence_ref
        });
        let receipt = RawPayload::new(receipt)
            .redact(&RedactionPolicy::default())
            .payload;
        let mut envelope = envelope_for_payload(
            store,
            EnvelopeInput {
                value_scan: None,
                source_id: record.source_id.clone(),
                operation: record.operation.clone(),
                source_kind: source_kind_for_source_id(&record.source_id),
                payload: receipt,
                root_path: "".to_string(),
                slice: SliceRequest {
                    path: None,
                    limit: Some(5),
                    // The receipt contains a nested EvidenceRef. Keep the
                    // small receipt intact so its completeness describes the
                    // exported selection rather than a formatter omission.
                    depth: Some(8),
                    fields: Vec::new(),
                    omit: Vec::new(),
                    extra: Extra::new(),
                },
                payload_bytes: bytes.len().try_into().unwrap_or(u64::MAX),
                observation_id: entry.observation_id.clone(),
                provenance: entry.provenance.clone(),
                cache: Some(cache),
                effects: None,
                auto_upgrade_audit: None,
                redacted_paths: 0,
                cache_disabled_reason: None,
                warnings,
                schema_hints: BTreeMap::new(),
                next_action_operation: None,
                additional_next_actions: Vec::new(),
                observation_parser: None,
                lens: None,
            },
            None,
        )?;
        if let Some(observation) = envelope.observation.as_mut() {
            observation.completeness.root_path = target_path;
            observation.completeness.path_scoped = !observation.completeness.root_path.is_empty();
        }
        return Ok(envelope);
    }

    envelope_for_payload(
        store,
        EnvelopeInput {
            value_scan: None,
            source_id: record.source_id.clone(),
            operation: record.operation.clone(),
            source_kind: source_kind_for_source_id(&record.source_id),
            payload: payload.into_redacted(),
            root_path: record.root_path.clone(),
            slice,
            payload_bytes: entry.payload_bytes,
            observation_id: entry.observation_id.clone(),
            provenance: entry.provenance.clone(),
            cache: Some(cache),
            effects: None,
            auto_upgrade_audit: None,
            redacted_paths: 0,
            cache_disabled_reason: None,
            warnings,
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: Vec::new(),
            observation_parser: None,
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
    let redacted = RawPayload::new(payload).redact(&RedactionPolicy::default());
    let payload = redacted.payload;
    let payload_hash = store.put_payload(&payload)?;
    let payload_bytes = json_len_u64(payload.as_value())?;
    let mut entry = new_cache_entry(
        cache_key.clone(),
        payload_hash.clone(),
        "prog".to_string(),
        operation.clone(),
        payload_bytes,
        86_400,
    );
    let (availability, capture) = complete_capture(payload_bytes, true, false);
    set_response_capture_budget(capture.budget.clone());
    let observation_id = record_capture(
        store,
        payload_hash,
        availability,
        capture,
        cache_key.clone(),
        "prog".to_string(),
        operation.clone(),
        entry.provenance.clone(),
        Some(cache_key.clone()),
        false,
        None,
        None,
        None,
    )?;
    entry.observation_id = Some(observation_id);
    let cache_retained = store.put_entry(&cache_key, &entry)?;
    let slice = SliceRequest {
        path: None,
        limit: None,
        depth: None,
        fields: Vec::new(),
        omit: Vec::new(),
        extra: Extra::new(),
    };
    let scoped = ScopedSlice::root(slice.clone())?;
    let projection = expand(&payload, &scoped, &PreviewPolicy::default())?;
    let cursor = if projection.omitted.is_empty() || !cache_retained {
        None
    } else {
        Some(store.create_cursor(&cache_key, "prog", &operation, "", 1, 86_400)?)
    };
    envelope_for_payload(
        store,
        EnvelopeInput {
            value_scan: None,
            source_id: "prog".to_string(),
            operation,
            source_kind: Some("internal".to_string()),
            payload,
            root_path: "".to_string(),
            slice,
            payload_bytes,
            observation_id: entry.observation_id.clone(),
            provenance: entry.provenance.clone(),
            cache: Some(if cache_retained {
                cache_info(CacheStatus::Stored, &entry, Some(0))
            } else {
                CacheInfo {
                    status: CacheStatus::Skipped,
                    ttl_seconds: None,
                    expires_at: None,
                    age_seconds: None,
                }
            }),
            effects: None,
            auto_upgrade_audit: None,
            redacted_paths: 0,
            cache_disabled_reason: None,
            warnings: Vec::new(),
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: Vec::new(),
            observation_parser: None,
            lens: None,
        },
        cursor,
    )
}

fn delta_observations(store: &Store, args: &DeltaArgs) -> Result<prog_core::ObservationDelta> {
    compare_observation_ids(store, &args.baseline, &args.subject)
}

fn compare_observation_ids(
    store: &Store,
    baseline_id: &str,
    subject_id: &str,
) -> Result<prog_core::ObservationDelta> {
    let baseline = store
        .get_observation(baseline_id)?
        .ok_or_else(|| CoreError::BadArgs {
            operation: "delta".to_string(),
            reason: format!("unknown baseline observation '{baseline_id}'"),
        })?;
    let subject = store
        .get_observation(subject_id)?
        .ok_or_else(|| CoreError::BadArgs {
            operation: "delta".to_string(),
            reason: format!("unknown subject observation '{subject_id}'"),
        })?;
    let findings_for =
        |observation: &prog_core::ObservationRecord| -> Result<Vec<prog_core::Finding>> {
            let Some(payload) = store.get_payload(&observation.payload_hash)? else {
                return Ok(Vec::new());
            };
            prog_core::ranked_findings(
                payload.as_value(),
                &FindingOptions {
                    limit: 100,
                    ..FindingOptions::default()
                },
            )
        };
    let baseline_findings = findings_for(&baseline)?;
    let subject_findings = findings_for(&subject)?;
    Ok(prog_core::compare_observations(
        &baseline,
        &subject,
        &baseline_findings,
        &subject_findings,
    ))
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

const OBSERVATION_PARSERS: &[ObservationParser] = &[
    ObservationParser {
        id: "sarif",
        detect: detect_sarif_observation,
        parse: parse_sarif_observation,
    },
    ObservationParser {
        id: "ndjson",
        detect: detect_ndjson_observation,
        parse: parse_ndjson_observation,
    },
    ObservationParser {
        id: "json",
        detect: detect_json_observation,
        parse: parse_json_observation,
    },
    ObservationParser {
        id: "junit_xml",
        detect: detect_junit_xml_observation,
        parse: parse_junit_xml_observation,
    },
    ObservationParser {
        id: "html_basic",
        detect: detect_html_observation,
        parse: parse_html_observation,
    },
    ObservationParser {
        id: "unified_diff",
        detect: detect_diff_observation,
        parse: parse_diff_observation,
    },
    ObservationParser {
        id: "table",
        detect: detect_table_observation,
        parse: parse_table_observation,
    },
    ObservationParser {
        id: "text_fallback",
        detect: detect_text_observation,
        parse: parse_text_observation,
    },
];

fn normalize_observation(bytes: &[u8], mime: &str) -> Result<NormalizedObservation> {
    let mut parser_errors = Vec::new();
    for parser in OBSERVATION_PARSERS {
        let Some(matched) = (parser.detect)(bytes, mime) else {
            continue;
        };
        match (parser.parse)(bytes, mime, matched) {
            Ok(mut normalized) => {
                normalized.warnings.extend(parser_errors);
                return Ok(normalized);
            }
            Err(error) => parser_errors.push(format!("{}: {error}", parser.id)),
        }
    }

    if is_binaryish(bytes) {
        return Err(CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "input appears to be binary; pass a text, JSON, NDJSON, diff, XML, HTML, or SARIF artifact".to_string(),
        });
    }
    let mut normalized = parse_text_observation(
        bytes,
        mime,
        ParserMatch {
            confidence: 0.25,
            reason: "all specific parsers failed; using text fallback",
        },
    )?;
    normalized.warnings.extend(parser_errors);
    Ok(normalized)
}

fn detect_sarif_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    if !normalized_mime.contains("sarif") && !normalized_mime.contains("json") {
        return None;
    }
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    if value.get("runs").and_then(Value::as_array).is_some()
        && value.get("version").and_then(Value::as_str).is_some()
    {
        Some(ParserMatch {
            confidence: 0.98,
            reason: "JSON object contains SARIF version and runs",
        })
    } else {
        None
    }
}

fn parse_sarif_observation(
    bytes: &[u8],
    mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let mut normalized = parse_json_observation(bytes, mime, matched)?;
    normalized.kind = "sarif".to_string();
    normalized.parser.id = "sarif";
    normalized.parser.label = "SARIF JSON";
    normalized.parser.path_semantics = "sarif JSON pointer";
    Ok(normalized)
}

fn detect_ndjson_observation(_bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    (normalized_mime.contains("ndjson") || normalized_mime.contains("jsonlines")).then_some(
        ParserMatch {
            confidence: 0.98,
            reason: "mime declares NDJSON or JSON Lines",
        },
    )
}

fn detect_json_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    if normalized_mime.contains("ndjson") || normalized_mime.contains("jsonlines") {
        return None;
    }
    (normalized_mime.contains("json") || sniff_mime_from_bytes(bytes) == "application/json")
        .then_some(ParserMatch {
            confidence: 0.95,
            reason: "mime or byte sniffing indicates JSON",
        })
}

fn parse_json_observation(
    bytes: &[u8],
    mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let payload = serde_json::from_slice(bytes).map_err(|error| CoreError::BadArgs {
        operation: "observe".to_string(),
        reason: format!("input with mime '{mime}' must be valid JSON: {error}"),
    })?;
    Ok(NormalizedObservation {
        kind: "json".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "json",
            label: "JSON tree",
            confidence: matched.confidence,
            lossy: false,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer",
            range_semantics: "tree nodes",
        },
        warnings: Vec::new(),
    })
}

fn parse_ndjson_observation(
    bytes: &[u8],
    _mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
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
        parser: ObservationParserInfo {
            id: "ndjson",
            label: "NDJSON records",
            confidence: matched.confidence,
            lossy: false,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer over /records",
            range_semantics: "line-delimited records",
        },
        warnings: Vec::new(),
    })
}

fn detect_junit_xml_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    let prefix = text_prefix(bytes).to_ascii_lowercase();
    (normalized_mime.contains("junit")
        || (normalized_mime.contains("xml")
            && (prefix.contains("<testsuite") || prefix.contains("<testsuites"))))
    .then_some(ParserMatch {
        confidence: 0.90,
        reason: "mime or XML root indicates JUnit",
    })
}

fn parse_junit_xml_observation(
    bytes: &[u8],
    _mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let (text, mut warnings, utf8_valid) = decode_text(bytes);
    let cases = parse_junit_cases(&text);
    let mut payload = text_payload(&text, bytes.len(), utf8_valid, "junit_xml");
    payload["testcases"] = Value::Array(cases);
    payload["testcase_count"] = json!(payload["testcases"].as_array().map_or(0, Vec::len));
    Ok(NormalizedObservation {
        kind: "junit_xml".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "junit_xml",
            label: "JUnit XML",
            confidence: matched.confidence,
            lossy: true,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer over parsed testcases and /lines",
            range_semantics: "line ranges from XML text",
        },
        warnings: {
            warnings.push(
                "JUnit XML parser is lightweight and preserves raw line expansion".to_string(),
            );
            warnings
        },
    })
}

fn detect_html_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    let prefix = text_prefix(bytes).to_ascii_lowercase();
    (normalized_mime.contains("html")
        || prefix.contains("<html")
        || prefix.contains("<!doctype html"))
    .then_some(ParserMatch {
        confidence: 0.88,
        reason: "mime or text prefix indicates HTML",
    })
}

fn parse_html_observation(
    bytes: &[u8],
    _mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let (text, mut warnings, utf8_valid) = decode_text(bytes);
    let mut payload = text_payload(&text, bytes.len(), utf8_valid, "html");
    payload["title"] = json!(extract_tag_text(&text, "title"));
    payload["headings"] = Value::Array(
        ["h1", "h2", "h3"]
            .into_iter()
            .flat_map(|tag| extract_all_tag_text(&text, tag))
            .map(Value::String)
            .collect(),
    );
    payload["links"] = Value::Array(extract_links(&text));
    Ok(NormalizedObservation {
        kind: "html".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "html_basic",
            label: "Basic HTML",
            confidence: matched.confidence,
            lossy: true,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer over title/headings/links and /lines",
            range_semantics: "line ranges from HTML source",
        },
        warnings: {
            warnings.push("HTML parser does not render or execute the document".to_string());
            warnings
        },
    })
}

fn detect_diff_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    let prefix = text_prefix(bytes);
    (normalized_mime.contains("diff")
        || normalized_mime.contains("patch")
        || prefix.starts_with("diff --git")
        || prefix.contains("\n--- ")
        || prefix.contains("\n+++ "))
    .then_some(ParserMatch {
        confidence: 0.86,
        reason: "mime or diff markers indicate unified diff",
    })
}

fn parse_diff_observation(
    bytes: &[u8],
    _mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let (text, warnings, utf8_valid) = decode_text(bytes);
    let mut payload = text_payload(&text, bytes.len(), utf8_valid, "unified_diff");
    payload["files"] = Value::Array(parse_diff_files(&text));
    Ok(NormalizedObservation {
        kind: "unified_diff".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "unified_diff",
            label: "Unified diff",
            confidence: matched.confidence,
            lossy: false,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer over diff files and /lines",
            range_semantics: "line ranges from diff text",
        },
        warnings,
    })
}

fn detect_text_observation(bytes: &[u8], _mime: &str) -> Option<ParserMatch> {
    (!is_binaryish(bytes)).then_some(ParserMatch {
        confidence: 0.50,
        reason: "fallback text parser accepted non-binary bytes",
    })
}

fn detect_table_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let text = std::str::from_utf8(bytes).ok()?;
    let detection = prog_core::table::detect_table(text, mime)?;
    Some(ParserMatch {
        confidence: detection.confidence,
        reason: detection.reason,
    })
}

fn parse_table_observation(
    bytes: &[u8],
    mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let (text, mut warnings, utf8_valid) = decode_text(bytes);
    let detection =
        prog_core::table::detect_table(&text, mime).ok_or_else(|| CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "input matched a table detector but could not be re-detected during parse"
                .to_string(),
        })?;
    let table = prog_core::table::parse_table(&text, detection.format).ok_or_else(|| {
        CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "table detected but no rows parsed".to_string(),
        }
    })?;
    let lossy = table.lossy || !utf8_valid;
    let payload = json!({
        "format": table.format.id(),
        "columns": table.columns,
        "rows": table.rows,
        "row_count": table.row_count(),
        "column_count": table.column_count(),
        "byte_count": bytes.len(),
        "truncated": false,
        "utf8_valid": utf8_valid,
        "lossy": lossy
    });
    if table.lossy {
        warnings.push(format!(
            "table parsed as {} with a lossy structural assumption; cells are original strings",
            table.format.id()
        ));
    }
    Ok(NormalizedObservation {
        kind: "table".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "table",
            label: table.format.label(),
            confidence: matched.confidence,
            lossy,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer over /rows",
            range_semantics: "row indices over /rows",
        },
        warnings,
    })
}

fn parse_text_observation(
    bytes: &[u8],
    _mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    if is_binaryish(bytes) {
        return Err(CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "input appears to be binary; pass a text, JSON, or NDJSON artifact".to_string(),
        });
    }

    let (text, warnings, utf8_valid) = decode_text(bytes);
    let mut payload = text_payload(&text, bytes.len(), utf8_valid, "text");
    payload["repeated_stack_traces"] = json!(count_repeated_stack_trace_lines(&text));
    Ok(NormalizedObservation {
        kind: "text".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "text_fallback",
            label: "Text fallback",
            confidence: matched.confidence,
            lossy: !utf8_valid,
            fallback: true,
            reason: matched.reason,
            path_semantics: "json pointer over /lines",
            range_semantics: "line ranges from text",
        },
        warnings,
    })
}

fn text_payload(text: &str, byte_count: usize, utf8_valid: bool, format: &str) -> Value {
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

    json!({
        "format": format,
        "head": head,
        "tail": tail,
        "lines": lines,
        "line_count": line_count,
        "byte_count": byte_count,
        "utf8_valid": utf8_valid
    })
}

fn parser_metadata(parser: &ObservationParserInfo) -> Value {
    json!({
        "id": parser.id,
        "label": parser.label,
        "confidence": parser.confidence,
        "lossy": parser.lossy,
        "fallback": parser.fallback,
        "reason": parser.reason,
        "path_semantics": parser.path_semantics,
        "range_semantics": parser.range_semantics
    })
}

fn decode_text(bytes: &[u8]) -> (String, Vec<String>, bool) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_string(), Vec::new(), true),
        Err(_) => (
            String::from_utf8_lossy(bytes).to_string(),
            vec!["input was not valid UTF-8; replacement characters were used".to_string()],
            false,
        ),
    }
}

fn text_prefix(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_string()
}

fn parse_junit_cases(text: &str) -> Vec<Value> {
    text.match_indices("<testcase")
        .map(|(start, _)| {
            let opening_end = text[start..]
                .find('>')
                .map(|offset| start + offset)
                .unwrap_or(text.len());
            let tag = &text[start..opening_end];
            let self_closing = tag.trim_end().ends_with('/');
            let case_end = if self_closing {
                opening_end.saturating_add(1)
            } else {
                text[opening_end..]
                    .find("</testcase>")
                    .map(|offset| opening_end + offset + "</testcase>".len())
                    .unwrap_or(opening_end.saturating_add(1))
            }
            .min(text.len());
            let body = &text[opening_end.min(text.len())..case_end];
            let failure = extract_tag_text(body, "failure")
                .map(|value| value.chars().take(2_000).collect::<String>());
            let error = extract_tag_text(body, "error")
                .map(|value| value.chars().take(2_000).collect::<String>());
            let skipped = body.to_ascii_lowercase().contains("<skipped");
            let status = if error.is_some() {
                "error"
            } else if failure.is_some() {
                "failed"
            } else if skipped {
                "skipped"
            } else {
                "passed"
            };
            json!({
                "name": extract_attr(tag, "name"),
                "classname": extract_attr(tag, "classname"),
                "time": extract_attr(tag, "time"),
                "status": status,
                "failure": failure,
                "error": error,
                "line_start": line_number_at(text, start),
                "line_end": line_number_at(text, case_end)
            })
        })
        .collect()
}

fn parse_diff_files(text: &str) -> Vec<Value> {
    let mut files = Vec::new();
    let mut current: Option<Map<String, Value>> = None;
    let mut hunk: Option<Map<String, Value>> = None;
    for (index, line) in text.lines().enumerate() {
        if let Some(path) = line.strip_prefix("diff --git ") {
            finish_diff_hunk(&mut current, &mut hunk, index);
            if let Some(file) = current.take() {
                files.push(Value::Object(file));
            }
            let mut file = Map::new();
            file.insert("header".to_string(), json!(path));
            file.insert("line_start".to_string(), json!(index + 1));
            file.insert("hunks".to_string(), Value::Array(Vec::new()));
            current = Some(file);
        } else if line.starts_with("@@") {
            finish_diff_hunk(&mut current, &mut hunk, index);
            let mut next = Map::new();
            next.insert("header".to_string(), json!(line));
            next.insert("line_start".to_string(), json!(index + 1));
            next.insert("lines".to_string(), Value::Array(Vec::new()));
            hunk = Some(next);
        } else if let Some(hunk) = hunk.as_mut()
            && let Some(lines) = hunk.get_mut("lines").and_then(Value::as_array_mut)
        {
            lines.push(Value::String(line.to_string()));
        }
    }
    finish_diff_hunk(&mut current, &mut hunk, text.lines().count());
    if let Some(file) = current {
        files.push(Value::Object(file));
    }
    files
}

fn finish_diff_hunk(
    file: &mut Option<Map<String, Value>>,
    hunk: &mut Option<Map<String, Value>>,
    end_index: usize,
) {
    let Some(mut completed) = hunk.take() else {
        return;
    };
    completed.insert("line_end".to_string(), json!(end_index.max(1)));
    if let Some(file) = file.as_mut()
        && let Some(hunks) = file.get_mut("hunks").and_then(Value::as_array_mut)
    {
        hunks.push(Value::Object(completed));
        file.insert("line_end".to_string(), json!(end_index.max(1)));
    }
}

fn extract_tag_text(text: &str, tag: &str) -> Option<String> {
    extract_all_tag_text(text, tag).into_iter().next()
}

fn extract_all_tag_text(text: &str, tag: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut offset = 0usize;
    while let Some(start) = lower[offset..].find(&open) {
        let absolute_start = offset + start;
        let Some(tag_end) = lower[absolute_start..].find('>') else {
            break;
        };
        let content_start = absolute_start + tag_end + 1;
        let Some(end) = lower[content_start..].find(&close) else {
            break;
        };
        let content_end = content_start + end;
        values.push(
            strip_tags(&text[content_start..content_end])
                .trim()
                .to_string(),
        );
        offset = content_end + close.len();
    }
    values
}

fn extract_links(text: &str) -> Vec<Value> {
    let lower = text.to_ascii_lowercase();
    let mut links = Vec::new();
    let mut offset = 0usize;
    while let Some(start) = lower[offset..].find("<a ") {
        let absolute_start = offset + start;
        let Some(tag_end) = lower[absolute_start..].find('>') else {
            break;
        };
        let tag = &text[absolute_start..absolute_start + tag_end];
        links.push(json!({
            "href": extract_attr(tag, "href"),
            "line_start": line_number_at(text, absolute_start)
        }));
        offset = absolute_start + tag_end + 1;
    }
    links
}

fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needles = [
            format!(" {attr}={quote}"),
            format!("\t{attr}={quote}"),
            format!("\n{attr}={quote}"),
        ];
        if let Some((start, needle)) = needles
            .iter()
            .filter_map(|needle| tag.find(needle).map(|start| (start, needle)))
            .next()
        {
            let value_start = start + needle.len();
            let value_end = tag[value_start..]
                .find(quote)
                .map(|offset| value_start + offset)
                .unwrap_or(tag.len());
            return Some(tag[value_start..value_end].to_string());
        }
    }
    None
}

fn strip_tags(raw: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn line_number_at(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset.min(text.len())].lines().count()
}

fn count_repeated_stack_trace_lines(text: &str) -> usize {
    let mut counts = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with("at ") || line.starts_with("File \"") {
            *counts.entry(line.to_string()).or_insert(0usize) += 1;
        }
    }
    counts.values().filter(|count| **count > 1).count()
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
    prog_core::redact_sensitive_text(line).0
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

fn load_lens(lens_dir: &Path, id: &str, context: &str) -> Result<LensManifest> {
    let manifests = load_lens_manifests(lens_dir, context)?;
    let mut matches = manifests
        .into_iter()
        .filter(|manifest| manifest.id == id)
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!("lens '{id}' not found in '{}'", lens_dir.to_string_lossy()),
        }),
        1 => Ok(matches.remove(0)),
        _ => Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{id}' is defined more than once in '{}'",
                lens_dir.to_string_lossy()
            ),
        }),
    }
}

fn load_lens_manifests(lens_dir: &Path, context: &str) -> Result<Vec<LensManifest>> {
    if !lens_dir.exists() {
        return Err(CoreError::BadArgs {
            operation: context.to_string(),
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
        let bytes = std::fs::metadata(&path)?.len();
        if bytes > 1024 * 1024 {
            return Err(CoreError::BadArgs {
                operation: context.to_string(),
                reason: format!(
                    "lens '{}' is {bytes} bytes; manifests are limited to 1 MiB",
                    path.to_string_lossy()
                ),
            });
        }
        let raw = std::fs::read_to_string(&path).map_err(|error| CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!("could not read lens '{}': {error}", path.to_string_lossy()),
        })?;
        let manifest = parse_lens_manifest(&path, &raw, context)?;
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

fn parse_lens_manifest(path: &Path, raw: &str, context: &str) -> Result<LensManifest> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => serde_json::from_str(raw).map_err(|error| CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{}' must be valid JSON: {error}",
                path.to_string_lossy()
            ),
        }),
        Some("yaml" | "yml") => serde_yaml_ng::from_str(raw).map_err(|error| CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{}' must be valid YAML: {error}",
                path.to_string_lossy()
            ),
        }),
        _ => Err(CoreError::BadArgs {
            operation: context.to_string(),
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
    validate_lens_matches(
        lens,
        "call --lens",
        LensMatchSubject {
            actual_source_id: &profile.id,
            source_kind: Some(profile.kind),
            actual_operation: &operation.id,
            mime: None,
            artifact_kind: None,
        },
    )
}

fn validate_lens_matches_observe(
    lens: &LensManifest,
    input: &ObservationInput,
    normalized: &NormalizedObservation,
) -> Result<()> {
    validate_lens_matches(
        lens,
        "observe --lens",
        LensMatchSubject {
            actual_source_id: "observe",
            source_kind: None,
            actual_operation: &input.name,
            mime: Some(&input.mime),
            artifact_kind: Some(&normalized.kind),
        },
    )
}

fn validate_lens_matches_run(lens: &LensManifest, operation: &str) -> Result<()> {
    validate_lens_matches(
        lens,
        "run --lens",
        LensMatchSubject {
            actual_source_id: "run",
            source_kind: Some(prog_core::SourceKind::Cli),
            actual_operation: operation,
            mime: None,
            artifact_kind: Some("run"),
        },
    )
}

struct LensMatchSubject<'a> {
    actual_source_id: &'a str,
    source_kind: Option<prog_core::SourceKind>,
    actual_operation: &'a str,
    mime: Option<&'a str>,
    artifact_kind: Option<&'a str>,
}

fn validate_lens_matches(
    lens: &LensManifest,
    context: &str,
    subject: LensMatchSubject<'_>,
) -> Result<()> {
    if let Some(source_id) = &lens.match_rules.source_id
        && source_id != subject.actual_source_id
    {
        return Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{}' matches source_id '{}', not '{}'",
                lens.id, source_id, subject.actual_source_id
            ),
        });
    }
    if let Some(source_kind) = lens.match_rules.source_kind {
        match subject.source_kind {
            Some(actual_source_kind) if source_kind == actual_source_kind => {}
            Some(actual_source_kind) => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches source_kind '{:?}', not '{:?}'",
                        lens.id, source_kind, actual_source_kind
                    ),
                });
            }
            None => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches source_kind '{:?}', but this artifact has no source_kind",
                        lens.id, source_kind
                    ),
                });
            }
        }
    }
    if let Some(expected_operation) = &lens.match_rules.operation
        && expected_operation != subject.actual_operation
    {
        return Err(CoreError::BadArgs {
            operation: context.to_string(),
            reason: format!(
                "lens '{}' matches operation '{}', not '{}'",
                lens.id, expected_operation, subject.actual_operation
            ),
        });
    }
    if let Some(expected_mime) = &lens.match_rules.mime {
        match subject.mime {
            Some(actual_mime) if expected_mime.eq_ignore_ascii_case(actual_mime) => {}
            Some(actual_mime) => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches mime '{}', not '{}'",
                        lens.id, expected_mime, actual_mime
                    ),
                });
            }
            None => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches mime '{}', but this artifact has no mime",
                        lens.id, expected_mime
                    ),
                });
            }
        }
    }
    if let Some(expected_artifact_kind) = &lens.match_rules.artifact_kind {
        match subject.artifact_kind {
            Some(actual_artifact_kind) if expected_artifact_kind == actual_artifact_kind => {}
            Some(actual_artifact_kind) => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches artifact_kind '{}', not '{}'",
                        lens.id, expected_artifact_kind, actual_artifact_kind
                    ),
                });
            }
            None => {
                return Err(CoreError::BadArgs {
                    operation: context.to_string(),
                    reason: format!(
                        "lens '{}' matches artifact_kind '{}', but this artifact has no artifact_kind",
                        lens.id, expected_artifact_kind
                    ),
                });
            }
        }
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
                received_error: result.received_error,
                not_modified: false,
            })
        }
    }
}

async fn execute_callable_conditional(
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
async fn execute_callable_url(
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

fn capture_budget_for_call(profile: &SourceProfile, operation: &OperationProfile) -> CaptureBudget {
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

fn capture_budget_for_run(args: &RunArgs) -> CaptureBudget {
    CaptureBudget {
        source: BudgetSource::Invocation,
        limits: vec![
            CaptureLimit {
                scope: "stdout".to_string(),
                max_bytes: Some(args.max_stdout_bytes as u64),
                max_duration_ms: Some(args.timeout_ms),
                max_work_units: None,
                extra: Extra::new(),
            },
            CaptureLimit {
                scope: "stderr".to_string(),
                max_bytes: Some(args.max_stderr_bytes as u64),
                max_duration_ms: Some(args.timeout_ms),
                max_work_units: None,
                extra: Extra::new(),
            },
        ],
        extra: Extra::new(),
    }
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
    }
}

fn cache_is_stale(cache: Option<&CacheInfo>) -> bool {
    cache.is_some_and(|cache| {
        matches!((cache.age_seconds, cache.ttl_seconds), (Some(age), Some(ttl)) if age >= ttl)
    })
}

fn cached_pagination_satisfies(pagination: &Value, requested_pages: usize) -> bool {
    let pages_fetched = pagination
        .get("pages_fetched")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let requested_pages = requested_pages.min(50) as u64;
    pagination.get("stop_reason").and_then(Value::as_str) == Some("no_more")
        || pages_fetched >= requested_pages
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

#[allow(clippy::too_many_arguments)]
fn record_capture(
    store: &Store,
    payload_hash: String,
    availability: EvidenceAvailability,
    capture: CaptureCompleteness,
    invocation_fingerprint: String,
    source_id: String,
    operation: String,
    provenance: Option<CallProvenance>,
    cache_key: Option<String>,
    redacted: bool,
    parser: Option<String>,
    lens: Option<&LensManifest>,
    source_state: Option<SourceStateToken>,
) -> Result<String> {
    let duration_ms = provenance.as_ref().and_then(|item| item.duration_ms);
    let status = provenance.as_ref().and_then(|item| item.status.clone());
    let captured_at = provenance.as_ref().map(|item| item.captured_at.clone());
    Ok(store
        .record_observation(NewObservation {
            payload_hash,
            availability,
            invocation_fingerprint,
            source_id,
            operation,
            captured_at,
            duration_ms,
            status,
            capture,
            redacted,
            parser,
            lens: lens.map(|item| item.id.clone()),
            source_state,
            provenance,
            cache_key,
            ..NewObservation::default()
        })?
        .observation_id)
}

fn complete_capture(
    stored_bytes: u64,
    persisted: bool,
    redacted: bool,
) -> (EvidenceAvailability, CaptureCompleteness) {
    let availability = if !persisted {
        EvidenceAvailability::MetadataOnly
    } else if redacted {
        EvidenceAvailability::Redacted
    } else {
        EvidenceAvailability::Recoverable
    };
    let mut capture = CaptureCompleteness::complete(stored_bytes);
    if availability != EvidenceAvailability::Recoverable {
        capture.can_prove_absence = false;
        capture.stop_reason = if redacted {
            CaptureStopReason::Redacted
        } else {
            CaptureStopReason::StorageLimit
        };
    }
    (availability, capture)
}

fn adapter_capture(
    provenance: Option<&CallProvenance>,
    payload: &Value,
    stored_bytes: u64,
    persisted: bool,
    redacted: bool,
) -> (EvidenceAvailability, CaptureCompleteness) {
    let adapter = provenance
        .and_then(|item| item.extra.get("adapter"))
        .and_then(Value::as_object);
    let generic_truncated = adapter
        .and_then(|item| item.get("truncated"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cli_truncated = adapter.is_some_and(|item| {
        ["stdout_truncated", "stderr_truncated"]
            .into_iter()
            .any(|field| item.get(field).and_then(Value::as_bool).unwrap_or(false))
    });
    if cli_truncated {
        return cli_adapter_capture(
            adapter.expect("CLI truncation requires adapter provenance"),
            payload,
            stored_bytes,
        );
    }
    if generic_truncated {
        let response_bytes = adapter
            .and_then(|item| item.get("response_bytes"))
            .and_then(Value::as_u64);
        let mcp_response = adapter.is_some_and(|item| item.contains_key("server_command"));
        let (total_bytes, captured_bytes, stop_reason) = if mcp_response {
            // MCP reports the complete response size before it projects the
            // bounded preview, so this is retention loss rather than a
            // transport capture limit.
            (
                response_bytes,
                response_bytes.unwrap_or(stored_bytes),
                CaptureStopReason::StorageLimit,
            )
        } else {
            // HTTP reports bytes read from the bounded body, but has no
            // trustworthy total once the body limit interrupts the stream.
            (
                None,
                response_bytes.unwrap_or(stored_bytes),
                CaptureStopReason::ByteLimit,
            )
        };
        return (
            EvidenceAvailability::CaptureTruncated,
            CaptureCompleteness {
                total_bytes,
                captured_bytes,
                stored_bytes,
                stop_reason,
                budget: CaptureBudget::default(),
                affected: vec![CaptureScope {
                    scope: "body".to_string(),
                    total_bytes,
                    captured_bytes,
                    stop_reason,
                    extra: Extra::new(),
                }],
                can_prove_absence: false,
                extra: Extra::new(),
            },
        );
    }
    complete_capture(stored_bytes, persisted, redacted)
}

fn cli_adapter_capture(
    adapter: &Map<String, Value>,
    payload: &Value,
    stored_bytes: u64,
) -> (EvidenceAvailability, CaptureCompleteness) {
    let mut total_bytes = 0u64;
    let mut captured_bytes = 0u64;
    let mut affected = Vec::new();
    for stream in ["stdout", "stderr"] {
        let total = adapter
            .get(&format!("{stream}_bytes"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let truncated = adapter
            .get(&format!("{stream}_truncated"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let captured = if truncated {
            cli_stream_captured_bytes(adapter, payload, stream).unwrap_or(0)
        } else {
            total
        };
        total_bytes = total_bytes.saturating_add(total);
        captured_bytes = captured_bytes.saturating_add(captured);
        if truncated {
            affected.push(CaptureScope {
                scope: stream.to_string(),
                total_bytes: Some(total),
                captured_bytes: captured,
                stop_reason: CaptureStopReason::ByteLimit,
                extra: Extra::new(),
            });
        }
    }
    (
        EvidenceAvailability::CaptureTruncated,
        CaptureCompleteness {
            total_bytes: Some(total_bytes),
            captured_bytes,
            stored_bytes,
            stop_reason: CaptureStopReason::ByteLimit,
            budget: CaptureBudget::default(),
            affected,
            can_prove_absence: false,
            extra: Extra::new(),
        },
    )
}

fn cli_stream_captured_bytes(
    adapter: &Map<String, Value>,
    payload: &Value,
    stream: &str,
) -> Option<u64> {
    let output = payload
        .get(stream)
        .or_else(|| (stream == "stdout").then_some(payload));
    output
        .and_then(|value| value.get("byte_count"))
        .and_then(Value::as_u64)
        .or_else(|| {
            adapter
                .get("diagnostics")
                .and_then(|value| value.get(stream))
                .and_then(|value| value.get("byte_count"))
                .and_then(Value::as_u64)
        })
}

fn run_capture_completeness(
    stdout: &RunCapture,
    stderr: &RunCapture,
    stored_bytes: u64,
    redacted: bool,
) -> (EvidenceAvailability, CaptureCompleteness) {
    let truncated = stdout.truncated || stderr.truncated;
    let captured_bytes = stdout.bytes.len().saturating_add(stderr.bytes.len()) as u64;
    let total_bytes = stdout.total_bytes.saturating_add(stderr.total_bytes) as u64;
    let reason = if truncated {
        CaptureStopReason::ByteLimit
    } else if redacted {
        CaptureStopReason::Redacted
    } else {
        CaptureStopReason::Complete
    };
    let availability = if truncated {
        EvidenceAvailability::CaptureTruncated
    } else if redacted {
        EvidenceAvailability::Redacted
    } else {
        EvidenceAvailability::Recoverable
    };
    (
        availability,
        CaptureCompleteness {
            total_bytes: Some(total_bytes),
            captured_bytes,
            stored_bytes,
            stop_reason: reason,
            budget: CaptureBudget::default(),
            affected: vec![
                CaptureScope {
                    scope: "stdout".to_string(),
                    total_bytes: Some(stdout.total_bytes as u64),
                    captured_bytes: stdout.bytes.len() as u64,
                    stop_reason: if stdout.truncated {
                        CaptureStopReason::ByteLimit
                    } else {
                        CaptureStopReason::Complete
                    },
                    extra: Extra::new(),
                },
                CaptureScope {
                    scope: "stderr".to_string(),
                    total_bytes: Some(stderr.total_bytes as u64),
                    captured_bytes: stderr.bytes.len() as u64,
                    stop_reason: if stderr.truncated {
                        CaptureStopReason::ByteLimit
                    } else {
                        CaptureStopReason::Complete
                    },
                    extra: Extra::new(),
                },
            ],
            can_prove_absence: !truncated && !redacted,
            extra: Extra::new(),
        },
    )
}

fn source_state_from_provenance(
    kind: prog_core::SourceKind,
    source_id: &str,
    operation: &str,
    invocation: &Value,
    provenance: &CallProvenance,
) -> Result<Option<SourceStateToken>> {
    if kind != prog_core::SourceKind::Http {
        return Ok(None);
    }
    let headers = provenance
        .extra
        .get("adapter")
        .and_then(|adapter| adapter.get("selected_headers"))
        .and_then(Value::as_object)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .as_str()
                        .map(|value| (name.to_ascii_lowercase(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    http_source_state(
        source_id,
        operation,
        invocation,
        &headers,
        &provenance.captured_at,
    )
}

fn cursor_lens_extra(lens: Option<&LensManifest>) -> Extra {
    let mut extra = Extra::new();
    if let Some(lens) = lens {
        extra.insert("lens_id".to_string(), json!(lens.id));
    }
    extra
}

fn cursor_for_projection(store: &Store, input: CursorInput<'_>) -> Result<Option<String>> {
    if !input.may_cache {
        return Ok(None);
    }
    // Validate the projected root before minting the cursor. Cacheable calls
    // always get a cursor so inspect/search/evidence work even when the first
    // preview happens to contain the entire small payload.
    project_with_lens(
        input.payload,
        input.root_path,
        input.slice,
        &PreviewPolicy::default(),
        input.lens,
    )?;
    Ok(Some(store.create_cursor_with_extra(
        input.cache_key,
        input.source_id,
        input.operation,
        input.root_path,
        RedactionPolicy::default().version,
        ttl_seconds(input.cache),
        cursor_lens_extra(input.lens),
    )?))
}

fn envelope_for_payload(
    store: &Store,
    input: EnvelopeInput,
    cursor: Option<String>,
) -> Result<DisclosureEnvelope> {
    let observation_record = input
        .observation_id
        .as_deref()
        .map(|id| store.get_observation(id))
        .transpose()?
        .flatten();
    let mut policy = PreviewPolicy {
        max_envelope_bytes: response_budget_bytes(),
        ..PreviewPolicy::default()
    };
    let mut last = None;
    let findings = ranked_findings_with_lens(
        input.payload.as_value(),
        &FindingOptions {
            goal: None,
            cursor: cursor.clone(),
            scope_path: Some(input.root_path.clone()),
            limit: 3,
            hints: CommandHintConfig::NAV_ALL,
        },
        input.lens.as_ref(),
    )
    .unwrap_or_default();
    for _ in 0..16 {
        let lens_projection = project_with_lens(
            &input.payload,
            &input.root_path,
            &input.slice,
            &policy,
            input.lens.as_ref(),
        )?;
        let mut envelope = make_envelope(
            &input,
            lens_projection,
            cursor.clone(),
            findings.clone(),
            observation_record.as_ref(),
        );
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
    if serde_json::to_vec(&envelope)?.len() > policy.max_envelope_bytes {
        envelope.schema_hints.clear();
        envelope.provenance = None;
        envelope.findings.truncate(1);
        envelope.next_actions.truncate(4);
        envelope.omitted.truncate(8);
        envelope.warnings.truncate(4);
        envelope
            .warnings
            .push("envelope metadata compacted to enforce max_envelope_bytes".to_string());
        finalize_envelope_bytes(&mut envelope)?;
    }
    if serde_json::to_vec(&envelope)?.len() > policy.max_envelope_bytes {
        envelope.data_preview =
            Value::String("«preview omitted to enforce envelope budget»".to_string());
        envelope.omitted.clear();
        envelope.next_actions.clear();
        envelope.warnings.truncate(1);
        finalize_envelope_bytes(&mut envelope)?;
    }
    compact_envelope_to_budget(&mut envelope)?;
    Ok(envelope)
}

fn make_envelope(
    input: &EnvelopeInput,
    lens_projection: prog_core::LensProjection,
    cursor: Option<String>,
    findings: Vec<prog_core::Finding>,
    observation_record: Option<&prog_core::ObservationRecord>,
) -> DisclosureEnvelope {
    let projection = lens_projection.projection;
    let preview = projection.preview;
    let omitted = projection.omitted;
    let observation = observation_metadata(input, &omitted, cursor.as_deref(), observation_record);
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
                &omitted,
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
                "id": lens.id
            }),
        );
    }
    if let Some(cursor) = cursor.as_deref() {
        let evidence_path = input.slice.path.as_deref().unwrap_or(&input.root_path);
        extra.insert(
            "evidence_ref".to_string(),
            serde_json::to_value(evidence_ref(EvidenceRefInput {
                source_id: &input.source_id,
                operation: &input.operation,
                cursor: Some(cursor),
                path: evidence_path,
                value: &preview,
                observation: observation_record,
                provenance: input.provenance.as_ref(),
                cache: input.cache.as_ref(),
                omitted: &omitted,
                redacted_paths: input.redacted_paths,
            }))
            .unwrap_or(Value::Null),
        );
    }
    DisclosureEnvelope {
        schema: DISCLOSURE_SCHEMA.to_string(),
        source_id: Some(input.source_id.clone()),
        operation: Some(input.operation.clone()),
        summary: Summary {
            kind: value_kind(input.payload.as_value()).to_string(),
            item_count: item_count(input.payload.as_value()),
            preview_count: item_count(&preview),
            payload_bytes: input.payload_bytes,
            approx_tokens: 0,
            envelope_bytes: None,
            extra: Extra::new(),
        },
        data_preview: preview,
        schema_hints: input.schema_hints.clone(),
        omitted,
        findings,
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
    observation_record: Option<&prog_core::ObservationRecord>,
) -> ObservationMetadata {
    let redacted_omissions = omitted
        .iter()
        .filter(|region| region.reason == OmissionReason::Redacted)
        .count();
    let redacted_count = input.redacted_paths.max(redacted_omissions);
    let truncated = omitted
        .iter()
        .any(|region| region.reason != OmissionReason::Redacted);
    let effective_root_path = input
        .slice
        .path
        .as_deref()
        .unwrap_or(&input.root_path)
        .to_string();
    let path_scoped = !effective_root_path.is_empty()
        || input.slice.path.is_some()
        || !input.slice.fields.is_empty()
        || !input.slice.omit.is_empty();
    let preview_complete = omitted.is_empty();
    let completeness_status = if truncated {
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
    let stale = cache_is_stale(input.cache.as_ref());
    let sensitive_cache_disabled = matches!(cache_status, Some(CacheStatus::Skipped))
        && input
            .effects
            .as_ref()
            .is_some_and(|effects| effects.sensitive);
    let mut metadata_extra = Extra::new();
    // Surface value-scan lossiness: when low-confidence secret-like shapes were
    // observed (and, by default, preserved verbatim), OR-fold that uncertainty
    // into the parser's `lossy`/`confidence` AND emit a disambiguating
    // `value_scan` extra entry so the cause is inspectable. When nothing was
    // observed, behavior is byte-identical to today.
    let parser_value = match (&input.observation_parser, &input.value_scan) {
        (Some(parser), Some(scan)) if scan.lossy() => {
            let mut folded = parser.clone();
            if let Some(obj) = folded.as_object_mut() {
                obj.insert("lossy".to_string(), Value::Bool(true));
                if let Some(confidence) = obj.get("confidence").and_then(Value::as_f64) {
                    obj.insert("confidence".to_string(), Value::from(confidence.min(0.6)));
                }
            }
            Some(folded)
        }
        (Some(parser), _) => Some(parser.clone()),
        _ => None,
    };
    if let Some(parser) = parser_value {
        metadata_extra.insert("parser".to_string(), parser);
    }
    if let Some(scan) = input.value_scan.as_ref().filter(|scan| scan.lossy()) {
        metadata_extra.insert(
            "value_scan".to_string(),
            json!({
                "lossy": true,
                "high_confidence_count": scan.high_confidence_redactions,
                "low_confidence_count": scan.low_confidence_observations,
            }),
        );
    }
    ObservationMetadata {
        observation_id: input.observation_id.clone(),
        completeness: ObservationCompleteness {
            status: completeness_status.to_string(),
            preview_complete,
            path_scoped,
            truncated,
            redacted: redacted_count > 0,
            omitted_count: omitted.len().try_into().unwrap_or(u64::MAX),
            redacted_count: redacted_count.try_into().unwrap_or(u64::MAX),
            root_path: effective_root_path,
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
            stale_after_seconds: input.cache.as_ref().and_then(|cache| cache.ttl_seconds),
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
            extra: {
                let mut trust_extra = Extra::new();
                // Surface the graded-evidence auto-upgrade provenance: when a
                // *proven* read-only op had its confirmation relaxed for this
                // call, record the evidence chain (grade + reason) so the
                // decision is inspectable. The relaxed EffectSet (carrying its
                // own extra["auto_upgrade"] stamp) flows to safety.effects.
                if let Some(reason) = &input.auto_upgrade_audit {
                    let grade = input
                        .effects
                        .as_ref()
                        .map(|effects| EvidenceGrade::from_extra(&effects.extra).as_str())
                        .unwrap_or("proven");
                    trust_extra.insert(
                        "auto_upgrade".to_string(),
                        json!({
                            "grade": grade,
                            "relaxed_requires_confirmation": true,
                            "reason": reason,
                        }),
                    );
                }
                trust_extra
            },
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
        availability: observation_record
            .map(|record| record.availability)
            .unwrap_or(EvidenceAvailability::Unavailable),
        capture: Some(observation_record.map_or_else(
            || CaptureCompleteness {
                total_bytes: None,
                captured_bytes: 0,
                stored_bytes: input.payload_bytes,
                stop_reason: CaptureStopReason::Unavailable,
                budget: CaptureBudget::unavailable(),
                affected: Vec::new(),
                can_prove_absence: false,
                extra: Extra::new(),
            },
            |record| record.capture.clone(),
        )),
        extra: metadata_extra,
    }
}

fn finalize_envelope_bytes(envelope: &mut DisclosureEnvelope) -> Result<usize> {
    // Both fields describe the delivered JSON, including their own encoded
    // digits. Iterate to the small fixed point rather than estimating from
    // the much larger cached payload.
    for _ in 0..8 {
        let bytes = serde_json::to_vec(envelope)?.len();
        let envelope_bytes = bytes.try_into().unwrap_or(u64::MAX);
        let approx_tokens = envelope_bytes.saturating_add(3) / 4;
        if envelope.summary.envelope_bytes == Some(envelope_bytes)
            && envelope.summary.approx_tokens == approx_tokens
        {
            return Ok(bytes);
        }
        envelope.summary.envelope_bytes = Some(envelope_bytes);
        envelope.summary.approx_tokens = approx_tokens;
    }
    Err(CoreError::Storage(
        "envelope size accounting did not converge".to_string(),
    ))
}

fn compact_envelope_to_budget(envelope: &mut DisclosureEnvelope) -> Result<()> {
    let budget = response_budget_bytes();
    while serde_json::to_vec(envelope)?.len() > budget && !envelope.findings.is_empty() {
        envelope.findings.pop();
    }
    if serde_json::to_vec(envelope)?.len() > budget
        && let Some(recipe) = envelope
            .extra
            .get_mut("recipe")
            .and_then(Value::as_object_mut)
    {
        recipe.remove("expanded_commands");
    }
    if serde_json::to_vec(envelope)?.len() > budget {
        envelope.data_preview = json!("preview omitted to enforce envelope budget");
        envelope.omitted.truncate(4);
        envelope.next_actions.truncate(4);
        envelope.warnings.truncate(2);
    }
    if serde_json::to_vec(envelope)?.len() > budget {
        // Keep the observation identity and cursor, which are the recovery
        // path for the payload, while dropping derivable presentation detail.
        envelope.provenance = None;
        envelope.cache = None;
        envelope.schema_hints.clear();
        envelope.extra.clear();
        envelope.omitted.truncate(1);
        envelope.next_actions.truncate(1);
        envelope.warnings.truncate(1);
    }
    finalize_envelope_bytes(envelope)?;
    Ok(())
}

/// Re-enforce `max_envelope_bytes` after the pagination `extra` block is
/// appended. The per-page `pages[]` index and the `merged_shape` grow with page
/// count and schema width, so a many-page or wide-shape call could push the
/// final envelope past the 16 KiB ceiling even though page 1 was bounded.
/// Progressively drop `pages[]` then `merged_shape` (keeping the tiny scalar
/// counters) until the serialized envelope fits, recording a warning each time
/// (invariant I11: pagination never escapes the envelope budget).
fn compact_pagination_extra_to_budget(envelope: &mut DisclosureEnvelope) -> Result<()> {
    let budget = response_budget_bytes();
    if serde_json::to_vec(envelope)?.len() <= budget {
        return Ok(());
    }
    let dropped_pages = envelope
        .extra
        .get_mut("pagination")
        .and_then(Value::as_object_mut)
        .is_some_and(|pagination| pagination.remove("pages").is_some());
    if dropped_pages {
        envelope
            .warnings
            .push("pagination page index compacted to enforce max_envelope_bytes".to_string());
    }
    if serde_json::to_vec(envelope)?.len() <= budget {
        finalize_envelope_bytes(envelope)?;
        return Ok(());
    }
    let dropped_shape = envelope
        .extra
        .get_mut("pagination")
        .and_then(Value::as_object_mut)
        .is_some_and(|pagination| pagination.remove("merged_shape").is_some());
    if dropped_shape {
        envelope
            .warnings
            .push("pagination merged shape compacted to enforce max_envelope_bytes".to_string());
    }
    finalize_envelope_bytes(envelope)?;
    Ok(())
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
    let redacted_args = redacted_profile_args(operation, args);
    let example = json!({
        "args": redacted_args,
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

fn redacted_profile_args(operation: &OperationProfile, args: &Value) -> Value {
    let sensitive_args = operation_sensitive_args(operation);
    RedactionPolicy::with_extra_persistence_names(&sensitive_args)
        .apply_persistence(args)
        .0
}

fn operation_sensitive_args(operation: &OperationProfile) -> Vec<String> {
    let mut names = BTreeSet::new();
    if let Some(invocation) = operation.extra.get("invocation").and_then(Value::as_object) {
        for kind in ["http", "cli"] {
            if let Some(config) = invocation.get(kind).and_then(Value::as_object)
                && let Some(values) = config.get("sensitive_args").and_then(Value::as_array)
            {
                names.extend(values.iter().filter_map(Value::as_str).map(str::to_string));
            }
        }
    }
    names.into_iter().collect()
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

fn merge_profiles(current: Option<SourceProfile>, mut authored: SourceProfile) -> SourceProfile {
    let Some(current) = current else {
        return authored;
    };

    if authored.disclosure_budget.is_none() {
        authored.disclosure_budget = current.disclosure_budget.clone();
    }

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

    for operation in &selected {
        let (effects, _) = effective_effects(&operation.effects, &profile.trust);
        let cache = effective_cache_policy(profile, operation);
        operations.push(operation_hint(operation, &effects, &cache));
    }

    Ok(json!({
        "source_id": profile.id,
        "kind": profile.kind,
        "revision": profile.revision,
        "operation_count": profile.operations.len(),
        "operations": operations,
        "suggested_next_calls": selected.iter().take(10).map(|operation| {
            json!({"kind": "call", "operation": operation.id, "reason": "operation is available in the source profile"})
        }).collect::<Vec<_>>()
    }))
}

fn operation_hint(operation: &OperationProfile, effects: &EffectSet, cache: &CachePolicy) -> Value {
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
        "effects": effects,
        "cache": cache,
        "risk_notes": risk_notes(effects),
        "next_actions": [
            NextAction {
                kind: "call".to_string(),
                operation: Some(operation.id.clone()),
                path: None,
                reason: Some("run this operation with JSON args".to_string()),
                extra: Extra::new(),
                ..NextAction::default()
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
    let rendered = render_budgeted_json(serde_json::to_value(value)?, pretty)?;
    println!("{rendered}");
    Ok(())
}

fn write_error(error: &CoreError, pretty: bool) -> ExitCode {
    let rendered = serde_json::to_value(error.envelope())
        .map_err(CoreError::from)
        .and_then(|value| render_budgeted_json(value, pretty));
    match rendered {
        Ok(json) => {
            println!("{json}");
            ExitCode::FAILURE
        }
        Err(_) => {
            let budget = disclosure_budget();
            let fallback = json!({
                "error": {
                    "kind": "budget_too_small",
                    "message": format!("response cannot fit in {} bytes", budget.effective_bytes),
                    "hint": format!("Raise --budget-bytes to at least {MIN_DISCLOSURE_BUDGET_BYTES}.")
                }
            });
            println!(
                "{}",
                serde_json::to_string(&fallback).unwrap_or_else(|_| {
                    "{\"error\":{\"kind\":\"json\",\"message\":\"failed to render error\",\"hint\":\"Report this bug.\"}}".to_string()
                })
            );
            ExitCode::FAILURE
        }
    }
}

fn render_budgeted_json(mut value: Value, pretty: bool) -> Result<String> {
    if !value.is_object() {
        value = json!({"result": value});
    }
    let budget = disclosure_budget();
    let capture_budget = response_capture_budget();
    let storage_budget = response_storage_budget();
    let mut metadata = json!({
        "source": budget.source,
        "requested_bytes": budget.requested_bytes,
        "requested_tokens": budget.requested_tokens,
        "effective_bytes": budget.effective_bytes,
        "token_estimator": TOKEN_ESTIMATOR,
        "actual_bytes": 0
    });
    value
        .as_object_mut()
        .expect("response value is an object")
        .insert("disclosure_budget".to_string(), metadata.clone());
    value
        .as_object_mut()
        .expect("response value is an object")
        .insert(
            "capture_budget".to_string(),
            serde_json::to_value(capture_budget)?,
        );
    value
        .as_object_mut()
        .expect("response value is an object")
        .insert(
            "storage_budget".to_string(),
            serde_json::to_value(storage_budget)?,
        );
    let mut use_pretty = pretty;
    for _ in 0..8 {
        let rendered = if use_pretty {
            serde_json::to_string_pretty(&value)?
        } else {
            serde_json::to_string(&value)?
        };
        // The trailing newline is part of stdout and therefore part of the
        // hard caller-visible byte ceiling.
        let bytes = rendered.len().saturating_add(1);
        if bytes > budget.effective_bytes && use_pretty {
            use_pretty = false;
            continue;
        }
        if bytes > budget.effective_bytes {
            return Err(CoreError::BudgetTooSmall {
                requested_bytes: budget.effective_bytes,
                minimum_bytes: bytes,
            });
        }
        metadata["actual_bytes"] = json!(bytes);
        value
            .as_object_mut()
            .expect("response value is an object")
            .insert("disclosure_budget".to_string(), metadata.clone());
        let final_rendered = if use_pretty {
            serde_json::to_string_pretty(&value)?
        } else {
            serde_json::to_string(&value)?
        };
        if final_rendered.len().saturating_add(1) == bytes {
            return Ok(final_rendered);
        }
    }
    Err(CoreError::Storage(
        "disclosure budget accounting did not converge".to_string(),
    ))
}

#[cfg(test)]
mod capture_lifecycle_tests {
    use super::*;

    fn provenance(adapter: Value) -> CallProvenance {
        let mut extra = Extra::new();
        extra.insert("adapter".to_string(), adapter);
        CallProvenance {
            source_call_id: "call_test".to_string(),
            cache_key: None,
            captured_at: "2026-07-13T00:00:00Z".to_string(),
            status: None,
            duration_ms: None,
            extra,
        }
    }

    #[test]
    fn cli_stdout_truncation_cannot_be_marked_recoverable() {
        let provenance = provenance(json!({
            "stdout_bytes": 100,
            "stderr_bytes": 20,
            "stdout_truncated": true,
            "stderr_truncated": false,
            "diagnostics": {"stderr": {"byte_count": 20}}
        }));
        let (availability, capture) = adapter_capture(
            Some(&provenance),
            &json!({"format": "text", "byte_count": 64, "truncated": true}),
            90,
            true,
            false,
        );

        assert_eq!(availability, EvidenceAvailability::CaptureTruncated);
        assert_eq!(capture.total_bytes, Some(120));
        assert_eq!(capture.captured_bytes, 84);
        assert!(!capture.can_prove_absence);
        assert_eq!(capture.affected.len(), 1);
        assert_eq!(capture.affected[0].scope, "stdout");
        assert_eq!(capture.affected[0].total_bytes, Some(100));
        assert_eq!(capture.affected[0].captured_bytes, 64);
    }

    #[test]
    fn cli_stderr_truncation_uses_diagnostic_capture_accounting() {
        let provenance = provenance(json!({
            "stdout_bytes": 20,
            "stderr_bytes": 100,
            "stdout_truncated": false,
            "stderr_truncated": true,
            "diagnostics": {"stderr": {"byte_count": 64, "truncated": true}}
        }));
        let (availability, capture) =
            adapter_capture(Some(&provenance), &json!({"ok": true}), 90, true, false);

        assert_eq!(availability, EvidenceAvailability::CaptureTruncated);
        assert_eq!(capture.total_bytes, Some(120));
        assert_eq!(capture.captured_bytes, 84);
        assert!(!capture.can_prove_absence);
        assert_eq!(capture.affected.len(), 1);
        assert_eq!(capture.affected[0].scope, "stderr");
        assert_eq!(capture.affected[0].total_bytes, Some(100));
        assert_eq!(capture.affected[0].captured_bytes, 64);
    }

    #[test]
    fn mcp_truncation_retains_its_known_total() {
        let provenance = provenance(json!({
            "server_command": ["mcp-server"],
            "response_bytes": 4096,
            "truncated": true
        }));
        let (availability, capture) = adapter_capture(
            Some(&provenance),
            &json!({"preview": "..."}),
            128,
            true,
            false,
        );

        assert_eq!(availability, EvidenceAvailability::CaptureTruncated);
        assert_eq!(capture.total_bytes, Some(4096));
        assert_eq!(capture.captured_bytes, 4096);
        assert_eq!(capture.stored_bytes, 128);
        assert_eq!(capture.stop_reason, CaptureStopReason::StorageLimit);
        assert_eq!(capture.affected[0].total_bytes, Some(4096));
        assert!(!capture.can_prove_absence);
    }

    #[test]
    fn evidence_ref_without_an_observation_fails_closed() {
        let value = json!({"status": "unknown"});
        let reference = evidence_ref(EvidenceRefInput {
            source_id: "test",
            operation: "read",
            cursor: None,
            path: "/status",
            value: &value,
            observation: None,
            provenance: None,
            cache: None,
            omitted: &[],
            redacted_paths: 0,
        });

        assert_eq!(reference.availability, EvidenceAvailability::Unavailable);
        assert_eq!(
            reference.capture.stop_reason,
            CaptureStopReason::Unavailable
        );
        assert!(!reference.capture.can_prove_absence);
    }

    #[test]
    fn refresh_warning_requires_an_expired_cache_budget() {
        let fresh = CacheInfo {
            status: CacheStatus::Hit,
            ttl_seconds: Some(60),
            expires_at: None,
            age_seconds: Some(59),
        };
        let expired = CacheInfo {
            age_seconds: Some(60),
            ..fresh.clone()
        };

        assert!(!cache_is_stale(Some(&fresh)));
        assert!(cache_is_stale(Some(&expired)));
        assert!(!cache_is_stale(None));
    }
}

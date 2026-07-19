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
    http::{DEFAULT_MAX_RESPONSE_BYTES, HttpOperation, HttpSource},
    mcp::{McpSource, McpTaskResult},
};
use prog_core::importers::{
    ImportContext, ImportReport, import_cli_help, import_json_schema, import_openapi,
};
use prog_core::{
    AuthRef, BudgetSource, CacheEntryMeta, CacheInfo, CachePolicy, CacheStatus, CallFlags,
    CallProvenance, CaptureBudget, CaptureCompleteness, CaptureLimit, CaptureScope,
    CaptureStopReason, CommandHintConfig, CoreError, DISCLOSURE_SCHEMA, DisclosureBudget,
    DisclosureEnvelope, EffectSet, EvidenceAvailability, EvidenceBlock, EvidenceGrade, EvidenceRef,
    ExpansionScope, Extra, FindingIdentityContext, FindingOptions, InspectRequest, InspectResponse,
    LensManifest, NewObservation, NewSessionEvent, NextAction, ObligationDeclarer,
    ObligationEvaluation, ObservationCompleteness, ObservationFreshness, ObservationMetadata,
    ObservationPayloadStatus, ObservationSafety, ObservationTrust, OmissionReason, OmittedRegion,
    OperationProfile, PersistedPayload, PreviewPolicy, RawPayload, ReadinessReport,
    RedactedPayload, RedactionPolicy, Result, SOURCE_PROFILE_SCHEMA, ScopedSlice, SearchOptions,
    SearchResponse, SelectionCoverage, SliceRequest, SourceProfile, SourceStateToken,
    StorageBudget, Store, Summary, TrustSettings, VERIFICATION_SCHEMA, ValidatedCursor,
    ValueScanReport, VerificationObligation, VerificationOperation, VerificationStateRelationship,
    VerificationStatus, build_inspect_response, cache_allowed, call_effect_warnings,
    canonical_json, check_call, check_discovery, cli_adapter_effects, cli_hardening_effects,
    effective_effects, evidence_block, expand, http_adapter_effects, http_hardening_effects,
    http_source_state, infer, join, lens_slice_request, new_cache_entry, project,
    project_with_lens, public_contract_schemas, ranked_findings_with_lens, render_hints,
    search_payload_with_lens, slice_value, tighten_effects, validate_lens_manifest,
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

mod commands;

use commands::{
    adapters::*,
    cache::cache_command,
    call::call_source,
    cost::cost_report,
    delta::{compare_observation_ids, delta_observations},
    discover::{discover_from_seed, discover_source, profile_operation},
    envelope::{
        adapter_capture, compact_envelope_to_budget, compact_pagination_extra_to_budget,
        complete_capture, cursor_for_projection, cursor_lens_extra, envelope_for_payload,
        evidence_ref, record_capture, run_capture_completeness, selection_coverage, shrink_policy,
        source_state_from_provenance,
    },
    expand::expand_cursor,
    hints::hints_source,
    init::init_integration,
    lenses::{
        load_lens, parse_json_argument, parse_view, validate_lens_matches_call,
        validate_lens_matches_observe, validate_lens_matches_run,
    },
    mcp_task::mcp_task_command,
    meta::meta_contracts,
    navigation::{evidence_cursor, inspect_cursor, search_cursor},
    observe::{
        normalize_observation, observe_artifact, redact_observed_text, sniff_mime_from_bytes,
    },
    paths::{
        annotate_path_omissions, append_missing_omitted_paths, collect_paths,
        expansion_next_actions, paths_cursor,
    },
    profiles::*,
    recipe::run_recipe,
    run::{RunProcessStatus, child_exit_code, redact_run_argv, run_command},
    session::{declare_recipe_obligation, readiness_report, session_show},
    source::{shell_quote, source_command},
};

#[cfg(test)]
use commands::run::{
    cargo_test_target_actions, go_test_target_actions, jest_vitest_target_actions,
    targeted_rerun_actions,
};

static RUN_CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
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

/// Per-invocation budgets threaded explicitly through one CLI run.
///
/// Replaces the three former process-global locked singletons so that the
/// disclosure precedence (flag → environment → profile → default) is enforced
/// by construction in [`Self::apply_profile_disclosure`] instead of depending
/// on call ordering between `resolve_disclosure_budget` and
/// `apply_profile_disclosure_budget`, and so that two distinct invocations can
/// coexist in one process (required for unit tests and for the #120 host
/// facade).
pub(crate) struct InvocationContext {
    disclosure: EffectiveDisclosureBudget,
    capture: CaptureBudget,
    storage: StorageBudget,
}

impl InvocationContext {
    fn new(disclosure: EffectiveDisclosureBudget) -> Self {
        Self {
            disclosure,
            capture: CaptureBudget::unavailable(),
            storage: StorageBudget::default(),
        }
    }

    /// Context for error rendering before the disclosure budget resolves
    /// (argument-parse or budget-resolution failures). Matches the previous
    /// `get_or_init` default so early error envelopes stay byte-identical.
    fn for_unresolved_budget() -> Self {
        Self::new(EffectiveDisclosureBudget {
            requested_bytes: None,
            requested_tokens: None,
            source: "default",
            effective_bytes: DEFAULT_DISCLOSURE_BUDGET_BYTES,
        })
    }

    /// Largest envelope body that fits under the disclosure budget once the
    /// reserved metadata overhead is subtracted.
    pub(crate) fn max_envelope_bytes(&self) -> usize {
        self.disclosure
            .effective_bytes
            .saturating_sub(BUDGET_METADATA_RESERVE_BYTES)
    }

    /// Apply a profile-owned disclosure ceiling only when no higher-precedence
    /// source (flag or environment) already set the budget. This is the
    /// previously-implicit `source != "default"` guard made explicit.
    pub(crate) fn apply_profile_disclosure(&mut self, profile: &SourceProfile) -> Result<()> {
        let Some(DisclosureBudget { max_bytes, .. }) = &profile.disclosure_budget else {
            return Ok(());
        };
        if self.disclosure.source != "default" {
            return Ok(());
        }
        self.disclosure = effective_disclosure_budget(Some(*max_bytes), None, "profile")?;
        Ok(())
    }

    pub(crate) fn set_capture(&mut self, budget: CaptureBudget) {
        self.capture = budget;
    }

    pub(crate) fn set_storage(&mut self, budget: StorageBudget) {
        self.storage = budget;
    }
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
    McpTask {
        #[command(subcommand)]
        command: McpTaskCommand,
    },
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

#[derive(Debug, Subcommand)]
enum McpTaskCommand {
    Start(McpTaskStartArgs),
    Get(McpTaskReferenceArgs),
    Result(McpTaskReferenceArgs),
    Cancel(McpTaskReferenceArgs),
}

#[derive(Debug, Args)]
struct McpTaskStartArgs {
    source_id: String,
    operation: String,
    #[arg(long)]
    args: String,
    #[arg(long)]
    ttl_ms: Option<u64>,
    #[arg(long)]
    yes: bool,
    #[arg(long)]
    parent_observation: Option<String>,
}

#[derive(Debug, Args)]
struct McpTaskReferenceArgs {
    source_id: String,
    task_id: String,
    #[arg(long)]
    parent_observation: Option<String>,
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

    /// Canonical family used to decide whether successive observations may be compared.
    #[arg(long)]
    comparison_family: Option<String>,

    /// Stable logical scope included in this capture; repeat for collections.
    #[arg(long = "selection-scope")]
    selection_scopes: Vec<String>,

    /// Assert that all supplied selection scopes are exhaustively represented.
    #[arg(long, requires = "selection_scopes")]
    selection_exhaustive: bool,

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

    /// Canonical family used to decide whether successive observations may be compared.
    #[arg(long)]
    comparison_family: Option<String>,

    /// Stable logical scope included in this capture; repeat for collections.
    #[arg(long = "selection-scope")]
    selection_scopes: Vec<String>,

    /// Assert that all supplied selection scopes are exhaustively represented.
    #[arg(long, requires = "selection_scopes")]
    selection_exhaustive: bool,

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

    /// Canonical family used to decide whether successive observations may be compared.
    #[arg(long)]
    comparison_family: Option<String>,

    /// Stable logical scope included in this capture; repeat for collections.
    #[arg(long = "selection-scope")]
    selection_scopes: Vec<String>,

    /// Assert that all supplied selection scopes are exhaustively represented.
    #[arg(long, requires = "selection_scopes")]
    selection_exhaustive: bool,

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

    /// Canonical family used to decide whether successive observations may be compared.
    #[arg(long)]
    comparison_family: Option<String>,

    /// Stable logical scope included in this capture; repeat for collections.
    #[arg(long = "selection-scope")]
    selection_scopes: Vec<String>,

    /// Assert that all supplied selection scopes are exhaustively represented.
    #[arg(long, requires = "selection_scopes")]
    selection_exhaustive: bool,

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
    ObligationAdd(Box<ObligationAddArgs>),
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

    /// Declarer of this obligation. Non-user declarations are always advisory.
    #[arg(long, value_enum, default_value_t = ObligationDeclarerArg::User)]
    declared_by: ObligationDeclarerArg,

    /// Exact argv represented by suitable evidence; never interpreted as a shell command.
    #[arg(long, num_args = 1.., conflicts_with = "source_operation")]
    expected_argv: Vec<String>,

    /// Source-native operation represented by suitable evidence.
    #[arg(long, conflicts_with = "expected_argv")]
    source_operation: Option<String>,

    /// Required validity relationship for workspace and source state.
    #[arg(long, value_enum, default_value_t = StateRelationshipArg::Any)]
    required_state: StateRelationshipArg,

    /// Advisory exact argv hint. It is displayed only and is never auto-run.
    #[arg(long, num_args = 1..)]
    advisory_argv: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum ObligationDeclarerArg {
    #[default]
    User,
    Recipe,
    Normalizer,
    Harness,
}

impl From<ObligationDeclarerArg> for ObligationDeclarer {
    fn from(value: ObligationDeclarerArg) -> Self {
        match value {
            ObligationDeclarerArg::User => Self::User,
            ObligationDeclarerArg::Recipe => Self::Recipe,
            ObligationDeclarerArg::Normalizer => Self::Normalizer,
            ObligationDeclarerArg::Harness => Self::Harness,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum StateRelationshipArg {
    #[default]
    Any,
    WorkspaceUnchanged,
    SourceUnchanged,
    WorkspaceAndSourceUnchanged,
}

impl From<StateRelationshipArg> for VerificationStateRelationship {
    fn from(value: StateRelationshipArg) -> Self {
        match value {
            StateRelationshipArg::Any => Self::Any,
            StateRelationshipArg::WorkspaceUnchanged => Self::WorkspaceUnchanged,
            StateRelationshipArg::SourceUnchanged => Self::SourceUnchanged,
            StateRelationshipArg::WorkspaceAndSourceUnchanged => Self::WorkspaceAndSourceUnchanged,
        }
    }
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
            return write_error(&error, false, &InvocationContext::for_unresolved_budget());
        }
    };

    let mut ctx = match resolve_disclosure_budget(&cli) {
        Ok(budget) => InvocationContext::new(budget),
        Err(error) => {
            return write_error(
                &error,
                cli.pretty,
                &InvocationContext::for_unresolved_budget(),
            );
        }
    };

    match run(&cli, &mut ctx).await {
        Ok(exit_code) => exit_code,
        Err(error) => write_error(&error, cli.pretty, &ctx),
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

fn open_store(dir: &Path, ctx: &mut InvocationContext) -> Result<Store> {
    let store = Store::open(dir)?;
    ctx.set_storage(store.storage_budget()?);
    Ok(store)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr.with_max_level(tracing::Level::TRACE))
        .try_init();
}

async fn run(cli: &Cli, ctx: &mut InvocationContext) -> Result<ExitCode> {
    match &cli.command {
        Command::Discover(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let report = discover_source(&store, args).await?;
            write_success(&report, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Source { command } => {
            let store = open_store(&cli.dir, ctx)?;
            let report = source_command(&store, &cli.dir, command).await?;
            write_success(&report, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Hints(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let response = hints_source(&store, args, ctx)?;
            write_success(&response, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Call(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let mut result = call_source(&store, &cli.lens_dir, args, ctx).await?;
            record_envelope_event(&store, &mut result.envelope, "call", ctx);
            write_success(&result.envelope, cli.pretty, ctx)?;
            Ok(if result.received_error {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Observe(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let mut envelope = observe_artifact(&store, &cli.lens_dir, args, ctx)?;
            record_envelope_event(&store, &mut envelope, "observe", ctx);
            write_success(&envelope, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Run(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let mut result = run_command(&store, &cli.lens_dir, args, ctx).await?;
            record_envelope_event(&store, &mut result.envelope, "run", ctx);
            write_success(&result.envelope, cli.pretty, ctx)?;
            Ok(if args.preserve_exit_code {
                child_exit_code(result.exit_code)
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Recipe(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let mut envelope = run_recipe(&store, &cli.lens_dir, args, ctx).await?;
            declare_recipe_obligation(&store, args, &envelope)?;
            record_envelope_event(&store, &mut envelope, "recipe", ctx);
            write_success(&envelope, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Init(args) => {
            let report = init_integration(args)?;
            write_success(&report, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Cost(args) => {
            let report = cost_report(args)?;
            write_success(&report, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Paths(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let response = paths_cursor(&store, args)?;
            record_navigation_event(
                &store,
                "paths",
                Some(&args.cursor),
                Some(&response.prefix),
                None,
                Some(format!("listed {} cached path(s)", response.paths.len())),
            );
            write_success(&response, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Inspect(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let response = inspect_cursor(&store, &cli.lens_dir, args, ctx)?;
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
            write_success(&response, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Evidence(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let response = evidence_cursor(&store, &cli.lens_dir, args, ctx)?;
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
            write_success(&response, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Search(args) => {
            let store = open_store(&cli.dir, ctx)?;
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
                ctx,
            )?;
            record_navigation_event(
                &store,
                "search",
                Some(&args.cursor),
                response.scope_path.as_deref(),
                None,
                Some(format!("found {} cached match(es)", response.hits.len())),
            );
            write_success(&response, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Find(args) => {
            let store = open_store(&cli.dir, ctx)?;
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
                ctx,
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
            write_success(&response, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Delta(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let delta = delta_observations(&store, args)?;
            write_success(&delta, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::McpTask { command } => {
            let store = open_store(&cli.dir, ctx)?;
            let output = mcp_task_command(&store, command, ctx).await?;
            write_success(&output, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Session { command } => {
            let store = open_store(&cli.dir, ctx)?;
            match command {
                SessionCommand::Start(args) => {
                    let trail = store.start_session(args.goal.clone())?;
                    write_success(&trail, cli.pretty, ctx)?;
                }
                SessionCommand::Show(args) => {
                    if args.readiness {
                        let session_id = args.session_id.as_deref();
                        let report = readiness_report(&store, session_id)?;
                        write_success(&report, cli.pretty, ctx)?;
                    } else {
                        let trail = session_show(&store, args)?;
                        write_success(&trail, cli.pretty, ctx)?;
                    }
                }
                SessionCommand::Note(args) => {
                    let event = store.record_session_event(NewSessionEvent {
                        kind: "conclusion".to_string(),
                        summary: Some(args.note.clone()),
                        ..NewSessionEvent::default()
                    })?;
                    write_success(&event, cli.pretty, ctx)?;
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
                        required: !args.optional && args.declared_by == ObligationDeclarerArg::User,
                        intended_check: args.intended_check.clone(),
                        required_scope: args.scope.clone(),
                        declared_by: args.declared_by.into(),
                        expected_operation: if !args.expected_argv.is_empty() {
                            Some(VerificationOperation::Argv(args.expected_argv.clone()))
                        } else {
                            args.source_operation
                                .clone()
                                .map(VerificationOperation::SourceOperation)
                        },
                        required_state: args.required_state.into(),
                        advisory_actions: (!args.advisory_argv.is_empty())
                            .then(|| NextAction {
                                kind: "verify".to_string(),
                                reason: Some("advisory only; executing it does not satisfy this obligation by itself".to_string()),
                                argv: Some(args.advisory_argv.clone()),
                                exactness: Some(prog_core::ActionExactness::Exact),
                                does_not_satisfy: vec![args.id.clone()],
                                extra: Extra::new(),
                                ..NextAction::default()
                            })
                            .into_iter()
                            .collect(),
                        comparison_family: args.comparison_family.clone(),
                        origin_observation_id: args.origin_observation.clone(),
                        expected_absent_fingerprint: args.expected_absent_fingerprint.clone(),
                        evidence_observation_id: args.evidence_observation.clone(),
                        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                        extra: Extra::new(),
                    };
                    store.put_obligation(&obligation)?;
                    write_success(&obligation, cli.pretty, ctx)?;
                }
                SessionCommand::ObligationList(args) => {
                    let report = readiness_report(&store, args.session_id.as_deref())?;
                    write_success(&report, cli.pretty, ctx)?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Expand(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let mut envelope = expand_cursor(&store, args, ctx)?;
            record_envelope_event(&store, &mut envelope, "expand", ctx);
            write_success(&envelope, cli.pretty, ctx)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Cache { command } => {
            let store = open_store(&cli.dir, ctx)?;
            cache_command(&store, command, cli.pretty, ctx)
        }
        Command::Meta(args) => {
            let store = open_store(&cli.dir, ctx)?;
            let envelope = meta_contracts(&store, args, ctx)?;
            write_success(&envelope, cli.pretty, ctx)?;
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

fn record_envelope_event(
    store: &Store,
    envelope: &mut DisclosureEnvelope,
    kind: &str,
    ctx: &InvocationContext,
) {
    if let Some(observation_id) = envelope
        .observation
        .as_ref()
        .and_then(|observation| observation.observation_id.as_deref())
        && let Ok(Some(subject)) = store.get_observation(observation_id)
        && let Ok(Some(baseline)) = store.latest_session_predecessor(
            &subject.invocation_fingerprint,
            subject.comparison_family.as_deref(),
            observation_id,
        )
        && let Ok(mut delta) =
            compare_observation_ids(store, &baseline.observation_id, &subject.observation_id)
    {
        delta.findings.truncate(10);
        envelope.extra.insert(
            "changes_since".to_string(),
            serde_json::to_value(delta).unwrap_or(Value::Null),
        );
        if let Err(error) = compact_envelope_to_budget(envelope, ctx.max_envelope_bytes()) {
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
    let requires_workspace = matches!(
        obligation.required_state,
        VerificationStateRelationship::WorkspaceUnchanged
            | VerificationStateRelationship::WorkspaceAndSourceUnchanged
    );
    if requires_workspace && evidence.workspace_state.is_none() {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Unverifiable,
            vec![
                "the obligation requires workspace-state evidence, but none was captured"
                    .to_string(),
            ],
            None,
        ));
    }
    if let Some(captured_workspace) = &evidence.workspace_state {
        let current_workspace = captured_workspace
            .root
            .as_deref()
            .map(prog_core::capture_workspace)
            .unwrap_or_else(|| prog_core::capture_workspace("."));
        let comparison = prog_core::compare_workspace(captured_workspace, &current_workspace);
        if comparison.validity != prog_core::WorkspaceValidity::Unchanged
            && (requires_workspace
                || obligation.required_state == VerificationStateRelationship::Any)
        {
            return Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Stale,
                comparison.reasons,
                None,
            ));
        }
    }
    let requires_source = matches!(
        obligation.required_state,
        VerificationStateRelationship::SourceUnchanged
            | VerificationStateRelationship::WorkspaceAndSourceUnchanged
    );
    if requires_source && evidence.source_validity != prog_core::SourceValidity::ConfirmedUnchanged
    {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Stale,
            vec!["the obligation requires source state confirmed unchanged".to_string()],
            None,
        ));
    }
    if let Some(expected_operation) = &obligation.expected_operation {
        let matches = match expected_operation {
            VerificationOperation::Argv(expected) => {
                evidence_argv(store, &evidence)?.is_some_and(|actual| actual == *expected)
            }
            VerificationOperation::SourceOperation(expected) => evidence.operation == *expected,
        };
        if !matches {
            return Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Stale,
                vec!["evidence does not match the obligation's declared operation".to_string()],
                None,
            ));
        }
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
            let expected_status = delta
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
            let new_regressions = delta
                .findings
                .iter()
                .filter(|finding| finding.status == prog_core::DeltaFindingStatus::New)
                .cloned()
                .collect::<Vec<_>>();
            let status =
                if expected_status == VerificationStatus::Passed && !new_regressions.is_empty() {
                    VerificationStatus::New
                } else {
                    expected_status
                };
            let reasons = match status {
                VerificationStatus::Passed => vec![
                    "the expected finding is absent under a comparable, complete observation"
                        .to_string(),
                ],
                VerificationStatus::Unknown => vec![
                    "the expected finding could not be evaluated from the comparable evidence"
                        .to_string(),
                ],
                VerificationStatus::New if !new_regressions.is_empty() => vec![
                    "the expected finding is absent, but comparable evidence contains new regression findings"
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
            let mut evaluation =
                obligation_evaluation(obligation, status, reasons, Some(delta.assessment));
            if !new_regressions.is_empty() {
                evaluation.extra.insert(
                    "new_regressions".to_string(),
                    serde_json::to_value(new_regressions)?,
                );
            }
            Ok(evaluation)
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

fn evidence_argv(
    store: &Store,
    observation: &prog_core::ObservationRecord,
) -> Result<Option<Vec<String>>> {
    let Some(payload) = store.get_payload(&observation.payload_hash)? else {
        return Ok(None);
    };
    Ok(payload
        .as_value()
        .pointer("/command/argv")
        .and_then(Value::as_array)
        .and_then(|argv| {
            argv.iter()
                .map(Value::as_str)
                .collect::<Option<Vec<_>>>()
                .map(|argv| argv.into_iter().map(ToOwned::to_owned).collect())
        }))
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

#[allow(clippy::too_many_arguments)]
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

fn write_error(error: &CoreError, pretty: bool, ctx: &InvocationContext) -> ExitCode {
    let rendered = serde_json::to_value(error.envelope())
        .map_err(CoreError::from)
        .and_then(|value| render_budgeted_json(value, pretty, ctx));
    match rendered {
        Ok(json) => {
            println!("{json}");
            ExitCode::FAILURE
        }
        Err(_) => {
            let budget = ctx.disclosure.clone();
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

fn render_budgeted_json(mut value: Value, pretty: bool, ctx: &InvocationContext) -> Result<String> {
    if !value.is_object() {
        value = json!({"result": value});
    }
    let budget = ctx.disclosure.clone();
    let capture_budget = ctx.capture.clone();
    let storage_budget = ctx.storage.clone();
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
mod invocation_context_tests {
    use super::*;

    #[test]
    fn two_contexts_hold_independent_disclosure_budgets() {
        // Two InvocationContexts with different disclosure budgets coexist in
        // one process and each renders through its own budget. This is
        // impossible with the former process-global singletons, which shared
        // one budget across every invocation (#184 acceptance).
        let small =
            InvocationContext::new(effective_disclosure_budget(Some(1024), None, "flag").unwrap());
        let large = InvocationContext::new(
            effective_disclosure_budget(Some(40_000), None, "flag").unwrap(),
        );

        assert_eq!(small.disclosure.source, "flag");
        assert_eq!(large.disclosure.source, "flag");
        assert_ne!(
            small.disclosure.effective_bytes,
            large.disclosure.effective_bytes
        );
        assert_eq!(
            small.max_envelope_bytes(),
            small.disclosure.effective_bytes - BUDGET_METADATA_RESERVE_BYTES,
        );
        assert!(small.max_envelope_bytes() < large.max_envelope_bytes());

        // Rendering through each context uses that context's own budget, not a
        // shared ambient one.
        let small_value: Value = serde_json::from_str(
            &render_budgeted_json(json!({"result": "ok"}), false, &small).unwrap(),
        )
        .unwrap();
        let large_value: Value = serde_json::from_str(
            &render_budgeted_json(json!({"result": "ok"}), false, &large).unwrap(),
        )
        .unwrap();
        assert_eq!(
            small_value["disclosure_budget"]["effective_bytes"],
            small.disclosure.effective_bytes
        );
        assert_eq!(
            large_value["disclosure_budget"]["effective_bytes"],
            large.disclosure.effective_bytes
        );
        assert_ne!(
            small_value["disclosure_budget"]["effective_bytes"],
            large_value["disclosure_budget"]["effective_bytes"]
        );
    }
}

#[cfg(test)]
mod capture_lifecycle_tests {
    use super::*;
    use crate::commands::mcp_task::{
        mcp_task_result_unavailable, record_mcp_task_unavailable_observation,
    };

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
    fn unavailable_mcp_task_result_is_durable_but_cannot_claim_completion() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = Store::open(store_dir.path()).unwrap();
        let profile: SourceProfile = serde_json::from_value(json!({
            "schema": SOURCE_PROFILE_SCHEMA,
            "id": "task_source",
            "kind": "mcp"
        }))
        .unwrap();
        let output = record_mcp_task_unavailable_observation(
            &store,
            &profile,
            "mcp_task.result",
            &CoreError::McpProtocol {
                operation: "tasks/result".to_string(),
                message: "task expired".to_string(),
                preview: json!({"code": -32002}),
            },
            "external-task-42",
            None,
        )
        .unwrap();

        assert_eq!(output.availability, EvidenceAvailability::Unavailable);
        assert_eq!(output.payload["status"], "unavailable");
        assert_eq!(output.payload["error"]["kind"], "mcp_protocol");

        let observation = store
            .get_observation(&output.observation_id)
            .unwrap()
            .unwrap();
        assert_eq!(observation.availability, EvidenceAvailability::Unavailable);
        assert_eq!(observation.status.as_deref(), Some("unavailable"));
        assert_eq!(
            observation.capture.stop_reason,
            CaptureStopReason::Unavailable
        );
        assert!(!observation.capture.can_prove_absence);
        assert!(observation.subject_keys.is_empty());
        let task_reference = observation.lineage.extra["mcp_task_ref"].as_str().unwrap();
        assert!(task_reference.starts_with("sha256:"));
        assert!(!task_reference.contains("external-task-42"));

        let evaluation = evaluate_obligation(
            &store,
            VerificationObligation {
                schema: VERIFICATION_SCHEMA.to_string(),
                id: "task-result".to_string(),
                session_id: "session-1".to_string(),
                required: true,
                intended_check: "retrieve task result".to_string(),
                required_scope: "target".to_string(),
                declared_by: ObligationDeclarer::User,
                expected_operation: Some(VerificationOperation::SourceOperation(
                    "mcp_task.result".to_string(),
                )),
                required_state: VerificationStateRelationship::Any,
                advisory_actions: Vec::new(),
                comparison_family: None,
                origin_observation_id: None,
                expected_absent_fingerprint: None,
                evidence_observation_id: Some(output.observation_id),
                created_at: "2026-07-17T00:00:00Z".to_string(),
                extra: Extra::new(),
            },
        )
        .unwrap();
        assert_eq!(evaluation.status, VerificationStatus::Unverifiable);
        assert_eq!(
            evaluation.reasons,
            vec!["the evidence payload is no longer available".to_string()]
        );
    }

    #[test]
    fn only_mcp_result_retrieval_failures_become_unavailable_evidence() {
        assert!(mcp_task_result_unavailable(&CoreError::McpTimeout {
            operation: "tasks/result".to_string(),
            timeout_ms: 1,
        }));
        assert!(mcp_task_result_unavailable(&CoreError::McpTransport {
            operation: "tasks/result".to_string(),
            message: "connection closed".to_string(),
        }));
        assert!(mcp_task_result_unavailable(&CoreError::McpProtocol {
            operation: "tasks/result".to_string(),
            message: "task expired".to_string(),
            preview: Value::Null,
        }));
        assert!(!mcp_task_result_unavailable(&CoreError::BadArgs {
            operation: "mcp-task".to_string(),
            reason: "invalid task reference".to_string(),
        }));
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

    fn section(lines: &[&str]) -> RunFailureSection {
        RunFailureSection {
            kind: "test",
            stream: "stdout",
            line_start: 1,
            line_end: lines.len(),
            lines: lines.iter().map(|line| (*line).to_string()).collect(),
            reason: "test failure".to_string(),
            priority: 90,
        }
    }

    #[test]
    fn rerun_emitters_escape_identities_and_label_exactness_conservatively() {
        let go = go_test_target_actions(
            &[
                "go".to_string(),
                "test".to_string(),
                "-run".to_string(),
                "old".to_string(),
                "./pkg".to_string(),
            ],
            &[section(&[
                "--- FAIL: TestÜnicode[case].x/child+value (0.00s)",
                "FAIL\t./pkg\t0.003s",
            ])],
            &["affected".to_string()],
        );
        assert_eq!(go.len(), 1);
        assert_eq!(go[0].exactness, Some(prog_core::ActionExactness::Exact));
        assert_eq!(
            go[0].argv,
            Some(vec![
                "go".to_string(),
                "test".to_string(),
                "./pkg".to_string(),
                "-run".to_string(),
                "^TestÜnicode\\[case\\]\\.x$/^child\\+value$".to_string(),
            ])
        );
        assert_eq!(go[0].does_not_satisfy, vec!["affected"]);

        let cargo_exact = cargo_test_target_actions(
            &[
                "cargo".to_string(),
                "test".to_string(),
                "--test".to_string(),
                "integration".to_string(),
            ],
            &[section(&["test crate::quoted_name ... FAILED"])],
            &[],
        );
        assert_eq!(
            cargo_exact[0].exactness,
            Some(prog_core::ActionExactness::Exact)
        );
        assert_eq!(
            cargo_exact[0].argv,
            Some(vec![
                "cargo".to_string(),
                "test".to_string(),
                "--test".to_string(),
                "integration".to_string(),
                "crate::quoted_name".to_string(),
                "--".to_string(),
                "--exact".to_string(),
            ])
        );

        let cargo_filter = cargo_test_target_actions(
            &["cargo".to_string(), "test".to_string()],
            &[section(&[
                "test duplicate_name ... FAILED",
                "test duplicate_name ... FAILED",
            ])],
            &[],
        );
        assert_eq!(cargo_filter.len(), 1);
        assert_eq!(
            cargo_filter[0].exactness,
            Some(prog_core::ActionExactness::Filter)
        );

        let exact_jest = jest_vitest_target_actions(
            &["jest".to_string()],
            &[section(&[
                "FAIL src/math.test.ts",
                "  ✕ handles \"quoted\" [case] (5 ms)",
            ])],
            &[],
        );
        assert_eq!(
            exact_jest[0].exactness,
            Some(prog_core::ActionExactness::Exact)
        );
        assert_eq!(
            exact_jest[0].argv,
            Some(vec![
                "jest".to_string(),
                "src/math.test.ts".to_string(),
                "--testNamePattern".to_string(),
                "^handles \"quoted\" \\[case\\]$".to_string(),
            ])
        );

        let filtered_vitest = jest_vitest_target_actions(
            &["vitest".to_string()],
            &[section(&["  ✕ only a name (4 ms)"])],
            &[],
        );
        assert_eq!(
            filtered_vitest[0].exactness,
            Some(prog_core::ActionExactness::Filter)
        );

        let approximate_jest = jest_vitest_target_actions(
            &["jest".to_string()],
            &[section(&["FAIL src/whole-file.test.ts"])],
            &[],
        );
        assert_eq!(
            approximate_jest[0].exactness,
            Some(prog_core::ActionExactness::Approximate)
        );
    }

    #[test]
    fn rerun_emitters_reject_ambiguous_or_option_like_identities() {
        let go = go_test_target_actions(
            &["go".to_string(), "test".to_string(), "./...".to_string()],
            &[section(&["--- FAIL: -not-a-test (0.00s)"])],
            &[],
        );
        assert!(go.is_empty());

        let ambiguous_go_package = go_test_target_actions(
            &["go".to_string(), "test".to_string()],
            &[section(&[
                "--- FAIL: TestOne (0.00s)",
                "FAIL\t./first\t0.003s",
                "FAIL\t./second\t0.004s",
            ])],
            &[],
        );
        assert!(ambiguous_go_package.is_empty());

        let cargo = cargo_test_target_actions(
            &["cargo".to_string(), "test".to_string()],
            &[section(&["test -not-a-filter ... FAILED"])],
            &[],
        );
        assert!(cargo.is_empty());

        let store_dir = tempfile::tempdir().unwrap();
        let unknown_tool = targeted_rerun_actions(
            &Store::open(store_dir.path()).unwrap(),
            &["jest-30".to_string()],
            &[section(&["FAIL src/file.test.ts"])],
        );
        assert!(unknown_tool.is_empty());
    }
}

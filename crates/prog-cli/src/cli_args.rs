//! CLI argument and subcommand definitions, split from `main.rs` as part of #183.
//!
//! Move-only: behavior, flag names, and the clap surface are byte-identical to
//! the previous inline definitions. Items are `pub(crate)` so `main.rs` (the
//! crate root) can consume them via `use crate::cli_args::*;`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use prog_core::{ObligationDeclarer, VerificationStateRelationship};

#[derive(Debug, Parser)]
#[command(
    name = "prog",
    version,
    about = "Progressive-disclosure gateway for APIs, CLIs, and MCP servers"
)]
pub(crate) struct Cli {
    #[arg(long, env = "PROG_DIR", default_value = "./.prog", global = true)]
    pub(crate) dir: PathBuf,

    #[arg(long, env = "PROG_LENS_DIR", default_value = "./lenses", global = true)]
    pub(crate) lens_dir: PathBuf,

    #[arg(long, global = true)]
    pub(crate) pretty: bool,

    /// Hard maximum number of bytes written in one model-visible JSON response.
    #[arg(long, global = true)]
    pub(crate) budget_bytes: Option<u64>,

    /// Approximate token convenience input, converted by the named bytes/4 estimator.
    #[arg(long, global = true)]
    pub(crate) budget_tokens: Option<u64>,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
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
pub(crate) enum McpTaskCommand {
    Start(McpTaskStartArgs),
    Get(McpTaskReferenceArgs),
    Result(McpTaskReferenceArgs),
    Cancel(McpTaskReferenceArgs),
}

#[derive(Debug, Args)]
pub(crate) struct McpTaskStartArgs {
    pub(crate) source_id: String,
    pub(crate) operation: String,
    #[arg(long)]
    pub(crate) args: String,
    #[arg(long)]
    pub(crate) ttl_ms: Option<u64>,
    #[arg(long)]
    pub(crate) yes: bool,
    #[arg(long)]
    pub(crate) parent_observation: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct McpTaskReferenceArgs {
    pub(crate) source_id: String,
    pub(crate) task_id: String,
    #[arg(long)]
    pub(crate) parent_observation: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum SourceKind {
    Http,
    Cli,
    Mcp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum ImportFormat {
    Auto,
    Openapi,
    JsonSchema,
    CliHelp,
}

impl ImportFormat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ImportFormat::Auto => "auto",
            ImportFormat::Openapi => "openapi",
            ImportFormat::JsonSchema => "json-schema",
            ImportFormat::CliHelp => "cli-help",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum AgentKind {
    Codex,
    ClaudeCode,
    Cursor,
    GeminiCli,
}

impl AgentKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            AgentKind::Codex => "codex",
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Cursor => "cursor",
            AgentKind::GeminiCli => "gemini-cli",
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct DiscoverArgs {
    pub(crate) source_id: String,

    #[arg(long)]
    pub(crate) kind: SourceKind,

    #[arg(long)]
    pub(crate) seed: String,

    #[arg(long = "import", value_enum)]
    pub(crate) import: Option<ImportFormat>,

    #[arg(long)]
    pub(crate) command_base: Option<String>,

    #[arg(long, default_value_t = 10)]
    pub(crate) max_schema_depth: usize,

    #[arg(long)]
    pub(crate) probe: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SourceCommand {
    AddHttp(SourceAddHttpArgs),
    AddCli(SourceAddCliArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SourceAddHttpArgs {
    pub(crate) source_id: String,

    #[arg(long)]
    pub(crate) operation: String,

    #[arg(long)]
    pub(crate) url: String,

    #[arg(long, default_value = "GET")]
    pub(crate) method: String,

    #[arg(long)]
    pub(crate) probe: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SourceAddCliArgs {
    pub(crate) source_id: String,

    #[arg(long)]
    pub(crate) operation: String,

    #[arg(long)]
    pub(crate) read_only: bool,

    #[arg(long)]
    pub(crate) probe: bool,

    /// Apply a conservatively detected structured-output flag when one is
    /// known to be valid for this CLI invocation.
    #[arg(long)]
    pub(crate) prefer_json: bool,

    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct HintsArgs {
    pub(crate) source_id: String,
    pub(crate) operation: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct CallArgs {
    pub(crate) source_id: String,
    pub(crate) operation: String,

    #[arg(long)]
    pub(crate) args: String,

    #[arg(long)]
    pub(crate) view: Option<String>,

    #[arg(long)]
    pub(crate) lens: Option<String>,

    #[arg(long)]
    pub(crate) yes: bool,

    #[arg(long)]
    pub(crate) no_cache: bool,

    #[arg(long)]
    pub(crate) refresh: bool,

    /// Canonical family used to decide whether successive observations may be compared.
    #[arg(long)]
    pub(crate) comparison_family: Option<String>,

    /// Stable logical scope included in this capture; repeat for collections.
    #[arg(long = "selection-scope")]
    pub(crate) selection_scopes: Vec<String>,

    /// Assert that all supplied selection scopes are exhaustively represented.
    #[arg(long, requires = "selection_scopes")]
    pub(crate) selection_exhaustive: bool,

    /// Follow pagination links for read-only operations, prefetching up to N
    /// pages into the local cache under hard page/byte/time caps.
    #[arg(long, default_value_t = 1)]
    pub(crate) pages: usize,
}

#[derive(Debug, Args)]
pub(crate) struct ObserveArgs {
    #[arg(long, conflicts_with = "stdin")]
    pub(crate) file: Option<PathBuf>,

    #[arg(long, conflicts_with = "file")]
    pub(crate) stdin: bool,

    #[arg(long)]
    pub(crate) mime: Option<String>,

    #[arg(long)]
    pub(crate) name: Option<String>,

    #[arg(long)]
    pub(crate) lens: Option<String>,

    /// Canonical family used to decide whether successive observations may be compared.
    #[arg(long)]
    pub(crate) comparison_family: Option<String>,

    /// Stable logical scope included in this capture; repeat for collections.
    #[arg(long = "selection-scope")]
    pub(crate) selection_scopes: Vec<String>,

    /// Assert that all supplied selection scopes are exhaustively represented.
    #[arg(long, requires = "selection_scopes")]
    pub(crate) selection_exhaustive: bool,

    #[arg(long, default_value_t = 86_400)]
    pub(crate) ttl_seconds: u64,
}

#[derive(Debug, Args)]
pub(crate) struct RunArgs {
    #[arg(long, default_value_t = 30_000)]
    pub(crate) timeout_ms: u64,

    #[arg(long, default_value_t = 1024 * 1024)]
    pub(crate) max_stdout_bytes: usize,

    #[arg(long, default_value_t = 1024 * 1024)]
    pub(crate) max_stderr_bytes: usize,

    #[arg(long, default_value_t = 86_400)]
    pub(crate) ttl_seconds: u64,

    #[arg(long)]
    pub(crate) preserve_exit_code: bool,

    #[arg(long)]
    pub(crate) out: Option<PathBuf>,

    #[arg(long)]
    pub(crate) lens: Option<String>,

    /// Canonical family used to decide whether successive observations may be compared.
    #[arg(long)]
    pub(crate) comparison_family: Option<String>,

    /// Stable logical scope included in this capture; repeat for collections.
    #[arg(long = "selection-scope")]
    pub(crate) selection_scopes: Vec<String>,

    /// Assert that all supplied selection scopes are exhaustively represented.
    #[arg(long, requires = "selection_scopes")]
    pub(crate) selection_exhaustive: bool,

    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    pub(crate) command: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum RecipeKind {
    CargoTest,
    Pytest,
    NpmTest,
    GoTest,
    GhIssues,
    DiffReview,
    LogsRootCause,
}

impl RecipeKind {
    pub(crate) fn as_str(self) -> &'static str {
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

    pub(crate) fn default_goal(self) -> &'static str {
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
pub(crate) struct RecipeArgs {
    #[arg(value_enum)]
    pub(crate) recipe: RecipeKind,

    #[arg(long)]
    pub(crate) goal: Option<String>,

    #[arg(long)]
    pub(crate) file: Option<PathBuf>,

    #[arg(long, default_value_t = 30_000)]
    pub(crate) timeout_ms: u64,

    #[arg(long, default_value_t = 86_400)]
    pub(crate) ttl_seconds: u64,

    /// Canonical family used to decide whether successive observations may be compared.
    #[arg(long)]
    pub(crate) comparison_family: Option<String>,

    /// Stable logical scope included in this capture; repeat for collections.
    #[arg(long = "selection-scope")]
    pub(crate) selection_scopes: Vec<String>,

    /// Assert that all supplied selection scopes are exhaustively represented.
    #[arg(long, requires = "selection_scopes")]
    pub(crate) selection_exhaustive: bool,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    #[arg(long, value_enum)]
    pub(crate) agent: AgentKind,

    #[arg(long)]
    pub(crate) project: bool,

    #[arg(long)]
    pub(crate) dry_run: bool,

    #[arg(long, default_value = ".")]
    pub(crate) root: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct CostArgs {
    #[arg(long)]
    pub(crate) model_profile: PathBuf,

    #[arg(long)]
    pub(crate) raw_file: PathBuf,

    #[arg(long)]
    pub(crate) mime: Option<String>,

    #[arg(long = "expand-path")]
    pub(crate) expand_paths: Vec<String>,

    #[arg(long, default_value_t = 0)]
    pub(crate) estimated_output_tokens: u64,

    #[arg(long, default_value_t = 3)]
    pub(crate) repeated_inspections: u64,
}

#[derive(Debug, Args)]
pub(crate) struct PathsArgs {
    pub(crate) cursor: String,

    #[arg(long, default_value = "")]
    pub(crate) prefix: String,

    #[arg(long)]
    pub(crate) reason: Option<String>,

    #[arg(long, value_delimiter = ',')]
    pub(crate) field: Vec<String>,

    #[arg(long)]
    pub(crate) omitted_only: bool,

    #[arg(long)]
    pub(crate) expandable_only: bool,

    #[arg(long, default_value_t = 200)]
    pub(crate) limit: usize,

    #[arg(long, default_value_t = 6)]
    pub(crate) depth: usize,
}

#[derive(Debug, Args)]
pub(crate) struct InspectArgs {
    pub(crate) cursor: String,

    #[arg(long)]
    pub(crate) goal: String,

    #[arg(long, default_value_t = 10)]
    pub(crate) limit: usize,

    #[arg(long)]
    pub(crate) kind: Option<String>,

    #[arg(long, default_value = "")]
    pub(crate) path: String,
}

#[derive(Debug, Args)]
pub(crate) struct EvidenceArgs {
    pub(crate) cursor: String,

    #[arg(long, default_value = "")]
    pub(crate) path: String,
}

#[derive(Debug, Args)]
pub(crate) struct SearchArgs {
    pub(crate) cursor: String,
    pub(crate) query: String,

    #[arg(long)]
    pub(crate) kind: Option<String>,

    #[arg(long, default_value = "")]
    pub(crate) path: String,

    #[arg(long, default_value_t = 20)]
    pub(crate) limit: usize,

    #[arg(long)]
    pub(crate) case_sensitive: bool,

    #[arg(long)]
    pub(crate) regex: bool,
}

#[derive(Debug, Args)]
pub(crate) struct FindArgs {
    pub(crate) cursor: String,

    #[arg(long)]
    pub(crate) kind: String,

    #[arg(long, default_value = "")]
    pub(crate) path: String,

    #[arg(long, default_value_t = 20)]
    pub(crate) limit: usize,
}

#[derive(Debug, Args)]
pub(crate) struct DeltaArgs {
    pub(crate) baseline: String,
    pub(crate) subject: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SessionCommand {
    Start(SessionStartArgs),
    Show(SessionShowArgs),
    Note(SessionNoteArgs),
    ObligationAdd(Box<ObligationAddArgs>),
    ObligationList(ObligationListArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SessionStartArgs {
    #[arg(long)]
    pub(crate) goal: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SessionShowArgs {
    pub(crate) session_id: Option<String>,

    /// Evaluate declared verification obligations instead of returning the session trail.
    #[arg(long)]
    pub(crate) readiness: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SessionNoteArgs {
    pub(crate) note: String,
}

#[derive(Debug, Args)]
pub(crate) struct ObligationAddArgs {
    /// Stable identifier, unique within the session.
    pub(crate) id: String,

    /// Human-readable check the agent intends to run or evaluate.
    #[arg(long = "check")]
    pub(crate) intended_check: String,

    /// Scope that this check covers, such as target, affected-suite, or regression-suite.
    #[arg(long)]
    pub(crate) scope: String,

    /// Canonical invocation family expected for evidence.
    #[arg(long)]
    pub(crate) comparison_family: Option<String>,

    /// Earlier observation containing the finding that must disappear.
    #[arg(long)]
    pub(crate) origin_observation: Option<String>,

    /// Stable finding fingerprint that must be absent from the evidence observation.
    #[arg(long)]
    pub(crate) expected_absent_fingerprint: Option<String>,

    /// Observation used to evaluate this obligation.
    #[arg(long)]
    pub(crate) evidence_observation: Option<String>,

    /// Record an advisory obligation that does not block readiness.
    #[arg(long)]
    pub(crate) optional: bool,

    /// Declarer of this obligation. Non-user declarations are always advisory.
    #[arg(long, value_enum, default_value_t = ObligationDeclarerArg::User)]
    pub(crate) declared_by: ObligationDeclarerArg,

    /// Exact argv represented by suitable evidence; never interpreted as a shell command.
    #[arg(long, num_args = 1.., conflicts_with = "source_operation")]
    pub(crate) expected_argv: Vec<String>,

    /// Source-native operation represented by suitable evidence.
    #[arg(long, conflicts_with = "expected_argv")]
    pub(crate) source_operation: Option<String>,

    /// Required validity relationship for workspace and source state.
    #[arg(long, value_enum, default_value_t = StateRelationshipArg::Any)]
    pub(crate) required_state: StateRelationshipArg,

    /// Advisory exact argv hint. It is displayed only and is never auto-run.
    #[arg(long, num_args = 1..)]
    pub(crate) advisory_argv: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum ObligationDeclarerArg {
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
pub(crate) enum StateRelationshipArg {
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
pub(crate) struct ObligationListArgs {
    /// Session to evaluate. Defaults to the active session.
    pub(crate) session_id: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ExpandArgs {
    pub(crate) cursor: String,

    #[arg(long, default_value = "")]
    pub(crate) path: String,

    #[arg(long)]
    pub(crate) limit: Option<usize>,

    #[arg(long)]
    pub(crate) depth: Option<usize>,

    #[arg(long, value_delimiter = ',')]
    pub(crate) fields: Vec<String>,

    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CacheCommand {
    List,
    Observations(CacheObservationsArgs),
    Get(CacheGetArgs),
    Purge(CachePurgeArgs),
    Retention(CacheRetentionArgs),
}

#[derive(Debug, Args)]
pub(crate) struct CacheObservationsArgs {
    #[arg(long, default_value_t = 20)]
    pub(crate) limit: usize,
}

#[derive(Debug, Args)]
pub(crate) struct CacheGetArgs {
    pub(crate) key: String,
}

#[derive(Debug, Args)]
pub(crate) struct CachePurgeArgs {
    #[arg(long)]
    pub(crate) source: Option<String>,

    #[arg(long)]
    pub(crate) expired: bool,

    #[arg(long)]
    pub(crate) all: bool,

    /// Retain at most this many bytes of redacted payload blobs, evicting
    /// oldest payload-reference groups while preserving metadata lineage.
    #[arg(long)]
    pub(crate) payload_budget_bytes: Option<u64>,
}

#[derive(Debug, Args)]
pub(crate) struct CacheRetentionArgs {
    /// Persist a maximum number of redacted payload bytes. Omit to keep the
    /// current value; use --clear-max-payload-bytes to remove the cap.
    #[arg(long, conflicts_with = "clear_max_payload_bytes")]
    pub(crate) max_payload_bytes: Option<u64>,

    /// Persist a maximum cache-entry age in seconds. Omit to keep the current
    /// value; use --clear-max-age-seconds to remove the cap.
    #[arg(long, conflicts_with = "clear_max_age_seconds")]
    pub(crate) max_age_seconds: Option<u64>,

    #[arg(long)]
    pub(crate) clear_max_payload_bytes: bool,

    #[arg(long)]
    pub(crate) clear_max_age_seconds: bool,
}

#[derive(Debug, Args)]
pub(crate) struct MetaArgs {
    pub(crate) contract: Option<String>,
}

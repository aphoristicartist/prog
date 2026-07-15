use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::redaction::RedactionConfig;
use crate::shape::Shape;

pub const SOURCE_PROFILE_SCHEMA: &str = "prog.source_profile";
pub const DISCLOSURE_SCHEMA: &str = "prog.disclosure";
pub const LENS_MANIFEST_SCHEMA: &str = "prog.lens_manifest";
pub const INSPECT_SCHEMA: &str = "prog.inspect";
pub const EVIDENCE_BLOCK_SCHEMA: &str = "prog.evidence";
pub const SEARCH_SCHEMA: &str = "prog.search";
pub const SESSION_SCHEMA: &str = "prog.session";
pub const OBSERVATION_SCHEMA: &str = "prog.observation";
pub const SOURCE_STATE_SCHEMA: &str = "prog.source_state";
pub const OBSERVATION_DELTA_SCHEMA: &str = "prog.observation_delta";
pub const VERIFICATION_SCHEMA: &str = "prog.verification";

pub type Extra = Map<String, Value>;

/// Reject compatibility-era profile data instead of attempting to interpret it.
/// Adapter metadata remains in `extra`, but contract identity and local profile
/// bookkeeping have one current representation during the pre-release period.
pub fn validate_source_profile(profile: &SourceProfile) -> crate::Result<()> {
    if profile.schema != SOURCE_PROFILE_SCHEMA {
        return Err(crate::CoreError::BadArgs {
            operation: "source profile".to_string(),
            reason: format!(
                "schema must be '{SOURCE_PROFILE_SCHEMA}', got '{}'",
                profile.schema
            ),
        });
    }
    if profile.revision == 0 {
        return Err(crate::CoreError::BadArgs {
            operation: "source profile".to_string(),
            reason: "revision must be greater than zero".to_string(),
        });
    }
    for legacy in ["schema_version", "version"] {
        if profile.extra.contains_key(legacy) {
            return Err(crate::CoreError::BadArgs {
                operation: "source profile".to_string(),
                reason: format!("'{legacy}' is unsupported; regenerate this pre-release profile"),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SourceProfile {
    pub schema: String,
    pub id: String,
    pub kind: SourceKind,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub operations: Vec<OperationProfile>,
    #[serde(default)]
    pub auth: Vec<AuthRef>,
    #[serde(default)]
    pub cache: CachePolicy,
    #[serde(default)]
    pub trust: TrustSettings,
    #[serde(default)]
    pub effect_defaults: EffectSet,
    #[serde(default)]
    pub redaction: RedactionConfig,
    /// Optional source-specific ceiling for model-visible JSON responses. CLI
    /// flags and environment variables take precedence over this policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disclosure_budget: Option<DisclosureBudget>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// A source-owned response disclosure policy. This intentionally contains no
/// token estimate: the byte ceiling is the enforceable contract, while token
/// estimates remain invocation conveniences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DisclosureBudget {
    pub max_bytes: u64,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Http,
    Cli,
    Mcp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct OperationProfile {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Value,
    #[serde(default)]
    pub output_shape: Option<Shape>,
    #[serde(default)]
    pub declared_output_schema: Option<Value>,
    #[serde(default)]
    pub effects: EffectSet,
    #[serde(default)]
    pub cache: CachePolicy,
    #[serde(default)]
    pub pagination: Option<Value>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct EffectSet {
    #[serde(default)]
    pub read_only: bool,
    #[serde(default = "default_true")]
    pub mutating: bool,
    #[serde(default = "default_true")]
    pub network: bool,
    #[serde(default = "default_true")]
    pub shell: bool,
    #[serde(default = "default_true")]
    pub sensitive: bool,
    #[serde(default)]
    pub cacheable: bool,
    #[serde(default = "default_true")]
    pub requires_confirmation: bool,
    #[serde(default, flatten)]
    pub extra: Extra,
}

impl Default for EffectSet {
    fn default() -> Self {
        Self {
            read_only: false,
            mutating: true,
            network: true,
            shell: true,
            sensitive: true,
            cacheable: false,
            requires_confirmation: true,
            extra: Extra::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CachePolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    #[serde(default)]
    pub refresh_after_seconds: Option<u64>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_seconds: None,
            refresh_after_seconds: None,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TrustSettings {
    #[serde(default)]
    pub allow_shell: bool,
    #[serde(default)]
    pub allow_network: bool,
    /// When true (default), an operation carrying *proven* read-only evidence
    /// from a trusted importer descriptor may skip confirmation automatically.
    /// Mutating, shell-backed, and sensitive operations are never relaxed.
    #[serde(default = "default_true")]
    pub auto_upgrade: bool,
    #[serde(default, flatten)]
    pub extra: Extra,
}

impl Default for TrustSettings {
    fn default() -> Self {
        Self {
            allow_shell: false,
            allow_network: false,
            auto_upgrade: true,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AuthRef {
    pub name: String,
    pub env: String,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DisclosureEnvelope {
    pub schema: String,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub operation: Option<String>,
    pub summary: Summary,
    #[serde(default)]
    pub data_preview: Value,
    #[serde(default)]
    pub schema_hints: BTreeMap<String, String>,
    #[serde(default)]
    pub omitted: Vec<OmittedRegion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub next_actions: Vec<NextAction>,
    #[serde(default)]
    pub provenance: Option<CallProvenance>,
    #[serde(default)]
    pub cache: Option<CacheInfo>,
    #[serde(default)]
    pub observation: Option<ObservationMetadata>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObservationMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_id: Option<String>,
    pub completeness: ObservationCompleteness,
    pub freshness: ObservationFreshness,
    pub trust: ObservationTrust,
    pub safety: ObservationSafety,
    pub payload: ObservationPayloadStatus,
    pub availability: EvidenceAvailability,
    /// Applied capture limits and resulting byte accounting. This is retained
    /// even for complete captures so agents never have to infer one budget
    /// layer from another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureCompleteness>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObservationCompleteness {
    pub status: String,
    pub preview_complete: bool,
    pub path_scoped: bool,
    pub truncated: bool,
    pub redacted: bool,
    pub omitted_count: u64,
    pub redacted_count: u64,
    pub root_path: String,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObservationFreshness {
    #[serde(default)]
    pub captured_at: Option<String>,
    #[serde(default)]
    pub age_seconds: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub stale_after_seconds: Option<u64>,
    pub stale: bool,
    pub refresh_recommended: bool,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObservationTrust {
    pub profile_backed: bool,
    #[serde(default)]
    pub source_kind: Option<String>,
    pub adapter_provenance: bool,
    #[serde(default)]
    pub provenance_status: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObservationSafety {
    pub redacted_before_persistence: bool,
    pub redacted_paths: u64,
    pub sensitive_cache_disabled: bool,
    #[serde(default)]
    pub cache_disabled_reason: Option<String>,
    #[serde(default)]
    pub effects: Option<EffectSet>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObservationPayloadStatus {
    #[serde(default)]
    pub cache_status: Option<CacheStatus>,
    pub cached: bool,
    pub expandable: bool,
    pub payload_bytes: u64,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct EvidenceRef {
    pub schema: String,
    pub source_id: String,
    pub operation: String,
    #[serde(default)]
    pub cursor: Option<String>,
    pub path: String,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub captured_at: Option<String>,
    #[serde(default)]
    pub cache_status: Option<CacheStatus>,
    #[serde(default)]
    pub age_seconds: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<String>,
    pub stale: bool,
    /// Lifecycle state of the immutable observation this reference names.
    /// `unavailable` is explicit when no observation record is attached.
    pub availability: EvidenceAvailability,
    /// Capture and retention facts for the referenced observation. These are
    /// intentionally observation-wide: a JSON pointer cannot prove that an
    /// unobserved sibling was absent.
    pub capture: CaptureCompleteness,
    pub redacted: bool,
    pub lossy: bool,
    #[serde(default)]
    pub redacted_slice_sha256: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// Ranked evidence-navigation response for `prog inspect`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct InspectResponse {
    pub schema: String,
    pub cursor: String,
    pub goal: String,
    #[serde(default)]
    pub normalized_goal: Option<String>,
    #[serde(default)]
    pub scope_path: Option<String>,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub omitted: Vec<OmittedRegion>,
    #[serde(default)]
    pub cache: Option<CacheInfo>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// One ranked candidate path that is likely to contain useful evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Finding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub rank: u64,
    pub kind: String,
    pub path: String,
    pub confidence: f64,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<LineRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_range: Option<ByteRange>,
    /// The single most relevant source location declared by the observed
    /// payload. This is distinct from `line_range`, which addresses lines in
    /// captured evidence rather than source code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_span: Option<SourceSpan>,
    /// Additional source locations declared by the observed payload. The
    /// extraction boundary caps and orders these deterministically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_spans: Vec<SourceSpan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction_state: Option<RedactionState>,
    #[serde(default)]
    pub commands: FindingCommandHints,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct FindingCommandHints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct LineRange {
    pub start: u64,
    pub end: u64,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// A provenance-backed source location extracted from a structured payload.
///
/// `path` is a normalized workspace-relative path. `uri` is retained for
/// external or virtual sources. Exactly one is present on spans emitted by
/// `prog`; the optional representation keeps deserialization forward-safe for
/// evidence captured before a producer could identify its locator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SourceSpan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    pub start_line: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_column: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u64>,
    /// `primary`, `related`, `generated`, or a producer-defined role.
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The deterministic extractor that produced this span.
    pub origin: String,
    pub exactness: SourceSpanExactness,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceSpanExactness {
    Exact,
    Range,
    Approximate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RedactionState {
    pub redacted: bool,
    #[serde(default)]
    pub redacted_paths: u64,
    #[serde(default)]
    pub lossy: bool,
    #[serde(default)]
    pub redaction_version: Option<u32>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// Compact citation-oriented evidence extracted from a cursor path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct EvidenceBlock {
    pub schema: String,
    pub cursor: String,
    pub path: String,
    pub kind: String,
    pub summary: String,
    #[serde(default)]
    pub excerpt: Value,
    #[serde(default)]
    pub citations: Vec<EvidenceCitation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<LineRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_range: Option<ByteRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<CallProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction_state: Option<RedactionState>,
    #[serde(default)]
    pub commands: FindingCommandHints,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheInfo>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct EvidenceCitation {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub excerpt: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<LineRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_range: Option<ByteRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction_state: Option<RedactionState>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// Search results over a redacted cached payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SearchResponse {
    pub schema: String,
    pub cursor: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub scope_path: Option<String>,
    #[serde(default)]
    pub hits: Vec<SearchHit>,
    #[serde(default)]
    pub omitted: Vec<OmittedRegion>,
    #[serde(default)]
    pub cache: Option<CacheInfo>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SearchHit {
    pub rank: u64,
    pub path: String,
    pub score: f64,
    pub match_kind: String,
    #[serde(default)]
    pub preview: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<LineRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_range: Option<ByteRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction_state: Option<RedactionState>,
    #[serde(default)]
    pub commands: FindingCommandHints,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Summary {
    pub kind: String,
    #[serde(default)]
    pub item_count: Option<u64>,
    #[serde(default)]
    pub preview_count: Option<u64>,
    #[serde(default)]
    pub payload_bytes: u64,
    #[serde(default)]
    pub approx_tokens: u64,
    #[serde(default)]
    pub envelope_bytes: Option<u64>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct OmittedRegion {
    pub path: String,
    pub reason: OmissionReason,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OmissionReason {
    LargeString,
    LongArray,
    ManyFields,
    DeepObject,
    NodeBudget,
    Redacted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionExactness {
    Exact,
    Filter,
    Approximate,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct NextAction {
    pub kind: String,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// Never a shell string. Consumers must execute only after their own policy checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exactness: Option<ActionExactness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub does_not_satisfy: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct LensManifest {
    pub schema: String,
    pub id: String,
    #[serde(default, rename = "match")]
    pub match_rules: LensMatch,
    #[serde(default)]
    pub view: LensView,
    #[serde(default)]
    pub omit: Vec<LensOmission>,
    #[serde(default)]
    pub next_actions: Vec<NextAction>,
    #[serde(default)]
    pub findings: Vec<LensFindingRule>,
    #[serde(default)]
    pub invariants: Vec<String>,
    #[serde(default)]
    pub fixtures: LensFixtures,
}

/// Declarative, data-only finding provider used by a lens manifest.
///
/// `path` may contain `*` wildcard segments. A rule emits findings only for
/// paths that actually exist in the persisted redacted payload. Optional
/// `contains_any` terms further restrict matches without executing lens code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct LensFindingRule {
    pub kind: String,
    pub path: String,
    pub confidence: f64,
    pub reason: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub contains_any: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// One compact event in an evidence-navigation session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SessionEvent {
    pub id: String,
    pub session_id: String,
    pub sequence: u64,
    pub timestamp: String,
    pub kind: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub evidence_ref: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// Machine-readable task-level trail over cached, redacted evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SessionTrail {
    pub schema: String,
    pub session_id: String,
    #[serde(default)]
    pub goal: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub events: Vec<SessionEvent>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct LensMatch {
    #[serde(default)]
    pub source_kind: Option<SourceKind>,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub artifact_kind: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct LensView {
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub depth: Option<usize>,
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct LensOmission {
    pub path: String,
    pub reason: OmissionReason,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub expandable: bool,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct LensFixtures {
    #[serde(default)]
    pub positive: Vec<String>,
    #[serde(default)]
    pub negative: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SliceRequest {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub depth: Option<usize>,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub omit: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CursorRecord {
    pub cache_key: String,
    pub source_id: String,
    pub operation: String,
    pub root_path: String,
    pub redaction_version: u32,
    pub created_at: String,
    pub expires_at: String,
    /// Immutable capture identity. Cursors are short-lived capabilities and
    /// must never be treated as the identity of the underlying observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_id: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CacheEntryMeta {
    pub key: String,
    pub payload_hash: String,
    pub source_id: String,
    pub operation: String,
    pub created_at: String,
    pub expires_at: String,
    pub payload_bytes: u64,
    #[serde(default)]
    pub cacheable: bool,
    #[serde(default)]
    pub sensitive: bool,
    /// The capture that produced this reusable cache entry. A cache hit must
    /// reference this record instead of fabricating a new execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_id: Option<String>,
    #[serde(default)]
    pub provenance: Option<CallProvenance>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CallProvenance {
    pub source_call_id: String,
    #[serde(default)]
    pub cache_key: Option<String>,
    pub captured_at: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CacheInfo {
    pub status: CacheStatus,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub age_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    Stored,
    Hit,
    Miss,
    Skipped,
    Expired,
}

/// Whether an observation's evidence can still be used for navigation or
/// absence claims. This is a lifecycle state, not a disclosure-preview state.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAvailability {
    Recoverable,
    CaptureTruncated,
    Redacted,
    Expired,
    MetadataOnly,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStopReason {
    Complete,
    ByteLimit,
    Timeout,
    Cancelled,
    Redacted,
    StorageLimit,
    Expired,
    Unavailable,
}

/// Where an applied execution or retention limit came from. Limits are kept
/// separate from their outcome so callers can distinguish "the source ended"
/// from "prog stopped at the configured cap".
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BudgetSource {
    #[default]
    Default,
    Profile,
    Operation,
    Invocation,
    StorePolicy,
    Unavailable,
}

/// One independently bounded capture channel. `max_work_units` is reserved
/// for sources whose work is not naturally measured in bytes or time, such as
/// pages or stream messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CaptureLimit {
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_work_units: Option<u64>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// The limits that governed one capture. This is immutable observation
/// evidence, not a request to alter future executions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CaptureBudget {
    #[serde(default)]
    pub source: BudgetSource,
    #[serde(default)]
    pub limits: Vec<CaptureLimit>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

impl CaptureBudget {
    pub fn unavailable() -> Self {
        Self {
            source: BudgetSource::Unavailable,
            limits: Vec::new(),
            extra: Extra::new(),
        }
    }
}

impl Default for CaptureBudget {
    fn default() -> Self {
        Self {
            source: BudgetSource::Default,
            limits: Vec::new(),
            extra: Extra::new(),
        }
    }
}

/// Persistent retention limits for redacted payload blobs. `None` means that
/// dimension is deliberately unbounded; this never borrows a capture or
/// disclosure limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct StorageBudget {
    #[serde(default)]
    pub source: BudgetSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_payload_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_seconds: Option<u64>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

impl Default for StorageBudget {
    fn default() -> Self {
        Self {
            source: BudgetSource::Default,
            max_payload_bytes: None,
            max_age_seconds: None,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CaptureScope {
    /// A bounded producer-declared channel, page, or JSON path such as
    /// `stdout`, `stderr`, `body`, or `/items/0`.
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub captured_bytes: u64,
    pub stop_reason: CaptureStopReason,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// Capture and retention facts for an immutable observation. Capture,
/// storage, and disclosure budgets are deliberately separate contracts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CaptureCompleteness {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub captured_bytes: u64,
    pub stored_bytes: u64,
    pub stop_reason: CaptureStopReason,
    #[serde(default)]
    pub budget: CaptureBudget,
    #[serde(default)]
    pub affected: Vec<CaptureScope>,
    pub can_prove_absence: bool,
    #[serde(default, flatten)]
    pub extra: Extra,
}

impl CaptureCompleteness {
    pub fn complete(stored_bytes: u64) -> Self {
        Self {
            total_bytes: Some(stored_bytes),
            captured_bytes: stored_bytes,
            stored_bytes,
            stop_reason: CaptureStopReason::Complete,
            budget: CaptureBudget::default(),
            affected: Vec::new(),
            can_prove_absence: true,
            extra: Extra::new(),
        }
    }

    pub fn unavailable(stored_bytes: u64) -> Self {
        Self {
            total_bytes: None,
            captured_bytes: 0,
            stored_bytes,
            stop_reason: CaptureStopReason::Unavailable,
            budget: CaptureBudget::unavailable(),
            affected: Vec::new(),
            can_prove_absence: false,
            extra: Extra::new(),
        }
    }
}

impl Default for CaptureCompleteness {
    fn default() -> Self {
        Self::unavailable(0)
    }
}

/// Directed, immutable relationships between captures. The relationship
/// values are opaque observation identifiers, never cursors or cache keys.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObservationLineage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revalidates_id: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// Immutable metadata for one real upstream, command, artifact, or internal
/// capture. Payload bytes remain in the payload store; cache entries and
/// cursors only refer to this record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObservationRecord {
    pub schema: String,
    pub observation_id: String,
    pub payload_hash: String,
    pub availability: EvidenceAvailability,
    pub invocation_fingerprint: String,
    pub source_id: String,
    pub operation: String,
    #[serde(default)]
    pub subject_keys: Vec<String>,
    pub captured_at: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub status: Option<String>,
    pub capture: CaptureCompleteness,
    pub redacted: bool,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub parser: Option<String>,
    #[serde(default)]
    pub lens: Option<String>,
    #[serde(default)]
    pub workspace_state: Option<String>,
    #[serde(default)]
    pub source_state: Option<SourceStateToken>,
    #[serde(default)]
    pub environment_state: Option<String>,
    #[serde(default)]
    pub lineage: ObservationLineage,
    #[serde(default)]
    pub provenance: Option<CallProvenance>,
    #[serde(default)]
    pub cache_key: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

/// Opaque evidence of the upstream state at capture time. Token values are
/// either safe validators (for example an HTTP ETag) or a one-way digest; no
/// transport credential or source payload belongs in this contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SourceStateToken {
    pub schema: String,
    pub kind: SourceStateKind,
    pub value: String,
    pub source_id: String,
    pub operation: String,
    #[serde(default)]
    pub subject_scope: Option<String>,
    pub captured_at: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceStateKind {
    HttpEtag,
    HttpLastModified,
    ChangeToken,
    McpModification,
}

/// Source-state validity is deliberately independent from cache age. A TTL
/// can make evidence stale, but it cannot prove the upstream changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceValidity {
    ConfirmedUnchanged,
    SourceChanged,
    StaleByTtl,
    ValidatorUnavailable,
    ValidatorExpired,
    RefreshFailed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubjectIdentity {
    Same,
    Different,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeRelationship {
    Equal,
    Superset,
    Subset,
    Overlap,
    Disjoint,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeltaFindingStatus {
    New,
    Persisting,
    Resolved,
    NotObserved,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ComparabilityAssessment {
    pub subject_identity: SubjectIdentity,
    pub scope_relationship: ScopeRelationship,
    pub invocation_match: bool,
    pub baseline_complete: bool,
    pub subject_complete: bool,
    pub normalization_compatible: bool,
    pub workspace_validity: String,
    pub source_validity: String,
    pub can_prove_absence: bool,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DeltaFinding {
    pub status: DeltaFindingStatus,
    pub fingerprint: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub baseline_path: Option<String>,
    #[serde(default)]
    pub subject_path: Option<String>,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObservationDelta {
    pub schema: String,
    pub baseline_observation_id: String,
    pub subject_observation_id: String,
    pub assessment: ComparabilityAssessment,
    #[serde(default)]
    pub findings: Vec<DeltaFinding>,
    #[serde(default)]
    pub counts: BTreeMap<String, u64>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Pending,
    Passed,
    Failed,
    Persisting,
    New,
    NotObserved,
    Stale,
    Unknown,
    Unverifiable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct VerificationObligation {
    pub schema: String,
    pub id: String,
    pub session_id: String,
    pub required: bool,
    pub intended_check: String,
    pub required_scope: String,
    #[serde(default)]
    pub comparison_family: Option<String>,
    #[serde(default)]
    pub origin_observation_id: Option<String>,
    #[serde(default)]
    pub expected_absent_fingerprint: Option<String>,
    #[serde(default)]
    pub evidence_observation_id: Option<String>,
    pub created_at: String,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObligationEvaluation {
    pub obligation: VerificationObligation,
    pub status: VerificationStatus,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub assessment: Option<ComparabilityAssessment>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ReadinessReport {
    pub schema: String,
    pub configured: bool,
    pub ready: bool,
    #[serde(default)]
    pub evaluations: Vec<ObligationEvaluation>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

pub fn canonical_json(value: &Value) -> crate::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&sort_json(value))?)
}

pub fn public_contract_schemas() -> crate::Result<Map<String, Value>> {
    let mut schemas = Map::new();
    insert_schema::<SourceProfile>(&mut schemas, "SourceProfile")?;
    insert_schema::<DisclosureBudget>(&mut schemas, "DisclosureBudget")?;
    insert_schema::<OperationProfile>(&mut schemas, "OperationProfile")?;
    insert_schema::<Shape>(&mut schemas, "Shape")?;
    insert_schema::<EffectSet>(&mut schemas, "EffectSet")?;
    insert_schema::<ObservationMetadata>(&mut schemas, "ObservationMetadata")?;
    insert_schema::<ObservationRecord>(&mut schemas, "ObservationRecord")?;
    insert_schema::<ObservationLineage>(&mut schemas, "ObservationLineage")?;
    insert_schema::<EvidenceAvailability>(&mut schemas, "EvidenceAvailability")?;
    insert_schema::<BudgetSource>(&mut schemas, "BudgetSource")?;
    insert_schema::<CaptureLimit>(&mut schemas, "CaptureLimit")?;
    insert_schema::<CaptureBudget>(&mut schemas, "CaptureBudget")?;
    insert_schema::<StorageBudget>(&mut schemas, "StorageBudget")?;
    insert_schema::<crate::StorageQuotaSummary>(&mut schemas, "StorageQuotaSummary")?;
    insert_schema::<crate::StorageBudgetSummary>(&mut schemas, "StorageBudgetSummary")?;
    insert_schema::<CaptureStopReason>(&mut schemas, "CaptureStopReason")?;
    insert_schema::<CaptureScope>(&mut schemas, "CaptureScope")?;
    insert_schema::<CaptureCompleteness>(&mut schemas, "CaptureCompleteness")?;
    insert_schema::<SourceStateToken>(&mut schemas, "SourceStateToken")?;
    insert_schema::<SourceStateKind>(&mut schemas, "SourceStateKind")?;
    insert_schema::<SourceValidity>(&mut schemas, "SourceValidity")?;
    insert_schema::<ComparabilityAssessment>(&mut schemas, "ComparabilityAssessment")?;
    insert_schema::<DeltaFinding>(&mut schemas, "DeltaFinding")?;
    insert_schema::<ObservationDelta>(&mut schemas, "ObservationDelta")?;
    insert_schema::<VerificationObligation>(&mut schemas, "VerificationObligation")?;
    insert_schema::<ObligationEvaluation>(&mut schemas, "ObligationEvaluation")?;
    insert_schema::<ReadinessReport>(&mut schemas, "ReadinessReport")?;
    insert_schema::<CachePolicy>(&mut schemas, "CachePolicy")?;
    insert_schema::<TrustSettings>(&mut schemas, "TrustSettings")?;
    insert_schema::<AuthRef>(&mut schemas, "AuthRef")?;
    insert_schema::<DisclosureEnvelope>(&mut schemas, "DisclosureEnvelope")?;
    insert_schema::<EvidenceRef>(&mut schemas, "EvidenceRef")?;
    insert_schema::<InspectResponse>(&mut schemas, "InspectResponse")?;
    insert_schema::<Finding>(&mut schemas, "Finding")?;
    insert_schema::<FindingCommandHints>(&mut schemas, "FindingCommandHints")?;
    insert_schema::<EvidenceBlock>(&mut schemas, "EvidenceBlock")?;
    insert_schema::<EvidenceCitation>(&mut schemas, "EvidenceCitation")?;
    insert_schema::<SearchResponse>(&mut schemas, "SearchResponse")?;
    insert_schema::<SearchHit>(&mut schemas, "SearchHit")?;
    insert_schema::<LineRange>(&mut schemas, "LineRange")?;
    insert_schema::<ByteRange>(&mut schemas, "ByteRange")?;
    insert_schema::<SourceSpan>(&mut schemas, "SourceSpan")?;
    insert_schema::<SourceSpanExactness>(&mut schemas, "SourceSpanExactness")?;
    insert_schema::<RedactionState>(&mut schemas, "RedactionState")?;
    insert_schema::<Summary>(&mut schemas, "Summary")?;
    insert_schema::<OmittedRegion>(&mut schemas, "OmittedRegion")?;
    insert_schema::<ActionExactness>(&mut schemas, "ActionExactness")?;
    insert_schema::<NextAction>(&mut schemas, "NextAction")?;
    insert_schema::<LensManifest>(&mut schemas, "LensManifest")?;
    insert_schema::<LensFindingRule>(&mut schemas, "LensFindingRule")?;
    insert_schema::<LensMatch>(&mut schemas, "LensMatch")?;
    insert_schema::<LensView>(&mut schemas, "LensView")?;
    insert_schema::<LensOmission>(&mut schemas, "LensOmission")?;
    insert_schema::<LensFixtures>(&mut schemas, "LensFixtures")?;
    insert_schema::<SessionEvent>(&mut schemas, "SessionEvent")?;
    insert_schema::<SessionTrail>(&mut schemas, "SessionTrail")?;
    insert_schema::<SliceRequest>(&mut schemas, "SliceRequest")?;
    insert_schema::<CursorRecord>(&mut schemas, "CursorRecord")?;
    insert_schema::<CacheEntryMeta>(&mut schemas, "CacheEntryMeta")?;
    insert_schema::<CallProvenance>(&mut schemas, "CallProvenance")?;
    insert_schema::<CacheInfo>(&mut schemas, "CacheInfo")?;
    insert_schema::<crate::store::CacheList>(&mut schemas, "CacheList")?;
    insert_schema::<crate::store::PurgeSummary>(&mut schemas, "PurgeSummary")?;
    Ok(schemas)
}

fn insert_schema<T: schemars::JsonSchema>(
    schemas: &mut Map<String, Value>,
    name: &'static str,
) -> crate::Result<()> {
    schemas.insert(
        name.to_string(),
        serde_json::to_value(schemars::schema_for!(T))?,
    );
    Ok(())
}

fn sort_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(sort_json).collect()),
        Value::Object(map) => {
            let mut sorted = Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                sorted.insert(key.clone(), sort_json(&map[key]));
            }
            Value::Object(sorted)
        }
        scalar => scalar.clone(),
    }
}

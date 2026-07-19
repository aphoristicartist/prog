//! Report and output structs split from `main.rs` as part of #183.
//!
//! Move-only: these are pure data types (no impl blocks) consumed by `run()`
//! and the `commands/*` modules. Items and fields are `pub(crate)` so the
//! crate root and sibling command modules reach them via `use crate::*`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use prog_adapters::{cli::CliSource, http::HttpSource, mcp::McpSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::run::RunProcessStatus;

use prog_core::{
    CacheInfo, CachePolicy, CallProvenance, DisclosureEnvelope, EffectSet, EvidenceRef,
    LensManifest, NextAction, OmissionReason, OmittedRegion, RedactedPayload, Result, SliceRequest,
    SourceProfile, ValueScanReport,
};

#[derive(Clone, Serialize)]
pub(crate) struct DiscoverReport {
    pub(crate) schema: &'static str,
    pub(crate) source_id: String,
    pub(crate) kind: prog_core::SourceKind,
    pub(crate) profile_revision: u64,
    pub(crate) operations_found: usize,
    pub(crate) operations_probed: usize,
    pub(crate) shapes_learned: usize,
    pub(crate) import_format: Option<String>,
    pub(crate) schemas_imported: usize,
    pub(crate) examples_inferred: usize,
    pub(crate) warnings: Vec<String>,
    pub(crate) effects_assumed: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct SourceAddReport {
    pub(crate) schema: &'static str,
    pub(crate) source_id: String,
    pub(crate) kind: prog_core::SourceKind,
    pub(crate) operation: String,
    pub(crate) generated_seed: Value,
    pub(crate) discovery: DiscoverReport,
    pub(crate) next_steps: Vec<String>,
    pub(crate) structured_output: Vec<StructuredOutputHint>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StructuredOutputHint {
    pub(crate) status: &'static str,
    pub(crate) flag: Vec<String>,
    pub(crate) confidence: &'static str,
    pub(crate) reason: String,
}

#[derive(Serialize)]
pub(crate) struct HintsResponse {
    pub(crate) schema: &'static str,
    pub(crate) source_id: String,
    pub(crate) profile_revision: u64,
    pub(crate) observation_id: String,
    pub(crate) hints: Value,
    pub(crate) omitted: Vec<OmittedRegion>,
    pub(crate) cursor: Option<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct PreparedDiscovery {
    pub(crate) profile: SourceProfile,
    pub(crate) probe: Option<ProbeSource>,
    pub(crate) warnings: Vec<String>,
    pub(crate) effects_assumed: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum ProbeSource {
    Http(HttpSource),
    Cli(CliSource),
    Mcp(McpSource),
}

#[derive(Debug, Deserialize)]
pub(crate) struct GenericSeed {
    #[serde(default)]
    pub(crate) kind: Option<String>,
}

#[derive(Debug)]
pub(crate) enum CallableSource {
    Http(HttpSource),
    Cli(CliSource),
    Mcp(McpSource),
}

#[derive(Debug)]
pub(crate) struct AdapterCall {
    pub(crate) data: Value,
    pub(crate) provenance: Value,
    pub(crate) status: Option<String>,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) pagination: Option<Value>,
    pub(crate) warnings: Vec<String>,
    pub(crate) received_error: bool,
    pub(crate) not_modified: bool,
}

pub(crate) struct CallSourceResult {
    pub(crate) envelope: DisclosureEnvelope,
    pub(crate) received_error: bool,
}

pub(crate) struct EnvelopeInput {
    pub(crate) source_id: String,
    pub(crate) operation: String,
    pub(crate) source_kind: Option<String>,
    pub(crate) payload: RedactedPayload,
    pub(crate) root_path: String,
    pub(crate) slice: SliceRequest,
    pub(crate) payload_bytes: u64,
    pub(crate) observation_id: Option<String>,
    pub(crate) provenance: Option<CallProvenance>,
    pub(crate) cache: Option<CacheInfo>,
    pub(crate) effects: Option<EffectSet>,
    /// Audit note recorded when trust auto-upgrade relaxed a *proven* read-only
    /// op's `requires_confirmation` for this call. When `Some`, the observation
    /// metadata surfaces the evidence chain (grade + reason) under
    /// `observation.trust.extra["auto_upgrade"]`.
    pub(crate) auto_upgrade_audit: Option<String>,
    pub(crate) redacted_paths: usize,
    pub(crate) cache_disabled_reason: Option<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) schema_hints: BTreeMap<String, String>,
    pub(crate) next_action_operation: Option<String>,
    pub(crate) additional_next_actions: Vec<NextAction>,
    pub(crate) observation_parser: Option<Value>,
    pub(crate) lens: Option<LensManifest>,
    pub(crate) value_scan: Option<ValueScanReport>,
}

pub(crate) struct CursorInput<'a> {
    pub(crate) cache_key: &'a str,
    pub(crate) source_id: &'a str,
    pub(crate) operation: &'a str,
    pub(crate) root_path: &'a str,
    pub(crate) payload: &'a RedactedPayload,
    pub(crate) slice: &'a SliceRequest,
    pub(crate) cache: &'a CachePolicy,
    pub(crate) may_cache: bool,
    pub(crate) lens: Option<&'a LensManifest>,
}

pub(crate) struct ObservationInput {
    pub(crate) name: String,
    pub(crate) input: Value,
    pub(crate) mime: String,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) struct NormalizedObservation {
    pub(crate) kind: String,
    pub(crate) payload: Value,
    pub(crate) parser: ObservationParserInfo,
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct ObservationParserInfo {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) confidence: f64,
    pub(crate) lossy: bool,
    pub(crate) fallback: bool,
    pub(crate) reason: &'static str,
    pub(crate) path_semantics: &'static str,
    pub(crate) range_semantics: &'static str,
}

pub(crate) struct ParserMatch {
    pub(crate) confidence: f64,
    pub(crate) reason: &'static str,
}

pub(crate) struct ObservationParser {
    pub(crate) id: &'static str,
    pub(crate) detect: fn(&[u8], &str) -> Option<ParserMatch>,
    pub(crate) parse: fn(&[u8], &str, ParserMatch) -> Result<NormalizedObservation>,
}

pub(crate) struct RunEnvelopeResult {
    pub(crate) envelope: DisclosureEnvelope,
    pub(crate) exit_code: RunExitCode,
}

#[derive(Clone, Copy)]
pub(crate) enum RunExitCode {
    Success,
    Code(i32),
    Signal(i32),
    Timeout,
    SpawnError,
}

pub(crate) struct RunCapture {
    pub(crate) stream: &'static str,
    pub(crate) bytes: Vec<u8>,
    pub(crate) total_bytes: usize,
    pub(crate) truncated: bool,
}

pub(crate) struct RunChunk {
    pub(crate) stream: &'static str,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) struct RunText {
    pub(crate) text: String,
    pub(crate) head: Vec<String>,
    pub(crate) tail: Vec<String>,
    pub(crate) line_count: usize,
    pub(crate) byte_count: usize,
    pub(crate) captured_bytes: usize,
    pub(crate) truncated: bool,
    pub(crate) utf8_valid: bool,
    pub(crate) redactions: usize,
}

#[derive(Clone)]
pub(crate) struct RunFailureSection {
    pub(crate) kind: &'static str,
    pub(crate) stream: &'static str,
    pub(crate) line_start: usize,
    pub(crate) line_end: usize,
    pub(crate) lines: Vec<String>,
    pub(crate) reason: String,
    pub(crate) priority: u8,
}

pub(crate) struct RunPayloadInput<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) argv: &'a [String],
    pub(crate) redacted_argv: &'a [String],
    pub(crate) cwd: &'a Path,
    pub(crate) started_at: chrono::DateTime<Utc>,
    pub(crate) ended_at: chrono::DateTime<Utc>,
    pub(crate) duration_ms: u64,
    pub(crate) status: &'a RunProcessStatus,
    pub(crate) stdout: &'a RunText,
    pub(crate) stderr: &'a RunText,
    pub(crate) combined: Vec<Value>,
    pub(crate) failure_sections: &'a [RunFailureSection],
    pub(crate) out: Option<&'a PathBuf>,
}

pub(crate) struct InitFileSpec {
    pub(crate) relative_path: String,
    pub(crate) content: String,
    pub(crate) executable: bool,
}

pub(crate) struct EvidenceRefInput<'a> {
    pub(crate) source_id: &'a str,
    pub(crate) operation: &'a str,
    pub(crate) cursor: Option<&'a str>,
    pub(crate) path: &'a str,
    pub(crate) value: &'a Value,
    pub(crate) observation: Option<&'a prog_core::ObservationRecord>,
    pub(crate) provenance: Option<&'a CallProvenance>,
    pub(crate) cache: Option<&'a CacheInfo>,
    pub(crate) omitted: &'a [OmittedRegion],
    pub(crate) redacted_paths: usize,
}

pub(crate) struct PathEvidenceContext<'a> {
    pub(crate) record: &'a prog_core::CursorRecord,
    pub(crate) entry: &'a prog_core::CacheEntryMeta,
    pub(crate) observation: Option<&'a prog_core::ObservationRecord>,
    pub(crate) cache: &'a CacheInfo,
    pub(crate) omitted: &'a [OmittedRegion],
    pub(crate) cursor: &'a str,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ModelCostProfile {
    #[serde(default)]
    pub(crate) schema: Option<String>,
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) input_price_per_million_tokens: Option<f64>,
    #[serde(default)]
    pub(crate) output_price_per_million_tokens: Option<f64>,
    pub(crate) context_window_tokens: u64,
    #[serde(default)]
    pub(crate) cache_read_price_per_million_tokens: Option<f64>,
    #[serde(default)]
    pub(crate) cache_write_price_per_million_tokens: Option<f64>,
    #[serde(default)]
    pub(crate) pricing_source: Option<String>,
    #[serde(default)]
    pub(crate) priced_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CostReport {
    pub(crate) schema: &'static str,
    pub(crate) model: CostModelSummary,
    pub(crate) input: CostInputSummary,
    pub(crate) scenarios: Vec<CostScenario>,
    pub(crate) warnings: Vec<String>,
    pub(crate) counterexamples: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CostModelSummary {
    pub(crate) model: String,
    pub(crate) input_price_per_million_tokens: f64,
    pub(crate) output_price_per_million_tokens: f64,
    pub(crate) context_window_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_read_price_per_million_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache_write_price_per_million_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pricing_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) priced_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CostInputSummary {
    pub(crate) raw_file: String,
    pub(crate) raw_bytes: u64,
    pub(crate) raw_tokens: u64,
    pub(crate) mime: String,
    pub(crate) expand_paths: Vec<String>,
    pub(crate) estimated_output_tokens: u64,
    pub(crate) repeated_inspections: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct CostScenario {
    pub(crate) name: &'static str,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) total_estimated_cost_usd: f64,
    pub(crate) baseline_input_tokens: u64,
    pub(crate) baseline_estimated_cost_usd: f64,
    pub(crate) savings_ratio: f64,
    pub(crate) fits_context: bool,
    pub(crate) lossless: bool,
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InitReport {
    pub(crate) schema: &'static str,
    pub(crate) agent: &'static str,
    pub(crate) scope: &'static str,
    pub(crate) root: String,
    pub(crate) dry_run: bool,
    pub(crate) files: Vec<InitFileReport>,
    pub(crate) next_steps: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InitFileReport {
    pub(crate) path: String,
    pub(crate) full_path: String,
    pub(crate) action: &'static str,
    pub(crate) executable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PathsResponse {
    pub(crate) schema: &'static str,
    pub(crate) cursor: String,
    pub(crate) source_id: String,
    pub(crate) operation: String,
    pub(crate) root_path: String,
    pub(crate) prefix: String,
    pub(crate) paths: Vec<PathEntry>,
    pub(crate) omitted: Vec<OmittedRegion>,
    pub(crate) next_actions: Vec<NextAction>,
    pub(crate) cache: CacheInfo,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PathEntry {
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) expandable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) omitted_reason: Option<OmissionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence_ref: Option<EvidenceRef>,
}

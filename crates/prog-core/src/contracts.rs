use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::shape::Shape;

pub const SOURCE_PROFILE_VERSION: &str = "prog.source_profile.v1";
pub const DISCLOSURE_VERSION: &str = "prog.disclosure.v1";
pub const LENS_MANIFEST_VERSION: &str = "prog.lens_manifest.v1";

pub type Extra = Map<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SourceProfile {
    pub schema_version: String,
    pub id: String,
    pub kind: SourceKind,
    #[serde(default)]
    pub version: u64,
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
    #[serde(default, flatten)]
    pub extra: Extra,
}

impl Default for TrustSettings {
    fn default() -> Self {
        Self {
            allow_shell: false,
            allow_network: false,
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
    pub schema_version: String,
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
    pub completeness: ObservationCompleteness,
    pub freshness: ObservationFreshness,
    pub trust: ObservationTrust,
    pub safety: ObservationSafety,
    pub payload: ObservationPayloadStatus,
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
    pub schema_version: String,
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
    pub redacted: bool,
    pub lossy: bool,
    #[serde(default)]
    pub redacted_slice_sha256: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct NextAction {
    pub kind: String,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default, flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct LensManifest {
    pub schema_version: String,
    pub id: String,
    #[serde(default)]
    pub version: u64,
    #[serde(default, rename = "match")]
    pub match_rules: LensMatch,
    #[serde(default)]
    pub view: LensView,
    #[serde(default)]
    pub omit: Vec<LensOmission>,
    #[serde(default)]
    pub next_actions: Vec<NextAction>,
    #[serde(default)]
    pub invariants: Vec<String>,
    #[serde(default)]
    pub fixtures: LensFixtures,
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
#[serde(rename_all = "snake_case")]
pub struct CacheInfo {
    pub status: CacheStatus,
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub age_seconds: Option<u64>,
    #[serde(default, flatten)]
    pub extra: Extra,
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

pub fn canonical_json(value: &Value) -> crate::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&sort_json(value))?)
}

pub fn public_contract_schemas() -> crate::Result<Map<String, Value>> {
    let mut schemas = Map::new();
    insert_schema::<SourceProfile>(&mut schemas, "SourceProfile")?;
    insert_schema::<OperationProfile>(&mut schemas, "OperationProfile")?;
    insert_schema::<Shape>(&mut schemas, "Shape")?;
    insert_schema::<EffectSet>(&mut schemas, "EffectSet")?;
    insert_schema::<ObservationMetadata>(&mut schemas, "ObservationMetadata")?;
    insert_schema::<CachePolicy>(&mut schemas, "CachePolicy")?;
    insert_schema::<TrustSettings>(&mut schemas, "TrustSettings")?;
    insert_schema::<AuthRef>(&mut schemas, "AuthRef")?;
    insert_schema::<DisclosureEnvelope>(&mut schemas, "DisclosureEnvelope")?;
    insert_schema::<EvidenceRef>(&mut schemas, "EvidenceRef")?;
    insert_schema::<Summary>(&mut schemas, "Summary")?;
    insert_schema::<OmittedRegion>(&mut schemas, "OmittedRegion")?;
    insert_schema::<NextAction>(&mut schemas, "NextAction")?;
    insert_schema::<LensManifest>(&mut schemas, "LensManifest")?;
    insert_schema::<LensMatch>(&mut schemas, "LensMatch")?;
    insert_schema::<LensView>(&mut schemas, "LensView")?;
    insert_schema::<LensOmission>(&mut schemas, "LensOmission")?;
    insert_schema::<LensFixtures>(&mut schemas, "LensFixtures")?;
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

use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use chrono::{DateTime, SecondsFormat, Utc};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    CacheEntryMeta, CallProvenance, CaptureCompleteness, CoreError, CursorRecord,
    EvidenceAvailability, OBSERVATION_SCHEMA, ObservationLineage, ObservationRecord,
    PersistedPayload, RedactedPayload, RedactionPolicy, Result, SESSION_SCHEMA, SessionEvent,
    SessionTrail, SourceProfile, VERIFICATION_SCHEMA, ValidatedCursor, VerificationObligation,
    canonical_json, validate_source_profile,
};

const PAYLOADS: TableDefinition<&str, &[u8]> = TableDefinition::new("payloads");
const ENTRIES: TableDefinition<&str, &[u8]> = TableDefinition::new("entries");
const CURSORS: TableDefinition<&str, &[u8]> = TableDefinition::new("cursors");
const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");
const OBSERVATIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("observations");
const OBSERVATION_SUBJECTS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("observation_subjects");
const OBSERVATION_LINEAGE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("observation_lineage");
const OBLIGATIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("obligations");
const STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("state");
const CURRENT_SESSION_KEY: &str = "current_session";
const STORE_SCHEMA_KEY: &str = "store_schema";
// Pre-release storage is intentionally reset, rather than migrated, whenever
// an immutable-record invariant changes. This is a contract identity, not a
// compatibility version.
const STORE_SCHEMA: &str = "prog.store.capture_lifecycle";

#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    db: Database,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CacheList {
    pub entries: Vec<CacheEntryMeta>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObservationList {
    pub observations: Vec<ObservationRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ObligationList {
    pub obligations: Vec<VerificationObligation>,
}

#[derive(Debug, Clone, Default)]
pub struct NewObservation {
    pub payload_hash: String,
    pub availability: EvidenceAvailability,
    pub invocation_fingerprint: String,
    pub source_id: String,
    pub operation: String,
    pub subject_keys: Vec<String>,
    pub captured_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub status: Option<String>,
    pub capture: CaptureCompleteness,
    pub redacted: bool,
    pub provider: Option<String>,
    pub parser: Option<String>,
    pub lens: Option<String>,
    pub workspace_state: Option<String>,
    pub source_state: Option<crate::SourceStateToken>,
    pub environment_state: Option<String>,
    pub lineage: ObservationLineage,
    pub provenance: Option<CallProvenance>,
    pub cache_key: Option<String>,
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PurgeSummary {
    pub purged_entries: usize,
    pub purged_payloads: usize,
    pub purged_cursors: usize,
    #[serde(default)]
    pub purged_sessions: usize,
    #[serde(default)]
    pub purged_observations: usize,
}

/// Result of enforcing a maximum retained size over redacted payload blobs.
/// Payloads are deduplicated by content hash, so byte accounting is per blob,
/// while eviction removes every cache entry that points at a selected blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct StorageQuotaSummary {
    pub max_payload_bytes: u64,
    pub payload_bytes_before: u64,
    pub payload_bytes_retained: u64,
    pub evicted_entries: usize,
    pub evicted_payloads: usize,
    pub evicted_cursors: usize,
    pub metadata_only_observations: usize,
}

#[derive(Debug, Clone, Default)]
pub struct NewSessionEvent {
    pub kind: String,
    pub cursor: Option<String>,
    pub path: Option<String>,
    pub evidence_ref: Option<String>,
    pub summary: Option<String>,
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Store {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let profiles = dir.join("profiles");
        let cache = dir.join("cache");
        let logs = dir.join("logs");
        fs::create_dir_all(&profiles)?;
        fs::create_dir_all(&cache)?;
        fs::create_dir_all(&logs)?;
        set_dir_permissions(&dir)?;
        set_dir_permissions(&profiles)?;
        set_dir_permissions(&cache)?;
        set_dir_permissions(&logs)?;

        let db_path = cache.join("data.redb");
        let store_existed = db_path.exists();
        let db = Database::create(&db_path).map_err(CoreError::storage)?;
        set_file_permissions(&db_path)?;

        let write = db.begin_write().map_err(CoreError::storage)?;
        {
            write.open_table(PAYLOADS).map_err(CoreError::storage)?;
            write.open_table(ENTRIES).map_err(CoreError::storage)?;
            write.open_table(CURSORS).map_err(CoreError::storage)?;
            write.open_table(SESSIONS).map_err(CoreError::storage)?;
            write.open_table(OBSERVATIONS).map_err(CoreError::storage)?;
            write
                .open_table(OBSERVATION_SUBJECTS)
                .map_err(CoreError::storage)?;
            write
                .open_table(OBSERVATION_LINEAGE)
                .map_err(CoreError::storage)?;
            write.open_table(OBLIGATIONS).map_err(CoreError::storage)?;
            write.open_table(STATE).map_err(CoreError::storage)?;
        }
        write.commit().map_err(CoreError::storage)?;

        let read = db.begin_read().map_err(CoreError::storage)?;
        let state = read.open_table(STATE).map_err(CoreError::storage)?;
        let store_schema = state
            .get(STORE_SCHEMA_KEY)
            .map_err(CoreError::storage)?
            .map(|value| String::from_utf8_lossy(value.value()).into_owned());
        drop(state);
        drop(read);
        if store_schema.as_deref() != Some(STORE_SCHEMA) {
            let write = db.begin_write().map_err(CoreError::storage)?;
            let dropped;
            {
                let mut entries = write.open_table(ENTRIES).map_err(CoreError::storage)?;
                let mut payloads = write.open_table(PAYLOADS).map_err(CoreError::storage)?;
                let mut cursors = write.open_table(CURSORS).map_err(CoreError::storage)?;
                let mut sessions = write.open_table(SESSIONS).map_err(CoreError::storage)?;
                let mut observations =
                    write.open_table(OBSERVATIONS).map_err(CoreError::storage)?;
                let mut observation_subjects = write
                    .open_table(OBSERVATION_SUBJECTS)
                    .map_err(CoreError::storage)?;
                let mut observation_lineage = write
                    .open_table(OBSERVATION_LINEAGE)
                    .map_err(CoreError::storage)?;
                let mut obligations = write.open_table(OBLIGATIONS).map_err(CoreError::storage)?;
                let mut state = write.open_table(STATE).map_err(CoreError::storage)?;
                dropped = if store_existed {
                    retain_none(&mut entries)?
                        + retain_none(&mut payloads)?
                        + retain_none(&mut cursors)?
                        + retain_none(&mut sessions)?
                        + retain_none(&mut observations)?
                        + retain_none(&mut observation_subjects)?
                        + retain_none(&mut observation_lineage)?
                        + retain_none(&mut obligations)?
                        + retain_none(&mut state)?
                } else {
                    0
                };
                state
                    .insert(STORE_SCHEMA_KEY, STORE_SCHEMA.as_bytes())
                    .map_err(CoreError::storage)?;
            }
            write.commit().map_err(CoreError::storage)?;
            if store_existed {
                eprintln!("{}", store_reset_notice(&dir, dropped));
            }
        }

        Ok(Self { dir, db })
    }

    pub fn put_payload(&self, payload: &RedactedPayload) -> Result<String> {
        let bytes = serde_json::to_vec(payload.as_value())?;
        let hash = format!("sha256:{}", hex_sha256(&bytes));
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        {
            let mut table = write.open_table(PAYLOADS).map_err(CoreError::storage)?;
            table
                .insert(hash.as_str(), bytes.as_slice())
                .map_err(CoreError::storage)?;
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(hash)
    }

    /// Return the digest used for persisted payload identity without storing
    /// bytes. Sensitive and explicitly non-cacheable captures still need an
    /// immutable, redacted payload reference in their metadata record.
    pub fn payload_hash(payload: &RedactedPayload) -> Result<String> {
        let bytes = serde_json::to_vec(payload.as_value())?;
        Ok(format!("sha256:{}", hex_sha256(&bytes)))
    }

    /// Append one immutable capture record. This method intentionally has no
    /// update counterpart: cache reuse records access elsewhere and must not
    /// rewrite the original execution evidence.
    pub fn record_observation(&self, input: NewObservation) -> Result<ObservationRecord> {
        validate_observation_input(&input)?;
        let redaction = RedactionPolicy::default();
        let (redacted_extra, _) = redaction.apply_persistence(&Value::Object(input.extra));
        let record = ObservationRecord {
            schema: OBSERVATION_SCHEMA.to_string(),
            observation_id: format!("obs_{}", Uuid::new_v4().simple()),
            payload_hash: input.payload_hash,
            availability: input.availability,
            invocation_fingerprint: input.invocation_fingerprint,
            source_id: input.source_id,
            operation: input.operation,
            subject_keys: input.subject_keys,
            captured_at: input.captured_at.unwrap_or_else(|| format_time(Utc::now())),
            duration_ms: input.duration_ms,
            status: input.status,
            capture: input.capture,
            redacted: input.redacted,
            provider: input.provider,
            parser: input.parser,
            lens: input.lens,
            workspace_state: input.workspace_state,
            source_state: input.source_state,
            environment_state: input.environment_state,
            lineage: input.lineage,
            provenance: input.provenance,
            cache_key: input.cache_key,
            extra: redacted_extra.as_object().cloned().unwrap_or_default(),
        };
        let bytes = serde_json::to_vec(&record)?;
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        {
            let mut table = write.open_table(OBSERVATIONS).map_err(CoreError::storage)?;
            let mut subjects = write
                .open_table(OBSERVATION_SUBJECTS)
                .map_err(CoreError::storage)?;
            let mut lineage = write
                .open_table(OBSERVATION_LINEAGE)
                .map_err(CoreError::storage)?;
            table
                .insert(record.observation_id.as_str(), bytes.as_slice())
                .map_err(CoreError::storage)?;
            for subject_key in &record.subject_keys {
                let key = observation_index_key(subject_key, &record.observation_id);
                subjects
                    .insert(key.as_str(), b"".as_slice())
                    .map_err(CoreError::storage)?;
            }
            for related_id in observation_lineage_ids(&record.lineage) {
                let key = observation_index_key(related_id, &record.observation_id);
                lineage
                    .insert(key.as_str(), b"".as_slice())
                    .map_err(CoreError::storage)?;
            }
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(record)
    }

    pub fn get_observation(&self, observation_id: &str) -> Result<Option<ObservationRecord>> {
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(OBSERVATIONS).map_err(CoreError::storage)?;
        let Some(value) = table.get(observation_id).map_err(CoreError::storage)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(value.value())?))
    }

    /// List retained records in newest-first, deterministic order. Callers may
    /// request at most 100 records to avoid turning metadata into a new large
    /// observation surface.
    pub fn list_observations(&self, limit: usize) -> Result<ObservationList> {
        let limit = limit.min(100);
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(OBSERVATIONS).map_err(CoreError::storage)?;
        let mut observations = Vec::new();
        for entry in table.iter().map_err(CoreError::storage)? {
            let (_, value) = entry.map_err(CoreError::storage)?;
            observations.push(serde_json::from_slice::<ObservationRecord>(value.value())?);
        }
        observations.sort_by(|left, right| {
            right
                .captured_at
                .cmp(&left.captured_at)
                .then_with(|| right.observation_id.cmp(&left.observation_id))
        });
        observations.truncate(limit);
        Ok(ObservationList { observations })
    }

    pub fn get_payload(&self, hash: &str) -> Result<Option<PersistedPayload>> {
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(PAYLOADS).map_err(CoreError::storage)?;
        let Some(bytes) = table.get(hash).map_err(CoreError::storage)? else {
            return Ok(None);
        };
        Ok(Some(PersistedPayload::from_store(serde_json::from_slice(
            bytes.value(),
        )?)))
    }

    pub fn cache_key(source_id: &str, operation: &str, args: &Value) -> Result<String> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(source_id.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(operation.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&canonical_json(args)?);
        Ok(format!("sha256:{}", hex_sha256(&bytes)))
    }

    pub fn put_entry(&self, key: &str, meta: &CacheEntryMeta) -> Result<()> {
        if !meta.cacheable || meta.sensitive {
            return Ok(());
        }
        let bytes = serde_json::to_vec(meta)?;
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        {
            let mut table = write.open_table(ENTRIES).map_err(CoreError::storage)?;
            table
                .insert(key, bytes.as_slice())
                .map_err(CoreError::storage)?;
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(())
    }

    pub fn get_entry(&self, key: &str) -> Result<Option<CacheEntryMeta>> {
        self.get_entry_at(key, Utc::now())
    }

    pub fn get_entry_at(&self, key: &str, now: DateTime<Utc>) -> Result<Option<CacheEntryMeta>> {
        let Some(meta) = self.read_entry(key)? else {
            return Ok(None);
        };
        if parse_time(&meta.expires_at)? <= now {
            self.mark_observations_expired(meta.observation_id.as_deref())?;
            return Ok(None);
        }
        Ok(Some(meta))
    }

    pub fn list_entries(&self, limit: usize) -> Result<CacheList> {
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(ENTRIES).map_err(CoreError::storage)?;
        let mut entries = Vec::new();
        for entry in table.iter().map_err(CoreError::storage)? {
            let (_, value) = entry.map_err(CoreError::storage)?;
            entries.push(serde_json::from_slice(value.value())?);
            if entries.len() >= limit {
                break;
            }
        }
        Ok(CacheList { entries })
    }

    pub fn create_cursor(
        &self,
        cache_key: &str,
        source_id: &str,
        operation: &str,
        root_path: &str,
        redaction_version: u32,
        ttl_seconds: i64,
    ) -> Result<String> {
        self.create_cursor_with_extra(
            cache_key,
            source_id,
            operation,
            root_path,
            redaction_version,
            ttl_seconds,
            serde_json::Map::new(),
        )
    }

    /// Mint a `pc1_` cursor carrying extra metadata in its `CursorRecord`.
    /// Used by auto-pagination to stamp each page cursor with
    /// `{kind:"page", page:N, args:...}` so the page cursors are observably
    /// distinct from a normal expand cursor while reusing the exact same
    /// fail-closed validation path (I9: stale/foreign/redaction-mismatch).
    #[allow(clippy::too_many_arguments)] // one more than create_cursor's 7-arg limit, for the page metadata map
    pub fn create_cursor_with_extra(
        &self,
        cache_key: &str,
        source_id: &str,
        operation: &str,
        root_path: &str,
        redaction_version: u32,
        ttl_seconds: i64,
        extra: serde_json::Map<String, serde_json::Value>,
    ) -> Result<String> {
        let token = format!("pc1_{}", Uuid::new_v4().simple());
        let now = Utc::now();
        let observation_id = self
            .get_entry(cache_key)?
            .and_then(|entry| entry.observation_id);
        let record = CursorRecord {
            cache_key: cache_key.to_string(),
            source_id: source_id.to_string(),
            operation: operation.to_string(),
            root_path: root_path.to_string(),
            redaction_version,
            created_at: format_time(now),
            expires_at: format_time(now + chrono::Duration::seconds(ttl_seconds)),
            observation_id,
            extra,
        };
        self.put_cursor(&token, &record)?;
        Ok(token)
    }

    pub fn put_cursor(&self, token: &str, record: &CursorRecord) -> Result<()> {
        let bytes = serde_json::to_vec(record)?;
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        {
            let mut table = write.open_table(CURSORS).map_err(CoreError::storage)?;
            table
                .insert(token, bytes.as_slice())
                .map_err(CoreError::storage)?;
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(())
    }

    pub fn get_cursor(&self, token: &str, redaction_version: u32) -> Result<ValidatedCursor> {
        self.get_cursor_at(token, redaction_version, Utc::now())
    }

    pub fn get_cursor_at(
        &self,
        token: &str,
        redaction_version: u32,
        now: DateTime<Utc>,
    ) -> Result<ValidatedCursor> {
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(CURSORS).map_err(CoreError::storage)?;
        let Some(bytes) = table.get(token).map_err(CoreError::storage)? else {
            return Err(CoreError::CursorNotFound(token.to_string()));
        };
        let record: CursorRecord = serde_json::from_slice(bytes.value())?;
        drop(table);
        drop(read);
        if parse_time(&record.expires_at)? <= now {
            self.mark_observations_expired(record.observation_id.as_deref())?;
            return Err(CoreError::CursorExpired(
                token.to_string(),
                record.expires_at.clone(),
            ));
        }
        if record.redaction_version != redaction_version {
            return Err(CoreError::RedactionVersionMismatch {
                cursor: token.to_string(),
                cursor_version: record.redaction_version,
                store_version: redaction_version,
            });
        }
        Ok(ValidatedCursor::new(token.to_string(), record))
    }

    pub fn start_session(&self, goal: Option<String>) -> Result<SessionTrail> {
        let now = format_time(Utc::now());
        let session_id = format!("ps1_{}", Uuid::new_v4().simple());
        let goal = goal.filter(|goal| !goal.trim().is_empty()).map(|goal| {
            let (redacted, _) = RedactionPolicy::default().apply_persistence(&Value::String(goal));
            redacted
                .as_str()
                .unwrap_or("[REDACTED:session_goal]")
                .chars()
                .take(500)
                .collect::<String>()
        });
        let trail = SessionTrail {
            schema: SESSION_SCHEMA.to_string(),
            session_id: session_id.clone(),
            goal,
            created_at: now.clone(),
            updated_at: now,
            events: Vec::new(),
            warnings: Vec::new(),
            extra: serde_json::Map::new(),
        };
        let bytes = serde_json::to_vec(&trail)?;
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        {
            let mut sessions = write.open_table(SESSIONS).map_err(CoreError::storage)?;
            let mut state = write.open_table(STATE).map_err(CoreError::storage)?;
            sessions
                .insert(session_id.as_str(), bytes.as_slice())
                .map_err(CoreError::storage)?;
            state
                .insert(CURRENT_SESSION_KEY, session_id.as_bytes())
                .map_err(CoreError::storage)?;
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(trail)
    }

    pub fn current_session_id(&self) -> Result<Option<String>> {
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let state = read.open_table(STATE).map_err(CoreError::storage)?;
        Ok(state
            .get(CURRENT_SESSION_KEY)
            .map_err(CoreError::storage)?
            .map(|value| String::from_utf8_lossy(value.value()).into_owned()))
    }

    pub fn get_session(&self, session_id: Option<&str>) -> Result<Option<SessionTrail>> {
        let owned;
        let session_id = match session_id {
            Some(session_id) => session_id,
            None => {
                owned = self.current_session_id()?;
                let Some(session_id) = owned.as_deref() else {
                    return Ok(None);
                };
                session_id
            }
        };
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let sessions = read.open_table(SESSIONS).map_err(CoreError::storage)?;
        let Some(value) = sessions.get(session_id).map_err(CoreError::storage)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(value.value())?))
    }

    /// Find the newest earlier capture in the active session with exactly the
    /// same canonical invocation identity. Session events are only an index;
    /// the immutable observation record remains the source of truth.
    pub fn latest_session_predecessor(
        &self,
        invocation_fingerprint: &str,
        exclude_observation_id: &str,
    ) -> Result<Option<ObservationRecord>> {
        let Some(trail) = self.get_session(None)? else {
            return Ok(None);
        };
        for event in trail.events.iter().rev() {
            let Some(observation_id) = event.extra.get("observation_id").and_then(Value::as_str)
            else {
                continue;
            };
            if observation_id == exclude_observation_id {
                continue;
            }
            let Some(observation) = self.get_observation(observation_id)? else {
                continue;
            };
            if observation.invocation_fingerprint == invocation_fingerprint {
                return Ok(Some(observation));
            }
        }
        Ok(None)
    }

    pub fn put_obligation(&self, obligation: &VerificationObligation) -> Result<()> {
        if obligation.schema != VERIFICATION_SCHEMA
            || obligation.id.trim().is_empty()
            || obligation.session_id.trim().is_empty()
            || obligation.intended_check.trim().is_empty()
        {
            return Err(CoreError::BadArgs {
                operation: "verification obligation".to_string(),
                reason: "schema, id, session_id, and intended_check are required".to_string(),
            });
        }
        let key = obligation_key(&obligation.session_id, &obligation.id)?;
        let bytes = serde_json::to_vec(obligation)?;
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        {
            let mut table = write.open_table(OBLIGATIONS).map_err(CoreError::storage)?;
            if table
                .get(key.as_str())
                .map_err(CoreError::storage)?
                .is_some()
            {
                return Err(CoreError::BadArgs {
                    operation: "verification obligation".to_string(),
                    reason: format!(
                        "obligation '{}' already exists in this session",
                        obligation.id
                    ),
                });
            }
            table
                .insert(key.as_str(), bytes.as_slice())
                .map_err(CoreError::storage)?;
        }
        write.commit().map_err(CoreError::storage)
    }

    pub fn list_obligations(&self, session_id: Option<&str>) -> Result<ObligationList> {
        let session_id = match session_id {
            Some(value) => value.to_string(),
            None => self.current_session_id()?.unwrap_or_default(),
        };
        if session_id.is_empty() {
            return Ok(ObligationList {
                obligations: Vec::new(),
            });
        }
        let prefix = format!("{session_id}\0");
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(OBLIGATIONS).map_err(CoreError::storage)?;
        let mut obligations = Vec::new();
        for entry in table.iter().map_err(CoreError::storage)? {
            let (key, value) = entry.map_err(CoreError::storage)?;
            if key.value().starts_with(&prefix) {
                obligations.push(serde_json::from_slice(value.value())?);
            }
        }
        obligations.sort_by(|left: &VerificationObligation, right| left.id.cmp(&right.id));
        Ok(ObligationList { obligations })
    }

    pub fn record_session_event(&self, input: NewSessionEvent) -> Result<SessionEvent> {
        if input.kind.trim().is_empty() {
            return Err(CoreError::BadArgs {
                operation: "session event".to_string(),
                reason: "event kind must not be empty".to_string(),
            });
        }
        let mut trail = match self.get_session(None)? {
            Some(trail) => trail,
            None => self.start_session(None)?,
        };
        let sequence = trail.events.last().map_or(1, |event| event.sequence + 1);
        let timestamp = format_time(Utc::now());
        let redaction = RedactionPolicy::default();
        let summary = input.summary.map(|summary| {
            let (redacted, _) = redaction.apply_persistence(&Value::String(summary));
            let summary = redacted.as_str().unwrap_or("[REDACTED:session_summary]");
            if summary.chars().count() <= 240 {
                return summary.to_string();
            }
            let mut truncated = summary.chars().take(237).collect::<String>();
            truncated.push_str("...");
            truncated
        });
        let (redacted_extra, _) = redaction.apply_persistence(&Value::Object(input.extra));
        let extra = redacted_extra.as_object().cloned().unwrap_or_default();
        let event = SessionEvent {
            id: format!("pe1_{}", Uuid::new_v4().simple()),
            session_id: trail.session_id.clone(),
            sequence,
            timestamp: timestamp.clone(),
            kind: input.kind,
            cursor: input.cursor,
            path: input.path,
            evidence_ref: input.evidence_ref,
            summary,
            extra,
        };
        trail.updated_at = timestamp;
        trail.events.push(event.clone());
        if trail.events.len() > 1_000 {
            let remove = trail.events.len() - 1_000;
            trail.events.drain(..remove);
            trail
                .warnings
                .push("oldest session events were dropped at the 1000-event cap".to_string());
        }
        let bytes = serde_json::to_vec(&trail)?;
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        {
            let mut sessions = write.open_table(SESSIONS).map_err(CoreError::storage)?;
            sessions
                .insert(trail.session_id.as_str(), bytes.as_slice())
                .map_err(CoreError::storage)?;
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(event)
    }

    pub fn purge_all(&self) -> Result<PurgeSummary> {
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        let summary;
        {
            let mut entries = write.open_table(ENTRIES).map_err(CoreError::storage)?;
            let mut payloads = write.open_table(PAYLOADS).map_err(CoreError::storage)?;
            let mut cursors = write.open_table(CURSORS).map_err(CoreError::storage)?;
            let mut sessions = write.open_table(SESSIONS).map_err(CoreError::storage)?;
            let mut observations = write.open_table(OBSERVATIONS).map_err(CoreError::storage)?;
            let mut observation_subjects = write
                .open_table(OBSERVATION_SUBJECTS)
                .map_err(CoreError::storage)?;
            let mut observation_lineage = write
                .open_table(OBSERVATION_LINEAGE)
                .map_err(CoreError::storage)?;
            let mut obligations = write.open_table(OBLIGATIONS).map_err(CoreError::storage)?;
            let mut state = write.open_table(STATE).map_err(CoreError::storage)?;
            summary = PurgeSummary {
                purged_entries: retain_none(&mut entries)?,
                purged_payloads: retain_none(&mut payloads)?,
                purged_cursors: retain_none(&mut cursors)?,
                purged_sessions: retain_none(&mut sessions)?,
                purged_observations: retain_none(&mut observations)?,
            };
            retain_none(&mut observation_subjects)?;
            retain_none(&mut observation_lineage)?;
            retain_none(&mut obligations)?;
            retain_none(&mut state)?;
            state
                .insert(STORE_SCHEMA_KEY, STORE_SCHEMA.as_bytes())
                .map_err(CoreError::storage)?;
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(summary)
    }

    pub fn purge_expired(&self, now: DateTime<Utc>) -> Result<PurgeSummary> {
        let expired = self.expired_entries(now)?;
        let keys = expired
            .iter()
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();
        let summary = self.purge_entries_and_cursors(&keys, None)?;
        self.mark_observations_expired(
            expired
                .iter()
                .filter_map(|entry| entry.observation_id.as_deref()),
        )?;
        Ok(summary)
    }

    pub fn purge_source(&self, source_id: &str) -> Result<PurgeSummary> {
        let mut keys = Vec::new();
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(ENTRIES).map_err(CoreError::storage)?;
        for entry in table.iter().map_err(CoreError::storage)? {
            let (key, value) = entry.map_err(CoreError::storage)?;
            let meta: CacheEntryMeta = serde_json::from_slice(value.value())?;
            if meta.source_id == source_id {
                keys.push(key.value().to_string());
            }
        }
        drop(table);
        drop(read);
        self.purge_entries_and_cursors(&keys, Some(source_id))
    }

    /// Enforce a hard quota over durable, already-redacted payload blobs.
    ///
    /// The eviction unit is a complete payload-reference group, never one
    /// entry from a shared blob. Groups are selected oldest first by their
    /// newest cache entry, then hash, so a live shared payload is never left
    /// behind with a missing expansion target. Immutable observations survive
    /// as metadata-only lineage records.
    pub fn enforce_payload_quota(&self, max_payload_bytes: u64) -> Result<StorageQuotaSummary> {
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        let summary;
        {
            let mut entries = write.open_table(ENTRIES).map_err(CoreError::storage)?;
            let mut payloads = write.open_table(PAYLOADS).map_err(CoreError::storage)?;
            let mut cursors = write.open_table(CURSORS).map_err(CoreError::storage)?;
            let mut observations = write.open_table(OBSERVATIONS).map_err(CoreError::storage)?;

            let mut payload_sizes = std::collections::BTreeMap::new();
            for item in payloads.iter().map_err(CoreError::storage)? {
                let (hash, bytes) = item.map_err(CoreError::storage)?;
                payload_sizes.insert(
                    hash.value().to_string(),
                    u64::try_from(bytes.value().len()).unwrap_or(u64::MAX),
                );
            }
            let payload_bytes_before = payload_sizes
                .values()
                .copied()
                .fold(0u64, u64::saturating_add);

            let mut groups: std::collections::BTreeMap<String, Vec<(String, CacheEntryMeta)>> =
                std::collections::BTreeMap::new();
            for item in entries.iter().map_err(CoreError::storage)? {
                let (key, value) = item.map_err(CoreError::storage)?;
                let meta: CacheEntryMeta = serde_json::from_slice(value.value())?;
                groups
                    .entry(meta.payload_hash.clone())
                    .or_default()
                    .push((key.value().to_string(), meta));
            }

            let mut candidates = payload_sizes
                .iter()
                .map(|(hash, size)| {
                    let group = groups.remove(hash).unwrap_or_default();
                    let newest = group
                        .iter()
                        .map(|(_, entry)| entry.created_at.as_str())
                        .max()
                        .unwrap_or("");
                    (newest.to_string(), hash.clone(), *size, group)
                })
                .collect::<Vec<_>>();
            candidates
                .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

            let mut payload_bytes_retained = payload_bytes_before;
            let mut evicted_entries = 0;
            let mut evicted_payloads = 0;
            let mut evicted_cursors = 0;
            let mut metadata_only_observations = 0;
            for (_, hash, size, group) in candidates {
                if payload_bytes_retained <= max_payload_bytes {
                    break;
                }
                let keys = group.into_iter().map(|(key, _)| key).collect::<Vec<_>>();
                let key_set = keys.iter().map(String::as_str).collect();
                evicted_entries += remove_keys(&mut entries, &keys)?;
                if payloads
                    .remove(hash.as_str())
                    .map_err(CoreError::storage)?
                    .is_some()
                {
                    evicted_payloads += 1;
                    payload_bytes_retained = payload_bytes_retained.saturating_sub(size);
                }
                evicted_cursors += retain_cursors(&mut cursors, &key_set, None)?;
                metadata_only_observations +=
                    mark_payloads_metadata_only(&mut observations, std::slice::from_ref(&hash))?;
            }

            summary = StorageQuotaSummary {
                max_payload_bytes,
                payload_bytes_before,
                payload_bytes_retained,
                evicted_entries,
                evicted_payloads,
                evicted_cursors,
                metadata_only_observations,
            };
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(summary)
    }

    pub fn update_profile<F>(&self, id: &str, mut update: F) -> Result<SourceProfile>
    where
        F: FnMut(Option<SourceProfile>) -> SourceProfile,
    {
        validate_profile_id(id)?;
        let lock_path = self.dir.join("profiles").join(format!("{id}.lock"));
        let _lock = ProfileLock::acquire(&lock_path)?;

        let path = self.profile_path(id)?;
        let current = if path.exists() {
            Some(serde_json::from_slice(&fs::read(&path)?)?)
        } else {
            None
        };
        let current_revision = current
            .as_ref()
            .map_or(0, |profile: &SourceProfile| profile.revision);
        let mut next = update(current);
        if next.revision <= current_revision {
            next.revision = current_revision.saturating_add(1);
        }
        validate_source_profile(&next)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&next)?)?;
        set_file_permissions(&tmp)?;
        fs::rename(&tmp, &path)?;
        set_file_permissions(&path)?;
        Ok(next)
    }

    pub fn read_profile(&self, id: &str) -> Result<Option<SourceProfile>> {
        let path = self.profile_path(id)?;
        if !path.exists() {
            return Ok(None);
        }
        let profile = serde_json::from_slice(&fs::read(path)?)?;
        validate_source_profile(&profile)?;
        Ok(Some(profile))
    }

    fn profile_path(&self, id: &str) -> Result<PathBuf> {
        validate_profile_id(id)?;
        Ok(self.dir.join("profiles").join(format!("{id}.json")))
    }

    fn read_entry(&self, key: &str) -> Result<Option<CacheEntryMeta>> {
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(ENTRIES).map_err(CoreError::storage)?;
        let Some(bytes) = table.get(key).map_err(CoreError::storage)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(bytes.value())?))
    }

    fn expired_entries(&self, now: DateTime<Utc>) -> Result<Vec<CacheEntryMeta>> {
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(ENTRIES).map_err(CoreError::storage)?;
        let mut entries = Vec::new();
        for entry in table.iter().map_err(CoreError::storage)? {
            let (key, value) = entry.map_err(CoreError::storage)?;
            let meta: CacheEntryMeta = serde_json::from_slice(value.value())?;
            if parse_time(&meta.expires_at)? <= now {
                debug_assert_eq!(key.value(), meta.key);
                entries.push(meta);
            }
        }
        Ok(entries)
    }

    /// Mark observations stale by retention only while their redacted payload
    /// still exists. A later payload eviction is more restrictive and retains
    /// the existing `metadata_only` lifecycle state instead.
    fn mark_observations_expired<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<usize> {
        let ids = ids.into_iter().collect::<std::collections::BTreeSet<_>>();
        if ids.is_empty() {
            return Ok(0);
        }
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        let updated;
        {
            let payloads = write.open_table(PAYLOADS).map_err(CoreError::storage)?;
            let mut observations = write.open_table(OBSERVATIONS).map_err(CoreError::storage)?;
            let mut updates = Vec::new();
            for id in &ids {
                let Some(value) = observations.get(*id).map_err(CoreError::storage)? else {
                    continue;
                };
                let mut record: ObservationRecord = serde_json::from_slice(value.value())?;
                if payloads
                    .get(record.payload_hash.as_str())
                    .map_err(CoreError::storage)?
                    .is_some()
                    && matches!(
                        record.availability,
                        EvidenceAvailability::Recoverable
                            | EvidenceAvailability::CaptureTruncated
                            | EvidenceAvailability::Redacted
                    )
                {
                    record.availability = EvidenceAvailability::Expired;
                    updates.push(((*id).to_string(), serde_json::to_vec(&record)?));
                }
            }
            updated = updates.len();
            for (id, bytes) in updates {
                observations
                    .insert(id.as_str(), bytes.as_slice())
                    .map_err(CoreError::storage)?;
            }
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(updated)
    }

    fn purge_entries_and_cursors(
        &self,
        keys: &[String],
        source_id: Option<&str>,
    ) -> Result<PurgeSummary> {
        let key_set: std::collections::BTreeSet<&str> = keys.iter().map(String::as_str).collect();
        // Candidate payload hashes come from the entries being purged.
        let mut candidate_hashes = std::collections::BTreeSet::new();
        for key in keys {
            if let Some(entry) = self.read_entry(key)? {
                candidate_hashes.insert(entry.payload_hash);
            }
        }

        let write = self.db.begin_write().map_err(CoreError::storage)?;
        let summary;
        {
            let mut entries = write.open_table(ENTRIES).map_err(CoreError::storage)?;
            let mut payloads = write.open_table(PAYLOADS).map_err(CoreError::storage)?;
            let mut cursors = write.open_table(CURSORS).map_err(CoreError::storage)?;
            let mut observations = write.open_table(OBSERVATIONS).map_err(CoreError::storage)?;

            // Reference-count payload blobs: a candidate hash is only safe to
            // remove when no surviving entry still references it. Without this
            // check, purging one entry orphans the payload for any other entry
            // that shares the same content hash (`put_payload` dedupes by
            // sha256), breaking `expand` for the survivor.
            let mut surviving_hashes = std::collections::BTreeSet::new();
            for entry in entries.iter().map_err(CoreError::storage)? {
                let (key, value) = entry.map_err(CoreError::storage)?;
                if !key_set.contains(key.value()) {
                    let meta: CacheEntryMeta = serde_json::from_slice(value.value())?;
                    surviving_hashes.insert(meta.payload_hash);
                }
            }

            let purged_entries = remove_keys(&mut entries, keys)?;
            let orphaned: Vec<String> = candidate_hashes
                .into_iter()
                .filter(|hash| !surviving_hashes.contains(hash))
                .collect();
            let purged_payloads = remove_keys(&mut payloads, &orphaned)?;
            let purged_cursors = retain_cursors(&mut cursors, &key_set, source_id)?;
            let _ = mark_payloads_metadata_only(&mut observations, &orphaned)?;
            summary = PurgeSummary {
                purged_entries,
                purged_payloads,
                purged_cursors,
                purged_sessions: 0,
                purged_observations: 0,
            };
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(summary)
    }
}

pub fn new_cache_entry(
    key: String,
    payload_hash: String,
    source_id: String,
    operation: String,
    payload_bytes: u64,
    ttl_seconds: i64,
) -> CacheEntryMeta {
    let now = Utc::now();
    CacheEntryMeta {
        key,
        payload_hash,
        source_id,
        operation,
        created_at: format_time(now),
        expires_at: format_time(now + chrono::Duration::seconds(ttl_seconds)),
        payload_bytes,
        cacheable: true,
        sensitive: false,
        observation_id: None,
        provenance: Some(CallProvenance {
            source_call_id: format!("call_{}", Uuid::new_v4().simple()),
            cache_key: None,
            captured_at: format_time(now),
            status: None,
            duration_ms: None,
            extra: serde_json::Map::new(),
        }),
        extra: serde_json::Map::new(),
    }
}

fn retain_none<K: redb::Key + 'static, V: redb::Value + 'static>(
    table: &mut redb::Table<'_, K, V>,
) -> Result<usize> {
    let mut count = 0;
    table
        .retain(|_, _| {
            count += 1;
            false
        })
        .map_err(CoreError::storage)?;
    Ok(count)
}

/// Actionable one-line notice emitted when an unidentified pre-release store is
/// reset. Pure (no I/O): kept separate from the reset site so the exact wording
/// is unit-testable. The caller is responsible for gating this on `store_existed`
/// so first-run creation stays silent.
pub fn store_reset_notice(dir: &Path, dropped: usize) -> String {
    format!(
        "prog: unidentified pre-release store at {} was reset ({} records dropped); rerun your source to repopulate",
        dir.display(),
        dropped
    )
}

fn validate_observation_input(input: &NewObservation) -> Result<()> {
    for (name, value) in [
        ("payload_hash", &input.payload_hash),
        ("invocation_fingerprint", &input.invocation_fingerprint),
        ("source_id", &input.source_id),
        ("operation", &input.operation),
    ] {
        if value.trim().is_empty() {
            return Err(CoreError::BadArgs {
                operation: "observation record".to_string(),
                reason: format!("{name} must not be empty"),
            });
        }
    }
    for subject_key in &input.subject_keys {
        if subject_key.len() > 256 || !subject_key.contains(':') {
            return Err(CoreError::BadArgs {
                operation: "observation record".to_string(),
                reason: "subject keys must be namespaced and at most 256 bytes".to_string(),
            });
        }
        let lower = subject_key.to_ascii_lowercase();
        if ["token", "secret", "password", "authorization", "api_key"]
            .iter()
            .any(|needle| lower.contains(needle))
        {
            return Err(CoreError::BadArgs {
                operation: "observation record".to_string(),
                reason: "subject keys must not contain sensitive identifiers".to_string(),
            });
        }
    }
    if input.subject_keys.len() > 32 {
        return Err(CoreError::BadArgs {
            operation: "observation record".to_string(),
            reason: "at most 32 subject keys are allowed".to_string(),
        });
    }
    Ok(())
}

fn observation_index_key(left: &str, right: &str) -> String {
    format!("{left}\0{right}")
}

fn obligation_key(session_id: &str, id: &str) -> Result<String> {
    if id.len() > 128
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(CoreError::BadArgs {
            operation: "verification obligation".to_string(),
            reason: "id must be at most 128 ASCII letters, numbers, '.', '_', or '-'".to_string(),
        });
    }
    Ok(format!("{session_id}\0{id}"))
}

fn observation_lineage_ids(lineage: &ObservationLineage) -> impl Iterator<Item = &str> {
    [
        lineage.parent_id.as_deref(),
        lineage.supersedes_id.as_deref(),
        lineage.derived_from_id.as_deref(),
        lineage.revalidates_id.as_deref(),
    ]
    .into_iter()
    .flatten()
}

fn mark_payloads_metadata_only(
    table: &mut redb::Table<'_, &str, &[u8]>,
    hashes: &[String],
) -> Result<usize> {
    if hashes.is_empty() {
        return Ok(0);
    }
    let hashes: std::collections::BTreeSet<&str> = hashes.iter().map(String::as_str).collect();
    let mut updates = Vec::new();
    for entry in table.iter().map_err(CoreError::storage)? {
        let (id, value) = entry.map_err(CoreError::storage)?;
        let mut record: ObservationRecord = serde_json::from_slice(value.value())?;
        if hashes.contains(record.payload_hash.as_str())
            && matches!(
                record.availability,
                EvidenceAvailability::Recoverable
                    | EvidenceAvailability::CaptureTruncated
                    | EvidenceAvailability::Redacted
            )
        {
            record.availability = EvidenceAvailability::MetadataOnly;
            updates.push((id.value().to_string(), serde_json::to_vec(&record)?));
        }
    }
    let updated = updates.len();
    for (id, bytes) in updates {
        table
            .insert(id.as_str(), bytes.as_slice())
            .map_err(CoreError::storage)?;
    }
    Ok(updated)
}

fn remove_keys<V: redb::Value + 'static>(
    table: &mut redb::Table<'_, &str, V>,
    keys: &[String],
) -> Result<usize> {
    let mut count = 0;
    for key in keys {
        if table
            .remove(key.as_str())
            .map_err(CoreError::storage)?
            .is_some()
        {
            count += 1;
        }
    }
    Ok(count)
}

fn retain_cursors(
    table: &mut redb::Table<'_, &str, &[u8]>,
    cache_keys: &std::collections::BTreeSet<&str>,
    source_id: Option<&str>,
) -> Result<usize> {
    let mut count = 0;
    table
        .retain(|_, value| {
            let Ok(record) = serde_json::from_slice::<CursorRecord>(value) else {
                count += 1;
                return false;
            };
            let remove = cache_keys.contains(record.cache_key.as_str())
                || source_id.is_some_and(|source_id| source_id == record.source_id);
            if remove {
                count += 1;
            }
            !remove
        })
        .map_err(CoreError::storage)?;
    Ok(count)
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

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)
        .map_err(CoreError::storage)?
        .with_timezone(&Utc))
}

fn format_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn validate_profile_id(id: &str) -> Result<()> {
    let valid_chars = id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    if id.is_empty() || id == "." || id == ".." || !valid_chars {
        return Err(CoreError::BadArgs {
            operation: "profile".to_string(),
            reason: format!(
                "source id '{id}' must contain only ASCII letters, digits, '.', '_', or '-'"
            ),
        });
    }
    Ok(())
}

fn set_dir_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

struct ProfileLock {
    path: PathBuf,
}

impl ProfileLock {
    fn acquire(path: &Path) -> Result<Self> {
        for _ in 0..100 {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(_) => {
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(CoreError::Storage(format!(
            "timed out waiting for profile lock {}",
            path.display()
        )))
    }
}

impl Drop for ProfileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

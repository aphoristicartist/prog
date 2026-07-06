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
    CacheEntryMeta, CallProvenance, CoreError, CursorRecord, PersistedPayload, RedactedPayload,
    Result, SourceProfile, ValidatedCursor, canonical_json,
};

const PAYLOADS: TableDefinition<&str, &[u8]> = TableDefinition::new("payloads");
const ENTRIES: TableDefinition<&str, &[u8]> = TableDefinition::new("entries");
const CURSORS: TableDefinition<&str, &[u8]> = TableDefinition::new("cursors");

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PurgeSummary {
    pub purged_entries: usize,
    pub purged_payloads: usize,
    pub purged_cursors: usize,
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
        let db = Database::create(&db_path).map_err(CoreError::storage)?;
        set_file_permissions(&db_path)?;

        let write = db.begin_write().map_err(CoreError::storage)?;
        {
            write.open_table(PAYLOADS).map_err(CoreError::storage)?;
            write.open_table(ENTRIES).map_err(CoreError::storage)?;
            write.open_table(CURSORS).map_err(CoreError::storage)?;
        }
        write.commit().map_err(CoreError::storage)?;

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
        let token = format!("pc1_{}", Uuid::new_v4().simple());
        let now = Utc::now();
        let record = CursorRecord {
            cache_key: cache_key.to_string(),
            source_id: source_id.to_string(),
            operation: operation.to_string(),
            root_path: root_path.to_string(),
            redaction_version,
            created_at: format_time(now),
            expires_at: format_time(now + chrono::Duration::seconds(ttl_seconds)),
            extra: serde_json::Map::new(),
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
            return Err(CoreError::CursorNotFound(format!(
                "{token} in {}",
                self.dir.display()
            )));
        };
        let record: CursorRecord = serde_json::from_slice(bytes.value())?;
        if parse_time(&record.expires_at)? <= now {
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

    pub fn purge_all(&self) -> Result<PurgeSummary> {
        let write = self.db.begin_write().map_err(CoreError::storage)?;
        let summary;
        {
            let mut entries = write.open_table(ENTRIES).map_err(CoreError::storage)?;
            let mut payloads = write.open_table(PAYLOADS).map_err(CoreError::storage)?;
            let mut cursors = write.open_table(CURSORS).map_err(CoreError::storage)?;
            summary = PurgeSummary {
                purged_entries: retain_none(&mut entries)?,
                purged_payloads: retain_none(&mut payloads)?,
                purged_cursors: retain_none(&mut cursors)?,
            };
        }
        write.commit().map_err(CoreError::storage)?;
        Ok(summary)
    }

    pub fn purge_expired(&self, now: DateTime<Utc>) -> Result<PurgeSummary> {
        let expired = self.expired_entry_keys(now)?;
        self.purge_entries_and_cursors(&expired, None)
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

    pub fn update_profile<F>(&self, id: &str, mut update: F) -> Result<SourceProfile>
    where
        F: FnMut(Option<SourceProfile>) -> SourceProfile,
    {
        let lock_path = self.dir.join("profiles").join(format!("{id}.lock"));
        let _lock = ProfileLock::acquire(&lock_path)?;

        let path = self.profile_path(id);
        let current = if path.exists() {
            Some(serde_json::from_slice(&fs::read(&path)?)?)
        } else {
            None
        };
        let current_version = current
            .as_ref()
            .map_or(0, |profile: &SourceProfile| profile.version);
        let mut next = update(current);
        if next.version <= current_version {
            next.version = current_version.saturating_add(1);
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&next)?)?;
        set_file_permissions(&tmp)?;
        fs::rename(&tmp, &path)?;
        set_file_permissions(&path)?;
        Ok(next)
    }

    pub fn read_profile(&self, id: &str) -> Result<Option<SourceProfile>> {
        let path = self.profile_path(id);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
    }

    fn profile_path(&self, id: &str) -> PathBuf {
        self.dir.join("profiles").join(format!("{id}.json"))
    }

    fn read_entry(&self, key: &str) -> Result<Option<CacheEntryMeta>> {
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(ENTRIES).map_err(CoreError::storage)?;
        let Some(bytes) = table.get(key).map_err(CoreError::storage)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(bytes.value())?))
    }

    fn expired_entry_keys(&self, now: DateTime<Utc>) -> Result<Vec<String>> {
        let read = self.db.begin_read().map_err(CoreError::storage)?;
        let table = read.open_table(ENTRIES).map_err(CoreError::storage)?;
        let mut keys = Vec::new();
        for entry in table.iter().map_err(CoreError::storage)? {
            let (key, value) = entry.map_err(CoreError::storage)?;
            let meta: CacheEntryMeta = serde_json::from_slice(value.value())?;
            if parse_time(&meta.expires_at)? <= now {
                keys.push(key.value().to_string());
            }
        }
        Ok(keys)
    }

    fn purge_entries_and_cursors(
        &self,
        keys: &[String],
        source_id: Option<&str>,
    ) -> Result<PurgeSummary> {
        let key_set: std::collections::BTreeSet<&str> = keys.iter().map(String::as_str).collect();
        let mut payload_hashes = std::collections::BTreeSet::new();
        for key in keys {
            if let Some(entry) = self.read_entry(key)? {
                payload_hashes.insert(entry.payload_hash);
            }
        }

        let write = self.db.begin_write().map_err(CoreError::storage)?;
        let summary;
        {
            let mut entries = write.open_table(ENTRIES).map_err(CoreError::storage)?;
            let mut payloads = write.open_table(PAYLOADS).map_err(CoreError::storage)?;
            let mut cursors = write.open_table(CURSORS).map_err(CoreError::storage)?;
            let purged_entries = remove_keys(&mut entries, keys)?;
            let purged_payloads = remove_keys(
                &mut payloads,
                &payload_hashes.into_iter().collect::<Vec<_>>(),
            )?;
            let purged_cursors = retain_cursors(&mut cursors, &key_set, source_id)?;
            summary = PurgeSummary {
                purged_entries,
                purged_payloads,
                purged_cursors,
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

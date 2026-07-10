use std::{sync::Arc, thread};

use chrono::{Duration, SecondsFormat, Utc};
use prog_core::{
    CacheEntryMeta, CachePolicy, CursorRecord, EffectSet, ExpansionScope, NewSessionEvent,
    OperationProfile, PreviewPolicy, RawPayload, RedactedPayload, RedactionPolicy, ScopedSlice,
    SliceRequest, SourceKind, SourceProfile, Store, TrustSettings, expand, new_cache_entry,
};

#[test]
fn session_trail_is_persistent_bounded_and_purged_with_cache() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let started = store
        .start_session(Some("debug failing tests".to_string()))
        .unwrap();
    let event = store
        .record_session_event(NewSessionEvent {
            kind: "inspect".to_string(),
            cursor: Some("pc1_demo".to_string()),
            path: Some("/failure_sections/0".to_string()),
            summary: Some(format!(
                "Bearer abcdefghijklmnopqrstuvwxyz {}",
                "x".repeat(400)
            )),
            extra: serde_json::Map::from_iter([(
                "api_token".to_string(),
                json!("plain-session-secret"),
            )]),
            ..NewSessionEvent::default()
        })
        .unwrap();
    assert_eq!(event.sequence, 1);
    assert!(event.summary.unwrap().len() <= 240);
    drop(store);

    let reopened = Store::open(dir.path()).unwrap();
    let trail = reopened
        .get_session(Some(&started.session_id))
        .unwrap()
        .unwrap();
    assert_eq!(trail.goal.as_deref(), Some("debug failing tests"));
    assert_eq!(trail.events.len(), 1);
    assert!(
        !serde_json::to_string(&trail)
            .unwrap()
            .contains("plain-session-secret")
    );
    let purged = reopened.purge_all().unwrap();
    assert_eq!(purged.purged_sessions, 1);
    assert!(reopened.get_session(None).unwrap().is_none());
}
use proptest::prelude::*;
use serde_json::{Value, json};

#[test]
fn payloads_survive_across_store_process_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let payload = json!({"items": [{"id": 1, "body": "large"}]});
    let hash = {
        let store = Store::open(dir.path()).unwrap();
        store.put_payload(&redacted(payload.clone())).unwrap()
    };

    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(
        reopened.get_payload(&hash).unwrap().unwrap().as_value(),
        &payload
    );
}

#[test]
fn entries_respect_ttl_and_non_cacheable_sensitive_results_are_not_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let payload_hash = store.put_payload(&redacted(json!({"ok": true}))).unwrap();

    let mut entry = entry("fresh", &payload_hash, 60);
    store.put_entry(&entry.key.clone(), &entry).unwrap();
    assert!(store.get_entry(&entry.key).unwrap().is_some());

    entry.key = "expired".to_string();
    entry.expires_at = format_time(Utc::now() - Duration::seconds(1));
    store.put_entry(&entry.key.clone(), &entry).unwrap();
    assert!(store.get_entry(&entry.key).unwrap().is_none());

    entry.key = "sensitive".to_string();
    entry.sensitive = true;
    store.put_entry(&entry.key.clone(), &entry).unwrap();
    assert!(store.read_profile("missing").unwrap().is_none());
    assert!(store.get_entry(&entry.key).unwrap().is_none());

    entry.key = "not-cacheable".to_string();
    entry.sensitive = false;
    entry.cacheable = false;
    store.put_entry(&entry.key.clone(), &entry).unwrap();
    assert!(store.get_entry(&entry.key).unwrap().is_none());
}

#[test]
fn cursors_fail_closed_for_missing_expired_and_redaction_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let now = Utc::now();

    let missing = store.get_cursor_at("pc1_missing", 1, now).unwrap_err();
    assert_eq!(missing.kind(), "cursor_not_found");

    let expired = CursorRecord {
        cache_key: "cache".to_string(),
        source_id: "source".to_string(),
        operation: "op".to_string(),
        root_path: "".to_string(),
        redaction_version: 1,
        created_at: format_time(now - Duration::seconds(10)),
        expires_at: format_time(now - Duration::seconds(1)),
        extra: serde_json::Map::new(),
    };
    store.put_cursor("pc1_expired", &expired).unwrap();
    assert_eq!(
        store
            .get_cursor_at("pc1_expired", 1, now)
            .unwrap_err()
            .kind(),
        "cursor_expired"
    );

    let mut mismatched = expired;
    mismatched.expires_at = format_time(now + Duration::seconds(60));
    store.put_cursor("pc1_mismatch", &mismatched).unwrap();
    assert_eq!(
        store
            .get_cursor_at("pc1_mismatch", 2, now)
            .unwrap_err()
            .kind(),
        "redaction_version_mismatch"
    );
}

#[test]
fn purge_expired_cascades_to_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let payload_hash = store.put_payload(&redacted(json!({"ok": true}))).unwrap();
    let mut entry = entry("expired", &payload_hash, 60);
    entry.expires_at = format_time(Utc::now() - Duration::seconds(1));
    store.put_entry(&entry.key.clone(), &entry).unwrap();

    let cursor = CursorRecord {
        cache_key: entry.key.clone(),
        source_id: entry.source_id.clone(),
        operation: entry.operation.clone(),
        root_path: "".to_string(),
        redaction_version: 1,
        created_at: format_time(Utc::now()),
        expires_at: format_time(Utc::now() + Duration::seconds(60)),
        extra: serde_json::Map::new(),
    };
    store.put_cursor("pc1_cursor", &cursor).unwrap();

    let summary = store.purge_expired(Utc::now()).unwrap();
    assert_eq!(summary.purged_entries, 1);
    assert_eq!(summary.purged_cursors, 1);
    assert!(store.get_entry(&entry.key).unwrap().is_none());
    assert_eq!(
        store.get_cursor("pc1_cursor", 1).unwrap_err().kind(),
        "cursor_not_found"
    );
}

#[test]
fn purge_keeps_payload_blob_shared_with_a_surviving_entry() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    // `put_payload` dedupes by sha256, so two cache entries can share one blob.
    let hash = store.put_payload(&redacted(json!({"items": []}))).unwrap();

    let entry_a = new_cache_entry(
        "key-a".to_string(),
        hash.clone(),
        "a".to_string(),
        "op".to_string(),
        8,
        60,
    );
    let entry_b = new_cache_entry(
        "key-b".to_string(),
        hash.clone(),
        "b".to_string(),
        "op".to_string(),
        8,
        60,
    );
    store.put_entry("key-a", &entry_a).unwrap();
    store.put_entry("key-b", &entry_b).unwrap();

    // Purging source "a" must NOT orphan the blob that "b" still references.
    let summary = store.purge_source("a").unwrap();
    assert_eq!(summary.purged_entries, 1);
    assert_eq!(summary.purged_payloads, 0);
    assert!(store.get_payload(&hash).unwrap().is_some());
    assert!(store.get_entry("key-b").unwrap().is_some());
    assert!(store.get_entry("key-a").unwrap().is_none());

    // Purging the last surviving reference reclaims the blob.
    let summary = store.purge_source("b").unwrap();
    assert_eq!(summary.purged_entries, 1);
    assert_eq!(summary.purged_payloads, 1);
    assert!(store.get_payload(&hash).unwrap().is_none());
}

#[test]
fn profile_updates_converge_under_locking() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());

    let left_store = Arc::clone(&store);
    let left = thread::spawn(move || {
        left_store
            .update_profile("local", |current| add_operation(current, "left"))
            .unwrap();
    });
    let right_store = Arc::clone(&store);
    let right = thread::spawn(move || {
        right_store
            .update_profile("local", |current| add_operation(current, "right"))
            .unwrap();
    });
    left.join().unwrap();
    right.join().unwrap();

    let profile = store.read_profile("local").unwrap().unwrap();
    let mut operations: Vec<&str> = profile
        .operations
        .iter()
        .map(|operation| operation.id.as_str())
        .collect();
    operations.sort();
    assert_eq!(operations, vec!["left", "right"]);
    assert_eq!(profile.version, 2);
}

#[test]
fn profile_ids_cannot_escape_profiles_directory() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    for id in [
        "",
        ".",
        "..",
        "../outside",
        "nested/source",
        "nested\\source",
    ] {
        let update_error = store
            .update_profile(id, |current| add_operation(current, "op"))
            .unwrap_err();
        assert_eq!(update_error.kind(), "bad_args");

        let read_error = store.read_profile(id).unwrap_err();
        assert_eq!(read_error.kind(), "bad_args");
    }

    store
        .update_profile("github.issues-prod_1", |current| {
            add_operation(current, "op")
        })
        .unwrap();
    assert!(
        dir.path()
            .join("profiles")
            .join("github.issues-prod_1.json")
            .exists()
    );
    assert!(!dir.path().join("outside.json").exists());
}

#[cfg(unix)]
#[test]
fn store_uses_private_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join(".prog");
    Store::open(&store_dir).unwrap();

    assert_eq!(
        std::fs::metadata(&store_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(store_dir.join("cache/data.redb"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

proptest! {
    #[test]
    fn persistence_redaction_is_idempotent_and_removes_secret_values(secret in "[A-Z0-9]{8,32}") {
        let raw_secret = format!("SECRET-{secret}");
        let payload = json!({
            "token": raw_secret,
            "nested": [
                {"password": raw_secret, "safe": "visible"},
                {"deep": {"api_key": raw_secret}}
            ]
        });
        let policy = RedactionPolicy::default();

        let (once, paths) = policy.apply_persistence(&payload);
        let (twice, _) = policy.apply_persistence(&once);

        prop_assert_eq!(once.clone(), twice);
        prop_assert_eq!(paths.len(), 3);
        prop_assert!(!serde_json::to_string(&once).unwrap().contains(&raw_secret));
        prop_assert_eq!(once["nested"][0]["safe"].clone(), json!("visible"));
    }

    #[test]
    fn redacted_payload_stays_redacted_through_store_and_expansion(secret in "[A-Z0-9]{8,32}") {
        let raw_secret = format!("SECRET-{secret}");
        let payload = json!({
            "items": [
                {
                    "id": 1,
                    "token": raw_secret,
                    "nested": {"password": raw_secret},
                    "safe": "visible"
                }
            ]
        });
        let redacted = RawPayload::new(payload).redact(&RedactionPolicy::default()).payload;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let hash = store.put_payload(&redacted).unwrap();
        let stored = store.get_payload(&hash).unwrap().unwrap();
        let scoped = ScopedSlice::new(
            ExpansionScope::new("/items").unwrap(),
            SliceRequest {
                path: Some("/items/0".to_string()),
                limit: None,
                depth: None,
                fields: Vec::new(),
                omit: Vec::new(),
                extra: serde_json::Map::new(),
            },
        )
        .unwrap();
        let projection = expand(
            &stored,
            &scoped,
            &PreviewPolicy::default(),
        )
        .unwrap();

        prop_assert!(!serde_json::to_string(stored.as_value()).unwrap().contains(&raw_secret));
        prop_assert!(!serde_json::to_string(&projection).unwrap().contains(&raw_secret));
        prop_assert_eq!(projection.preview["safe"].clone(), json!("visible"));
    }
}

fn redacted(value: Value) -> RedactedPayload {
    RawPayload::new(value)
        .redact(&RedactionPolicy::default())
        .payload
}

fn entry(key: &str, payload_hash: &str, ttl_seconds: i64) -> CacheEntryMeta {
    let mut entry = new_cache_entry(
        key.to_string(),
        payload_hash.to_string(),
        "source".to_string(),
        "op".to_string(),
        12,
        ttl_seconds,
    );
    entry.key = key.to_string();
    entry
}

fn format_time(value: chrono::DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn add_operation(current: Option<SourceProfile>, id: &str) -> SourceProfile {
    let mut profile = current.unwrap_or_else(|| SourceProfile {
        schema_version: "prog.source_profile.v1".to_string(),
        id: "local".to_string(),
        kind: SourceKind::Cli,
        version: 0,
        description: None,
        operations: Vec::new(),
        auth: Vec::new(),
        cache: CachePolicy::default(),
        trust: TrustSettings::default(),
        effect_defaults: EffectSet::default(),
        redaction: prog_core::RedactionConfig::default(),
        extra: serde_json::Map::new(),
    });
    if !profile
        .operations
        .iter()
        .any(|operation| operation.id == id)
    {
        profile.operations.push(OperationProfile {
            id: id.to_string(),
            description: None,
            input_schema: Value::Null,
            output_shape: None,
            declared_output_schema: None,
            effects: EffectSet::default(),
            cache: CachePolicy::default(),
            pagination: None,
            extra: serde_json::Map::new(),
        });
    }
    profile
}

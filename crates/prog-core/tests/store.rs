use std::{fs, sync::Arc, thread};

use chrono::{Duration, SecondsFormat, Utc};
use prog_core::{
    BudgetSource, CacheEntryMeta, CachePolicy, CaptureCompleteness, CursorRecord, EffectSet,
    EvidenceAvailability, ExpansionScope, NewObservation, NewSessionEvent, ObservationLineage,
    OperationProfile, PreviewPolicy, RawPayload, RedactedPayload, RedactionPolicy, ScopedSlice,
    SliceRequest, SourceKind, SourceProfile, StorageBudget, Store, TrustSettings, expand,
    new_cache_entry, store_reset_notice,
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
fn existing_or_pre_capture_lifecycle_store_is_reset() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    fs::create_dir_all(&cache).unwrap();
    let db = redb::Database::create(cache.join("data.redb")).unwrap();
    let write = db.begin_write().unwrap();
    {
        const PAYLOADS: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("payloads");
        const STATE: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("state");
        let mut payloads = write.open_table(PAYLOADS).unwrap();
        let mut state = write.open_table(STATE).unwrap();
        let legacy_payload = br#"{"legacy":true}"#.to_vec();
        payloads
            .insert("sha256:legacy", legacy_payload.as_slice())
            .unwrap();
        state
            .insert("store_schema", b"prog.store.capture_lifecycle".as_slice())
            .unwrap();
    }
    write.commit().unwrap();
    drop(db);

    let store = Store::open(dir.path()).unwrap();
    assert!(store.get_payload("sha256:legacy").unwrap().is_none());

    // The reset emits an actionable notice naming the store dir and dropped
    // record count (pure helper, not stderr): "reset" + "rerun" + the count.
    let notice = store_reset_notice(dir.path(), 2);
    assert!(notice.contains("reset"), "notice: {notice}");
    assert!(notice.contains("rerun"), "notice: {notice}");
    assert!(notice.contains("2 records dropped"), "notice: {notice}");
    assert!(
        notice.contains(dir.path().to_str().unwrap()),
        "notice should name the store dir: {notice}"
    );
    // The dropped count flows through verbatim.
    assert!(!store_reset_notice(dir.path(), 0).contains("2 records dropped"));
}

#[test]
fn session_predecessor_requires_matching_comparison_family() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.start_session(None).unwrap();
    let payload_hash = store.put_payload(&redacted(json!({"ok": true}))).unwrap();
    let family_a = store
        .record_observation(NewObservation {
            payload_hash: payload_hash.clone(),
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: "sha256:same".to_string(),
            source_id: "source".to_string(),
            operation: "read".to_string(),
            comparison_family: Some("family-a".to_string()),
            capture: CaptureCompleteness::complete(11),
            ..NewObservation::default()
        })
        .unwrap();
    store
        .record_session_event(NewSessionEvent {
            kind: "call".to_string(),
            extra: serde_json::Map::from_iter([(
                "observation_id".to_string(),
                json!(family_a.observation_id),
            )]),
            ..NewSessionEvent::default()
        })
        .unwrap();
    let family_b = store
        .record_observation(NewObservation {
            payload_hash,
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: "sha256:same".to_string(),
            source_id: "source".to_string(),
            operation: "read".to_string(),
            comparison_family: Some("family-b".to_string()),
            capture: CaptureCompleteness::complete(11),
            ..NewObservation::default()
        })
        .unwrap();
    store
        .record_session_event(NewSessionEvent {
            kind: "call".to_string(),
            extra: serde_json::Map::from_iter([(
                "observation_id".to_string(),
                json!(family_b.observation_id),
            )]),
            ..NewSessionEvent::default()
        })
        .unwrap();

    assert!(
        store
            .latest_session_predecessor("sha256:same", Some("family-a"), &family_b.observation_id)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .latest_session_predecessor("sha256:same", Some("family-c"), &family_b.observation_id)
            .unwrap()
            .is_none()
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
        observation_id: None,
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
        observation_id: None,
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
fn expiry_marks_only_still_recoverable_observations_expired() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let now = Utc::now();
    let payload_hash = store.put_payload(&redacted(json!({"ok": true}))).unwrap();

    let expired_observation = store
        .record_observation(NewObservation {
            payload_hash: payload_hash.clone(),
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: "sha256:expired".to_string(),
            source_id: "source".to_string(),
            operation: "read".to_string(),
            capture: CaptureCompleteness::complete(11),
            ..NewObservation::default()
        })
        .unwrap();
    let fresh_observation = store
        .record_observation(NewObservation {
            payload_hash: payload_hash.clone(),
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: "sha256:fresh".to_string(),
            source_id: "source".to_string(),
            operation: "read".to_string(),
            capture: CaptureCompleteness::complete(11),
            ..NewObservation::default()
        })
        .unwrap();

    let mut expired_entry = entry("expired-observation", &payload_hash, 60);
    expired_entry.expires_at = format_time(now - Duration::seconds(1));
    expired_entry.observation_id = Some(expired_observation.observation_id.clone());
    store.put_entry(&expired_entry.key, &expired_entry).unwrap();
    let mut fresh_entry = entry("fresh-observation", &payload_hash, 60);
    fresh_entry.observation_id = Some(fresh_observation.observation_id.clone());
    store.put_entry(&fresh_entry.key, &fresh_entry).unwrap();

    assert!(
        store
            .get_entry_at(&expired_entry.key, now)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .get_observation(&expired_observation.observation_id)
            .unwrap()
            .unwrap()
            .availability,
        EvidenceAvailability::Expired
    );
    assert_eq!(
        store
            .get_observation(&fresh_observation.observation_id)
            .unwrap()
            .unwrap()
            .availability,
        EvidenceAvailability::Recoverable
    );

    // The same state transition occurs when only a cursor discovers expiry.
    let cursor_observation = store
        .record_observation(NewObservation {
            payload_hash: payload_hash.clone(),
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: "sha256:cursor-expired".to_string(),
            source_id: "source".to_string(),
            operation: "read".to_string(),
            capture: CaptureCompleteness::complete(11),
            ..NewObservation::default()
        })
        .unwrap();
    store
        .put_cursor(
            "pc1_expired_observation",
            &CursorRecord {
                cache_key: fresh_entry.key.clone(),
                source_id: "source".to_string(),
                operation: "read".to_string(),
                root_path: "".to_string(),
                redaction_version: 1,
                created_at: format_time(now - Duration::seconds(2)),
                expires_at: format_time(now - Duration::seconds(1)),
                observation_id: Some(cursor_observation.observation_id.clone()),
                extra: serde_json::Map::new(),
            },
        )
        .unwrap();
    assert_eq!(
        store
            .get_cursor_at("pc1_expired_observation", 1, now)
            .unwrap_err()
            .kind(),
        "cursor_expired"
    );
    assert_eq!(
        store
            .get_observation(&cursor_observation.observation_id)
            .unwrap()
            .unwrap()
            .availability,
        EvidenceAvailability::Expired
    );

    // Purging the stale entry keeps the shared payload and its lifecycle fact.
    let summary = store.purge_expired(now).unwrap();
    assert_eq!(summary.purged_entries, 1);
    assert_eq!(summary.purged_payloads, 0);
    assert!(store.get_payload(&payload_hash).unwrap().is_some());
    assert_eq!(
        store
            .get_observation(&expired_observation.observation_id)
            .unwrap()
            .unwrap()
            .availability,
        EvidenceAvailability::Expired
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
fn payload_quota_evicts_whole_shared_groups_and_preserves_metadata_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let old_payload = redacted(json!({"items": vec!["old"; 80]}));
    let new_payload = redacted(json!({"items": vec!["new"; 40]}));
    let old_hash = store.put_payload(&old_payload).unwrap();
    let new_hash = store.put_payload(&new_payload).unwrap();
    let new_bytes = serde_json::to_vec(new_payload.as_value()).unwrap().len() as u64;

    let old_observation = store
        .record_observation(NewObservation {
            payload_hash: old_hash.clone(),
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: "sha256:old".to_string(),
            source_id: "source".to_string(),
            operation: "read".to_string(),
            capture: CaptureCompleteness::complete(1),
            ..NewObservation::default()
        })
        .unwrap();
    let mut old_a = entry("old-a", &old_hash, 60);
    let mut old_b = entry("old-b", &old_hash, 60);
    old_a.created_at = "2020-01-01T00:00:00Z".to_string();
    old_b.created_at = "2020-01-01T00:00:00Z".to_string();
    old_a.observation_id = Some(old_observation.observation_id.clone());
    old_b.observation_id = Some(old_observation.observation_id.clone());
    store.put_entry(&old_a.key, &old_a).unwrap();
    store.put_entry(&old_b.key, &old_b).unwrap();
    for (token, cache_key) in [("pc1_old_a", "old-a"), ("pc1_old_b", "old-b")] {
        store
            .put_cursor(
                token,
                &CursorRecord {
                    cache_key: cache_key.to_string(),
                    source_id: "source".to_string(),
                    operation: "read".to_string(),
                    root_path: "".to_string(),
                    redaction_version: 1,
                    created_at: "2020-01-01T00:00:00Z".to_string(),
                    expires_at: "2030-01-01T00:00:00Z".to_string(),
                    observation_id: Some(old_observation.observation_id.clone()),
                    extra: serde_json::Map::new(),
                },
            )
            .unwrap();
    }

    let mut newest = entry("new", &new_hash, 60);
    newest.created_at = "2025-01-01T00:00:00Z".to_string();
    store.put_entry(&newest.key, &newest).unwrap();

    let summary = store.enforce_payload_quota(new_bytes).unwrap();
    assert!(summary.payload_bytes_before > new_bytes);
    assert_eq!(summary.payload_bytes_retained, new_bytes);
    assert_eq!(summary.evicted_entries, 2);
    assert_eq!(summary.evicted_payloads, 1);
    assert_eq!(summary.evicted_cursors, 2);
    assert_eq!(summary.metadata_only_observations, 1);
    assert!(store.get_payload(&old_hash).unwrap().is_none());
    assert!(store.get_entry("old-a").unwrap().is_none());
    assert!(store.get_entry("old-b").unwrap().is_none());
    assert!(store.get_payload(&new_hash).unwrap().is_some());
    assert!(store.get_entry("new").unwrap().is_some());
    assert_eq!(
        store
            .get_observation(&old_observation.observation_id)
            .unwrap()
            .unwrap()
            .availability,
        EvidenceAvailability::MetadataOnly
    );
    assert_eq!(
        store.get_cursor("pc1_old_a", 1).unwrap_err().kind(),
        "cursor_not_found"
    );

    let repeated = store.enforce_payload_quota(new_bytes).unwrap();
    assert_eq!(repeated.evicted_entries, 0);
    assert_eq!(repeated.evicted_payloads, 0);
    assert_eq!(repeated.payload_bytes_retained, new_bytes);
}

#[test]
fn storage_budget_persists_and_enforces_independent_age_and_byte_limits() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let old_payload = redacted(json!({"body": "old".repeat(128)}));
    let retained_payload = redacted(json!({"body": "new".repeat(32)}));
    let old_hash = store.put_payload(&old_payload).unwrap();
    let retained_hash = store.put_payload(&retained_payload).unwrap();
    let retained_bytes = serde_json::to_vec(retained_payload.as_value())
        .unwrap()
        .len() as u64;

    let old_observation = store
        .record_observation(NewObservation {
            payload_hash: old_hash.clone(),
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: "sha256:old-retention".to_string(),
            source_id: "source".to_string(),
            operation: "read".to_string(),
            capture: CaptureCompleteness::complete(1),
            ..NewObservation::default()
        })
        .unwrap();
    let mut old_entry = entry("old-retention", &old_hash, 86_400);
    old_entry.created_at = "2020-01-01T00:00:00Z".to_string();
    old_entry.observation_id = Some(old_observation.observation_id.clone());
    store.put_entry(&old_entry.key, &old_entry).unwrap();
    let mut retained_entry = entry("retained", &retained_hash, 86_400);
    retained_entry.created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    store
        .put_entry(&retained_entry.key, &retained_entry)
        .unwrap();

    let summary = store
        .set_storage_budget(&StorageBudget {
            source: BudgetSource::StorePolicy,
            max_payload_bytes: Some(retained_bytes),
            max_age_seconds: Some(60),
            extra: serde_json::Map::new(),
        })
        .unwrap();
    assert_eq!(summary.budget.source, BudgetSource::StorePolicy);
    assert_eq!(summary.budget.max_payload_bytes, Some(retained_bytes));
    assert_eq!(summary.budget.max_age_seconds, Some(60));
    assert_eq!(summary.age_purge.unwrap().purged_entries, 1);
    assert_eq!(
        summary.quota.unwrap().payload_bytes_retained,
        retained_bytes
    );
    assert!(store.get_entry("old-retention").unwrap().is_none());
    assert!(store.get_payload(&old_hash).unwrap().is_none());
    assert!(store.get_entry("retained").unwrap().is_some());
    assert!(store.get_payload(&retained_hash).unwrap().is_some());
    assert_eq!(
        store
            .get_observation(&old_observation.observation_id)
            .unwrap()
            .unwrap()
            .availability,
        EvidenceAvailability::MetadataOnly
    );

    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(
        reopened.storage_budget().unwrap(),
        StorageBudget {
            source: BudgetSource::StorePolicy,
            max_payload_bytes: Some(retained_bytes),
            max_age_seconds: Some(60),
            extra: serde_json::Map::new(),
        }
    );
    let repeated = reopened.enforce_storage_budget(Utc::now()).unwrap();
    assert_eq!(repeated.age_purge.unwrap().purged_entries, 0);
    assert_eq!(repeated.quota.unwrap().evicted_payloads, 0);

    reopened.purge_all().unwrap();
    assert_eq!(
        reopened.storage_budget().unwrap(),
        StorageBudget {
            source: BudgetSource::StorePolicy,
            max_payload_bytes: Some(retained_bytes),
            max_age_seconds: Some(60),
            extra: serde_json::Map::new(),
        }
    );
}

#[test]
fn observations_are_immutable_redacted_and_metadata_survive_payload_purge() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let payload = redacted(json!({"ok": true}));
    let payload_hash = store.put_payload(&payload).unwrap();
    let captured_at = "2026-07-13T12:00:00Z".to_string();
    let first = store
        .record_observation(NewObservation {
            payload_hash: payload_hash.clone(),
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: "sha256:invocation".to_string(),
            source_id: "source".to_string(),
            operation: "read".to_string(),
            subject_keys: vec!["account:sha256:abc".to_string()],
            captured_at: Some(captured_at.clone()),
            capture: CaptureCompleteness::complete(0),
            lineage: ObservationLineage::default(),
            extra: serde_json::Map::from_iter([(
                "api_token".to_string(),
                json!("plain-observation-secret"),
            )]),
            ..NewObservation::default()
        })
        .unwrap();
    let second = store
        .record_observation(NewObservation {
            payload_hash: payload_hash.clone(),
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: "sha256:invocation".to_string(),
            source_id: "source".to_string(),
            operation: "read".to_string(),
            captured_at: Some(captured_at),
            capture: CaptureCompleteness::complete(0),
            lineage: ObservationLineage {
                parent_id: Some(first.observation_id.clone()),
                ..ObservationLineage::default()
            },
            ..NewObservation::default()
        })
        .unwrap();

    assert_ne!(first.observation_id, second.observation_id);
    assert_eq!(first.invocation_fingerprint, second.invocation_fingerprint);
    assert_eq!(
        second.lineage.parent_id.as_deref(),
        Some(first.observation_id.as_str())
    );
    assert!(
        !serde_json::to_string(&first)
            .unwrap()
            .contains("plain-observation-secret")
    );
    assert_eq!(first.extra["api_token"], "[REDACTED:secret_field]");

    let mut entry = entry("observation-cache", &payload_hash, 60);
    entry.observation_id = Some(first.observation_id.clone());
    store.put_entry(&entry.key.clone(), &entry).unwrap();
    let summary = store.purge_source("source").unwrap();
    assert_eq!(summary.purged_payloads, 1);
    let retained = store
        .get_observation(&first.observation_id)
        .unwrap()
        .unwrap();
    assert_eq!(retained.availability, EvidenceAvailability::MetadataOnly);
    assert_eq!(
        store
            .get_observation(&second.observation_id)
            .unwrap()
            .unwrap()
            .availability,
        EvidenceAvailability::MetadataOnly
    );
}

#[test]
fn observations_have_bounded_stable_listing_and_reject_sensitive_subject_keys() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for (index, captured_at) in ["2026-07-12T12:00:00Z", "2026-07-13T12:00:00Z"]
        .into_iter()
        .enumerate()
    {
        store
            .record_observation(NewObservation {
                payload_hash: format!("sha256:{index}"),
                availability: EvidenceAvailability::MetadataOnly,
                invocation_fingerprint: format!("sha256:invoke-{index}"),
                source_id: "source".to_string(),
                operation: "read".to_string(),
                captured_at: Some(captured_at.to_string()),
                capture: CaptureCompleteness::complete(0),
                ..NewObservation::default()
            })
            .unwrap();
    }
    let listed = store.list_observations(1).unwrap();
    assert_eq!(listed.observations.len(), 1);
    assert_eq!(listed.observations[0].captured_at, "2026-07-13T12:00:00Z");

    let error = store
        .record_observation(NewObservation {
            payload_hash: "sha256:payload".to_string(),
            invocation_fingerprint: "sha256:invoke".to_string(),
            source_id: "source".to_string(),
            operation: "read".to_string(),
            subject_keys: vec!["account:api_token=plain-secret".to_string()],
            ..NewObservation::default()
        })
        .unwrap_err();
    assert_eq!(error.kind(), "bad_args");
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
    assert_eq!(profile.revision, 2);
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
        schema: "prog.source_profile".to_string(),
        id: "local".to_string(),
        kind: SourceKind::Cli,
        revision: 0,
        description: None,
        operations: Vec::new(),
        auth: Vec::new(),
        cache: CachePolicy::default(),
        trust: TrustSettings::default(),
        effect_defaults: EffectSet::default(),
        redaction: prog_core::RedactionConfig::default(),
        disclosure_budget: None,
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

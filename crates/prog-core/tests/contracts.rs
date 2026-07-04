use prog_core::{
    AuthRef, CacheEntryMeta, CacheInfo, CachePolicy, CallProvenance, CursorRecord,
    DISCLOSURE_VERSION, DisclosureEnvelope, EffectSet, NextAction, OmittedRegion, SliceRequest,
    SourceProfile, Summary, TrustSettings, canonical_json, public_contract_schemas,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn assert_extra_roundtrips<T>(value: Value, key: &str)
where
    T: DeserializeOwned + Serialize,
{
    let decoded: T = serde_json::from_value(value).expect("contract should deserialize");
    let encoded = serde_json::to_value(decoded).expect("contract should serialize");
    assert_eq!(encoded[key], json!("kept"));
}

#[test]
fn unknown_fields_survive_roundtrip_for_public_contracts() {
    assert_extra_roundtrips::<SourceProfile>(
        json!({
            "schema_version": "prog.source_profile.v1",
            "id": "local",
            "kind": "cli",
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<AuthRef>(
        json!({"name": "token", "env": "TOKEN_ENV", "x_future": "kept"}),
        "x_future",
    );
    assert_extra_roundtrips::<prog_core::OperationProfile>(
        json!({"id": "list", "x_future": "kept"}),
        "x_future",
    );
    assert_extra_roundtrips::<CachePolicy>(json!({"x_future": "kept"}), "x_future");
    assert_extra_roundtrips::<TrustSettings>(json!({"x_future": "kept"}), "x_future");
    assert_extra_roundtrips::<EffectSet>(json!({"x_future": "kept"}), "x_future");
    assert_extra_roundtrips::<DisclosureEnvelope>(
        json!({
            "schema_version": DISCLOSURE_VERSION,
            "summary": {"kind": "object"},
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<Summary>(json!({"kind": "array", "x_future": "kept"}), "x_future");
    assert_extra_roundtrips::<OmittedRegion>(
        json!({"path": "/items", "reason": "long_array", "x_future": "kept"}),
        "x_future",
    );
    assert_extra_roundtrips::<NextAction>(
        json!({"kind": "expand", "x_future": "kept"}),
        "x_future",
    );
    assert_extra_roundtrips::<SliceRequest>(json!({"x_future": "kept"}), "x_future");
    assert_extra_roundtrips::<CursorRecord>(
        json!({
            "cache_key": "sha256:abc",
            "source_id": "local",
            "operation": "list",
            "root_path": "",
            "redaction_version": 1,
            "created_at": "2026-07-04T00:00:00Z",
            "expires_at": "2026-07-05T00:00:00Z",
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<CacheEntryMeta>(
        json!({
            "key": "sha256:abc",
            "payload_hash": "sha256:def",
            "source_id": "local",
            "operation": "list",
            "created_at": "2026-07-04T00:00:00Z",
            "expires_at": "2026-07-05T00:00:00Z",
            "payload_bytes": 42,
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<CallProvenance>(
        json!({
            "source_call_id": "call_1",
            "captured_at": "2026-07-04T00:00:00Z",
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<CacheInfo>(
        json!({"status": "stored", "x_future": "kept"}),
        "x_future",
    );
}

#[test]
fn version_fields_are_required_on_versioned_contracts() {
    let source_error = serde_json::from_value::<SourceProfile>(json!({
        "id": "missing_version",
        "kind": "cli"
    }))
    .unwrap_err();
    assert!(source_error.to_string().contains("schema_version"));

    let envelope_error = serde_json::from_value::<DisclosureEnvelope>(json!({
        "summary": {"kind": "object"}
    }))
    .unwrap_err();
    assert!(envelope_error.to_string().contains("schema_version"));
}

#[test]
fn effect_defaults_fail_closed_when_fields_are_absent() {
    let effects: EffectSet = serde_json::from_value(json!({})).unwrap();

    assert!(!effects.read_only);
    assert!(effects.mutating);
    assert!(effects.network);
    assert!(effects.shell);
    assert!(effects.sensitive);
    assert!(!effects.cacheable);
    assert!(effects.requires_confirmation);
}

#[test]
fn explicit_effect_flags_are_not_overwritten_by_defaults() {
    let effects: EffectSet = serde_json::from_value(json!({
        "read_only": true,
        "mutating": false,
        "network": false,
        "shell": false,
        "sensitive": false,
        "cacheable": true,
        "requires_confirmation": false
    }))
    .unwrap();

    assert!(effects.read_only);
    assert!(!effects.mutating);
    assert!(!effects.network);
    assert!(!effects.shell);
    assert!(!effects.sensitive);
    assert!(effects.cacheable);
    assert!(!effects.requires_confirmation);
}

#[test]
fn canonical_json_sorts_object_keys_recursively_without_sorting_arrays() {
    let left = json!({
        "b": 2,
        "a": {"d": 4, "c": 3},
        "items": [{"z": 1, "a": 2}, {"b": 1, "a": 2}]
    });
    let right = json!({
        "items": [{"a": 2, "z": 1}, {"a": 2, "b": 1}],
        "a": {"c": 3, "d": 4},
        "b": 2
    });
    let reordered_array = json!({
        "a": {"c": 3, "d": 4},
        "b": 2,
        "items": [{"a": 2, "b": 1}, {"a": 2, "z": 1}]
    });

    assert_eq!(
        canonical_json(&left).unwrap(),
        canonical_json(&right).unwrap()
    );
    assert_ne!(
        canonical_json(&left).unwrap(),
        canonical_json(&reordered_array).unwrap()
    );
}

#[test]
fn schemas_generate_for_all_public_contracts() {
    let schemas = public_contract_schemas().unwrap();
    for expected in [
        "SourceProfile",
        "OperationProfile",
        "Shape",
        "EffectSet",
        "CachePolicy",
        "TrustSettings",
        "AuthRef",
        "DisclosureEnvelope",
        "Summary",
        "OmittedRegion",
        "NextAction",
        "SliceRequest",
        "CursorRecord",
        "CacheEntryMeta",
        "CallProvenance",
        "CacheInfo",
        "CacheList",
        "PurgeSummary",
    ] {
        assert!(
            schemas.contains_key(expected),
            "schema missing for {expected}"
        );
    }
}

#[test]
fn source_profile_fixtures_deserialize() {
    for fixture in [
        "fixtures/http_source_profile.json",
        "fixtures/cli_source_profile.json",
        "fixtures/mcp_source_profile.json",
    ] {
        let raw =
            std::fs::read_to_string(format!("{}/tests/{fixture}", env!("CARGO_MANIFEST_DIR")))
                .expect("fixture should be readable");
        let profile: SourceProfile = serde_json::from_str(&raw).expect("fixture should decode");
        assert_eq!(profile.schema_version, "prog.source_profile.v1");
        assert!(!profile.operations.is_empty());
    }
}

use prog_core::{
    AuthRef, CacheEntryMeta, CacheInfo, CachePolicy, CallProvenance, CursorRecord,
    DISCLOSURE_VERSION, DisclosureEnvelope, EVIDENCE_BLOCK_VERSION, EffectSet, EvidenceBlock,
    EvidenceRef, Finding, FindingCommandHints, INSPECT_VERSION, InspectResponse,
    LENS_MANIFEST_VERSION, LensManifest, NextAction, OmittedRegion, SEARCH_VERSION, SearchResponse,
    SliceRequest, SourceProfile, Summary, TrustSettings, canonical_json, public_contract_schemas,
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
    assert_extra_roundtrips::<EvidenceRef>(
        json!({
            "schema_version": "prog.evidence_ref.v1",
            "source_id": "observe",
            "operation": "artifact",
            "path": "/items/0",
            "stale": false,
            "redacted": false,
            "lossy": false,
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<InspectResponse>(
        json!({
            "schema_version": INSPECT_VERSION,
            "cursor": "pc1_demo",
            "goal": "find the root cause",
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<Finding>(
        json!({
            "rank": 1,
            "kind": "test_failure",
            "path": "/failure_sections/0",
            "confidence": 0.95,
            "reason": "first failing test",
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<EvidenceBlock>(
        json!({
            "schema_version": EVIDENCE_BLOCK_VERSION,
            "cursor": "pc1_demo",
            "path": "/failure_sections/0",
            "kind": "test_failure",
            "summary": "first failing test",
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<SearchResponse>(
        json!({
            "schema_version": SEARCH_VERSION,
            "cursor": "pc1_demo",
            "query": "NullPointerException",
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
    assert_extra_roundtrips::<LensManifest>(
        json!({
            "schema_version": LENS_MANIFEST_VERSION,
            "id": "local.items",
            "version": 1,
            "x_future": "kept"
        }),
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

    let inspect_error = serde_json::from_value::<InspectResponse>(json!({
        "cursor": "pc1_demo",
        "goal": "find root cause"
    }))
    .unwrap_err();
    assert!(inspect_error.to_string().contains("schema_version"));

    let evidence_error = serde_json::from_value::<EvidenceBlock>(json!({
        "cursor": "pc1_demo",
        "path": "/failure_sections/0",
        "kind": "test_failure",
        "summary": "first failing test"
    }))
    .unwrap_err();
    assert!(evidence_error.to_string().contains("schema_version"));

    let search_error = serde_json::from_value::<SearchResponse>(json!({
        "cursor": "pc1_demo",
        "query": "panic"
    }))
    .unwrap_err();
    assert!(search_error.to_string().contains("schema_version"));
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
        "ObservationMetadata",
        "EvidenceRef",
        "InspectResponse",
        "Finding",
        "FindingCommandHints",
        "EvidenceBlock",
        "EvidenceCitation",
        "SearchResponse",
        "SearchHit",
        "LineRange",
        "ByteRange",
        "RedactionState",
        "Summary",
        "OmittedRegion",
        "NextAction",
        "LensManifest",
        "LensMatch",
        "LensView",
        "LensOmission",
        "LensFixtures",
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
fn evidence_navigation_contracts_cover_north_star_workflow() {
    let response = InspectResponse {
        schema_version: INSPECT_VERSION.to_string(),
        cursor: "pc1_demo".to_string(),
        goal: "find the root cause".to_string(),
        normalized_goal: Some("root_cause".to_string()),
        scope_path: None,
        findings: vec![Finding {
            rank: 1,
            kind: "rust_compile_error".to_string(),
            path: "/failure_sections/0".to_string(),
            confidence: 0.96,
            reason: "first compiler error with file and line evidence".to_string(),
            title: Some("first compiler error".to_string()),
            severity: Some("error".to_string()),
            source: Some("generic.run.failure_sections".to_string()),
            lens_id: Some("cargo-test".to_string()),
            evidence_ref: Some(EvidenceRef {
                schema_version: "prog.evidence_ref.v1".to_string(),
                source_id: "run".to_string(),
                operation: "cargo".to_string(),
                cursor: Some("pc1_demo".to_string()),
                path: "/failure_sections/0".to_string(),
                uri: None,
                captured_at: Some("2026-07-07T00:00:00Z".to_string()),
                cache_status: None,
                age_seconds: Some(0),
                expires_at: None,
                stale: false,
                redacted: true,
                lossy: false,
                redacted_slice_sha256: Some("sha256:abc".to_string()),
                extra: Default::default(),
            }),
            line_range: None,
            byte_range: None,
            redaction_state: None,
            commands: FindingCommandHints {
                inspect: None,
                expand: Some("prog expand pc1_demo --path /failure_sections/0".to_string()),
                evidence: Some("prog evidence pc1_demo --path /failure_sections/0".to_string()),
                search: None,
                extra: Default::default(),
            },
            extra: Default::default(),
        }],
        omitted: Vec::new(),
        cache: None,
        warnings: Vec::new(),
        extra: Default::default(),
    };

    let encoded = serde_json::to_value(&response).unwrap();
    assert_eq!(encoded["schema_version"], INSPECT_VERSION);
    assert_eq!(encoded["findings"][0]["kind"], "rust_compile_error");
    assert_eq!(
        encoded["findings"][0]["commands"]["evidence"],
        "prog evidence pc1_demo --path /failure_sections/0"
    );

    let decoded: InspectResponse = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.findings[0].confidence, 0.96);
    assert_eq!(
        decoded.findings[0].commands.expand.as_deref(),
        Some("prog expand pc1_demo --path /failure_sections/0")
    );
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

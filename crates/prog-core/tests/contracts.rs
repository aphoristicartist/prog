use prog_core::{
    AuthRef, CacheEntryMeta, CacheInfo, CachePolicy, CallProvenance, CaptureCompleteness,
    CursorRecord, DISCLOSURE_SCHEMA, DisclosureBudget, DisclosureEnvelope, EVIDENCE_BLOCK_SCHEMA,
    EffectSet, EvidenceAvailability, EvidenceBlock, EvidenceRef, Finding, FindingCommandHints,
    INSPECT_SCHEMA, InspectResponse, LENS_MANIFEST_SCHEMA, LensFindingRule, LensManifest,
    NextAction, OmittedRegion, SEARCH_SCHEMA, SearchResponse, SessionEvent, SessionTrail,
    SliceRequest, SourceProfile, Summary, TrustSettings, canonical_json, public_contract_schemas,
    validate_source_profile,
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
            "schema": "prog.source_profile",
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
            "schema": DISCLOSURE_SCHEMA,
            "summary": {"kind": "object"},
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<EvidenceRef>(
        json!({
            "schema": "prog.evidence_ref",
            "source_id": "observe",
            "operation": "artifact",
            "path": "/items/0",
            "stale": false,
            "availability": "recoverable",
            "capture": {
                "total_bytes": 8,
                "captured_bytes": 8,
                "stored_bytes": 8,
                "stop_reason": "complete",
                "can_prove_absence": true
            },
            "redacted": false,
            "lossy": false,
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<InspectResponse>(
        json!({
            "schema": INSPECT_SCHEMA,
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
            "schema": EVIDENCE_BLOCK_SCHEMA,
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
            "schema": SEARCH_SCHEMA,
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
    assert_extra_roundtrips::<LensFindingRule>(
        json!({
            "kind": "error",
            "path": "/errors/*",
            "confidence": 0.9,
            "reason": "error evidence",
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<SessionEvent>(
        json!({
            "id": "pe1_demo",
            "session_id": "ps1_demo",
            "sequence": 1,
            "timestamp": "2026-07-09T00:00:00Z",
            "kind": "inspect",
            "x_future": "kept"
        }),
        "x_future",
    );
    assert_extra_roundtrips::<SessionTrail>(
        json!({
            "schema": "prog.session",
            "session_id": "ps1_demo",
            "created_at": "2026-07-09T00:00:00Z",
            "updated_at": "2026-07-09T00:00:00Z",
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
}

#[test]
fn next_action_exactness_variants_roundtrip_as_typed_values() {
    for (exactness, expected) in [
        ("exact", prog_core::ActionExactness::Exact),
        ("filter", prog_core::ActionExactness::Filter),
        ("approximate", prog_core::ActionExactness::Approximate),
    ] {
        let action: NextAction = serde_json::from_value(json!({
            "kind": "rerun",
            "exactness": exactness,
            "argv": ["tool", "argument"]
        }))
        .expect("next action should deserialize");
        assert_eq!(action.exactness, Some(expected));
        assert_eq!(
            serde_json::to_value(action).unwrap()["exactness"],
            exactness
        );
    }
}

#[test]
fn source_location_schema_snapshot_preserves_search_hit_parity() {
    let schemas = public_contract_schemas().expect("public schemas");
    let source_span = schemas.get("SourceSpan").expect("SourceSpan schema");
    let search_hit = schemas.get("SearchHit").expect("SearchHit schema");
    let snapshot = json!({
        "source_span_required": source_span["required"],
        "source_span_properties": source_span["properties"]
            .as_object()
            .expect("SourceSpan properties")
            .keys()
            .collect::<Vec<_>>(),
        "search_hit_location_properties": {
            "primary_span": search_hit["properties"].get("primary_span").is_some(),
            "related_spans": search_hit["properties"].get("related_spans").is_some(),
            "line_range": search_hit["properties"].get("line_range").is_some(),
            "byte_range": search_hit["properties"].get("byte_range").is_some(),
            "redaction_state": search_hit["properties"].get("redaction_state").is_some()
        }
    });
    assert_eq!(
        snapshot,
        json!({
            "source_span_required": ["start_line", "role", "origin", "exactness"],
            "source_span_properties": [
                "path", "uri", "start_line", "start_column", "end_line", "end_column",
                "role", "label", "origin", "exactness", "redaction_state"
            ],
            "search_hit_location_properties": {
                "byte_range": true,
                "line_range": true,
                "primary_span": true,
                "redaction_state": true,
                "related_spans": true
            }
        })
    );
}

#[test]
fn source_profile_disclosure_budget_is_typed_and_forward_compatible() {
    let profile: SourceProfile = serde_json::from_value(json!({
        "schema": "prog.source_profile",
        "id": "local",
        "kind": "cli",
        "disclosure_budget": {"max_bytes": 4096, "x_future": "kept"}
    }))
    .unwrap();
    assert_eq!(
        profile.disclosure_budget,
        Some(DisclosureBudget {
            max_bytes: 4096,
            extra: serde_json::Map::from_iter([("x_future".to_string(), json!("kept"))]),
        })
    );
}

#[test]
fn lens_manifest_input_rejects_legacy_and_unknown_fields() {
    for value in [
        json!({
            "schema_version": "prog.lens_manifest.v1",
            "id": "legacy"
        }),
        json!({
            "schema": LENS_MANIFEST_SCHEMA,
            "id": "legacy",
            "version": 1
        }),
        json!({
            "schema": LENS_MANIFEST_SCHEMA,
            "id": "unknown",
            "x_future": true
        }),
    ] {
        assert!(serde_json::from_value::<LensManifest>(value).is_err());
    }
}

#[test]
fn cache_info_rejects_unknown_fields() {
    // CacheInfo is a closed result-envelope struct (mirroring LensManifest):
    // an unknown field must fail to deserialize rather than silently round-trip
    // through the flattened `extra`.
    let error = serde_json::from_value::<CacheInfo>(json!({
        "status": "stored",
        "x_future": "kept"
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn source_profile_rejects_legacy_compatibility_fields() {
    let profile: SourceProfile = serde_json::from_value(json!({
        "schema": "prog.source_profile",
        "id": "legacy",
        "kind": "cli",
        "revision": 1,
        "version": 1
    }))
    .unwrap();
    let error = validate_source_profile(&profile).unwrap_err();
    assert!(error.to_string().contains("unsupported"));
}

#[test]
fn schema_identity_is_required_on_prog_contracts() {
    let source_error = serde_json::from_value::<SourceProfile>(json!({
        "id": "missing_version",
        "kind": "cli"
    }))
    .unwrap_err();
    assert!(source_error.to_string().contains("schema"));

    let envelope_error = serde_json::from_value::<DisclosureEnvelope>(json!({
        "summary": {"kind": "object"}
    }))
    .unwrap_err();
    assert!(envelope_error.to_string().contains("schema"));

    let inspect_error = serde_json::from_value::<InspectResponse>(json!({
        "cursor": "pc1_demo",
        "goal": "find root cause"
    }))
    .unwrap_err();
    assert!(inspect_error.to_string().contains("schema"));

    let evidence_error = serde_json::from_value::<EvidenceBlock>(json!({
        "cursor": "pc1_demo",
        "path": "/failure_sections/0",
        "kind": "test_failure",
        "summary": "first failing test"
    }))
    .unwrap_err();
    assert!(evidence_error.to_string().contains("schema"));

    let search_error = serde_json::from_value::<SearchResponse>(json!({
        "cursor": "pc1_demo",
        "query": "panic"
    }))
    .unwrap_err();
    assert!(search_error.to_string().contains("schema"));

    let session_error = serde_json::from_value::<SessionTrail>(json!({
        "session_id": "ps1_demo",
        "created_at": "2026-07-09T00:00:00Z",
        "updated_at": "2026-07-09T00:00:00Z"
    }))
    .unwrap_err();
    assert!(session_error.to_string().contains("schema"));
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
        "DisclosureBudget",
        "OperationProfile",
        "Shape",
        "EffectSet",
        "CachePolicy",
        "TrustSettings",
        "AuthRef",
        "DisclosureEnvelope",
        "ObservationMetadata",
        "ObservationRecord",
        "WorkspaceState",
        "WorkspacePathState",
        "WorkspaceValidity",
        "WorkspaceComparison",
        "EvidenceAvailability",
        "BudgetSource",
        "CaptureLimit",
        "CaptureBudget",
        "StorageBudget",
        "StorageBudgetSummary",
        "CaptureStopReason",
        "CaptureScope",
        "CaptureCompleteness",
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
        "SourceSpan",
        "SourceSpanExactness",
        "RedactionState",
        "Summary",
        "OmittedRegion",
        "ActionExactness",
        "NextAction",
        "LensManifest",
        "LensFindingRule",
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
        "SessionEvent",
        "SessionTrail",
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
        schema: INSPECT_SCHEMA.to_string(),
        cursor: "pc1_demo".to_string(),
        goal: "find the root cause".to_string(),
        normalized_goal: Some("root_cause".to_string()),
        scope_path: None,
        findings: vec![Finding {
            occurrence_id: None,
            fingerprint: None,
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
                schema: "prog.evidence_ref".to_string(),
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
                availability: EvidenceAvailability::Recoverable,
                capture: CaptureCompleteness::complete(64),
                redacted: true,
                lossy: false,
                redacted_slice_sha256: Some("sha256:abc".to_string()),
                extra: Default::default(),
            }),
            line_range: None,
            byte_range: None,
            primary_span: None,
            related_spans: Vec::new(),
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
    assert_eq!(encoded["schema"], INSPECT_SCHEMA);
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
        assert_eq!(profile.schema, "prog.source_profile");
        assert!(!profile.operations.is_empty());
    }
}

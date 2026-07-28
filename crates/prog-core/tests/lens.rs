use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use prog_core::{
    FindingOptions, LENS_MANIFEST_SCHEMA, LensManifest, PreviewPolicy, RawPayload, RedactedPayload,
    RedactionPolicy, SliceRequest, lens_slice_request, project_with_lens,
    ranked_findings_with_lens, validate_lens_manifest,
};
use serde_json::{Value, json};

fn manifest(value: Value) -> LensManifest {
    serde_json::from_value(value).expect("manifest should deserialize")
}

fn empty_slice() -> SliceRequest {
    SliceRequest {
        path: None,
        limit: None,
        depth: None,
        fields: Vec::new(),
        omit: Vec::new(),
        extra: serde_json::Map::new(),
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should canonicalize")
}

fn redacted(value: Value) -> RedactedPayload {
    RawPayload::new(value)
        .redact(&RedactionPolicy::default())
        .payload
}

#[test]
fn lens_manifest_projects_fields_omissions_actions_and_redaction() {
    let lens = manifest(json!({
        "schema": LENS_MANIFEST_SCHEMA,
        "id": "github.issues.triage",
        "view": {
            "root": "/items",
            "limit": 1,
            "fields": {
                "id": "/id",
                "labels": "/labels/*/name",
                "state": "/state",
                "title": "/title",
                "token": "/token"
            }
        },
        "omit": [
            {
                "path": "/items/*/body",
                "reason": "large_string",
                "detail": "body is expandable on demand",
                "expandable": true
            }
        ],
        "next_actions": [
            {
                "kind": "expand",
                "path": "/items/{index}/body",
                "reason": "inspect body only for relevant rows"
            }
        ]
    }));
    validate_lens_manifest(&lens).unwrap();

    let payload = redacted(json!({
        "items": [
            {
                "id": 1,
                "state": "open",
                "title": "First",
                "body": "large body",
                "token": "[REDACTED:secret_field]",
                "labels": [{"name": "bug"}, {"name": "urgent"}]
            },
            {
                "id": 2,
                "state": "closed",
                "title": "Second",
                "body": "other body",
                "labels": [{"name": "docs"}]
            }
        ],
        "meta": {"count": 2}
    }));

    let slice = lens_slice_request(&lens, &empty_slice()).unwrap();
    let root = slice.path.as_deref().unwrap();
    let projected = project_with_lens(
        &payload,
        root,
        &slice,
        &PreviewPolicy::default(),
        Some(&lens),
    )
    .unwrap();

    assert_eq!(projected.projection.preview.as_array().unwrap().len(), 1);
    assert_eq!(projected.projection.preview[0]["id"], json!(1));
    assert_eq!(projected.projection.preview[0]["labels"], json!(["bug"]));
    assert_eq!(
        projected.projection.preview[0]["token"],
        json!("«redacted»")
    );
    assert!(projected.projection.preview[0].get("body").is_none());
    assert!(projected.projection.omitted.iter().any(
        |omitted| omitted.path == "/items/*/body" && omitted.extra["expandable"] == json!(true)
    ));
    assert!(
        projected
            .projection
            .omitted
            .iter()
            .any(|omitted| omitted.path == "/items/0/token")
    );
    assert_eq!(
        projected.next_actions[0].path.as_deref(),
        Some("/items/{index}/body")
    );
}

#[test]
fn lens_manifest_validation_rejects_bad_contracts_and_escaping_paths() {
    let wrong_schema = manifest(json!({
        "schema": "prog.wrong",
        "id": "bad"
    }));
    assert!(
        validate_lens_manifest(&wrong_schema)
            .unwrap_err()
            .to_string()
            .contains("schema")
    );

    let bad_selector = manifest(json!({
        "schema": LENS_MANIFEST_SCHEMA,
        "id": "bad-selector",
        "view": {
            "fields": {"title": "title"}
        }
    }));
    assert!(
        validate_lens_manifest(&bad_selector)
            .unwrap_err()
            .to_string()
            .contains("view.fields.title")
    );

    let escaping_omit = manifest(json!({
        "schema": LENS_MANIFEST_SCHEMA,
        "id": "escaping",
        "view": {"root": "/items"},
        "omit": [{"path": "/meta", "reason": "deep_object"}]
    }));
    assert!(
        validate_lens_manifest(&escaping_omit)
            .unwrap_err()
            .to_string()
            .contains("outside view.root")
    );
}

#[test]
fn missing_lens_fields_are_not_fabricated() {
    let lens = manifest(json!({
        "schema": LENS_MANIFEST_SCHEMA,
        "id": "missing-fields",
        "view": {
            "root": "/items",
            "fields": {
                "id": "/id",
                "missing": "/missing"
            }
        }
    }));
    let payload = redacted(json!({"items": [{"id": 1}]}));
    let slice = lens_slice_request(&lens, &empty_slice()).unwrap();
    let projected = project_with_lens(
        &payload,
        slice.path.as_deref().unwrap(),
        &slice,
        &PreviewPolicy::default(),
        Some(&lens),
    )
    .unwrap();

    assert_eq!(projected.projection.preview[0]["id"], json!(1));
    assert!(projected.projection.preview[0].get("missing").is_none());
}

#[test]
fn first_party_lens_pack_is_valid_unique_fixture_backed_and_token_efficient() {
    let lens_dir = repo_root().join("lenses");
    let mut ids = BTreeSet::new();
    let mut manifest_count = 0usize;

    for entry in std::fs::read_dir(&lens_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        manifest_count += 1;
        let raw = std::fs::read_to_string(&path).unwrap();
        let lens: LensManifest = serde_json::from_str(&raw).unwrap();
        validate_lens_manifest(&lens).unwrap();
        assert!(ids.insert(lens.id.clone()), "duplicate lens id {}", lens.id);
        assert!(
            !lens.invariants.is_empty(),
            "{} should document invariants",
            lens.id
        );
        assert!(
            !lens.fixtures.positive.is_empty(),
            "{} should include positive fixtures",
            lens.id
        );
        assert!(
            !lens.fixtures.negative.is_empty(),
            "{} should include counterexample fixtures",
            lens.id
        );

        for fixture in lens
            .fixtures
            .positive
            .iter()
            .chain(lens.fixtures.negative.iter())
        {
            assert!(
                lens_dir.join(fixture).exists(),
                "{} references missing fixture {}",
                lens.id,
                fixture
            );
        }

        for fixture in &lens.fixtures.positive {
            let payload_raw = std::fs::read_to_string(lens_dir.join(fixture)).unwrap();
            let payload: Value = serde_json::from_str(&payload_raw).unwrap();
            let payload = redacted(payload);
            let slice = lens_slice_request(&lens, &empty_slice()).unwrap();
            let root_path = slice.path.as_deref().unwrap_or("");
            let projected = project_with_lens(
                &payload,
                root_path,
                &slice,
                &PreviewPolicy::default(),
                Some(&lens),
            )
            .unwrap();
            let lens_visible = json!({
                "data_preview": projected.projection.preview,
                "omitted": projected.projection.omitted,
                "next_actions": projected.next_actions
            });
            let lens_bytes = serde_json::to_vec(&lens_visible).unwrap().len();
            let raw_bytes = payload_raw.len();
            let simple_truncation_bytes = raw_bytes.min(2048);
            assert!(
                lens_bytes < raw_bytes,
                "{} fixture {} should project smaller than raw payload: lens={} raw={}",
                lens.id,
                fixture,
                lens_bytes,
                raw_bytes
            );
            assert!(
                lens_bytes <= simple_truncation_bytes,
                "{} fixture {} should beat the simple 2KiB truncation baseline: lens={} truncation={}",
                lens.id,
                fixture,
                lens_bytes,
                simple_truncation_bytes
            );
            assert!(
                !serde_json::to_string(&lens_visible)
                    .unwrap()
                    .contains("plain-secret"),
                "{} fixture {} should not expose unredacted fixture secrets",
                lens.id,
                fixture
            );
        }
    }

    assert!(
        manifest_count >= 5,
        "first-party pack should ship at least five lenses"
    );
}

/// A wildcard lens rule must select by *content*, not by position in the
/// payload. Candidate paths used to be resolved in document order up to a fixed
/// cap and only then tested against `contains_any`, which made a rule over
/// `/lines/*` structurally blind past the cap — the "truncate and lose the
/// answer" failure prog exists to prevent, inside prog.
#[test]
fn lens_rule_matches_content_far_beyond_the_candidate_cap() {
    let target = 3_000usize;
    let lines = (0..4_000usize)
        .map(|index| {
            if index == target {
                json!({"number": index + 1, "text": "svc: FATAL unrecoverable commit failure"})
            } else {
                json!({"number": index + 1, "text": "svc: request completed ok"})
            }
        })
        .collect::<Vec<_>>();
    let payload = json!({"lines": lines});

    let lens = manifest(json!({
        "schema": LENS_MANIFEST_SCHEMA,
        "id": "cap-probe",
        "match": {"source_id": "observe", "artifact_kind": "text"},
        "view": {"limit": 20, "depth": 4, "fields": {}},
        "findings": [{
            "kind": "log_fatal",
            "path": "/lines/*",
            "confidence": 0.97,
            "reason": "fatal marker",
            "severity": "fatal",
            "contains_any": ["fatal"]
        }]
    }));

    let findings =
        ranked_findings_with_lens(&payload, &FindingOptions::default(), Some(&lens)).unwrap();

    let matched = findings
        .iter()
        .find(|finding| finding.path == format!("/lines/{target}"));
    assert!(
        matched.is_some(),
        "lens rule must classify a match at index {target}; got paths {:?}",
        findings.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    assert_eq!(matched.unwrap().kind, "log_fatal");
}

/// An unrecoverable FATAL line must outrank retryable ERROR noise. Ranking them
/// equally buried real root causes underneath transient retries.
#[test]
fn fatal_markers_outrank_retryable_error_markers() {
    let mut lines = (0..40usize)
        .map(|index| json!({"number": index + 1, "text": "svc: ERROR transient retry 3 of 5"}))
        .collect::<Vec<_>>();
    lines.push(json!({"number": 41, "text": "svc: FATAL data corruption, not retryable"}));
    let payload = json!({"lines": lines});

    let lens: LensManifest = serde_json::from_slice(
        &std::fs::read(repo_root().join("lenses/logs.json")).expect("logs lens should read"),
    )
    .expect("logs lens should deserialize");

    let findings =
        ranked_findings_with_lens(&payload, &FindingOptions::default(), Some(&lens)).unwrap();

    assert_eq!(
        findings[0].path,
        "/lines/40",
        "the FATAL line must rank first; got {:?}",
        findings
            .iter()
            .map(|f| (&f.path, &f.kind, f.confidence))
            .collect::<Vec<_>>()
    );
    assert_eq!(findings[0].kind, "log_fatal");
    assert_eq!(findings[0].severity.as_deref(), Some("fatal"));
}

/// `--goal` must reorder equally-strong findings of the *same* kind, and must
/// never promote weaker evidence above stronger. Goal intent used only to
/// separate different kinds, which made it inert on log payloads where every
/// candidate shares one kind.
#[test]
fn goal_reorders_same_kind_findings_without_demoting_stronger_evidence() {
    let payload = json!({"lines": [
        {"number": 1, "text": "auth: ERROR token refresh failed"},
        {"number": 2, "text": "pool: ERROR worker checkout failed"},
        {"number": 3, "text": "svc: FATAL unrecoverable disk failure"},
    ]});

    let lens: LensManifest = serde_json::from_slice(
        &std::fs::read(repo_root().join("lenses/logs.json")).expect("logs lens should read"),
    )
    .expect("logs lens should deserialize");

    let rank_for = |goal: &str| {
        let options = FindingOptions {
            goal: Some(goal.to_string()),
            ..FindingOptions::default()
        };
        ranked_findings_with_lens(&payload, &options, Some(&lens))
            .unwrap()
            .into_iter()
            .map(|finding| finding.path)
            .collect::<Vec<_>>()
    };

    let auth = rank_for("investigate the auth token refresh");
    let pool = rank_for("investigate the pool worker checkout");

    assert_ne!(
        auth, pool,
        "two different goals must not produce identical orderings"
    );
    // The FATAL line is strictly stronger evidence (0.97 vs 0.92) and stays
    // first under both goals: a goal orders ties, it does not outrank evidence.
    assert_eq!(auth[0], "/lines/2");
    assert_eq!(pool[0], "/lines/2");
    assert!(
        auth.iter().position(|p| p == "/lines/0") < auth.iter().position(|p| p == "/lines/1"),
        "auth goal should surface the auth line before the pool line: {auth:?}"
    );
    assert!(
        pool.iter().position(|p| p == "/lines/1") < pool.iter().position(|p| p == "/lines/0"),
        "pool goal should surface the pool line before the auth line: {pool:?}"
    );
}

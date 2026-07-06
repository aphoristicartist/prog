use prog_core::{
    LENS_MANIFEST_VERSION, LensManifest, PreviewPolicy, SliceRequest, lens_slice_request,
    project_with_lens, validate_lens_manifest,
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

#[test]
fn lens_manifest_projects_fields_omissions_actions_and_redaction() {
    let lens = manifest(json!({
        "schema_version": LENS_MANIFEST_VERSION,
        "id": "github.issues.triage",
        "version": 1,
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

    let payload = json!({
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
    });

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
    let missing_version = manifest(json!({
        "schema_version": LENS_MANIFEST_VERSION,
        "id": "bad",
        "version": 0
    }));
    assert!(
        validate_lens_manifest(&missing_version)
            .unwrap_err()
            .to_string()
            .contains("version")
    );

    let bad_selector = manifest(json!({
        "schema_version": LENS_MANIFEST_VERSION,
        "id": "bad-selector",
        "version": 1,
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
        "schema_version": LENS_MANIFEST_VERSION,
        "id": "escaping",
        "version": 1,
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
        "schema_version": LENS_MANIFEST_VERSION,
        "id": "missing-fields",
        "version": 1,
        "view": {
            "root": "/items",
            "fields": {
                "id": "/id",
                "missing": "/missing"
            }
        }
    }));
    let payload = json!({"items": [{"id": 1}]});
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

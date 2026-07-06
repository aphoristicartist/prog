use prog_core::{
    ExpansionScope, OmissionReason, PreviewPolicy, RawPayload, RedactionPolicy, ScopedSlice,
    SliceRequest, expand,
    pointer::{is_within, parse},
    project,
};
use proptest::prelude::*;
use serde_json::{Value, json};

#[test]
fn projection_is_deterministic_and_budgeted() {
    let payload = json!({
        "items": (0..100).map(|i| json!({"id": i, "body": "x".repeat(2048)})).collect::<Vec<_>>(),
        "wide": (0..100).map(|i| (format!("k{i:03}"), json!(i))).collect::<serde_json::Map<_, _>>()
    });
    let policy = PreviewPolicy::default();

    let left = project(&payload, &policy, "");
    let right = project(&payload, &policy, "");
    assert_eq!(
        serde_json::to_vec(&left).unwrap(),
        serde_json::to_vec(&right).unwrap()
    );
    assert!(serde_json::to_vec(&left).unwrap().len() <= policy.max_envelope_bytes);
    assert!(
        left.omitted
            .iter()
            .any(|region| region.reason == OmissionReason::LongArray)
    );
    assert!(
        left.omitted
            .iter()
            .any(|region| region.reason == OmissionReason::ManyFields)
    );
}

#[test]
fn projection_marks_strings_arrays_objects_and_redaction() {
    let payload = json!({
        "body": "abcdef",
        "secret": "[REDACTED:token]",
        "items": [1, 2, 3],
        "nested": {"a": {"b": {"c": 1}}}
    });
    let policy = PreviewPolicy {
        array_items: 2,
        object_fields: 8,
        string_chars: 3,
        depth: 2,
        node_budget: 100,
        max_envelope_bytes: 16 * 1024,
    };

    let projection = project(&payload, &policy, "");
    assert_eq!(projection.preview["body"], json!("abc…"));
    assert_eq!(projection.preview["secret"], json!("«redacted»"));
    assert_eq!(projection.preview["items"], json!([1, 2]));
    assert_eq!(
        projection.preview["nested"]["a"],
        json!("«object: 1 fields»")
    );
    assert!(
        projection
            .omitted
            .iter()
            .any(|region| region.path == "/body" && region.reason == OmissionReason::LargeString)
    );
    assert!(
        projection
            .omitted
            .iter()
            .any(|region| region.path == "/secret" && region.reason == OmissionReason::Redacted)
    );
}

#[test]
fn expand_rejects_paths_outside_cursor_boundary_segment_wise() {
    let payload = json!({"a": {"child": 1}, "ab": 2, "a/b": 3});
    let payload = redacted(payload);
    let policy = PreviewPolicy::default();

    assert!(expand(&payload, &scoped("/a", slice("/a/child")), &policy).is_ok());
    let outside = ScopedSlice::new(ExpansionScope::new("/a").unwrap(), slice("/ab")).unwrap_err();
    assert_eq!(outside.kind(), "path_outside_boundary");

    let escaped = ScopedSlice::new(ExpansionScope::new("/a").unwrap(), slice("/a~1b")).unwrap_err();
    assert_eq!(escaped.kind(), "path_outside_boundary");
}

#[test]
fn expand_reports_actionable_path_not_found() {
    let payload = json!({"items": [{"id": 1}]});
    let payload = redacted(payload);
    let error = expand(
        &payload,
        &scoped("", slice("/items/0/missing")),
        &PreviewPolicy::default(),
    )
    .unwrap_err();

    assert_eq!(error.kind(), "path_not_found");
    assert!(error.to_string().contains("/items/0/missing"));
    assert!(error.to_string().contains("keys [id]"));
}

#[test]
fn expand_applies_fields_and_omit_to_objects_and_arrays_of_objects() {
    let payload = json!({
        "items": [
            {"id": 1, "body": "a", "secret": "[REDACTED:token]"},
            {"id": 2, "body": "b", "secret": "[REDACTED:token]"}
        ]
    });
    let payload = redacted(payload);
    let request = SliceRequest {
        path: Some("/items".to_string()),
        limit: Some(10),
        depth: Some(4),
        fields: vec!["id".to_string(), "secret".to_string()],
        omit: vec!["secret".to_string()],
        extra: serde_json::Map::new(),
    };

    let projection = expand(&payload, &scoped("", request), &PreviewPolicy::default()).unwrap();
    assert_eq!(projection.preview, json!([{"id": 1}, {"id": 2}]));
    assert!(projection.omitted.is_empty());
}

#[test]
fn redaction_sentinel_never_reappears_through_projection_or_expansion() {
    let payload = json!({"outer": {"secret": "[REDACTED:api_key]"}});
    let redacted_payload = redacted(payload.clone());
    let policy = PreviewPolicy::default();

    let projection = project(&payload, &policy, "");
    assert!(
        !serde_json::to_string(&projection)
            .unwrap()
            .contains("[REDACTED:api_key]")
    );

    let expanded = expand(
        &redacted_payload,
        &scoped("", slice("/outer/secret")),
        &policy,
    )
    .unwrap();
    assert_eq!(expanded.preview, json!("«redacted»"));
    assert!(
        !serde_json::to_string(&expanded)
            .unwrap()
            .contains("[REDACTED:api_key]")
    );
}

fn slice(path: &str) -> SliceRequest {
    SliceRequest {
        path: Some(path.to_string()),
        limit: None,
        depth: None,
        fields: Vec::new(),
        omit: Vec::new(),
        extra: serde_json::Map::new(),
    }
}

fn scoped(root_path: &str, request: SliceRequest) -> ScopedSlice {
    ScopedSlice::new(ExpansionScope::new(root_path).unwrap(), request).unwrap()
}

fn redacted(value: Value) -> prog_core::RedactedPayload {
    RawPayload::new(value)
        .redact(&RedactionPolicy::default())
        .payload
}

fn arbitrary_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i32>().prop_map(|n| json!(n)),
        arbitrary_string().prop_map(Value::String),
    ];

    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
            prop::collection::btree_map(arbitrary_key(), inner, 0..8)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

fn arbitrary_string() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-zA-Z0-9]{0,256}".prop_map(|value| value),
        Just("x".repeat(512)),
        Just("[REDACTED:token]".to_string()),
        Just("unicode snowman ☃ and emoji 🙂".to_string()),
        Just("line1\nline2\twith tab".to_string()),
    ]
}

fn arbitrary_key() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z/~]{0,8}".prop_map(|value| value),
        Just("token".to_string()),
        Just("api_key".to_string()),
        Just("password".to_string()),
        Just("slash/key".to_string()),
        Just("tilde~key".to_string()),
        Just("unicode🙂".to_string()),
    ]
}

proptest! {
    #[test]
    fn projection_never_fabricates_values(value in arbitrary_json()) {
        let projection = project(&value, &PreviewPolicy::default(), "");
        assert_no_fabrication(&value, &projection.preview);
    }

    #[test]
    fn projection_is_byte_deterministic(value in arbitrary_json()) {
        let policy = PreviewPolicy::default();
        let left = project(&value, &policy, "");
        let right = project(&value, &policy, "");
        prop_assert_eq!(serde_json::to_vec(&left).unwrap(), serde_json::to_vec(&right).unwrap());
    }

    #[test]
    fn pointer_containment_is_segment_based(prefix in pointer_segments(), suffix in pointer_segments()) {
        let boundary = pointer_from_segments(&prefix);
        let mut child = prefix.clone();
        child.extend(suffix);
        let child_path = pointer_from_segments(&child);

        prop_assert!(is_within(&boundary, &child_path).unwrap());

        let mut sibling = prefix.clone();
        if let Some(last) = sibling.last_mut() {
            last.push('x');
            let sibling_path = pointer_from_segments(&sibling);
            if parse(&boundary).unwrap() != parse(&sibling_path).unwrap() {
                prop_assert!(!is_within(&boundary, &sibling_path).unwrap());
            }
        }
    }

    #[test]
    fn expansion_rejects_generated_segment_siblings(prefix in pointer_segments()) {
        prop_assume!(!prefix.is_empty());
        let boundary = pointer_from_segments(&prefix);
        let mut sibling = prefix.clone();
        let last = sibling.last_mut().expect("prefix is non-empty");
        last.push('x');
        let sibling_path = pointer_from_segments(&sibling);
        prop_assume!(parse(&boundary).unwrap() != parse(&sibling_path).unwrap());

        let error =
            ScopedSlice::new(ExpansionScope::new(&boundary).unwrap(), slice(&sibling_path))
                .unwrap_err();
        prop_assert_eq!(error.kind(), "path_outside_boundary");
    }
}

fn pointer_segments() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("[a-z/~]{0,6}", 0..5)
}

fn pointer_from_segments(segments: &[String]) -> String {
    if segments.is_empty() {
        return String::new();
    }

    let mut path = String::new();
    for segment in segments {
        path.push('/');
        path.push_str(&segment.replace('~', "~0").replace('/', "~1"));
    }
    path
}

fn assert_no_fabrication(source: &Value, preview: &Value) {
    match (source, preview) {
        (Value::Object(source), Value::Object(preview)) => {
            for (key, value) in preview {
                assert_no_fabrication(&source[key], value);
            }
        }
        (Value::Array(source), Value::Array(preview)) => {
            assert!(preview.len() <= source.len());
            for (index, value) in preview.iter().enumerate() {
                assert_no_fabrication(&source[index], value);
            }
        }
        (Value::String(source), Value::String(preview)) => {
            if is_marker(preview) {
                return;
            }
            if let Some(prefix) = preview.strip_suffix('…') {
                assert!(source.starts_with(prefix));
            } else {
                assert_eq!(source, preview);
            }
        }
        (_, Value::String(preview)) if is_marker(preview) => {}
        _ => assert_eq!(source, preview),
    }
}

fn is_marker(value: &str) -> bool {
    value.starts_with('«') && value.ends_with('»')
}

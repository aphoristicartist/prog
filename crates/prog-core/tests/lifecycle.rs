use chrono::Utc;
use prog_core::{
    CacheEntryMeta, ExpansionScope, JsonPointer, PreviewPolicy, RawPayload, RedactionPolicy,
    ScopedSlice, SliceRequest, Store, expand, new_cache_entry,
};
use serde_json::json;

#[test]
fn payload_typestate_requires_redaction_before_persistence() {
    let raw_secret = "plain-secret-value";
    let raw = RawPayload::new(json!({
        "token": raw_secret,
        "safe": "visible"
    }));
    let redacted = raw.redact(&RedactionPolicy::default());

    assert_eq!(redacted.redacted_paths, vec!["/token"]);
    assert!(!redacted.payload.as_value().to_string().contains(raw_secret));

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let hash = store.put_payload(&redacted.payload).unwrap();
    let persisted = store.get_payload(&hash).unwrap().unwrap();

    assert!(!persisted.as_value().to_string().contains(raw_secret));
    assert_eq!(persisted.as_value()["safe"], json!("visible"));
}

#[test]
fn scoped_slice_validates_json_pointer_syntax_and_scope() {
    assert_eq!(
        JsonPointer::parse("items").unwrap_err().kind(),
        "bad_pointer"
    );
    assert_eq!(
        JsonPointer::parse("/items/~2bad").unwrap_err().kind(),
        "bad_pointer"
    );

    let scope = ExpansionScope::new("/items").unwrap();
    let inside = ScopedSlice::new(scope.clone(), slice("/items/0")).unwrap();
    assert_eq!(inside.root_path().as_str(), "/items");
    assert_eq!(inside.target_path().as_str(), "/items/0");

    let outside = ScopedSlice::new(scope, slice("/items2/0")).unwrap_err();
    assert_eq!(outside.kind(), "path_outside_boundary");
}

#[test]
fn validated_cursor_creates_expansion_scope_capability() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let payload = RawPayload::new(json!({
        "items": [{"id": 1, "body": "target"}],
        "meta": {"count": 1}
    }))
    .redact(&RedactionPolicy::default())
    .payload;
    let hash = store.put_payload(&payload).unwrap();
    let key = Store::cache_key("source", "op", &json!({"q": 1})).unwrap();
    let entry = entry(key.clone(), hash);
    store.put_entry(&key, &entry).unwrap();
    let cursor = store
        .create_cursor(
            &key,
            "source",
            "op",
            "/items",
            RedactionPolicy::default().version,
            60,
        )
        .unwrap();
    let validated = store
        .get_cursor(&cursor, RedactionPolicy::default().version)
        .unwrap();
    let persisted = store.get_payload(&entry.payload_hash).unwrap().unwrap();
    let scoped = ScopedSlice::new(
        ExpansionScope::from_cursor(&validated).unwrap(),
        slice("/items/0/body"),
    )
    .unwrap();
    let projected = expand(&persisted, &scoped, &PreviewPolicy::default()).unwrap();

    assert_eq!(validated.token(), cursor);
    assert_eq!(projected.preview, json!("target"));
    assert_eq!(
        ScopedSlice::new(
            ExpansionScope::from_cursor(&validated).unwrap(),
            slice("/meta/count")
        )
        .unwrap_err()
        .kind(),
        "path_outside_boundary"
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

fn entry(key: String, payload_hash: String) -> CacheEntryMeta {
    let mut entry = new_cache_entry(
        key,
        payload_hash,
        "source".to_string(),
        "op".to_string(),
        42,
        60,
    );
    entry.created_at = Utc::now().to_rfc3339();
    entry
}

//! Regression coverage for the persistence-redaction security boundary
//! (INVARIANTS.md I2). The default policy must catch the common compound,
//! hyphenated, and mixed-case spellings of secret field names — not only the
//! literal underscore forms — and `is_sensitive_name` must do the same for
//! argv/flag tokens.

use prog_core::{RedactionPolicy, is_sensitive_name};
use serde_json::json;

#[test]
fn compound_secret_field_names_are_redacted_before_persistence() {
    let payload = json!({
        "access_token": "eyJ_ACCESS",
        "refresh_token": "rt_REFRESH",
        "client_secret": "cs_CLIENT",
        "x_api_key": "sk_API",
        "deep": {"authorization_code": "ac_AUTH"},
    });
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);

    let serialized = serde_json::to_string(&redacted).unwrap();
    for secret in ["eyJ_ACCESS", "rt_REFRESH", "cs_CLIENT", "sk_API", "ac_AUTH"] {
        assert!(
            !serialized.contains(secret),
            "secret {secret:?} survived persistence redaction"
        );
    }
    // Each compound name is caught by a default keyword via substring matching.
    assert_eq!(paths.len(), 5);
    assert!(paths.contains(&"/access_token".to_string()));
    assert!(paths.contains(&"/refresh_token".to_string()));
    assert!(paths.contains(&"/client_secret".to_string()));
    assert!(paths.contains(&"/x_api_key".to_string()));
    assert!(paths.contains(&"/deep/authorization_code".to_string()));
}

#[test]
fn hyphenated_and_mixed_case_secret_fields_are_redacted() {
    let payload = json!({
        "api-key": "k1",
        "Api-Key": "k2",
        "API_KEY": "k3",
        "X-API-KEY": "k4"
    });
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    let serialized = serde_json::to_string(&redacted).unwrap();
    for secret in ["k1", "k2", "k3", "k4"] {
        assert!(!serialized.contains(secret));
    }
    assert_eq!(paths.len(), 4);
}

#[test]
fn benign_field_names_are_not_redacted() {
    let payload = json!({
        "account_id": "a",
        "user_email": "u@example.com",
        "item_count": 3,
        "title": "t"
    });
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    assert!(paths.is_empty(), "no field should be redacted: {paths:?}");
    assert_eq!(redacted["account_id"], json!("a"));
    assert_eq!(redacted["user_email"], json!("u@example.com"));
    assert_eq!(redacted["item_count"], json!(3));
}

#[test]
fn is_sensitive_name_catches_compound_and_cased_flag_tokens() {
    for sensitive in [
        "access_token",
        "access-token",
        "ACCESS-TOKEN",
        "refreshToken",
        "passwd",
        "--passwd",
        "x-api-key",
        "X-Api-Key",
        "clientSecret",
        "api_key",
        "apiKey",
        "Authorization",
    ] {
        assert!(
            is_sensitive_name(sensitive),
            "{sensitive:?} should be recognized as sensitive"
        );
    }
    for benign in ["account_id", "user_email", "item_count", "title", "id"] {
        assert!(
            !is_sensitive_name(benign),
            "{benign:?} should NOT be recognized as sensitive"
        );
    }
}

#[test]
fn with_extra_persistence_names_redacts_declared_only_names() {
    // "service_key" contains no default keyword, so the default policy leaves
    // it intact — this is the exact gap that leaked declared-sensitive values
    // to disk from adapter provenance.args.
    let payload = json!({"service_key": "SK-LIVE-1234"});
    let (default_redacted, default_paths) = RedactionPolicy::default().apply_persistence(&payload);
    assert!(default_paths.is_empty());
    assert_eq!(default_redacted["service_key"], json!("SK-LIVE-1234"));

    let extra = vec!["service_key".to_string()];
    let (redacted, paths) =
        RedactionPolicy::with_extra_persistence_names(&extra).apply_persistence(&payload);
    assert_eq!(paths, vec!["/service_key".to_string()]);
    let serialized = serde_json::to_string(&redacted).unwrap();
    assert!(!serialized.contains("SK-LIVE-1234"));
    assert!(serialized.contains("[REDACTED:declared_sensitive]"));
}

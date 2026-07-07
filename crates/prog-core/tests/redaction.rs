//! Regression coverage for the persistence-redaction security boundary
//! (INVARIANTS.md I2). The default policy must catch the common compound,
//! hyphenated, and mixed-case spellings of secret field names — not only the
//! literal underscore forms — and `is_sensitive_name` must do the same for
//! argv/flag tokens.

use prog_core::{RedactionConfig, RedactionPolicy, is_sensitive_name, redact_sensitive_text};
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

#[test]
fn allowlist_protects_benign_token_session_fields() {
    // These previously matched a default keyword by substring and were wrongly
    // wiped; the built-in allowlist now exempts them.
    let payload = json!({
        "max_tokens": 1024,
        "total_tokens": 4096,
        "token_count": 12,
        "session_timeout": 30,
        "cookie_consent": true,
        "secretary": "alex",
        "secretary_email": "alex@example.com"
    });
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    assert!(paths.is_empty(), "no field should be redacted: {paths:?}");
    assert_eq!(redacted["max_tokens"], json!(1024));
    assert_eq!(redacted["session_timeout"], json!(30));
    assert_eq!(redacted["secretary_email"], json!("alex@example.com"));
}

#[test]
fn bare_tokens_field_is_redacted() {
    // A field literally named "tokens" commonly carries credentials, so it is
    // NOT on the allowlist (only the compound metric forms are) and must be
    // redacted under the default policy.
    let payload = json!({"tokens": "sk-live-1234567890"});
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    assert_eq!(paths, vec!["/tokens".to_string()]);
    let serialized = serde_json::to_string(&redacted).unwrap();
    assert!(!serialized.contains("sk-live-1234567890"));
}

#[test]
fn default_keywords_include_access_signing_pwd() {
    let payload = json!({
        "access_key": "AKIAIOSF",
        "signing_key": "sk-sign",
        "pwd": "hunter2",
        "aws_access_key": "AKIA2"
    });
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    let serialized = serde_json::to_string(&redacted).unwrap();
    for secret in ["AKIAIOSF", "sk-sign", "hunter2", "AKIA2"] {
        assert!(!serialized.contains(secret), "{secret} leaked");
    }
    assert_eq!(paths.len(), 4);
}

#[test]
fn camel_case_secrets_redacted_benign_camelcase_allowlisted() {
    let payload = json!({
        "refreshToken": "rt",
        "clientSecret": "cs",
        "accessToken": "at",
        "apiKey": "ak",
        "maxTokens": 2048,
        "totalTokens": 8192,
        "tokenCount": 5,
        "sessionTimeout": 60,
        "secretaryEmail": "s@example.com"
    });
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    let serialized = serde_json::to_string(&redacted).unwrap();
    for secret in ["rt", "cs", "at", "ak"] {
        assert!(
            !serialized.contains(secret),
            "camelCase secret {secret} leaked"
        );
    }
    assert_eq!(redacted["maxTokens"], json!(2048));
    assert_eq!(redacted["sessionTimeout"], json!(60));
    assert_eq!(redacted["secretaryEmail"], json!("s@example.com"));
    assert_eq!(paths.len(), 4);
}

#[test]
fn is_sensitive_name_reflects_keywords_and_allowlist() {
    for sensitive in [
        "access_key",
        "signing_key",
        "pwd",
        "auth_token",
        "access-key",
        "--access-key",
    ] {
        assert!(
            is_sensitive_name(sensitive),
            "{sensitive:?} should be sensitive"
        );
    }
    for benign in [
        "max_tokens",
        "session_timeout",
        "secretary",
        "token_count",
        "author_name",
    ] {
        assert!(
            !is_sensitive_name(benign),
            "{benign:?} should NOT be sensitive"
        );
    }
}

#[test]
fn sensitive_text_values_are_redacted_across_common_formats() {
    let text =
        "Authorization: Bearer SECRET123\ntoken=abc api-key: def\ncookie: sid=ghi\nprivate_key pem";
    let (redacted, count) = redact_sensitive_text(text);

    for secret in ["Bearer SECRET123", "abc", "def", "sid=ghi", "pem"] {
        assert!(!redacted.contains(secret), "{secret} leaked in {redacted}");
    }
    assert_eq!(count, 5);
    assert!(redacted.contains("Authorization: [REDACTED:observed_text_secret]"));
    assert!(redacted.contains("token=[REDACTED:observed_text_secret]"));
    assert!(redacted.contains("api-key: [REDACTED:observed_text_secret]"));
}

#[test]
fn from_config_replaces_keywords_when_set() {
    let config = RedactionConfig {
        keywords: Some(vec!["sig".to_string()]),
        ..RedactionConfig::default()
    };
    let policy = RedactionPolicy::from_config(&config);
    let (redacted, paths) = policy.apply_persistence(&json!({
        "access_token": "should-survive",
        "mysig": "should-redact"
    }));
    assert_eq!(redacted["access_token"], json!("should-survive"));
    assert_eq!(paths, vec!["/mysig".to_string()]);
}

#[test]
fn from_config_extra_keywords_and_allowlist_compose() {
    let config = RedactionConfig {
        extra_keywords: vec!["service_key".to_string()],
        allowlist: vec!["access_token".to_string()],
        ..RedactionConfig::default()
    };
    let policy = RedactionPolicy::from_config(&config);
    let (redacted, paths) = policy.apply_persistence(&json!({
        "access_token": "survives",
        "client_secret": "redacted",
        "service_key": "sk"
    }));
    assert_eq!(redacted["access_token"], json!("survives"));
    let serialized = serde_json::to_string(&redacted).unwrap();
    assert!(!serialized.contains("redacted"));
    assert!(!serialized.contains("\"sk\""));
    assert_eq!(paths.len(), 2);
}

#[test]
fn redaction_config_round_trips() {
    let config = RedactionConfig {
        extra_keywords: vec!["service_key".to_string()],
        allowlist: vec!["max_tokens".to_string()],
        keywords: Some(vec!["token".to_string()]),
        scan_values: true,
        extra: serde_json::Map::new(),
    };
    let json_str = serde_json::to_string(&config).unwrap();
    let back: RedactionConfig = serde_json::from_str(&json_str).unwrap();
    assert_eq!(config, back);
    // Default deserializes from an empty object (backward compatible).
    let empty: RedactionConfig = serde_json::from_str("{}").unwrap();
    assert!(empty.extra_keywords.is_empty());
    assert!(empty.allowlist.is_empty());
    assert!(empty.keywords.is_none());
}

#[test]
fn default_policy_keeps_version_one() {
    // Pinned: the matcher was broadened (#74) but the version is intentionally
    // NOT bumped, so existing cursors/caches stay valid by design.
    assert_eq!(RedactionPolicy::default().version, 1);
    assert_eq!(
        RedactionPolicy::from_config(&RedactionConfig::default()).version,
        1
    );
}

#[test]
fn value_embedded_bearer_token_is_redacted_under_benign_key() {
    let payload = json!({
        "command": "curl -H 'Authorization: Bearer dGhpcyBpcyBhIHZlcnkgbG9uZyB0b2tlbg' https://svc"
    });
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    let serialized = serde_json::to_string(&redacted).unwrap();
    assert!(!serialized.contains("dGhpcyBpcyBhIHZlcnkgbG9uZyB0b2tlbg"));
    assert_eq!(paths, vec!["/command".to_string()]);
}

#[test]
fn value_embedded_pem_block_is_redacted() {
    let payload = json!({
        "config": "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEAZ9vXYb...long...\n-----END RSA PRIVATE KEY-----"
    });
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    let serialized = serde_json::to_string(&redacted).unwrap();
    assert!(!serialized.contains("MIIEpAIBAAKCAQEAZ9vXYb"));
    assert_eq!(paths, vec!["/config".to_string()]);
}

#[test]
fn value_embedded_jwt_is_redacted() {
    let jwt =
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    let payload = json!({"metadata": format!("token={jwt}")});
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    let serialized = serde_json::to_string(&redacted).unwrap();
    assert!(!serialized.contains("SflKxwRJSMeKKF2QT4fwpMeJf36POk6y"));
    assert_eq!(paths, vec!["/metadata".to_string()]);
}

#[test]
fn value_embedded_sensitive_url_param_is_redacted() {
    let payload = json!({
        "url": "https://api.example.com/data?api_key=sk-live-1234567890&state=open"
    });
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    let serialized = serde_json::to_string(&redacted).unwrap();
    assert!(!serialized.contains("sk-live-1234567890"));
    assert_eq!(paths, vec!["/url".to_string()]);
}

#[test]
fn benign_long_strings_are_not_redacted_by_value_scan() {
    let payload = json!({
        "description": "The bearer of this certificate is authorized to access the system.",
        "logline": "2026-07-06T12:00:00Z INFO request completed in 42ms",
        "paragraph": "This sentence has no secrets despite being reasonably long and wordy."
    });
    let (_redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    assert!(paths.is_empty(), "no value should be redacted: {paths:?}");
}

#[test]
fn bearer_followed_by_a_path_is_not_false_flagged() {
    // A filesystem path after "bearer" must not be treated as a bearer token:
    // the token class excludes path separators. The value is preserved.
    let payload = json!({"command": "bearer /usr/local/bin/verylongtoolname here"});
    let (redacted, paths) = RedactionPolicy::default().apply_persistence(&payload);
    assert!(
        paths.is_empty(),
        "a bearer-prefixed path must not be redacted: {paths:?}"
    );
    assert_eq!(
        redacted["command"],
        json!("bearer /usr/local/bin/verylongtoolname here")
    );
}

#[test]
fn value_scan_can_be_disabled_via_config() {
    let config = RedactionConfig {
        scan_values: false,
        ..RedactionConfig::default()
    };
    let policy = RedactionPolicy::from_config(&config);
    let payload = json!({
        "url": "https://x.example.com/data?api_key=sk-live-1234567890"
    });
    let (redacted, paths) = policy.apply_persistence(&payload);
    assert!(paths.is_empty());
    assert_eq!(
        redacted["url"],
        json!("https://x.example.com/data?api_key=sk-live-1234567890")
    );
}

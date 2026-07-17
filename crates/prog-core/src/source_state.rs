use chrono::DateTime;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    CoreError, Extra, Result, SOURCE_STATE_SCHEMA, SourceStateKind, SourceStateToken,
    SourceValidity, canonical_json,
};

const MAX_VALIDATOR_BYTES: usize = 512;

/// Produce one scoped state token from response validators. Strong and weak
/// ETags are both protocol-valid; ETag takes precedence because a conditional
/// request can use it without relying on server clock semantics.
pub fn http_source_state(
    source_id: &str,
    operation: &str,
    invocation: &Value,
    headers: &std::collections::BTreeMap<String, String>,
    captured_at: &str,
) -> Result<Option<SourceStateToken>> {
    let subject_scope = Some(invocation_scope(invocation)?);
    if let Some(etag) = headers.get("etag") {
        validate_http_validator("etag", etag)?;
        return Ok(Some(SourceStateToken {
            schema: SOURCE_STATE_SCHEMA.to_string(),
            kind: SourceStateKind::HttpEtag,
            value: etag.clone(),
            source_id: source_id.to_string(),
            operation: operation.to_string(),
            subject_scope,
            captured_at: captured_at.to_string(),
            validity: SourceValidity::Unknown,
            expires_at: None,
            provider: Some("http".to_string()),
            extra: Extra::new(),
        }));
    }
    if let Some(last_modified) = headers.get("last-modified") {
        validate_http_last_modified(last_modified)?;
        return Ok(Some(SourceStateToken {
            schema: SOURCE_STATE_SCHEMA.to_string(),
            kind: SourceStateKind::HttpLastModified,
            value: last_modified.clone(),
            source_id: source_id.to_string(),
            operation: operation.to_string(),
            subject_scope,
            captured_at: captured_at.to_string(),
            validity: SourceValidity::Unknown,
            expires_at: None,
            provider: Some("http".to_string()),
            extra: Extra::new(),
        }));
    }
    Ok(None)
}

/// Store provider-specific opaque state only as a digest. This is appropriate
/// for change tokens and MCP annotations that could contain tenant or secret
/// material and therefore cannot be emitted as public evidence.
pub fn opaque_source_state(
    kind: SourceStateKind,
    value: &str,
    source_id: &str,
    operation: &str,
    invocation: &Value,
    captured_at: &str,
    provider: &str,
) -> Result<SourceStateToken> {
    validate_opaque_token(value)?;
    Ok(SourceStateToken {
        schema: SOURCE_STATE_SCHEMA.to_string(),
        kind,
        value: format!("sha256:{}", hex_sha256(value.as_bytes())),
        source_id: source_id.to_string(),
        operation: operation.to_string(),
        subject_scope: Some(invocation_scope(invocation)?),
        captured_at: captured_at.to_string(),
        validity: SourceValidity::Unknown,
        expires_at: None,
        provider: Some(provider.to_string()),
        extra: Extra::new(),
    })
}

pub fn invocation_scope(invocation: &Value) -> Result<String> {
    Ok(format!(
        "sha256:{}",
        hex_sha256(&canonical_json(invocation)?)
    ))
}

pub fn validate_http_validator(name: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_VALIDATOR_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(CoreError::BadArgs {
            operation: "source state".to_string(),
            reason: format!("invalid HTTP {name} validator"),
        });
    }
    if name == "etag" && !(value.starts_with('"') || value.starts_with("W/\"")) {
        return Err(CoreError::BadArgs {
            operation: "source state".to_string(),
            reason: "HTTP ETag must be quoted or weak-quoted".to_string(),
        });
    }
    Ok(())
}

fn validate_http_last_modified(value: &str) -> Result<()> {
    validate_http_validator("last-modified", value)?;
    DateTime::parse_from_rfc2822(value).map_err(|_| CoreError::BadArgs {
        operation: "source state".to_string(),
        reason: "HTTP Last-Modified must be a valid RFC 2822 timestamp".to_string(),
    })?;
    Ok(())
}

fn validate_opaque_token(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_VALIDATOR_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(CoreError::BadArgs {
            operation: "source state".to_string(),
            reason: "opaque source-state token must be bounded printable text".to_string(),
        });
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    #[test]
    fn etag_is_scoped_and_preferred_over_last_modified() {
        let token = http_source_state(
            "source",
            "read",
            &json!({"id": 7}),
            &BTreeMap::from([
                ("etag".to_string(), "W/\"opaque\"".to_string()),
                (
                    "last-modified".to_string(),
                    "Mon, 13 Jul 2026 12:00:00 GMT".to_string(),
                ),
            ]),
            "2026-07-13T12:00:00Z",
        )
        .unwrap()
        .unwrap();
        assert_eq!(token.kind, SourceStateKind::HttpEtag);
        assert_eq!(token.value, "W/\"opaque\"");
        assert!(token.subject_scope.unwrap().starts_with("sha256:"));
    }

    #[test]
    fn invalid_and_secret_bearing_tokens_are_rejected_or_hashed() {
        assert!(validate_http_validator("etag", "plain").is_err());
        assert!(validate_http_validator("etag", "\"a\r\nb\"").is_err());
        assert!(
            http_source_state(
                "source",
                "read",
                &json!({}),
                &BTreeMap::from([("last-modified".to_string(), "not a date".to_string())]),
                "2026-07-13T12:00:00Z",
            )
            .is_err()
        );
        let opaque = opaque_source_state(
            SourceStateKind::ChangeToken,
            "customer-secret-token",
            "source",
            "read",
            &json!({}),
            "2026-07-13T12:00:00Z",
            "mcp",
        )
        .unwrap();
        assert!(
            !serde_json::to_string(&opaque)
                .unwrap()
                .contains("customer-secret-token")
        );
    }
}

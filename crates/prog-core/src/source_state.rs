use chrono::DateTime;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    CoreError, Extra, Result, SOURCE_STATE_SCHEMA, SourceStateKind, SourceStateSelector,
    SourceStateToken, SourceValidity, canonical_json,
};

const MAX_VALIDATOR_BYTES: usize = 512;
const MAX_SELECTOR_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStateCapture {
    pub token: Option<SourceStateToken>,
    pub validity: SourceValidity,
    pub warnings: Vec<String>,
}

impl Default for SourceStateCapture {
    fn default() -> Self {
        Self {
            token: None,
            validity: SourceValidity::Unknown,
            warnings: Vec::new(),
        }
    }
}

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

/// Capture HTTP state without allowing an invalid server-supplied validator to
/// discard the response that was already received. Invalid validators remain
/// explicit uncertainty and are never sent on a later conditional request.
pub fn capture_http_source_state(
    source_id: &str,
    operation: &str,
    invocation: &Value,
    headers: &std::collections::BTreeMap<String, String>,
    captured_at: &str,
) -> SourceStateCapture {
    match http_source_state(source_id, operation, invocation, headers, captured_at) {
        Ok(token) => SourceStateCapture {
            token,
            ..SourceStateCapture::default()
        },
        Err(_) => SourceStateCapture {
            validity: SourceValidity::ValidatorUnavailable,
            warnings: vec![
                "source supplied an invalid HTTP validator; it was not persisted or reused"
                    .to_string(),
            ],
            ..SourceStateCapture::default()
        },
    }
}

pub fn validate_source_state_selector(selector: &SourceStateSelector) -> Result<()> {
    validate_selector_pointer(&selector.path)?;
    if let Some(path) = &selector.expires_at_path {
        validate_selector_pointer(path)?;
    }
    Ok(())
}

/// Extract a declared scalar change token from raw response data and hash it
/// immediately. Missing, redacted, non-scalar, oversized, or expired evidence
/// degrades validity but never exposes the selected value.
pub fn capture_profile_source_state(
    selector: &SourceStateSelector,
    payload: &Value,
    source_id: &str,
    operation: &str,
    invocation: &Value,
    captured_at: &str,
) -> Result<SourceStateCapture> {
    validate_source_state_selector(selector)?;
    let Some(value) = crate::pointer::get(payload, &selector.path)? else {
        return Ok(unavailable_selector_capture(
            "declared source-state selector did not resolve to a value",
        ));
    };
    let Some(value) = scalar_token(value) else {
        return Ok(unavailable_selector_capture(
            "declared source-state selector did not resolve to a bounded non-redacted scalar",
        ));
    };
    let mut token = match opaque_source_state(
        SourceStateKind::ChangeToken,
        &value,
        source_id,
        operation,
        invocation,
        captured_at,
        "profile_selector",
    ) {
        Ok(token) => token,
        Err(_) => {
            return Ok(unavailable_selector_capture(
                "declared source-state value was malformed or exceeded its bound",
            ));
        }
    };

    let mut validity = SourceValidity::Unknown;
    let mut warnings = Vec::new();
    if let Some(expiry_path) = &selector.expires_at_path {
        let expiry = crate::pointer::get(payload, expiry_path)?;
        let Some(expiry) = expiry.and_then(Value::as_str) else {
            validity = SourceValidity::ValidatorUnavailable;
            warnings.push(
                "declared source-state expiry selector did not resolve to an RFC 3339 string"
                    .to_string(),
            );
            token.validity = validity;
            return Ok(SourceStateCapture {
                token: Some(token),
                validity,
                warnings,
            });
        };
        let Ok(expiry_time) = DateTime::parse_from_rfc3339(expiry) else {
            validity = SourceValidity::ValidatorUnavailable;
            warnings.push("declared source-state expiry was not valid RFC 3339".to_string());
            token.validity = validity;
            return Ok(SourceStateCapture {
                token: Some(token),
                validity,
                warnings,
            });
        };
        token.expires_at = Some(expiry.to_string());
        if DateTime::parse_from_rfc3339(captured_at)
            .map(|capture_time| expiry_time <= capture_time)
            .unwrap_or(true)
        {
            validity = SourceValidity::ValidatorExpired;
            warnings.push("declared source-state token was already expired at capture".to_string());
        }
    }
    token.validity = validity;
    Ok(SourceStateCapture {
        token: Some(token),
        validity,
        warnings,
    })
}

/// Preserve the protocol-defined MCP `annotations.lastModified` signal as a
/// hashed modification token. Arbitrary `_meta` never reaches this function.
pub fn capture_mcp_source_state(
    payload: &Value,
    source_id: &str,
    operation: &str,
    invocation: &Value,
    captured_at: &str,
) -> Result<SourceStateCapture> {
    let Some(content) = payload.get("_prog_mcp_content") else {
        return Ok(SourceStateCapture::default());
    };
    let Some(blocks) = content.as_array() else {
        return Ok(unavailable_selector_capture(
            "MCP content metadata was bounded before a modification annotation could be proven",
        ));
    };
    let mut values = blocks
        .iter()
        .filter_map(|block| {
            block
                .get("annotations")
                .and_then(|annotations| annotations.get("lastModified"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    let Some(value) = values.first() else {
        return Ok(SourceStateCapture::default());
    };
    if values.len() != 1 || DateTime::parse_from_rfc3339(value).is_err() {
        return Ok(unavailable_selector_capture(
            "MCP supplied ambiguous or malformed modification annotations",
        ));
    }
    let mut token = opaque_source_state(
        SourceStateKind::McpModification,
        value,
        source_id,
        operation,
        invocation,
        captured_at,
        "mcp",
    )?;
    token.extra.insert(
        "annotation".to_string(),
        Value::String("lastModified".to_string()),
    );
    Ok(SourceStateCapture {
        token: Some(token),
        ..SourceStateCapture::default()
    })
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

fn validate_selector_pointer(path: &str) -> Result<()> {
    let valid_escape = |bytes: &[u8]| {
        let mut index = 0usize;
        while index < bytes.len() {
            if bytes[index] == b'~' {
                index += 1;
                if index >= bytes.len() || !matches!(bytes[index], b'0' | b'1') {
                    return false;
                }
            }
            index += 1;
        }
        true
    };
    if path.is_empty()
        || path.len() > MAX_SELECTOR_BYTES
        || !path.starts_with('/')
        || path.contains('*')
        || !valid_escape(path.as_bytes())
    {
        return Err(CoreError::BadArgs {
            operation: "source state selector".to_string(),
            reason: "selector must be one bounded exact RFC 6901 pointer without wildcards"
                .to_string(),
        });
    }
    crate::pointer::parse(path)?;
    Ok(())
}

fn scalar_token(value: &Value) -> Option<String> {
    let value = match value {
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Null | Value::Array(_) | Value::Object(_) => return None,
    };
    (!value.contains("[REDACTED:")
        && !value.contains('\u{00ab}')
        && !value.contains('\u{00bb}')
        && value.len() <= MAX_VALIDATOR_BYTES)
        .then_some(value)
}

fn unavailable_selector_capture(warning: &str) -> SourceStateCapture {
    SourceStateCapture {
        validity: SourceValidity::ValidatorUnavailable,
        warnings: vec![warning.to_string()],
        ..SourceStateCapture::default()
    }
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

    #[test]
    fn profile_selector_is_exact_escaping_aware_hashed_and_expiry_aware() {
        let selector = SourceStateSelector {
            path: "/meta/a~1b".to_string(),
            expires_at_path: Some("/meta/expires".to_string()),
        };
        let capture = capture_profile_source_state(
            &selector,
            &json!({
                "meta": {
                    "a/b": "customer-secret-change-token",
                    "expires": "2026-07-13T11:59:59Z"
                }
            }),
            "source",
            "read",
            &json!({"id": 7}),
            "2026-07-13T12:00:00Z",
        )
        .unwrap();
        let token = capture.token.unwrap();
        assert_eq!(token.kind, SourceStateKind::ChangeToken);
        assert!(token.value.starts_with("sha256:"));
        assert!(!token.value.contains("customer-secret"));
        assert_eq!(token.validity, SourceValidity::ValidatorExpired);
        assert_eq!(capture.validity, SourceValidity::ValidatorExpired);
    }

    #[test]
    fn selector_missing_malformed_redacted_and_ambiguous_forms_fail_closed() {
        for payload in [
            json!({}),
            json!({"version": {"nested": 1}}),
            json!({"version": "[REDACTED:value]"}),
            json!({"version": null}),
        ] {
            let capture = capture_profile_source_state(
                &SourceStateSelector {
                    path: "/version".to_string(),
                    expires_at_path: None,
                },
                &payload,
                "source",
                "read",
                &json!({}),
                "2026-07-13T12:00:00Z",
            )
            .unwrap();
            assert!(capture.token.is_none());
            assert_eq!(capture.validity, SourceValidity::ValidatorUnavailable);
        }
        for path in ["version", "/items/*/version", "/bad~2escape", ""] {
            assert!(
                validate_source_state_selector(&SourceStateSelector {
                    path: path.to_string(),
                    expires_at_path: None,
                })
                .is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn mcp_modification_annotation_is_hashed_and_ambiguity_is_not_state() {
        let payload = json!({
            "_prog_mcp_content": [{
                "type": "resource_link",
                "annotations": {"lastModified": "2026-07-17T00:00:00Z"}
            }]
        });
        let capture = capture_mcp_source_state(
            &payload,
            "source",
            "read",
            &json!({}),
            "2026-07-17T00:01:00Z",
        )
        .unwrap();
        let token = capture.token.unwrap();
        assert_eq!(token.kind, SourceStateKind::McpModification);
        assert!(token.value.starts_with("sha256:"));
        assert!(!token.value.contains("2026-07-17"));

        let absent = capture_mcp_source_state(
            &json!({"_prog_mcp_content": [{"type": "text"}]}),
            "source",
            "read",
            &json!({}),
            "2026-07-17T00:01:00Z",
        )
        .unwrap();
        assert!(absent.token.is_none());
        assert_eq!(absent.validity, SourceValidity::Unknown);

        let ambiguous = capture_mcp_source_state(
            &json!({"_prog_mcp_content": [
                {"annotations": {"lastModified": "2026-07-17T00:00:00Z"}},
                {"annotations": {"lastModified": "2026-07-18T00:00:00Z"}}
            ]}),
            "source",
            "read",
            &json!({}),
            "2026-07-17T00:01:00Z",
        )
        .unwrap();
        assert!(ambiguous.token.is_none());
        assert_eq!(ambiguous.validity, SourceValidity::ValidatorUnavailable);
    }

    #[test]
    fn invalid_http_validator_is_uncertainty_not_a_capture_error() {
        let capture = capture_http_source_state(
            "source",
            "read",
            &json!({}),
            &BTreeMap::from([("etag".to_string(), "unquoted".to_string())]),
            "2026-07-13T12:00:00Z",
        );
        assert!(capture.token.is_none());
        assert_eq!(capture.validity, SourceValidity::ValidatorUnavailable);
    }
}

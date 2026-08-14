//! Deterministic evaluation for read-back verification of externally executed mutations.
//!
//! This module never executes an operation. It only validates exact paths,
//! fingerprints bounded scalar identity/version values, and compares a fresh
//! persisted read-back with a user-authored [`ActionIntent`](crate::ActionIntent).

use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ActionIntent, CoreError, ExpectedStateChange, ObservationRecord, ReadbackCheck,
    ReadbackVerificationStatus, Result, SourceValidity, canonical_json,
};

const MAX_POINTER_BYTES: usize = 512;
const MAX_FINGERPRINT_VALUE_BYTES: usize = 2_048;
const MAX_EXPECTED_CHANGES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadbackEvaluation {
    pub status: ReadbackVerificationStatus,
    pub checks: Vec<ReadbackCheck>,
    pub reasons: Vec<String>,
}

pub fn validate_readback_pointer(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > MAX_POINTER_BYTES || path.contains('*') {
        return Err(bad_readback(
            "paths must be non-root, bounded, exact RFC 6901 pointers without wildcards",
        ));
    }
    let Some(rest) = path.strip_prefix('/') else {
        return Err(CoreError::BadPointer(path.to_string()));
    };
    for segment in rest.split('/') {
        let bytes = segment.as_bytes();
        let mut index = 0usize;
        while index < bytes.len() {
            if bytes[index] == b'~' {
                let Some(next) = bytes.get(index + 1) else {
                    return Err(CoreError::BadPointer(path.to_string()));
                };
                if !matches!(next, b'0' | b'1') {
                    return Err(CoreError::BadPointer(path.to_string()));
                }
                index += 2;
            } else {
                index += 1;
            }
        }
    }
    crate::pointer::parse(path).map(|_| ())
}

pub fn fingerprint_readback_scalar(payload: &Value, path: &str) -> Result<String> {
    validate_readback_pointer(path)?;
    let value =
        crate::pointer::get(payload, path)?.ok_or_else(|| bad_readback("path is missing"))?;
    if !matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    ) || contains_redaction(value)
    {
        return Err(bad_readback(
            "identity and version paths must resolve to non-redacted scalar values",
        ));
    }
    let bytes = canonical_json(value)?;
    if bytes.len() > MAX_FINGERPRINT_VALUE_BYTES {
        return Err(bad_readback(
            "identity or version value exceeds the 2048-byte verification bound",
        ));
    }
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

pub fn validate_expected_changes(
    pre_payload: &Value,
    changes: &[ExpectedStateChange],
) -> Result<()> {
    if changes.is_empty() || changes.len() > MAX_EXPECTED_CHANGES {
        return Err(bad_readback(
            "expected changes must contain between 1 and 64 exact path mappings",
        ));
    }
    let mut paths = std::collections::BTreeSet::new();
    let mut differs = false;
    for change in changes {
        validate_readback_pointer(&change.path)?;
        if !paths.insert(change.path.as_str()) {
            return Err(bad_readback("expected change paths must be unique"));
        }
        if contains_redaction(&change.expected) {
            return Err(bad_readback(
                "expected values must not contain redaction markers",
            ));
        }
        let expected_bytes = canonical_json(&change.expected)?;
        if expected_bytes.len() > MAX_FINGERPRINT_VALUE_BYTES {
            return Err(bad_readback(
                "an expected value exceeds the 2048-byte verification bound",
            ));
        }
        differs |= crate::pointer::get(pre_payload, &change.path)? != Some(&change.expected);
    }
    if !differs {
        return Err(bad_readback(
            "at least one expected value must differ from the pre-mutation observation",
        ));
    }
    Ok(())
}

pub fn evaluate_readback(
    intent: &ActionIntent,
    readback_record: &ObservationRecord,
    readback_payload: &Value,
    mutation_response: Option<(&ObservationRecord, &Value)>,
    now: DateTime<Utc>,
) -> ReadbackEvaluation {
    let mut checks = Vec::new();
    let mut reasons = Vec::new();

    if readback_record.availability != crate::EvidenceAvailability::Recoverable {
        return unverifiable("the read-back payload is unavailable");
    }
    if readback_record.source_id != intent.source_id
        || readback_record.operation != intent.read_operation
    {
        return unverifiable("the read-back does not represent the declared source operation");
    }
    if matches!(
        readback_record.source_validity,
        SourceValidity::StaleByTtl
            | SourceValidity::ValidatorExpired
            | SourceValidity::RefreshFailed
    ) {
        return unverifiable("the read-back source-state evidence is stale or refresh failed");
    }
    if let Some(source_state) = &readback_record.source_state
        && (matches!(
            source_state.validity,
            SourceValidity::StaleByTtl
                | SourceValidity::ValidatorExpired
                | SourceValidity::RefreshFailed
        ) || source_state
            .expires_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|expires_at| expires_at <= now))
    {
        return unverifiable("the read-back source-state validator is expired or stale");
    }

    let identity = match fingerprint_readback_scalar(readback_payload, &intent.identity_path) {
        Ok(value) => value,
        Err(_) => return unverifiable("the read-back identity is missing, redacted, or unbounded"),
    };
    if identity != intent.pre_identity_fingerprint {
        return unverifiable("the read-back identity does not match the pre-mutation entity");
    }
    checks.push(ReadbackCheck {
        path: intent.identity_path.clone(),
        matched: true,
    });

    let version = match fingerprint_readback_scalar(readback_payload, &intent.version_path) {
        Ok(value) => value,
        Err(_) => return unverifiable("the read-back version is missing, redacted, or unbounded"),
    };

    if let Some((response_record, response_payload)) = mutation_response {
        if response_record
            .status
            .as_deref()
            .is_some_and(is_precondition_failure)
        {
            return stale_precondition("the mutation response reported a stale precondition");
        }
        if let Ok(response_identity) =
            fingerprint_readback_scalar(response_payload, &intent.identity_path)
            && response_identity != intent.pre_identity_fingerprint
        {
            return unverifiable("the mutation response refers to a different entity");
        }
        if let Ok(response_version) =
            fingerprint_readback_scalar(response_payload, &intent.version_path)
            && response_version != version
        {
            return stale_precondition(
                "the mutation response version differs from the independent read-back",
            );
        }
    }

    let mut all_match = true;
    for change in &intent.expected_changes {
        let matched = match crate::pointer::get(readback_payload, &change.path) {
            Ok(Some(actual)) if !contains_redaction(actual) => actual == &change.expected,
            _ => {
                return unverifiable(
                    "an expected path is missing or redacted in the independent read-back",
                );
            }
        };
        all_match &= matched;
        checks.push(ReadbackCheck {
            path: change.path.clone(),
            matched,
        });
    }

    if !all_match {
        if intent
            .eventual_consistency_until
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|deadline| deadline > now)
        {
            reasons.push(
                "expected state is not visible yet; the declared consistency window remains open"
                    .to_string(),
            );
            return ReadbackEvaluation {
                status: ReadbackVerificationStatus::Pending,
                checks,
                reasons,
            };
        }
        reasons.push("the independent read-back does not match every expected value".to_string());
        return ReadbackEvaluation {
            status: ReadbackVerificationStatus::Mismatched,
            checks,
            reasons,
        };
    }

    if version == intent.pre_version_fingerprint {
        return stale_precondition(
            "the expected state is present but the entity version did not advance",
        );
    }

    reasons.push(
        "the independent read-back matches the declared entity and expected state at a new version"
            .to_string(),
    );
    ReadbackEvaluation {
        status: ReadbackVerificationStatus::Verified,
        checks,
        reasons,
    }
}

fn is_precondition_failure(status: &str) -> bool {
    status == "409" || status == "412" || status.starts_with("409 ") || status.starts_with("412 ")
}

fn contains_redaction(value: &Value) -> bool {
    match value {
        Value::String(value) => {
            value.contains("[REDACTED:") || value.contains('\u{00ab}') && value.contains("redacted")
        }
        Value::Array(values) => values.iter().any(contains_redaction),
        Value::Object(map) => map.values().any(contains_redaction),
        _ => false,
    }
}

fn unverifiable(reason: &str) -> ReadbackEvaluation {
    ReadbackEvaluation {
        status: ReadbackVerificationStatus::Unverifiable,
        checks: Vec::new(),
        reasons: vec![reason.to_string()],
    }
}

fn stale_precondition(reason: &str) -> ReadbackEvaluation {
    ReadbackEvaluation {
        status: ReadbackVerificationStatus::StalePrecondition,
        checks: Vec::new(),
        reasons: vec![reason.to_string()],
    }
}

fn bad_readback(reason: &str) -> CoreError {
    CoreError::BadArgs {
        operation: "read-back verification".to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ACTION_INTENT_SCHEMA, CaptureCompleteness, EvidenceAvailability, Extra, ObservationLineage,
        SelectionCoverage,
    };
    use serde_json::json;

    fn intent() -> ActionIntent {
        let pre = json!({"id": "e-1", "version": 1, "state": "old"});
        ActionIntent {
            schema: ACTION_INTENT_SCHEMA.to_string(),
            intent_id: "intent-1".to_string(),
            session_id: "session-1".to_string(),
            source_id: "api".to_string(),
            read_operation: "get".to_string(),
            read_args: json!({"id": "e-1"}),
            pre_observation_id: "pre".to_string(),
            identity_path: "/id".to_string(),
            version_path: "/version".to_string(),
            pre_identity_fingerprint: fingerprint_readback_scalar(&pre, "/id").unwrap(),
            pre_version_fingerprint: fingerprint_readback_scalar(&pre, "/version").unwrap(),
            expected_changes: vec![ExpectedStateChange {
                path: "/state".to_string(),
                expected: json!("new"),
            }],
            eventual_consistency_until: None,
            obligation_id: "verify-intent-1".to_string(),
            created_at: "2026-08-11T00:00:00Z".to_string(),
            extra: Extra::new(),
        }
    }

    fn record() -> ObservationRecord {
        ObservationRecord {
            schema: crate::OBSERVATION_SCHEMA.to_string(),
            observation_id: "readback".to_string(),
            payload_hash: "hash".to_string(),
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: "invocation".to_string(),
            source_id: "api".to_string(),
            operation: "get".to_string(),
            comparison_family: None,
            selection: SelectionCoverage::default(),
            captured_at: "2026-08-11T00:00:00Z".to_string(),
            duration_ms: None,
            status: Some("200".to_string()),
            capture: CaptureCompleteness::complete(128),
            redacted: false,
            provider: Some("http".to_string()),
            parser: None,
            lens: None,
            workspace_state: None,
            source_state: None,
            source_validity: SourceValidity::Unknown,
            lineage: ObservationLineage::default(),
            provenance: None,
            cache_key: None,
            extra: Extra::new(),
        }
    }

    #[test]
    fn verifies_exact_identity_new_version_and_expected_state() {
        let result = evaluate_readback(
            &intent(),
            &record(),
            &json!({"id": "e-1", "version": 2, "state": "new"}),
            None,
            Utc::now(),
        );
        assert_eq!(result.status, ReadbackVerificationStatus::Verified);
    }

    #[test]
    fn mismatch_and_redacted_identity_fail_closed() {
        let mismatch = evaluate_readback(
            &intent(),
            &record(),
            &json!({"id": "e-1", "version": 2, "state": "other"}),
            None,
            Utc::now(),
        );
        assert_eq!(mismatch.status, ReadbackVerificationStatus::Mismatched);

        let redacted = evaluate_readback(
            &intent(),
            &record(),
            &json!({"id": "[REDACTED:id]", "version": 2, "state": "new"}),
            None,
            Utc::now(),
        );
        assert_eq!(redacted.status, ReadbackVerificationStatus::Unverifiable);
    }

    #[test]
    fn conflict_and_concurrent_version_are_stale_preconditions() {
        let mut conflict = record();
        conflict.status = Some("412".to_string());
        let result = evaluate_readback(
            &intent(),
            &record(),
            &json!({"id": "e-1", "version": 2, "state": "new"}),
            Some((&conflict, &json!({}))),
            Utc::now(),
        );
        assert_eq!(result.status, ReadbackVerificationStatus::StalePrecondition);

        let result = evaluate_readback(
            &intent(),
            &record(),
            &json!({"id": "e-1", "version": 3, "state": "new"}),
            Some((&record(), &json!({"id": "e-1", "version": 2}))),
            Utc::now(),
        );
        assert_eq!(result.status, ReadbackVerificationStatus::StalePrecondition);
    }

    #[test]
    fn eventual_mismatch_is_pending_only_before_deadline() {
        let mut action = intent();
        action.eventual_consistency_until = Some("2099-01-01T00:00:00Z".to_string());
        let result = evaluate_readback(
            &action,
            &record(),
            &json!({"id": "e-1", "version": 2, "state": "old"}),
            None,
            Utc::now(),
        );
        assert_eq!(result.status, ReadbackVerificationStatus::Pending);
    }

    #[test]
    fn expired_validator_is_unverifiable() {
        let mut observation = record();
        observation.source_validity = SourceValidity::ValidatorExpired;
        let result = evaluate_readback(
            &intent(),
            &observation,
            &json!({"id": "e-1", "version": 2, "state": "new"}),
            None,
            Utc::now(),
        );
        assert_eq!(result.status, ReadbackVerificationStatus::Unverifiable);
    }

    #[test]
    fn invalid_or_noop_intents_are_rejected() {
        let pre = json!({"state": "old"});
        let no_change = vec![ExpectedStateChange {
            path: "/state".to_string(),
            expected: json!("old"),
        }];
        assert!(validate_expected_changes(&pre, &no_change).is_err());
        assert!(validate_readback_pointer("/bad~2path").is_err());
        assert!(validate_readback_pointer("/items/*/id").is_err());
    }
}

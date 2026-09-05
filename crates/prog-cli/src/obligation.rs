//! Verification-obligation evaluation, split from `main.rs` as part of #183.
//!
//! Readiness is evaluated from currently available evidence; immutable receipts
//! retain their historical status even after supporting payloads are evicted.

use crate::commands::delta::compare_observation_ids;
use serde_json::Value;

use prog_core::{
    Extra, ObligationEvaluation, ReadbackVerificationStatus, Result, Store, VerificationObligation,
    VerificationOperation, VerificationStateRelationship, VerificationStatus,
};

pub(crate) fn evaluate_obligation(
    store: &Store,
    obligation: VerificationObligation,
) -> Result<ObligationEvaluation> {
    if let Some(receipt_id) = obligation
        .extra
        .get("readback_receipt_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    {
        let Some(receipt) = store.get_readback_receipt(&receipt_id)? else {
            return Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Unverifiable,
                vec![format!("read-back receipt '{receipt_id}' is unavailable")],
                None,
            ));
        };
        if receipt.obligation_id != obligation.id {
            return Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Unverifiable,
                vec!["the read-back receipt names a different obligation".to_string()],
                receipt.assessment,
            ));
        }
        if receipt.status == ReadbackVerificationStatus::Verified
            && let Some(reason) = readback_evidence_unavailable(store, &obligation, &receipt)?
        {
            return Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Unverifiable,
                vec![reason],
                receipt.assessment,
            ));
        }
        let status = match receipt.status {
            ReadbackVerificationStatus::Verified => VerificationStatus::Passed,
            ReadbackVerificationStatus::Mismatched => VerificationStatus::Failed,
            ReadbackVerificationStatus::StalePrecondition => VerificationStatus::Stale,
            ReadbackVerificationStatus::Pending => VerificationStatus::Pending,
            ReadbackVerificationStatus::ReadbackFailed
            | ReadbackVerificationStatus::Unverifiable => VerificationStatus::Unverifiable,
        };
        return Ok(obligation_evaluation(
            obligation,
            status,
            receipt.reasons,
            receipt.assessment,
        ));
    }
    let Some(evidence_id) = obligation.evidence_observation_id.clone() else {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Pending,
            vec!["no evidence observation has been attached".to_string()],
            None,
        ));
    };
    let Some(evidence) = store.get_observation(&evidence_id)? else {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Unverifiable,
            vec![format!(
                "evidence observation '{evidence_id}' is unavailable"
            )],
            None,
        ));
    };
    if evidence.availability != prog_core::EvidenceAvailability::Recoverable {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Unverifiable,
            vec!["the evidence payload is no longer available".to_string()],
            None,
        ));
    }
    if !evidence.capture.can_prove_absence {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Unverifiable,
            vec!["the evidence observation is incomplete or truncated".to_string()],
            None,
        ));
    }
    let requires_workspace = matches!(
        obligation.required_state,
        VerificationStateRelationship::WorkspaceUnchanged
            | VerificationStateRelationship::WorkspaceAndSourceUnchanged
    );
    if requires_workspace && evidence.workspace_state.is_none() {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Unverifiable,
            vec![
                "the obligation requires workspace-state evidence, but none was captured"
                    .to_string(),
            ],
            None,
        ));
    }
    if let Some(captured_workspace) = &evidence.workspace_state {
        let current_workspace = captured_workspace
            .root
            .as_deref()
            .map(prog_core::capture_workspace)
            .unwrap_or_else(|| prog_core::capture_workspace("."));
        let comparison = prog_core::compare_workspace(captured_workspace, &current_workspace);
        if comparison.validity != prog_core::WorkspaceValidity::Unchanged
            && (requires_workspace
                || obligation.required_state == VerificationStateRelationship::Any)
        {
            return Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Stale,
                comparison.reasons,
                None,
            ));
        }
    }
    let requires_source = matches!(
        obligation.required_state,
        VerificationStateRelationship::SourceUnchanged
            | VerificationStateRelationship::WorkspaceAndSourceUnchanged
    );
    if requires_source && evidence.source_validity != prog_core::SourceValidity::ConfirmedUnchanged
    {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Stale,
            vec!["the obligation requires source state confirmed unchanged".to_string()],
            None,
        ));
    }
    if let Some(expected_operation) = &obligation.expected_operation {
        let matches = match expected_operation {
            VerificationOperation::Argv(expected) => {
                evidence_argv(store, &evidence)?.is_some_and(|actual| actual == *expected)
            }
            VerificationOperation::SourceOperation(expected) => evidence.operation == *expected,
        };
        if !matches {
            return Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Stale,
                vec!["evidence does not match the obligation's declared operation".to_string()],
                None,
            ));
        }
    }
    if let Some(family) = obligation.comparison_family.as_deref()
        && evidence.comparison_family.as_deref() != Some(family)
    {
        return Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Stale,
            vec!["evidence does not match the declared comparison family".to_string()],
            None,
        ));
    }

    match (
        obligation.origin_observation_id.clone(),
        obligation.expected_absent_fingerprint.clone(),
    ) {
        (Some(origin_id), Some(expected_fingerprint)) => {
            let delta = compare_observation_ids(store, &origin_id, &evidence_id)?;
            let expected_status = delta
                .findings
                .iter()
                .find(|finding| finding.fingerprint == expected_fingerprint)
                .map(|finding| match finding.status {
                    prog_core::DeltaFindingStatus::Resolved => VerificationStatus::Passed,
                    prog_core::DeltaFindingStatus::Persisting => VerificationStatus::Persisting,
                    prog_core::DeltaFindingStatus::New => VerificationStatus::New,
                    prog_core::DeltaFindingStatus::NotObserved => VerificationStatus::NotObserved,
                    prog_core::DeltaFindingStatus::Unknown => VerificationStatus::Unknown,
                })
                .unwrap_or(VerificationStatus::Unknown);
            let new_regressions = delta
                .findings
                .iter()
                .filter(|finding| finding.status == prog_core::DeltaFindingStatus::New)
                .cloned()
                .collect::<Vec<_>>();
            let status =
                if expected_status == VerificationStatus::Passed && !new_regressions.is_empty() {
                    VerificationStatus::New
                } else {
                    expected_status
                };
            let reasons = match status {
                VerificationStatus::Passed => vec![
                    "the expected finding is absent under a comparable, complete observation"
                        .to_string(),
                ],
                VerificationStatus::Unknown => vec![
                    "the expected finding could not be evaluated from the comparable evidence"
                        .to_string(),
                ],
                VerificationStatus::New if !new_regressions.is_empty() => vec![
                    "the expected finding is absent, but comparable evidence contains new regression findings"
                        .to_string(),
                ],
                _ => delta
                    .findings
                    .iter()
                    .find(|finding| finding.fingerprint == expected_fingerprint)
                    .map(|finding| finding.reasons.clone())
                    .filter(|reasons| !reasons.is_empty())
                    .unwrap_or_else(|| delta.assessment.reasons.clone()),
            };
            let mut evaluation =
                obligation_evaluation(obligation, status, reasons, Some(delta.assessment));
            if !new_regressions.is_empty() {
                evaluation.extra.insert(
                    "new_regressions".to_string(),
                    serde_json::to_value(new_regressions)?,
                );
            }
            Ok(evaluation)
        }
        (None, None) => match command_success(store, &evidence)? {
            Some(true) => Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Passed,
                vec!["a complete command observation exited successfully".to_string()],
                None,
            )),
            Some(false) => Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Failed,
                vec!["the evidence command did not exit successfully".to_string()],
                None,
            )),
            None => Ok(obligation_evaluation(
                obligation,
                VerificationStatus::Unknown,
                vec![
                    "evidence has no explicit finding comparison or successful command result"
                        .to_string(),
                ],
                None,
            )),
        },
        _ => Ok(obligation_evaluation(
            obligation,
            VerificationStatus::Unknown,
            vec![
                "origin observation and expected finding fingerprint must be supplied together"
                    .to_string(),
            ],
            None,
        )),
    }
}

/// A historical exact-value verification is not a current availability proof.
/// Check its links and all supporting payloads offline, without imposing delta's
/// unrelated `can_prove_absence` requirement or rerunning the source.
fn readback_evidence_unavailable(
    store: &Store,
    obligation: &VerificationObligation,
    receipt: &prog_core::ReadbackVerificationReceipt,
) -> Result<Option<String>> {
    let Some(readback_id) = receipt.readback_observation_id.as_deref() else {
        return Ok(Some(
            "the verified receipt has no read-back evidence observation".to_string(),
        ));
    };
    if obligation.evidence_observation_id.as_deref() != Some(readback_id)
        || obligation
            .extra
            .get("action_intent_id")
            .and_then(Value::as_str)
            != Some(receipt.intent_id.as_str())
    {
        return Ok(Some(
            "the read-back receipt does not match the obligation's evidence or action intent"
                .to_string(),
        ));
    }
    let Some(intent) = store.get_action_intent(&receipt.intent_id)? else {
        return Ok(Some(
            "the read-back action intent is unavailable".to_string(),
        ));
    };
    if intent.session_id != obligation.session_id
        || intent.obligation_id != obligation.id
        || intent.pre_observation_id != receipt.pre_observation_id
    {
        return Ok(Some("the read-back action intent does not match this session, obligation, or pre-mutation evidence".to_string()));
    }
    for (role, observation_id) in [
        ("pre-mutation", Some(receipt.pre_observation_id.as_str())),
        ("read-back", Some(readback_id)),
        (
            "mutation-response",
            receipt.mutation_response_observation_id.as_deref(),
        ),
    ] {
        let Some(observation_id) = observation_id else {
            continue;
        };
        let Some(observation) = store.get_observation(observation_id)? else {
            return Ok(Some(format!(
                "the {role} evidence observation is unavailable"
            )));
        };
        if matches!(
            observation.availability,
            prog_core::EvidenceAvailability::Expired
                | prog_core::EvidenceAvailability::MetadataOnly
                | prog_core::EvidenceAvailability::Unavailable
        ) || store.get_payload(&observation.payload_hash)?.is_none()
        {
            return Ok(Some(format!(
                "the {role} evidence payload is no longer available"
            )));
        }
    }
    Ok(None)
}

fn command_success(
    store: &Store,
    observation: &prog_core::ObservationRecord,
) -> Result<Option<bool>> {
    let Some(payload) = store.get_payload(&observation.payload_hash)? else {
        return Ok(None);
    };
    Ok(payload
        .as_value()
        .pointer("/command/success")
        .and_then(Value::as_bool))
}

fn evidence_argv(
    store: &Store,
    observation: &prog_core::ObservationRecord,
) -> Result<Option<Vec<String>>> {
    let Some(payload) = store.get_payload(&observation.payload_hash)? else {
        return Ok(None);
    };
    Ok(payload
        .as_value()
        .pointer("/command/argv")
        .and_then(Value::as_array)
        .and_then(|argv| {
            argv.iter()
                .map(Value::as_str)
                .collect::<Option<Vec<_>>>()
                .map(|argv| argv.into_iter().map(ToOwned::to_owned).collect())
        }))
}

fn obligation_evaluation(
    obligation: VerificationObligation,
    status: VerificationStatus,
    reasons: Vec<String>,
    assessment: Option<prog_core::ComparabilityAssessment>,
) -> ObligationEvaluation {
    ObligationEvaluation {
        obligation,
        status,
        reasons,
        assessment,
        extra: Extra::new(),
    }
}

#[cfg(test)]
mod readback_tests {
    use super::*;
    use prog_core::{EvidenceAvailability, NewObservation, RawPayload, RedactionPolicy};
    use serde_json::json;

    fn observation(store: &Store, role: &str, fault: &str) -> String {
        if fault == "missing_record" {
            return format!("missing-{role}");
        }
        let payload_hash = if fault == "missing_payload" {
            format!("missing-payload-{role}")
        } else {
            store
                .put_payload(
                    &RawPayload::new(json!({"role": role}))
                        .redact(&RedactionPolicy::default())
                        .payload,
                )
                .unwrap()
        };
        store
            .record_observation(NewObservation {
                payload_hash,
                availability: if fault == "metadata_only" {
                    EvidenceAvailability::MetadataOnly
                } else {
                    EvidenceAvailability::Recoverable
                },
                invocation_fingerprint: role.to_string(),
                source_id: "entity".to_string(),
                operation: "get".to_string(),
                comparison_family: None,
                selection: Default::default(),
                captured_at: None,
                duration_ms: None,
                status: None,
                capture: Default::default(),
                redacted: false,
                provider: None,
                parser: None,
                lens: None,
                workspace_state: None,
                source_state: None,
                source_validity: prog_core::SourceValidity::Unknown,
                lineage: Default::default(),
                provenance: None,
                cache_key: None,
                extra: Extra::new(),
            })
            .unwrap()
            .observation_id
    }

    fn fixture(
        store: &Store,
        role: &str,
        fault: &str,
    ) -> (
        VerificationObligation,
        prog_core::ReadbackVerificationReceipt,
    ) {
        let evidence = |current_role| {
            observation(
                store,
                current_role,
                if role == current_role { fault } else { "" },
            )
        };
        let pre = evidence("pre-mutation");
        let readback = evidence("read-back");
        let mutation = evidence("mutation-response");
        let intent: prog_core::ActionIntent = serde_json::from_value(json!({
            "schema": prog_core::ACTION_INTENT_SCHEMA,
            "intent_id": "intent", "session_id": "session", "source_id": "entity",
            "read_operation": "get", "read_args": {}, "pre_observation_id": pre,
            "identity_path": "/id", "version_path": "/version",
            "pre_identity_fingerprint": "identity", "pre_version_fingerprint": "version",
            "obligation_id": "check", "created_at": "2026-09-05T00:00:00Z"
        }))
        .unwrap();
        store.put_action_intent(&intent).unwrap();
        let obligation: VerificationObligation = serde_json::from_value(json!({
            "schema": prog_core::VERIFICATION_SCHEMA, "id": "check", "session_id": "session",
            "required": true, "intended_check": "verify state", "required_scope": "entity",
            "evidence_observation_id": readback, "created_at": "2026-09-05T00:00:00Z",
            "readback_receipt_id": "receipt", "action_intent_id": "intent"
        }))
        .unwrap();
        let receipt: prog_core::ReadbackVerificationReceipt = serde_json::from_value(json!({
            "schema": prog_core::READBACK_VERIFICATION_SCHEMA, "receipt_id": "receipt",
            "intent_id": "intent", "status": "verified", "obligation_id": "check",
            "pre_observation_id": pre, "readback_observation_id": readback,
            "mutation_response_observation_id": mutation, "created_at": "2026-09-05T00:00:00Z"
        }))
        .unwrap();
        store.put_readback_receipt(&receipt).unwrap();
        (obligation, receipt)
    }

    #[test]
    fn each_supporting_observation_and_payload_must_remain_available() {
        for role in ["pre-mutation", "read-back", "mutation-response"] {
            for fault in ["missing_record", "missing_payload", "metadata_only"] {
                let dir = tempfile::tempdir().unwrap();
                let store = Store::open(dir.path()).unwrap();
                let (obligation, receipt) = fixture(&store, role, fault);
                let result = evaluate_obligation(&store, obligation).unwrap();
                assert_eq!(
                    result.status,
                    VerificationStatus::Unverifiable,
                    "{role}: {fault}"
                );
                assert!(result.reasons[0].contains(role), "{result:?}");
                assert_eq!(
                    store.get_readback_receipt("receipt").unwrap(),
                    Some(receipt)
                );
            }
        }
    }

    #[test]
    fn readback_availability_does_not_require_delta_absence_proof() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let (obligation, receipt) = fixture(&store, "", "");
        let observation = store
            .get_observation(receipt.readback_observation_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert!(!observation.capture.can_prove_absence);
        assert_eq!(
            evaluate_obligation(&store, obligation).unwrap().status,
            VerificationStatus::Passed
        );
    }

    #[test]
    fn receipt_failures_and_broken_links_never_pass() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let (obligation, original) = fixture(&store, "", "");
        for (status, expected) in [
            (
                ReadbackVerificationStatus::Mismatched,
                VerificationStatus::Failed,
            ),
            (
                ReadbackVerificationStatus::Pending,
                VerificationStatus::Pending,
            ),
            (
                ReadbackVerificationStatus::StalePrecondition,
                VerificationStatus::Stale,
            ),
            (
                ReadbackVerificationStatus::ReadbackFailed,
                VerificationStatus::Unverifiable,
            ),
            (
                ReadbackVerificationStatus::Unverifiable,
                VerificationStatus::Unverifiable,
            ),
        ] {
            let mut receipt = original.clone();
            receipt.receipt_id = format!("receipt-{status:?}");
            receipt.status = status;
            store.put_readback_receipt(&receipt).unwrap();
            let mut obligation = obligation.clone();
            obligation
                .extra
                .insert("readback_receipt_id".to_string(), json!(receipt.receipt_id));
            assert_eq!(
                evaluate_obligation(&store, obligation).unwrap().status,
                expected
            );
        }
        for fault in [
            "missing_receipt",
            "wrong_obligation",
            "wrong_session",
            "wrong_intent",
            "wrong_evidence",
            "missing_readback",
            "missing_intent",
            "wrong_pre",
        ] {
            let mut declaration = obligation.clone();
            let mut receipt = original.clone();
            receipt.receipt_id = format!("receipt-{fault}");
            declaration
                .extra
                .insert("readback_receipt_id".to_string(), json!(receipt.receipt_id));
            match fault {
                "wrong_obligation" => receipt.obligation_id = "other".to_string(),
                "wrong_session" => declaration.session_id = "other".to_string(),
                "wrong_intent" => {
                    declaration
                        .extra
                        .insert("action_intent_id".to_string(), json!("other"));
                }
                "wrong_evidence" => declaration.evidence_observation_id = Some("other".to_string()),
                "missing_readback" => receipt.readback_observation_id = None,
                "missing_intent" => {
                    receipt.intent_id = "missing".to_string();
                    declaration
                        .extra
                        .insert("action_intent_id".to_string(), json!("missing"));
                }
                "wrong_pre" => receipt.pre_observation_id = "other".to_string(),
                _ => {}
            }
            if fault != "missing_receipt" {
                store.put_readback_receipt(&receipt).unwrap();
            }
            assert_eq!(
                evaluate_obligation(&store, declaration).unwrap().status,
                VerificationStatus::Unverifiable,
                "{fault}"
            );
        }
        assert_eq!(
            store.get_readback_receipt("receipt").unwrap(),
            Some(original)
        );
    }
}

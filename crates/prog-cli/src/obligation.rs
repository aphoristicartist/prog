//! Verification-obligation evaluation, split from `main.rs` as part of #183.
//!
//! Move-only: `evaluate_obligation` is `pub(crate)` (consumed by the
//! verification/session command path); its helpers stay module-private.

use crate::commands::delta::compare_observation_ids;
use serde_json::Value;

use prog_core::{
    Extra, ObligationEvaluation, Result, Store, VerificationObligation, VerificationOperation,
    VerificationStateRelationship, VerificationStatus,
};

pub(crate) fn evaluate_obligation(
    store: &Store,
    obligation: VerificationObligation,
) -> Result<ObligationEvaluation> {
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
        && evidence.invocation_fingerprint != family
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

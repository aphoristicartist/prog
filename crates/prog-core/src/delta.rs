use std::collections::{BTreeMap, BTreeSet};

use crate::{
    ComparabilityAssessment, DeltaFinding, DeltaFindingStatus, EvidenceAvailability, Extra,
    Finding, OBSERVATION_DELTA_SCHEMA, ObservationDelta, ObservationRecord, ScopeRelationship,
    SourceStateKind, SubjectIdentity, WorkspaceValidity, compare_workspace,
};

const MAX_DELTA_FINDINGS: usize = 100;

pub fn compare_observations(
    baseline: &ObservationRecord,
    subject: &ObservationRecord,
    baseline_findings: &[Finding],
    subject_findings: &[Finding],
) -> ObservationDelta {
    let assessment = assess(baseline, subject);
    let baseline_by_fingerprint = findings_by_fingerprint(baseline_findings);
    let subject_by_fingerprint = findings_by_fingerprint(subject_findings);
    let all: BTreeSet<&String> = baseline_by_fingerprint
        .keys()
        .chain(subject_by_fingerprint.keys())
        .collect();
    let mut findings = Vec::new();
    for fingerprint in all {
        let baseline_finding = baseline_by_fingerprint.get(fingerprint);
        let subject_finding = subject_by_fingerprint.get(fingerprint);
        let status = match (baseline_finding, subject_finding) {
            (None, Some(_)) => DeltaFindingStatus::New,
            (Some(_), Some(_)) => DeltaFindingStatus::Persisting,
            (Some(_), None) if assessment.can_prove_absence => DeltaFindingStatus::Resolved,
            (Some(_), None) if assessment.subject_complete => DeltaFindingStatus::NotObserved,
            (Some(_), None) => DeltaFindingStatus::Unknown,
            (None, None) => unreachable!("union contains this fingerprint"),
        };
        let title = subject_finding
            .or(baseline_finding)
            .and_then(|finding| finding.title.clone())
            .map(|title| truncate(&title, 180));
        findings.push(DeltaFinding {
            status,
            fingerprint: fingerprint.clone(),
            title,
            baseline_path: baseline_finding.map(|finding| finding.path.clone()),
            subject_path: subject_finding.map(|finding| finding.path.clone()),
            reasons: if matches!(status, DeltaFindingStatus::Resolved) {
                vec!["absence is proven by the comparability assessment".to_string()]
            } else if matches!(
                status,
                DeltaFindingStatus::Unknown | DeltaFindingStatus::NotObserved
            ) {
                assessment.reasons.clone()
            } else {
                Vec::new()
            },
            extra: Extra::new(),
        });
    }
    findings.sort_by(|left, right| {
        delta_priority(left.status)
            .cmp(&delta_priority(right.status))
            .then_with(|| left.fingerprint.cmp(&right.fingerprint))
    });
    findings.truncate(MAX_DELTA_FINDINGS);
    let mut counts = BTreeMap::new();
    for finding in &findings {
        *counts
            .entry(format!("{:?}", finding.status).to_ascii_lowercase())
            .or_insert(0) += 1;
    }
    ObservationDelta {
        schema: OBSERVATION_DELTA_SCHEMA.to_string(),
        baseline_observation_id: baseline.observation_id.clone(),
        subject_observation_id: subject.observation_id.clone(),
        assessment,
        findings,
        counts,
        extra: Extra::new(),
    }
}

fn assess(baseline: &ObservationRecord, subject: &ObservationRecord) -> ComparabilityAssessment {
    let invocation_match = baseline.invocation_fingerprint == subject.invocation_fingerprint;
    let subject_identity =
        if baseline.source_id == subject.source_id && baseline.operation == subject.operation {
            SubjectIdentity::Same
        } else {
            SubjectIdentity::Different
        };
    let scope_relationship = if invocation_match {
        ScopeRelationship::Equal
    } else {
        ScopeRelationship::Unknown
    };
    let normalization_compatible = baseline.parser == subject.parser
        && baseline.lens == subject.lens
        && baseline.provider == subject.provider;
    let workspace_validity = match (&baseline.workspace_state, &subject.workspace_state) {
        (Some(baseline), Some(subject)) => match compare_workspace(baseline, subject).validity {
            WorkspaceValidity::Unchanged => "unchanged",
            WorkspaceValidity::Changed => "changed",
            WorkspaceValidity::NotApplicable => "not_applicable",
            WorkspaceValidity::Unknown => "unknown",
        },
        _ => "unknown",
    }
    .to_string();
    let source_validity = source_validity(baseline, subject);
    let payloads_available = baseline.availability == EvidenceAvailability::Recoverable
        && subject.availability == EvidenceAvailability::Recoverable;
    let mut reasons = Vec::new();
    if !invocation_match {
        reasons.push("canonical invocation fingerprints differ".to_string());
    }
    if !baseline.capture.can_prove_absence || !subject.capture.can_prove_absence {
        reasons.push("one or both observations are incomplete or truncated".to_string());
    }
    if !normalization_compatible {
        reasons.push("provider, parser, or lens identity differs".to_string());
    }
    if !matches!(source_validity.as_str(), "valid" | "not_required") {
        reasons.push("source state cannot prove comparable coverage".to_string());
    }
    if !payloads_available {
        reasons.push("one or both redacted payloads are no longer available".to_string());
    }
    let can_prove_absence = invocation_match
        && matches!(subject_identity, SubjectIdentity::Same)
        && baseline.capture.can_prove_absence
        && subject.capture.can_prove_absence
        && normalization_compatible
        && payloads_available
        && matches!(source_validity.as_str(), "valid" | "not_required");
    ComparabilityAssessment {
        subject_identity,
        scope_relationship,
        invocation_match,
        baseline_complete: baseline.capture.can_prove_absence,
        subject_complete: subject.capture.can_prove_absence,
        normalization_compatible,
        workspace_validity,
        source_validity,
        can_prove_absence,
        reasons,
        extra: Extra::new(),
    }
}

fn source_validity(baseline: &ObservationRecord, subject: &ObservationRecord) -> String {
    match (&baseline.source_state, &subject.source_state) {
        (None, None) => "not_required".to_string(),
        (Some(left), Some(right))
            if left.source_id == right.source_id
                && left.operation == right.operation
                && left.subject_scope == right.subject_scope
                && matches!(
                    left.kind,
                    SourceStateKind::HttpEtag | SourceStateKind::HttpLastModified
                )
                && matches!(
                    right.kind,
                    SourceStateKind::HttpEtag | SourceStateKind::HttpLastModified
                ) =>
        {
            "valid".to_string()
        }
        _ => "unknown".to_string(),
    }
}

fn findings_by_fingerprint(findings: &[Finding]) -> BTreeMap<String, &Finding> {
    findings
        .iter()
        .filter_map(|finding| {
            finding
                .fingerprint
                .as_ref()
                .map(|fingerprint| (fingerprint.clone(), finding))
        })
        .collect()
}

fn delta_priority(status: DeltaFindingStatus) -> u8 {
    match status {
        DeltaFindingStatus::New => 0,
        DeltaFindingStatus::Persisting => 1,
        DeltaFindingStatus::Resolved => 2,
        DeltaFindingStatus::NotObserved => 3,
        DeltaFindingStatus::Unknown => 4,
    }
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    format!(
        "{}...",
        value
            .chars()
            .take(limit.saturating_sub(3))
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        CaptureCompleteness, EvidenceAvailability, FindingCommandHints, ObservationLineage,
    };

    fn observation(id: &str, invocation: &str, complete: bool) -> ObservationRecord {
        ObservationRecord {
            schema: "prog.observation".to_string(),
            observation_id: id.to_string(),
            payload_hash: "sha256:x".to_string(),
            availability: EvidenceAvailability::Recoverable,
            invocation_fingerprint: invocation.to_string(),
            source_id: "run".to_string(),
            operation: "test".to_string(),
            subject_keys: Vec::new(),
            captured_at: "2026-07-13T12:00:00Z".to_string(),
            duration_ms: None,
            status: None,
            capture: if complete {
                CaptureCompleteness::complete(1)
            } else {
                CaptureCompleteness {
                    total_bytes: None,
                    captured_bytes: 1,
                    stored_bytes: 1,
                    stop_reason: crate::CaptureStopReason::ByteLimit,
                    budget: crate::CaptureBudget::default(),
                    affected: Vec::new(),
                    can_prove_absence: false,
                    extra: Extra::new(),
                }
            },
            redacted: false,
            provider: None,
            parser: None,
            lens: None,
            workspace_state: None,
            source_state: None,
            environment_state: None,
            lineage: ObservationLineage::default(),
            provenance: None,
            cache_key: None,
            extra: Extra::new(),
        }
    }
    fn finding(fingerprint: &str) -> Finding {
        Finding {
            occurrence_id: Some(format!("fi_{fingerprint}")),
            fingerprint: Some(fingerprint.to_string()),
            rank: 1,
            kind: "test_failure".to_string(),
            path: "/failures/0".to_string(),
            confidence: 1.0,
            reason: "failure".to_string(),
            title: Some(fingerprint.to_string()),
            severity: None,
            source: None,
            lens_id: None,
            evidence_ref: None,
            line_range: None,
            byte_range: None,
            primary_span: None,
            related_spans: Vec::new(),
            redaction_state: None,
            commands: FindingCommandHints::default(),
            extra: Extra::new(),
        }
    }

    #[test]
    fn comparable_delta_reports_exact_new_persisting_and_resolved_findings() {
        let delta = compare_observations(
            &observation("a", "same", true),
            &observation("b", "same", true),
            &[finding("old"), finding("persist")],
            &[finding("persist"), finding("new")],
        );
        assert!(delta.assessment.can_prove_absence);
        assert_eq!(
            delta.counts,
            BTreeMap::from([
                (String::from("new"), 1),
                (String::from("persisting"), 1),
                (String::from("resolved"), 1)
            ])
        );
    }

    #[test]
    fn incomplete_or_different_invocations_never_resolve() {
        let delta = compare_observations(
            &observation("a", "one", true),
            &observation("b", "two", false),
            &[finding("old")],
            &[],
        );
        assert!(!delta.assessment.can_prove_absence);
        assert_eq!(delta.findings[0].status, DeltaFindingStatus::Unknown);
        assert!(
            serde_json::to_string(&delta)
                .unwrap()
                .contains("canonical invocation")
        );
        let _ = json!(delta);
    }

    #[test]
    fn redacted_or_metadata_only_evidence_never_proves_absence() {
        for availability in [
            EvidenceAvailability::Redacted,
            EvidenceAvailability::CaptureTruncated,
            EvidenceAvailability::MetadataOnly,
            EvidenceAvailability::Expired,
            EvidenceAvailability::Unavailable,
        ] {
            let baseline = observation("a", "same", true);
            let mut subject = observation("b", "same", true);
            subject.availability = availability;
            subject.capture.can_prove_absence = false;
            let delta = compare_observations(&baseline, &subject, &[finding("old")], &[]);
            assert!(!delta.assessment.can_prove_absence, "{availability:?}");
            assert_eq!(delta.findings[0].status, DeltaFindingStatus::Unknown);
        }
    }
}

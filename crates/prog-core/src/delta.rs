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
            // A declared but non-exhaustive selection is a targeted rerun.
            // Its missing findings were not observed, not resolved.
            (Some(_), None)
                if !subject.selection.exhaustive && !normalized_scopes(subject).is_empty() =>
            {
                DeltaFindingStatus::NotObserved
            }
            (Some(finding), None)
                if finding_is_outside_subject_coverage(finding, subject, &assessment) =>
            {
                DeltaFindingStatus::NotObserved
            }
            (Some(_), None) => DeltaFindingStatus::Unknown,
            (None, None) => unreachable!("union contains this fingerprint"),
        };
        let title = subject_finding
            .or(baseline_finding)
            .and_then(|finding| finding.title.clone())
            .map(|title| truncate(&title, 180));
        let evidence = subject_finding.or(baseline_finding);
        let availability = if subject_finding.is_some() {
            subject.availability
        } else {
            baseline.availability
        };
        findings.push(DeltaFinding {
            status,
            fingerprint: fingerprint.clone(),
            title,
            baseline_path: baseline_finding.map(|finding| finding.path.clone()),
            subject_path: subject_finding.map(|finding| finding.path.clone()),
            evidence_ref: evidence.and_then(|finding| finding.evidence_ref.clone()),
            availability,
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
    let mut counts = BTreeMap::new();
    for finding in &findings {
        *counts
            .entry(format!("{:?}", finding.status).to_ascii_lowercase())
            .or_insert(0) += 1;
    }
    let truncated = findings.len() > MAX_DELTA_FINDINGS;
    findings.truncate(MAX_DELTA_FINDINGS);
    ObservationDelta {
        schema: OBSERVATION_DELTA_SCHEMA.to_string(),
        baseline_observation_id: baseline.observation_id.clone(),
        subject_observation_id: subject.observation_id.clone(),
        assessment,
        findings,
        counts,
        truncated,
        extra: Extra::new(),
    }
}

fn assess(baseline: &ObservationRecord, subject: &ObservationRecord) -> ComparabilityAssessment {
    let invocation_match = baseline.invocation_fingerprint == subject.invocation_fingerprint;
    let comparison_family_match = baseline.comparison_family == subject.comparison_family;
    let subject_identity =
        if baseline.source_id == subject.source_id && baseline.operation == subject.operation {
            SubjectIdentity::Same
        } else {
            SubjectIdentity::Different
        };
    let scope_relationship = scope_relationship(baseline, subject);
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
    if !comparison_family_match {
        reasons.push("comparison families differ".to_string());
    }
    capture_reasons("baseline", baseline, &mut reasons);
    capture_reasons("subject", subject, &mut reasons);
    if !baseline.capture.can_prove_absence || !subject.capture.can_prove_absence {
        reasons.push("one or both observations are incomplete or truncated".to_string());
    }
    if !normalization_compatible {
        reasons.push("provider, parser, or lens identity differs".to_string());
    }
    if source_validity != crate::SourceValidity::ConfirmedUnchanged {
        reasons.push("source state cannot prove comparable coverage".to_string());
    }
    if !payloads_available {
        reasons.push("one or both redacted payloads are no longer available".to_string());
    }
    let selection_covers_absence = baseline.selection.exhaustive
        && subject.selection.exhaustive
        && !normalized_scopes(baseline).is_empty()
        && !normalized_scopes(subject).is_empty();
    if !selection_covers_absence {
        reasons.push("selection coverage is unknown or not exhaustive".to_string());
    }
    let can_prove_absence = invocation_match
        && comparison_family_match
        && matches!(subject_identity, SubjectIdentity::Same)
        && matches!(
            scope_relationship,
            ScopeRelationship::Equal | ScopeRelationship::Superset
        )
        && baseline.capture.can_prove_absence
        && subject.capture.can_prove_absence
        && selection_covers_absence
        && normalization_compatible
        && payloads_available
        && source_validity == crate::SourceValidity::ConfirmedUnchanged;
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

fn scope_relationship(
    baseline: &ObservationRecord,
    subject: &ObservationRecord,
) -> ScopeRelationship {
    let baseline_scopes = normalized_scopes(baseline);
    let subject_scopes = normalized_scopes(subject);
    if !baseline_scopes.is_empty() && !subject_scopes.is_empty() {
        if baseline_scopes == subject_scopes {
            return ScopeRelationship::Equal;
        }
        if baseline_scopes.is_subset(&subject_scopes) {
            return ScopeRelationship::Superset;
        }
        if subject_scopes.is_subset(&baseline_scopes) {
            return ScopeRelationship::Subset;
        }
        if baseline_scopes.is_disjoint(&subject_scopes) {
            return ScopeRelationship::Disjoint;
        }
        return ScopeRelationship::Overlap;
    }
    ScopeRelationship::Unknown
}

fn normalized_scopes(observation: &ObservationRecord) -> BTreeSet<String> {
    observation
        .selection
        .scopes
        .iter()
        .map(|scope| scope.trim())
        .filter(|scope| !scope.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn finding_is_outside_subject_coverage(
    finding: &Finding,
    subject: &ObservationRecord,
    assessment: &ComparabilityAssessment,
) -> bool {
    match assessment.scope_relationship {
        ScopeRelationship::Disjoint => true,
        ScopeRelationship::Subset | ScopeRelationship::Overlap => {
            if !subject.selection.exhaustive {
                return false;
            }
            let scopes = normalized_scopes(subject);
            !scopes.is_empty()
                && !scopes
                    .iter()
                    .any(|scope| path_is_within_scope(&finding.path, scope))
        }
        ScopeRelationship::Equal | ScopeRelationship::Superset | ScopeRelationship::Unknown => {
            false
        }
    }
}

fn path_is_within_scope(path: &str, scope: &str) -> bool {
    path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn capture_reasons(prefix: &str, observation: &ObservationRecord, reasons: &mut Vec<String>) {
    let capture = &observation.capture;
    if capture.stop_reason != crate::CaptureStopReason::Complete {
        reasons.push(format!(
            "{prefix} capture stopped: {}",
            capture_stop_reason_name(capture.stop_reason)
        ));
    }
    for affected in &capture.affected {
        reasons.push(format!(
            "{prefix} capture scope '{}' stopped: {}",
            affected.scope,
            capture_stop_reason_name(affected.stop_reason)
        ));
    }
    if !observation.selection.scopes.is_empty() && !observation.selection.exhaustive {
        reasons.push(format!("{prefix} selection is not exhaustive"));
    }
}

fn capture_stop_reason_name(reason: crate::CaptureStopReason) -> &'static str {
    match reason {
        crate::CaptureStopReason::Complete => "complete",
        crate::CaptureStopReason::ByteLimit => "byte_limit",
        crate::CaptureStopReason::Timeout => "timeout",
        crate::CaptureStopReason::Cancelled => "cancelled",
        crate::CaptureStopReason::Redacted => "redacted",
        crate::CaptureStopReason::StorageLimit => "storage_limit",
        crate::CaptureStopReason::Expired => "expired",
        crate::CaptureStopReason::Unavailable => "unavailable",
    }
}

fn source_validity(
    baseline: &ObservationRecord,
    subject: &ObservationRecord,
) -> crate::SourceValidity {
    if baseline.source_validity != crate::SourceValidity::Unknown {
        return baseline.source_validity;
    }
    if subject.source_validity != crate::SourceValidity::Unknown {
        return subject.source_validity;
    }
    match (&baseline.source_state, &subject.source_state) {
        (None, None) => crate::SourceValidity::ConfirmedUnchanged,
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
                )
                && left.value == right.value =>
        {
            crate::SourceValidity::ConfirmedUnchanged
        }
        (Some(left), Some(right))
            if left.source_id == right.source_id
                && left.operation == right.operation
                && left.subject_scope == right.subject_scope =>
        {
            crate::SourceValidity::SourceChanged
        }
        _ => crate::SourceValidity::Unknown,
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
        SelectionCoverage,
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
            comparison_family: Some("test".to_string()),
            selection: SelectionCoverage {
                scopes: vec!["/all".to_string()],
                exhaustive: true,
                extra: Extra::new(),
            },
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
            source_validity: crate::SourceValidity::Unknown,
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
    fn truncation_discloses_itself_and_counts_reflect_the_full_comparison() {
        let subject_findings = (0..150)
            .map(|index| finding(&format!("new-{index}")))
            .collect::<Vec<_>>();
        let delta = compare_observations(
            &observation("a", "same", true),
            &observation("b", "same", true),
            &[],
            &subject_findings,
        );
        assert!(delta.truncated);
        assert_eq!(delta.findings.len(), 100);
        assert_eq!(delta.counts.get("new"), Some(&150));
    }

    #[test]
    fn comparison_is_deterministic_across_repeated_runs() {
        let baseline = observation("a", "same", true);
        let subject = observation("b", "same", true);
        let baseline_findings = [finding("old"), finding("persist")];
        let subject_findings = [finding("persist"), finding("new")];
        let first =
            compare_observations(&baseline, &subject, &baseline_findings, &subject_findings);
        let second =
            compare_observations(&baseline, &subject, &baseline_findings, &subject_findings);
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
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

    #[test]
    fn scope_matrix_computes_every_declared_relationship() {
        let cases = [
            (vec!["/items/a"], vec!["/items/a"], ScopeRelationship::Equal),
            (
                vec!["/items/a"],
                vec!["/items/a", "/items/b"],
                ScopeRelationship::Superset,
            ),
            (
                vec!["/items/a", "/items/b"],
                vec!["/items/a"],
                ScopeRelationship::Subset,
            ),
            (
                vec!["/items/a", "/items/b"],
                vec!["/items/b", "/items/c"],
                ScopeRelationship::Overlap,
            ),
            (
                vec!["/items/a"],
                vec!["/items/b"],
                ScopeRelationship::Disjoint,
            ),
        ];
        for (baseline_scopes, subject_scopes, expected) in cases {
            let mut baseline = observation("a", "same", true);
            baseline.selection = selection(&baseline_scopes);
            let mut subject = observation("b", "different", true);
            subject.selection = selection(&subject_scopes);
            assert_eq!(
                compare_observations(&baseline, &subject, &[], &[])
                    .assessment
                    .scope_relationship,
                expected
            );
        }

        let mut baseline = observation("a", "one", true);
        baseline.selection = SelectionCoverage::default();
        let mut subject = observation("b", "two", true);
        subject.selection = SelectionCoverage::default();
        let unknown = compare_observations(&baseline, &subject, &[], &[]);
        assert_eq!(
            unknown.assessment.scope_relationship,
            ScopeRelationship::Unknown
        );
    }

    #[test]
    fn incomplete_subject_marks_only_proven_outside_selection_not_observed() {
        let mut baseline = observation("a", "same", true);
        baseline.selection = selection(&["/items/a", "/items/b"]);
        let mut subject = observation("b", "different", false);
        subject.selection = selection(&["/items/a"]);

        let outside =
            compare_observations(&baseline, &subject, &[finding_at("old", "/items/b")], &[]);
        assert_eq!(outside.findings[0].status, DeltaFindingStatus::NotObserved);
        let inside =
            compare_observations(&baseline, &subject, &[finding_at("old", "/items/a")], &[]);
        assert_eq!(inside.findings[0].status, DeltaFindingStatus::Unknown);
    }

    #[test]
    fn self_comparison_has_only_persisting_findings() {
        let observation = observation("same", "same", true);
        let delta = compare_observations(
            &observation,
            &observation,
            &[finding("known")],
            &[finding("known")],
        );
        assert_eq!(
            delta.counts,
            BTreeMap::from([(String::from("persisting"), 1)])
        );
        // "No changes" alone is not a valid self-comparison proof: it must
        // also carry an assessment that actually could have proven absence,
        // not one that happens to find nothing because it couldn't compare.
        assert_eq!(delta.assessment.subject_identity, SubjectIdentity::Same);
        assert!(delta.assessment.can_prove_absence);
        assert!(delta.assessment.reasons.is_empty());
    }

    #[test]
    fn exact_twelve_to_three_plus_one_delta_is_counted() {
        let baseline_findings = (0..12)
            .map(|index| finding(&format!("before-{index}")))
            .collect::<Vec<_>>();
        let subject_findings = (0..3)
            .map(|index| finding(&format!("before-{index}")))
            .chain(std::iter::once(finding("after-0")))
            .collect::<Vec<_>>();
        let delta = compare_observations(
            &observation("a", "same", true),
            &observation("b", "same", true),
            &baseline_findings,
            &subject_findings,
        );
        assert_eq!(
            delta.counts,
            BTreeMap::from([
                (String::from("new"), 1),
                (String::from("persisting"), 3),
                (String::from("resolved"), 9),
            ])
        );
    }

    #[test]
    fn pagination_provider_and_family_changes_never_resolve() {
        let baseline = observation("a", "same", true);
        let mut pagination_capped = observation("b", "same", false);
        pagination_capped.capture.stop_reason = crate::CaptureStopReason::ByteLimit;
        let mut provider_changed = observation("c", "same", true);
        provider_changed.provider = Some("other".to_string());
        let mut family_changed = observation("d", "same", true);
        family_changed.comparison_family = Some("other".to_string());
        for subject in [pagination_capped, provider_changed, family_changed] {
            let delta = compare_observations(&baseline, &subject, &[finding("old")], &[]);
            assert!(!delta.assessment.can_prove_absence);
            assert_ne!(delta.findings[0].status, DeltaFindingStatus::Resolved);
        }
    }

    #[test]
    fn unkeyed_collections_remain_unknown() {
        let mut baseline = observation("a", "first", true);
        baseline.selection = SelectionCoverage::default();
        let mut subject = observation("b", "second", true);
        subject.selection = SelectionCoverage::default();
        let delta = compare_observations(&baseline, &subject, &[finding("old")], &[]);
        assert_eq!(delta.findings[0].status, DeltaFindingStatus::Unknown);
        assert_eq!(
            delta.assessment.scope_relationship,
            ScopeRelationship::Unknown
        );
    }

    fn selection(scopes: &[&str]) -> SelectionCoverage {
        SelectionCoverage {
            scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
            exhaustive: true,
            extra: Extra::new(),
        }
    }

    fn finding_at(fingerprint: &str, path: &str) -> Finding {
        let mut finding = finding(fingerprint);
        finding.path = path.to_string();
        finding
    }
}

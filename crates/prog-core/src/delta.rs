use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::{
    ComparabilityAssessment, DeltaFinding, DeltaFindingStatus, EvidenceAvailability, Extra,
    Finding, OBSERVATION_DELTA_SCHEMA, ObservationDelta, ObservationRecord, ScopeRelationship,
    SubjectIdentity, WorkspaceValidity, compare_workspace,
};

const MAX_DELTA_FINDINGS: usize = 100;
const MAX_NAMED_TESTS: usize = 5;
const MAX_PROVIDER_WARNINGS: usize = 3;

/// Test outcomes a coding provider retained in an observation payload.
///
/// `executed` comes from the runner's own completion summary and counts every
/// test that actually ran, so it is available even when quiet output names
/// only failing tests. `tests` maps each named test id to its status.
/// `warnings` are the provider's own explanations of why its selection may be
/// unproven (for example Cargo's fail-fast), surfaced as delta remediation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestPopulation {
    pub tests: BTreeMap<String, String>,
    pub executed: Option<u64>,
    /// Failed plus errored tests according to the run's own summary.
    pub failing: Option<u64>,
    /// Skipped or ignored tests according to the run's own summary.
    pub skipped: Option<u64>,
    /// Ids of every test record whose status is `skipped`, sorted, with
    /// repeats kept (two harnesses can each skip a `tests::slow`).
    pub skipped_tests: Vec<String>,
    pub warnings: Vec<String>,
}

impl TestPopulation {
    /// Read `/provider/normalized` from a persisted run payload. `None` when
    /// the observation has no coding-provider test records.
    pub fn from_payload(payload: &Value) -> Option<Self> {
        let normalized = payload.pointer("/provider/normalized")?;
        let records = normalized.get("tests")?.as_array()?;
        let mut skipped_tests = records
            .iter()
            .filter(|record| record.get("status").and_then(Value::as_str) == Some("skipped"))
            .filter_map(|record| record.get("node_id")?.as_str().map(ToOwned::to_owned))
            .collect::<Vec<_>>();
        skipped_tests.sort();
        let tests = records
            .iter()
            .filter_map(|record| {
                Some((
                    record.get("node_id")?.as_str()?.to_string(),
                    record.get("status")?.as_str()?.to_string(),
                ))
            })
            .collect();
        let executed = normalized
            .pointer("/counts/executed")
            .and_then(Value::as_u64);
        let failing = normalized.get("counts").and_then(|counts| {
            let failed = counts.get("failed").and_then(Value::as_u64);
            let errors = counts.get("error").and_then(Value::as_u64);
            (failed.is_some() || errors.is_some())
                .then(|| failed.unwrap_or(0).saturating_add(errors.unwrap_or(0)))
        });
        let skipped = normalized
            .pointer("/counts/skipped")
            .and_then(Value::as_u64);
        let warnings = payload
            .pointer("/provider/warnings")
            .and_then(Value::as_array)
            .map(|warnings| {
                warnings
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            tests,
            executed,
            failing,
            skipped,
            skipped_tests,
            warnings,
        })
    }

    /// Whether passing tests are listed by name (verbose output), so a test
    /// missing from this population was not run rather than merely unnamed.
    fn names_passing_tests(&self) -> bool {
        self.tests.values().any(|status| status == "passed")
    }
}

/// Reasons the subject's test population cannot support an absence proof
/// against the baseline. Empty when the populations are comparable or when
/// neither side recorded tests.
///
/// A disappeared failure only means "fixed" when the same test ran again and
/// passed. A deleted, skipped, ignored, or deselected test produces the same
/// clean output as a real fix, so any narrowing of what actually ran blocks
/// the proof.
pub fn test_population_reasons(
    baseline: Option<&TestPopulation>,
    subject: Option<&TestPopulation>,
) -> Vec<String> {
    let Some(baseline) = baseline else {
        return Vec::new();
    };
    let Some(subject) = subject else {
        return vec![
            "the baseline recorded test results but the subject recorded none".to_string(),
        ];
    };
    let mut reasons = Vec::new();
    if let (Some(before), Some(after)) = (baseline.executed, subject.executed)
        && after < before
    {
        reasons.push(format!(
            "the subject executed {after} tests but the baseline executed {before}; the test population shrank"
        ));
    }
    let named_failing = baseline
        .tests
        .values()
        .filter(|status| matches!(status.as_str(), "failed" | "error"))
        .count() as u64;
    if let Some(failing) = baseline.failing
        && failing > named_failing
    {
        reasons.push(format!(
            "the baseline names {named_failing} of its {failing} failing tests, so the rest cannot be matched to a later pass; rerun verbosely"
        ));
    }
    let mut narrowed = Vec::new();
    let mut unnamed = Vec::new();
    for (test, status) in &baseline.tests {
        if !matches!(status.as_str(), "failed" | "error") {
            continue;
        }
        match subject.tests.get(test).map(String::as_str) {
            Some("passed" | "failed" | "error") => {}
            Some(other) => narrowed.push(format!("{test} ({other})")),
            None if subject.names_passing_tests() => narrowed.push(format!("{test} (missing)")),
            // Quiet output names only failures. An unnamed test may have
            // passed, or been deleted while another test took its place in
            // the count; the output cannot tell these apart.
            None => unnamed.push(test.clone()),
        }
    }
    if !unnamed.is_empty() {
        let shown = unnamed
            .iter()
            .take(MAX_NAMED_TESTS)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = unnamed.len().saturating_sub(MAX_NAMED_TESTS);
        reasons.push(format!(
            "quiet output does not name passing tests, so {shown}{} cannot be shown to have passed; rerun verbosely (pytest -v, cargo test without -q, go test -v)",
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ));
    }
    if !narrowed.is_empty() {
        let shown = narrowed
            .iter()
            .take(MAX_NAMED_TESTS)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = narrowed.len().saturating_sub(MAX_NAMED_TESTS);
        reasons.push(format!(
            "baseline-failing tests did not run again in the subject: {shown}{}",
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        ));
    }
    reasons
}

pub fn compare_observations(
    baseline: &ObservationRecord,
    subject: &ObservationRecord,
    baseline_findings: &[Finding],
    subject_findings: &[Finding],
) -> ObservationDelta {
    compare_observations_with_tests(
        baseline,
        subject,
        baseline_findings,
        subject_findings,
        None,
        None,
    )
}

/// How many tests both runs skipped, when skips are the only gap in the
/// subject's selection and both runs name exactly the same skipped tests
/// (with the same repeats) as their own summaries count.
///
/// A test skipped in both runs ran in neither, so it cannot hide a baseline
/// finding. Anything else stays unproven: a test skipped in only one run, a
/// skip the output does not name (`pytest -q`), a filter, or an early stop.
fn shared_skipped_tests(
    subject: &ObservationRecord,
    baseline_tests: Option<&TestPopulation>,
    subject_tests: Option<&TestPopulation>,
) -> Option<u64> {
    if !subject.selection.excludes_only_skipped_tests() {
        return None;
    }
    let (before, after) = (baseline_tests?, subject_tests?);
    let skipped = after.skipped?;
    (skipped > 0
        && before.skipped == Some(skipped)
        && after.skipped_tests.len() as u64 == skipped
        && before.skipped_tests == after.skipped_tests)
        .then_some(skipped)
}

/// [`compare_observations`] plus the test-population check: absence is only
/// provable when every test that ran in the baseline ran again.
pub fn compare_observations_with_tests(
    baseline: &ObservationRecord,
    subject: &ObservationRecord,
    baseline_findings: &[Finding],
    subject_findings: &[Finding],
    baseline_tests: Option<&TestPopulation>,
    subject_tests: Option<&TestPopulation>,
) -> ObservationDelta {
    let shared_skips = shared_skipped_tests(subject, baseline_tests, subject_tests);
    let mut assessment = assess(baseline, subject, shared_skips);
    let population_reasons = test_population_reasons(baseline_tests, subject_tests);
    if !population_reasons.is_empty() {
        assessment.can_prove_absence = false;
        assessment.reasons.extend(population_reasons);
    }
    if !assessment.can_prove_absence {
        // The provider already knows how to make its selection provable (for
        // example `--no-fail-fast`); repeat that remediation where the agent
        // reads why the comparison failed.
        for (side, population) in [("baseline", baseline_tests), ("subject", subject_tests)] {
            for warning in population
                .map(|population| population.warnings.as_slice())
                .unwrap_or_default()
                .iter()
                .take(MAX_PROVIDER_WARNINGS)
            {
                let reason = format!("{side} provider: {warning}");
                if !assessment.reasons.contains(&reason) {
                    assessment.reasons.push(reason);
                }
            }
        }
    }
    for (side, findings) in [
        ("baseline", baseline_findings),
        ("subject", subject_findings),
    ] {
        let collisions = fingerprint_collisions(findings);
        if collisions > 0 {
            assessment.reasons.push(format!(
                "{side} has {collisions} findings sharing a fingerprint with another finding; each fingerprint is compared once"
            ));
        }
    }
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
        records: None,
        extra: Extra::new(),
    }
}

/// Absence is judged from the subject: it must cover the whole selection.
/// The baseline need not, because a run that stopped early or skipped tests
/// can only miss findings it never reported, and only reported findings can
/// be resolved. `shared_skips` is [`shared_skipped_tests`]: when set, the
/// subject counts as exhaustive for the tests that ran.
fn assess(
    baseline: &ObservationRecord,
    subject: &ObservationRecord,
    shared_skips: Option<u64>,
) -> ComparabilityAssessment {
    let selection_exhaustive = |observation: &ObservationRecord| {
        observation.selection.exhaustive
            || shared_skips.is_some() && observation.selection.excludes_only_skipped_tests()
    };
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
    capture_reasons(
        "baseline",
        baseline,
        selection_exhaustive(baseline),
        &mut reasons,
    );
    capture_reasons(
        "subject",
        subject,
        selection_exhaustive(subject),
        &mut reasons,
    );
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
    let selection_covers_absence = selection_exhaustive(subject)
        && !normalized_scopes(baseline).is_empty()
        && !normalized_scopes(subject).is_empty();
    if !selection_covers_absence {
        reasons.push("selection coverage is unknown or not exhaustive".to_string());
    } else if let Some(skipped) = shared_skips {
        reasons.push(format!(
            "both runs skipped the same {skipped} named tests; absence is proven only for the tests that ran"
        ));
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

fn capture_reasons(
    prefix: &str,
    observation: &ObservationRecord,
    selection_exhaustive: bool,
    reasons: &mut Vec<String>,
) {
    let capture = &observation.capture;
    if capture.stop_reason != crate::CaptureStopReason::Complete {
        reasons.push(format!(
            "{prefix} capture stopped: {}",
            capture_stop_reason_name(capture.stop_reason)
        ));
    }
    for affected in capture
        .affected
        .iter()
        .filter(|affected| affected.stop_reason != crate::CaptureStopReason::Complete)
    {
        reasons.push(format!(
            "{prefix} capture scope '{}' stopped: {}",
            affected.scope,
            capture_stop_reason_name(affected.stop_reason)
        ));
    }
    if !observation.selection.scopes.is_empty() && !selection_exhaustive {
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
        crate::CaptureStopReason::DerivationWindowed => "derivation_windowed",
    }
}

fn source_validity(
    baseline: &ObservationRecord,
    subject: &ObservationRecord,
) -> crate::SourceValidity {
    // A subject-side negative assessment is direct evidence that this later
    // capture cannot prove source stability. It must win even if an error
    // response happens to repeat the baseline validator.
    if !matches!(
        subject.source_validity,
        crate::SourceValidity::Unknown | crate::SourceValidity::ConfirmedUnchanged
    ) {
        return subject.source_validity;
    }
    match (&baseline.source_state, &subject.source_state) {
        (Some(left), Some(right))
            if left.source_id == right.source_id
                && left.operation == right.operation
                && left.subject_scope == right.subject_scope
                && left.kind == right.kind =>
        {
            if left.value == right.value {
                crate::SourceValidity::ConfirmedUnchanged
            } else {
                crate::SourceValidity::SourceChanged
            }
        }
        // With no native validators, only the subject observation's own
        // capture-time assessment can describe its current validity. Never
        // reuse the baseline's status: it says nothing about the later
        // capture and could turn a subject marked `source_changed` into a
        // false `confirmed_unchanged` comparison.
        (None, None) => subject.source_validity,
        // `confirmed_unchanged` is not trusted when only one side has a
        // token: the token pair is incomplete, so equality cannot be
        // established. Negative subject assessments returned above.
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

/// Findings whose fingerprint repeats an earlier finding in the same
/// observation. They are merged by [`findings_by_fingerprint`]; the count makes
/// that merge visible instead of silent.
fn fingerprint_collisions(findings: &[Finding]) -> usize {
    let mut seen = BTreeSet::new();
    findings
        .iter()
        .filter_map(|finding| finding.fingerprint.as_deref())
        .filter(|fingerprint| !seen.insert(*fingerprint))
        .count()
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
        SelectionCoverage, SourceStateKind, SourceStateToken,
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
            source_validity: crate::SourceValidity::ConfirmedUnchanged,
            workspace_state: None,
            source_state: None,
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

    fn etag(value: &str) -> SourceStateToken {
        SourceStateToken {
            schema: "prog.source_state".to_string(),
            kind: SourceStateKind::HttpEtag,
            value: value.to_string(),
            source_id: "api".to_string(),
            operation: "get".to_string(),
            subject_scope: None,
            captured_at: "2026-07-13T12:00:00Z".to_string(),
            validity: crate::SourceValidity::Unknown,
            expires_at: None,
            provider: Some("http".to_string()),
            extra: Extra::new(),
        }
    }

    fn population(tests: &[(&str, &str)], executed: Option<u64>) -> TestPopulation {
        TestPopulation {
            tests: tests
                .iter()
                .map(|(id, status)| ((*id).to_string(), (*status).to_string()))
                .collect(),
            executed,
            failing: None,
            skipped: None,
            skipped_tests: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// A run in which every selected test ran except the named skips.
    fn skipping_run(id: &str) -> ObservationRecord {
        let mut run = observation(id, "same", true);
        run.selection.exhaustive = false;
        run.selection.extra.insert(
            crate::SELECTION_EXCLUDES_SKIPPED_TESTS.to_string(),
            Value::Bool(true),
        );
        run
    }

    fn with_skips(mut tests: TestPopulation, counted: u64, named: &[&str]) -> TestPopulation {
        tests.skipped = Some(counted);
        tests.skipped_tests = named.iter().map(|name| (*name).to_string()).collect();
        tests.skipped_tests.sort();
        for name in named {
            tests
                .tests
                .insert((*name).to_string(), "skipped".to_string());
        }
        tests
    }

    #[test]
    fn test_population_reads_provider_records_and_executed_counts() {
        let payload = json!({"provider": {"normalized": {
            "tests": [{"node_id": "t::a", "status": "failed"}, {"node_id": "t::b", "status": "passed"}],
            "counts": {"passed": 1, "failed": 1, "executed": 2}
        }}});
        let mut expected = population(&[("t::a", "failed"), ("t::b", "passed")], Some(2));
        expected.failing = Some(1);
        assert_eq!(TestPopulation::from_payload(&payload), Some(expected));
        assert_eq!(TestPopulation::from_payload(&json!({"stdout": {}})), None);
    }

    #[test]
    fn test_population_reasons_block_every_narrowing_and_allow_real_fixes() {
        let baseline = population(&[("t::a", "failed"), ("t::b", "passed")], Some(2));
        // A real fix: same tests ran, the failing one passed.
        let fixed = population(&[("t::a", "passed"), ("t::b", "passed")], Some(2));
        assert!(test_population_reasons(Some(&baseline), Some(&fixed)).is_empty());
        // Quiet output names only failures. A fix and a swap (the failing
        // test deleted while a new passing test keeps the count) print the
        // same thing, so neither proves the fix; rerunning verbosely does.
        let quiet = population(&[], Some(2));
        let reasons = test_population_reasons(Some(&baseline), Some(&quiet)).join("; ");
        assert!(
            reasons.contains("quiet output does not name passing tests, so t::a cannot be shown"),
            "{reasons}"
        );
        assert!(reasons.contains("rerun verbosely"), "{reasons}");
        let swapped = population(&[("t::b", "passed"), ("t::new", "passed")], Some(2));
        let reasons = test_population_reasons(Some(&baseline), Some(&swapped)).join("; ");
        assert!(reasons.contains("t::a (missing)"), "{reasons}");
        // A baseline that counts more failures than it names cannot match
        // the unnamed ones to a later pass, whatever the subject shows.
        let mut counted = population(&[("t::b", "passed")], Some(2));
        counted.failing = Some(1);
        let reasons = test_population_reasons(Some(&counted), Some(&fixed)).join("; ");
        assert!(
            reasons.contains("names 0 of its 1 failing tests"),
            "{reasons}"
        );
        let mut named = baseline.clone();
        named.failing = Some(1);
        assert!(test_population_reasons(Some(&named), Some(&fixed)).is_empty());
        // Still failing is a persisting finding, not a narrowing.
        let still = population(&[("t::a", "failed"), ("t::b", "passed")], Some(2));
        assert!(test_population_reasons(Some(&baseline), Some(&still)).is_empty());

        let deleted = population(&[("t::b", "passed")], Some(1));
        let reasons = test_population_reasons(Some(&baseline), Some(&deleted)).join("; ");
        assert!(
            reasons.contains("executed 1 tests but the baseline executed 2"),
            "{reasons}"
        );
        assert!(reasons.contains("t::a (missing)"), "{reasons}");

        let skipped = population(&[("t::a", "skipped"), ("t::b", "passed")], Some(1));
        let reasons = test_population_reasons(Some(&baseline), Some(&skipped)).join("; ");
        assert!(reasons.contains("t::a (skipped)"), "{reasons}");

        let quiet_deleted = population(&[], Some(1));
        assert!(!test_population_reasons(Some(&baseline), Some(&quiet_deleted)).is_empty());

        let none = test_population_reasons(Some(&baseline), None);
        assert_eq!(
            none,
            vec!["the baseline recorded test results but the subject recorded none".to_string()]
        );
        assert!(test_population_reasons(None, Some(&deleted)).is_empty());
    }

    #[test]
    fn delta_reasons_skip_completed_scopes_and_disclose_collisions_and_remediation() {
        let mut baseline = observation("a", "same", true);
        baseline.capture.affected.push(crate::CaptureScope {
            scope: "stdout".to_string(),
            total_bytes: Some(10),
            captured_bytes: 10,
            stop_reason: crate::CaptureStopReason::Complete,
            extra: Extra::new(),
        });
        let mut subject = observation("b", "same", true);
        subject.selection.exhaustive = false;
        let baseline_tests = population(&[("t::a", "failed")], Some(1));
        let mut subject_tests = population(&[("t::a", "failed")], Some(1));
        subject_tests.warnings =
            vec!["use --no-fail-fast to prove selection exhaustion".to_string()];
        let delta = compare_observations_with_tests(
            &baseline,
            &subject,
            &[finding("dup"), finding("dup")],
            &[],
            Some(&baseline_tests),
            Some(&subject_tests),
        );
        assert!(!delta.assessment.can_prove_absence);
        let reasons = delta.assessment.reasons.join("; ");
        assert!(!reasons.contains("stopped: complete"), "{reasons}");
        assert!(
            reasons.contains("subject provider: use --no-fail-fast"),
            "{reasons}"
        );
        assert!(
            reasons.contains("baseline has 1 findings sharing a fingerprint"),
            "{reasons}"
        );
    }

    #[test]
    fn narrowed_test_population_never_resolves_a_disappeared_finding() {
        let baseline_tests = population(&[("t::a", "failed"), ("t::b", "passed")], Some(2));
        let deleted = population(&[("t::b", "passed")], Some(1));
        let delta = compare_observations_with_tests(
            &observation("a", "same", true),
            &observation("b", "same", true),
            &[finding("panic")],
            &[],
            Some(&baseline_tests),
            Some(&deleted),
        );
        assert!(!delta.assessment.can_prove_absence);
        assert_eq!(delta.counts.get("resolved"), None);
        assert_eq!(delta.findings[0].status, DeltaFindingStatus::Unknown);
        assert!(
            delta.findings[0]
                .reasons
                .iter()
                .any(|reason| reason.contains("test population shrank"))
        );

        let fixed = population(&[("t::a", "passed"), ("t::b", "passed")], Some(2));
        let delta = compare_observations_with_tests(
            &observation("a", "same", true),
            &observation("b", "same", true),
            &[finding("panic")],
            &[],
            Some(&baseline_tests),
            Some(&fixed),
        );
        assert!(delta.assessment.can_prove_absence);
        assert_eq!(delta.findings[0].status, DeltaFindingStatus::Resolved);
    }

    /// #297: one ignored or skipped test used to make every real fix
    /// unprovable. A test both runs skipped ran in neither, so the fix is
    /// provable; any other skip still blocks the proof.
    #[test]
    fn shared_named_skips_prove_a_fix_and_every_other_skip_does_not() {
        let compare = |baseline: &ObservationRecord,
                       subject: &ObservationRecord,
                       before: &TestPopulation,
                       after: &TestPopulation| {
            compare_observations_with_tests(
                baseline,
                subject,
                &[finding("panic")],
                &[],
                Some(before),
                Some(after),
            )
        };
        let failing = with_skips(
            population(&[("t::a", "failed"), ("t::b", "passed")], Some(2)),
            1,
            &["t::slow"],
        );
        let fixed = with_skips(
            population(&[("t::a", "passed"), ("t::b", "passed")], Some(2)),
            1,
            &["t::slow"],
        );
        let delta = compare(&skipping_run("a"), &skipping_run("b"), &failing, &fixed);
        assert!(delta.assessment.can_prove_absence, "{:?}", delta.assessment);
        assert_eq!(delta.findings[0].status, DeltaFindingStatus::Resolved);
        let reasons = delta.assessment.reasons.join("; ");
        assert!(
            reasons.contains("both runs skipped the same 1 named tests"),
            "{reasons}"
        );
        assert!(!reasons.contains("not exhaustive"), "{reasons}");

        let refused = |before: &TestPopulation,
                       after: &TestPopulation,
                       baseline: &ObservationRecord,
                       subject: &ObservationRecord| {
            let delta = compare(baseline, subject, before, after);
            assert!(
                !delta.assessment.can_prove_absence,
                "{:?}",
                delta.assessment
            );
            assert_ne!(delta.findings[0].status, DeltaFindingStatus::Resolved);
        };
        let (a, b) = (skipping_run("a"), skipping_run("b"));
        // The failing test itself skipped in the subject.
        let hidden = with_skips(
            population(&[("t::b", "passed")], Some(1)),
            2,
            &["t::a", "t::slow"],
        );
        refused(&failing, &hidden, &a, &b);
        // A different test skipped while the count stays the same.
        let swapped = with_skips(
            population(&[("t::a", "passed"), ("t::slow", "passed")], Some(2)),
            1,
            &["t::b"],
        );
        refused(&failing, &swapped, &a, &b);
        // Quiet output counts the skip without naming it.
        let mut unnamed_before = failing.clone();
        unnamed_before.skipped_tests.clear();
        let mut unnamed_after = fixed.clone();
        unnamed_after.skipped_tests.clear();
        refused(&unnamed_before, &unnamed_after, &a, &b);
        // Two harnesses that each skip a `t::slow` versus one that skips it once.
        let twice = with_skips(fixed.clone(), 2, &["t::slow", "t::slow"]);
        refused(&failing, &twice, &a, &b);
        // A skip in only one run: the baseline was exhaustive.
        let full_baseline = population(
            &[
                ("t::a", "failed"),
                ("t::b", "passed"),
                ("t::slow", "passed"),
            ],
            Some(3),
        );
        refused(&full_baseline, &fixed, &observation("a", "same", true), &b);
        // Skips plus another gap (an early stop, a filter) in the subject.
        let mut early = observation("b", "same", true);
        early.selection.exhaustive = false;
        refused(&failing, &fixed, &a, &early);
        // Without test populations nothing can be matched.
        let delta = compare_observations(&a, &b, &[finding("panic")], &[]);
        assert!(!delta.assessment.can_prove_absence);
    }

    /// #299: a failing fail-fast or early-stopped baseline still reports the
    /// failures it saw. Absence is judged from the subject, which must cover
    /// the whole selection; the population rules still apply.
    #[test]
    fn a_non_exhaustive_baseline_resolves_only_what_an_exhaustive_subject_proves() {
        let mut stopped = observation("a", "same", true);
        stopped.selection.exhaustive = false;
        let failing = population(&[("t::a", "failed"), ("t::b", "passed")], Some(2));
        let compare = |subject: &ObservationRecord, after: &TestPopulation| {
            compare_observations_with_tests(
                &stopped,
                subject,
                &[finding("panic")],
                &[],
                Some(&failing),
                Some(after),
            )
        };
        let fixed = population(
            &[
                ("t::a", "passed"),
                ("t::b", "passed"),
                ("t::later", "passed"),
            ],
            Some(3),
        );
        let delta = compare(&observation("b", "same", true), &fixed);
        assert!(delta.assessment.can_prove_absence, "{:?}", delta.assessment);
        assert_eq!(delta.findings[0].status, DeltaFindingStatus::Resolved);
        assert!(
            delta
                .assessment
                .reasons
                .iter()
                .any(|reason| reason == "baseline selection is not exhaustive"),
            "{:?}",
            delta.assessment.reasons
        );

        // The subject must still cover everything.
        let mut also_stopped = observation("b", "same", true);
        also_stopped.selection.exhaustive = false;
        assert!(!compare(&also_stopped, &fixed).assessment.can_prove_absence);
        // A deleted failing test is still a narrowed population.
        let deleted = population(&[("t::b", "passed"), ("t::later", "passed")], Some(2));
        let delta = compare(&observation("b", "same", true), &deleted);
        assert!(!delta.assessment.can_prove_absence);
        assert_ne!(delta.findings[0].status, DeltaFindingStatus::Resolved);
        // An incomplete subject capture or a different command never proves.
        assert!(
            !compare(&observation("b", "same", false), &fixed)
                .assessment
                .can_prove_absence
        );
        assert!(
            !compare(&observation("b", "other", true), &fixed)
                .assessment
                .can_prove_absence
        );
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
    fn pairwise_source_state_cannot_be_overridden_by_the_baseline_status() {
        let mut baseline = observation("a", "same", true);
        baseline.source_id = "api".to_string();
        baseline.operation = "get".to_string();
        baseline.source_state = Some(etag("v1"));
        baseline.source_validity = crate::SourceValidity::ConfirmedUnchanged;

        let mut changed = observation("b", "same", true);
        changed.source_id = "api".to_string();
        changed.operation = "get".to_string();
        changed.source_state = Some(etag("v2"));
        changed.source_validity = crate::SourceValidity::SourceChanged;

        let delta = compare_observations(&baseline, &changed, &[finding("old")], &[]);
        assert_eq!(
            delta.assessment.source_validity,
            crate::SourceValidity::SourceChanged
        );
        assert!(!delta.assessment.can_prove_absence);
        assert_eq!(delta.findings[0].status, DeltaFindingStatus::Unknown);

        baseline.source_validity = crate::SourceValidity::SourceChanged;
        let mut unchanged = observation("c", "same", true);
        unchanged.source_id = "api".to_string();
        unchanged.operation = "get".to_string();
        unchanged.source_state = Some(etag("v1"));
        unchanged.source_validity = crate::SourceValidity::Unknown;
        // The baseline's capture-time status cannot override two equal,
        // native source tokens for this later pair.
        let delta = compare_observations(&baseline, &unchanged, &[finding("old")], &[]);
        assert_eq!(
            delta.assessment.source_validity,
            crate::SourceValidity::ConfirmedUnchanged
        );
        assert!(delta.assessment.can_prove_absence);
        assert_eq!(delta.findings[0].status, DeltaFindingStatus::Resolved);

        unchanged.source_validity = crate::SourceValidity::RefreshFailed;
        let delta = compare_observations(&baseline, &unchanged, &[finding("old")], &[]);
        assert_eq!(
            delta.assessment.source_validity,
            crate::SourceValidity::RefreshFailed
        );
        assert!(!delta.assessment.can_prove_absence);
    }

    #[test]
    fn opaque_change_and_mcp_tokens_participate_in_pairwise_validity() {
        for kind in [
            SourceStateKind::ChangeToken,
            SourceStateKind::McpModification,
        ] {
            let mut baseline = observation("a", "same", true);
            let mut subject = observation("b", "same", true);
            let mut left = etag("sha256:same");
            left.kind = kind;
            let right = left.clone();
            baseline.source_state = Some(left);
            subject.source_state = Some(right);
            baseline.source_validity = crate::SourceValidity::Unknown;
            subject.source_validity = crate::SourceValidity::Unknown;
            assert_eq!(
                compare_observations(&baseline, &subject, &[], &[])
                    .assessment
                    .source_validity,
                crate::SourceValidity::ConfirmedUnchanged
            );

            subject.source_state.as_mut().unwrap().value = "sha256:changed".to_string();
            assert_eq!(
                compare_observations(&baseline, &subject, &[], &[])
                    .assessment
                    .source_validity,
                crate::SourceValidity::SourceChanged
            );
        }
    }

    #[test]
    fn missing_source_tokens_require_a_subject_side_validity_proof() {
        let mut baseline = observation("a", "same", true);
        baseline.source_validity = crate::SourceValidity::Unknown;
        let mut unknown = observation("b", "same", true);
        unknown.source_validity = crate::SourceValidity::Unknown;
        let delta = compare_observations(&baseline, &unknown, &[finding("old")], &[]);
        assert_eq!(
            delta.assessment.source_validity,
            crate::SourceValidity::Unknown
        );
        assert!(!delta.assessment.can_prove_absence);

        unknown.source_validity = crate::SourceValidity::ConfirmedUnchanged;
        let delta = compare_observations(&baseline, &unknown, &[finding("old")], &[]);
        assert_eq!(
            delta.assessment.source_validity,
            crate::SourceValidity::ConfirmedUnchanged
        );
        assert!(delta.assessment.can_prove_absence);
    }

    #[test]
    fn different_validator_kinds_are_not_comparable_even_when_values_match() {
        let mut baseline = observation("a", "same", true);
        baseline.source_id = "api".to_string();
        baseline.operation = "get".to_string();
        baseline.source_state = Some(etag("same-value"));

        let mut subject = observation("b", "same", true);
        subject.source_id = "api".to_string();
        subject.operation = "get".to_string();
        let mut last_modified = etag("same-value");
        last_modified.kind = SourceStateKind::HttpLastModified;
        subject.source_state = Some(last_modified);
        subject.source_validity = crate::SourceValidity::Unknown;

        let delta = compare_observations(&baseline, &subject, &[finding("old")], &[]);
        assert_eq!(
            delta.assessment.source_validity,
            crate::SourceValidity::Unknown
        );
        assert!(!delta.assessment.can_prove_absence);
        assert_eq!(delta.findings[0].status, DeltaFindingStatus::Unknown);

        subject.source_state.as_mut().unwrap().value = "different-value".to_string();
        let delta = compare_observations(&baseline, &subject, &[finding("old")], &[]);
        assert_eq!(
            delta.assessment.source_validity,
            crate::SourceValidity::Unknown
        );
        assert!(!delta.assessment.can_prove_absence);
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
    fn assess_is_not_provable_when_capture_was_derivation_windowed() {
        let baseline = observation("a", "same", true);
        let mut subject = observation("b", "same", true);
        // Mirrors what `prog run`'s capture completeness reports when a
        // stream's head/tail-of-N finding-derivation window doesn't cover
        // the full captured output: bytes are complete, but absence cannot
        // be proven beyond the window.
        subject.capture.can_prove_absence = false;
        subject.capture.affected = vec![crate::CaptureScope {
            scope: "stdout".to_string(),
            total_bytes: Some(1),
            captured_bytes: 1,
            stop_reason: crate::CaptureStopReason::DerivationWindowed,
            extra: Extra::new(),
        }];
        let delta = compare_observations(&baseline, &subject, &[finding("old")], &[]);
        assert!(!delta.assessment.can_prove_absence);
        assert_eq!(delta.findings[0].status, DeltaFindingStatus::Unknown);
        assert!(
            delta
                .assessment
                .reasons
                .iter()
                .any(|reason| reason.contains("derivation_windowed")),
            "expected a reason mentioning derivation_windowed, got {:?}",
            delta.assessment.reasons
        );
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

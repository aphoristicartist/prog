//! Immutable-observation delta command.

use crate::*;

pub(crate) fn delta_observations(
    store: &Store,
    args: &DeltaArgs,
) -> Result<prog_core::ObservationDelta> {
    compare_observation_ids(store, &args.baseline, &args.subject)
}

pub(crate) fn compare_observation_ids(
    store: &Store,
    baseline_id: &str,
    subject_id: &str,
) -> Result<prog_core::ObservationDelta> {
    let baseline = store
        .get_observation(baseline_id)?
        .ok_or_else(|| CoreError::BadArgs {
            operation: "delta".to_string(),
            reason: format!("unknown baseline observation '{baseline_id}'"),
        })?;
    let subject = store
        .get_observation(subject_id)?
        .ok_or_else(|| CoreError::BadArgs {
            operation: "delta".to_string(),
            reason: format!("unknown subject observation '{subject_id}'"),
        })?;
    let baseline_findings = delta_findings_for_observation(store, &baseline)?;
    let subject_findings = delta_findings_for_observation(store, &subject)?;
    Ok(prog_core::compare_observations(
        &baseline,
        &subject,
        &baseline_findings,
        &subject_findings,
    ))
}

fn delta_findings_for_observation(
    store: &Store,
    observation: &prog_core::ObservationRecord,
) -> Result<Vec<prog_core::Finding>> {
    let Some(payload) = store.get_payload(&observation.payload_hash)? else {
        return Ok(Vec::new());
    };
    let mut findings = prog_core::ranked_findings(
        payload.as_value(),
        &FindingOptions {
            // Absence must be evaluated against every candidate the bounded
            // derivation visited. The observation's CaptureCompleteness is
            // already forced false when traversal itself hit a bound.
            limit: usize::MAX,
            identity: FindingIdentityContext {
                provider: observation.provider.clone(),
                parser: observation.parser.clone(),
                lens: observation.lens.clone(),
            },
            ..FindingOptions::default()
        },
    )?;
    for finding in &mut findings {
        let Some(value) = prog_core::pointer::get(payload.as_value(), &finding.path)? else {
            continue;
        };
        finding.evidence_ref = Some(evidence_ref(EvidenceRefInput {
            source_id: &observation.source_id,
            operation: &observation.operation,
            cursor: None,
            path: &finding.path,
            value,
            observation: Some(observation),
            provenance: observation.provenance.as_ref(),
            cache: None,
            omitted: &[],
            redacted_paths: 0,
        }));
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_derives_every_finding_within_the_bounded_payload_traversal() {
        let store_dir = tempfile::tempdir().unwrap();
        let store = Store::open(store_dir.path()).unwrap();
        let raw = RawPayload::new(Value::Array(
            (0..150)
                .map(|index| json!({"error": format!("failure {index}")}))
                .collect(),
        ));
        let payload = raw.redact(&RedactionPolicy::default()).payload;
        let payload_hash = store.put_payload(&payload).unwrap();
        let observation = store
            .record_observation(NewObservation {
                payload_hash,
                availability: EvidenceAvailability::Recoverable,
                invocation_fingerprint: "same".to_string(),
                source_id: "fixture".to_string(),
                operation: "read".to_string(),
                selection: SelectionCoverage {
                    scopes: vec!["/".to_string()],
                    exhaustive: true,
                    extra: Extra::new(),
                },
                capture: CaptureCompleteness::complete(1),
                source_validity: prog_core::SourceValidity::ConfirmedUnchanged,
                ..NewObservation::default()
            })
            .unwrap();

        let findings = delta_findings_for_observation(&store, &observation).unwrap();
        assert_eq!(
            findings
                .iter()
                .filter(|finding| finding.kind == "generic_error_field")
                .count(),
            150
        );
    }
}

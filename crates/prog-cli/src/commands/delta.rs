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
            limit: 100,
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

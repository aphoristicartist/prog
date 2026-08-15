//! Agent-facing status facade over the canonical delta and readiness engines.

use crate::*;
use prog_core::{STATUS_SCHEMA, StatusReport};

pub(crate) fn status_report(
    store: &Store,
    args: &StatusArgs,
    max_envelope_bytes: usize,
) -> Result<StatusReport> {
    let mut readiness = readiness_report(store, args.session_id.as_deref())?;
    bound_readiness_report(&mut readiness, max_envelope_bytes)?;
    let delta = match (&args.baseline, &args.subject) {
        (Some(baseline), Some(subject)) => {
            let mut delta = compare_observation_ids(store, baseline, subject)?;
            bound_delta_response(&mut delta, max_envelope_bytes)?;
            Some(delta)
        }
        (None, None) => None,
        _ => {
            return Err(CoreError::BadArgs {
                operation: "status".to_string(),
                reason: "--baseline and --subject must be supplied together".to_string(),
            });
        }
    };
    let mut report = StatusReport {
        schema: STATUS_SCHEMA.to_string(),
        readiness,
        delta,
        extra: Extra::new(),
    };
    bound_status_response(&mut report, max_envelope_bytes)?;
    Ok(report)
}

fn bound_status_response(report: &mut StatusReport, max_envelope_bytes: usize) -> Result<()> {
    let content_budget = max_envelope_bytes.saturating_sub(1_024);
    let total_findings = report
        .delta
        .as_ref()
        .map_or(0, |delta| delta.findings.len());
    let total_evaluations = report.readiness.evaluations.len();
    let total_blockers = report.readiness.blockers.len();
    while serde_json::to_vec(report)?.len() > content_budget {
        if let Some(delta) = &mut report.delta
            && !delta.findings.is_empty()
        {
            delta.findings.pop();
            delta.truncated = true;
            continue;
        }
        if report.readiness.evaluations.pop().is_some() {
            continue;
        }
        if report.readiness.blockers.pop().is_some() {
            continue;
        }
        break;
    }
    let retained_findings = report
        .delta
        .as_ref()
        .map_or(0, |delta| delta.findings.len());
    if retained_findings < total_findings
        || report.readiness.evaluations.len() < total_evaluations
        || report.readiness.blockers.len() < total_blockers
    {
        report.extra.insert(
            "compaction".to_string(),
            json!({
                "reason": "disclosure_budget",
                "retained_delta_findings": retained_findings,
                "total_delta_findings": total_findings,
                "retained_evaluations": report.readiness.evaluations.len(),
                "total_evaluations": total_evaluations,
                "retained_blockers": report.readiness.blockers.len(),
                "total_blockers": total_blockers,
                "delta_counts_and_readiness_decisions_are_complete": true
            }),
        );
    }
    // Adding the compaction receipt may itself cross the budget. Prefer
    // dropping retained detail over losing the receipt or full decisions.
    while serde_json::to_vec(report)?.len() > content_budget {
        if let Some(delta) = &mut report.delta
            && !delta.findings.is_empty()
        {
            delta.findings.pop();
        } else if report.readiness.evaluations.pop().is_none()
            && report.readiness.blockers.pop().is_none()
        {
            break;
        }
        let retained_findings = report
            .delta
            .as_ref()
            .map_or(0, |delta| delta.findings.len());
        if let Some(compaction) = report.extra.get_mut("compaction") {
            compaction["retained_delta_findings"] = json!(retained_findings);
            compaction["retained_evaluations"] = json!(report.readiness.evaluations.len());
            compaction["retained_blockers"] = json!(report.readiness.blockers.len());
        }
    }
    Ok(())
}

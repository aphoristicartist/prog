//! Session display and readiness commands.

use crate::*;

pub(crate) fn declare_recipe_obligation(
    store: &Store,
    args: &RecipeArgs,
    envelope: &DisclosureEnvelope,
) -> Result<()> {
    let Some(observation_id) = envelope
        .observation
        .as_ref()
        .and_then(|observation| observation.observation_id.as_deref())
    else {
        return Ok(());
    };
    let session = match store.get_session(None)? {
        Some(session) => session,
        None => store.start_session(Some(format!(
            "recipe {} verification",
            args.recipe.as_str()
        )))?,
    };
    let id = format!(
        "recipe.{}.{}",
        args.recipe.as_str(),
        &observation_id[..12.min(observation_id.len())]
    );
    let obligation = VerificationObligation {
        schema: VERIFICATION_SCHEMA.to_string(),
        id,
        session_id: session.session_id,
        required: false,
        intended_check: format!("review {} recipe evidence", args.recipe.as_str()),
        required_scope: "recipe-observation".to_string(),
        declared_by: ObligationDeclarer::Recipe,
        expected_operation: None,
        required_state: VerificationStateRelationship::Any,
        advisory_actions: Vec::new(),
        comparison_family: args.comparison_family.clone(),
        origin_observation_id: None,
        expected_absent_fingerprint: None,
        evidence_observation_id: Some(observation_id.to_string()),
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        extra: Extra::new(),
    };
    store.put_obligation(&obligation)
}

pub(crate) fn session_show(
    store: &Store,
    args: &SessionShowArgs,
) -> Result<prog_core::SessionTrail> {
    let mut trail = store
        .get_session(args.session_id.as_deref())?
        .ok_or_else(|| CoreError::BadArgs {
            operation: "session show".to_string(),
            reason: "no session exists; run `prog session start --goal <goal>`".to_string(),
        })?;
    let mut unavailable = 0usize;
    for event in &mut trail.events {
        let Some(cursor) = event.cursor.as_deref() else {
            continue;
        };
        match store.get_cursor(cursor) {
            Ok(_) => {
                event
                    .extra
                    .insert("cursor_status".to_string(), json!("available"));
            }
            Err(error) => {
                unavailable += 1;
                event
                    .extra
                    .insert("cursor_status".to_string(), json!(error.kind()));
            }
        }
    }
    if unavailable > 0 {
        trail.warnings.push(format!(
            "{unavailable} event cursor(s) are expired, missing, or incompatible with the current redaction policy"
        ));
    }
    Ok(trail)
}

pub(crate) fn readiness_report(store: &Store, session_id: Option<&str>) -> Result<ReadinessReport> {
    let obligations = store.list_obligations(session_id)?.obligations;
    if obligations.is_empty() {
        return Ok(ReadinessReport {
            schema: VERIFICATION_SCHEMA.to_string(),
            configured: false,
            ready: false,
            evaluations: Vec::new(),
            blockers: vec!["no verification obligations are declared for this session".to_string()],
            extra: Extra::new(),
        });
    }

    let mut evaluations = Vec::with_capacity(obligations.len());
    let mut blockers = Vec::new();
    for obligation in obligations {
        let evaluation = evaluate_obligation(store, obligation)?;
        if evaluation.obligation.required && evaluation.status != VerificationStatus::Passed {
            blockers.push(format!(
                "{}: {}",
                evaluation.obligation.id,
                evaluation.reasons.join("; ")
            ));
        }
        evaluations.push(evaluation);
    }
    Ok(ReadinessReport {
        schema: VERIFICATION_SCHEMA.to_string(),
        configured: true,
        ready: blockers.is_empty(),
        evaluations,
        blockers,
        extra: Extra::new(),
    })
}

/// Bound model-visible readiness data while preserving the full report's
/// conservative `configured` and `ready` decisions. Tail evaluations are
/// deterministic because obligations are stored in stable id order.
pub(crate) fn bound_readiness_report(
    report: &mut ReadinessReport,
    max_envelope_bytes: usize,
) -> Result<()> {
    let content_budget = max_envelope_bytes.saturating_sub(1_024);
    let total_evaluations = report.evaluations.len();
    let total_blockers = report.blockers.len();
    let mut shortened_blockers = false;
    for blocker in &mut report.blockers {
        if blocker.chars().count() > 512 {
            let prefix = blocker.chars().take(512).collect::<String>();
            *blocker = format!("{prefix}…");
            shortened_blockers = true;
        }
    }
    while serde_json::to_vec(report)?.len() > content_budget && !report.evaluations.is_empty() {
        report.evaluations.pop();
    }
    while serde_json::to_vec(report)?.len() > content_budget && !report.blockers.is_empty() {
        report.blockers.pop();
    }
    if shortened_blockers
        || report.evaluations.len() < total_evaluations
        || report.blockers.len() < total_blockers
    {
        report.extra.insert(
            "compaction".to_string(),
            json!({
                "reason": "disclosure_budget",
                "retained_evaluations": report.evaluations.len(),
                "total_evaluations": total_evaluations,
                "retained_blockers": report.blockers.len(),
                "total_blockers": total_blockers,
                "decisions_are_complete": true,
                "blocker_text_shortened": shortened_blockers
            }),
        );
    }
    while serde_json::to_vec(report)?.len() > content_budget && !report.evaluations.is_empty() {
        report.evaluations.pop();
        update_readiness_compaction(report);
    }
    while serde_json::to_vec(report)?.len() > content_budget && !report.blockers.is_empty() {
        report.blockers.pop();
        update_readiness_compaction(report);
    }
    Ok(())
}

fn update_readiness_compaction(report: &mut ReadinessReport) {
    let retained_evaluations = report.evaluations.len();
    let retained_blockers = report.blockers.len();
    if let Some(compaction) = report.extra.get_mut("compaction") {
        compaction["retained_evaluations"] = json!(retained_evaluations);
        compaction["retained_blockers"] = json!(retained_blockers);
    }
}

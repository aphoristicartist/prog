//! Session display and readiness commands.

use crate::*;

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

//! Read-back verification for mutations executed outside `prog`.

use crate::*;

pub(crate) fn begin_verification(
    store: &Store,
    args: &VerificationBeginArgs,
) -> Result<prog_core::ActionIntent> {
    let pre = store
        .get_observation(&args.pre_observation)?
        .ok_or_else(|| {
            verification_error(format!(
                "unknown pre-mutation observation '{}'",
                args.pre_observation
            ))
        })?;
    if pre.availability != EvidenceAvailability::Recoverable {
        return Err(verification_error(
            "the pre-mutation payload is not recoverable",
        ));
    }
    let pre_payload = store
        .get_payload(&pre.payload_hash)?
        .ok_or_else(|| verification_error("the pre-mutation payload is unavailable"))?;
    let source_id = args.source_id.as_deref().unwrap_or(&pre.source_id);
    let read_operation = args.read_operation.as_deref().unwrap_or(&pre.operation);
    let profile = store
        .read_profile(source_id)?
        .ok_or_else(|| CoreError::UnknownSource(source_id.to_string()))?;
    let operation = profile_operation(&profile, read_operation)?;
    ensure_safe_readback_operation(&profile, operation)?;

    let read_args = parse_json_argument(&args.read_args, "verification begin --read-args")?;
    validate_call_args(operation, &read_args)?;
    let expected_value = parse_json_argument(&args.expected, "verification begin --expected")?;
    let expected = expected_value
        .as_object()
        .ok_or_else(|| verification_error("--expected must be a JSON object"))?;
    let mut expected_changes = expected
        .iter()
        .map(|(path, value)| prog_core::ExpectedStateChange {
            path: path.clone(),
            expected: value.clone(),
        })
        .collect::<Vec<_>>();
    expected_changes.sort_by(|left, right| left.path.cmp(&right.path));
    prog_core::validate_expected_changes(pre_payload.as_value(), &expected_changes)?;
    let pre_identity_fingerprint =
        prog_core::fingerprint_readback_scalar(pre_payload.as_value(), &args.identity_path)?;
    let pre_version_fingerprint =
        prog_core::fingerprint_readback_scalar(pre_payload.as_value(), &args.version_path)?;

    let redaction = resolve_redaction(Some(&profile));
    ensure_redaction_safe(&redaction, &read_args, "read arguments")?;
    ensure_redaction_safe(
        &redaction,
        &serde_json::to_value(&expected_changes)?,
        "expected state",
    )?;

    let session = match store.get_session(None)? {
        Some(session) => session,
        None => store.start_session(None)?,
    };
    let intent_id = format!("intent_{}", uuid::Uuid::new_v4().simple());
    let obligation_id = format!("readback_{intent_id}");
    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let eventual_consistency_until = args
        .eventual_consistency_ms
        .map(|milliseconds| {
            let milliseconds = i64::try_from(milliseconds).map_err(|_| {
                verification_error("--eventual-consistency-ms exceeds the supported range")
            })?;
            let deadline = Utc::now()
                .checked_add_signed(chrono::Duration::milliseconds(milliseconds))
                .ok_or_else(|| verification_error("eventual-consistency deadline overflowed"))?;
            Ok::<_, CoreError>(deadline.to_rfc3339_opts(SecondsFormat::Millis, true))
        })
        .transpose()?;

    let obligation = VerificationObligation {
        schema: VERIFICATION_SCHEMA.to_string(),
        id: obligation_id.clone(),
        session_id: session.session_id.clone(),
        required: true,
        intended_check: format!(
            "independently read back {} {} and match the declared expected state",
            source_id, read_operation
        ),
        required_scope: "external-mutation-readback".to_string(),
        declared_by: ObligationDeclarer::User,
        expected_operation: Some(VerificationOperation::SourceOperation(
            read_operation.to_string(),
        )),
        required_state: VerificationStateRelationship::Any,
        advisory_actions: Vec::new(),
        comparison_family: pre.comparison_family.clone(),
        origin_observation_id: None,
        expected_absent_fingerprint: None,
        evidence_observation_id: None,
        created_at: created_at.clone(),
        extra: {
            let mut extra = Extra::new();
            extra.insert("action_intent_id".to_string(), json!(intent_id));
            extra
        },
    };
    let intent = prog_core::ActionIntent {
        schema: prog_core::ACTION_INTENT_SCHEMA.to_string(),
        intent_id,
        session_id: session.session_id,
        source_id: source_id.to_string(),
        read_operation: read_operation.to_string(),
        read_args,
        pre_observation_id: pre.observation_id,
        identity_path: args.identity_path.clone(),
        version_path: args.version_path.clone(),
        pre_identity_fingerprint,
        pre_version_fingerprint,
        expected_changes,
        eventual_consistency_until,
        obligation_id,
        created_at,
        extra: Extra::new(),
    };
    store.put_obligation(&obligation)?;
    store.put_action_intent(&intent)?;
    Ok(intent)
}

pub(crate) async fn readback_verification(
    store: &Store,
    lens_dir: Option<&Path>,
    args: &VerificationReadbackArgs,
    ctx: &mut InvocationContext,
) -> Result<prog_core::ReadbackVerificationReceipt> {
    let intent = store
        .get_action_intent(&args.intent_id)?
        .ok_or_else(|| verification_error(format!("unknown action intent '{}'", args.intent_id)))?;
    let profile = store
        .read_profile(&intent.source_id)?
        .ok_or_else(|| CoreError::UnknownSource(intent.source_id.clone()))?;
    let operation = profile_operation(&profile, &intent.read_operation)?;
    ensure_safe_readback_operation(&profile, operation)?;

    let call_args = CallArgs {
        source_id: intent.source_id.clone(),
        operation: intent.read_operation.clone(),
        args: String::from_utf8(prog_core::canonical_json(&intent.read_args)?)
            .map_err(|_| verification_error("stored read arguments are not valid UTF-8 JSON"))?,
        view: None,
        lens: None,
        yes: args.yes,
        no_cache: false,
        refresh: true,
        comparison_family: store
            .get_observation(&intent.pre_observation_id)?
            .and_then(|observation| observation.comparison_family),
        selection_scopes: Vec::new(),
        selection_exhaustive: false,
        pages: 1,
    };

    let call = match call_source(store, lens_dir, &call_args, ctx).await {
        Ok(call) => call,
        Err(error) => {
            return persist_receipt(
                store,
                &intent,
                ReceiptInput {
                    status: prog_core::ReadbackVerificationStatus::ReadbackFailed,
                    readback_observation_id: None,
                    mutation_response_observation_id: args.mutation_response.clone(),
                    checks: Vec::new(),
                    reasons: vec![format!("independent read-back failed: {error}")],
                    assessment: None,
                },
            );
        }
    };
    let readback_id = call
        .envelope
        .observation
        .as_ref()
        .and_then(|metadata| metadata.observation_id.clone())
        .ok_or_else(|| verification_error("the read-back produced no observation identity"))?;
    let readback_record = store
        .get_observation(&readback_id)?
        .ok_or_else(|| verification_error("the read-back observation is unavailable"))?;
    let assessment = compare_observation_ids(
        store,
        &intent.pre_observation_id,
        &readback_record.observation_id,
    )?
    .assessment;

    if call.received_error {
        return persist_receipt(
            store,
            &intent,
            ReceiptInput {
                status: prog_core::ReadbackVerificationStatus::ReadbackFailed,
                readback_observation_id: Some(readback_id),
                mutation_response_observation_id: args.mutation_response.clone(),
                checks: Vec::new(),
                reasons: vec![
                    "the independent read operation returned an upstream error".to_string(),
                ],
                assessment: Some(assessment),
            },
        );
    }

    let Some(readback_payload) = store.get_payload(&readback_record.payload_hash)? else {
        return persist_receipt(
            store,
            &intent,
            ReceiptInput {
                status: prog_core::ReadbackVerificationStatus::Unverifiable,
                readback_observation_id: Some(readback_id),
                mutation_response_observation_id: args.mutation_response.clone(),
                checks: Vec::new(),
                reasons: vec!["the read-back payload was not retained".to_string()],
                assessment: Some(assessment),
            },
        );
    };

    let mutation = match args.mutation_response.as_deref() {
        Some(observation_id) => {
            let record = store.get_observation(observation_id)?.ok_or_else(|| {
                verification_error(format!(
                    "unknown mutation-response observation '{observation_id}'"
                ))
            })?;
            if record.status.as_deref().is_some_and(precondition_status) {
                return persist_receipt(
                    store,
                    &intent,
                    ReceiptInput {
                        status: prog_core::ReadbackVerificationStatus::StalePrecondition,
                        readback_observation_id: Some(readback_id),
                        mutation_response_observation_id: args.mutation_response.clone(),
                        checks: Vec::new(),
                        reasons: vec![
                            "the mutation response reported a stale precondition".to_string(),
                        ],
                        assessment: Some(assessment),
                    },
                );
            }
            let Some(payload) = store.get_payload(&record.payload_hash)? else {
                return persist_receipt(
                    store,
                    &intent,
                    ReceiptInput {
                        status: prog_core::ReadbackVerificationStatus::Unverifiable,
                        readback_observation_id: Some(readback_id),
                        mutation_response_observation_id: args.mutation_response.clone(),
                        checks: Vec::new(),
                        reasons: vec![
                            "the supplied mutation-response payload is unavailable".to_string(),
                        ],
                        assessment: Some(assessment),
                    },
                );
            };
            Some((record, payload))
        }
        None => None,
    };
    let evaluation = prog_core::evaluate_readback(
        &intent,
        &readback_record,
        readback_payload.as_value(),
        mutation
            .as_ref()
            .map(|(record, payload)| (record, payload.as_value())),
        Utc::now(),
    );
    persist_receipt(
        store,
        &intent,
        ReceiptInput {
            status: evaluation.status,
            readback_observation_id: Some(readback_id),
            mutation_response_observation_id: args.mutation_response.clone(),
            checks: evaluation.checks,
            reasons: evaluation.reasons,
            assessment: Some(assessment),
        },
    )
}

struct ReceiptInput {
    status: prog_core::ReadbackVerificationStatus,
    readback_observation_id: Option<String>,
    mutation_response_observation_id: Option<String>,
    checks: Vec<prog_core::ReadbackCheck>,
    reasons: Vec<String>,
    assessment: Option<prog_core::ComparabilityAssessment>,
}

fn persist_receipt(
    store: &Store,
    intent: &prog_core::ActionIntent,
    input: ReceiptInput,
) -> Result<prog_core::ReadbackVerificationReceipt> {
    let receipt = prog_core::ReadbackVerificationReceipt {
        schema: prog_core::READBACK_VERIFICATION_SCHEMA.to_string(),
        receipt_id: format!("receipt_{}", uuid::Uuid::new_v4().simple()),
        intent_id: intent.intent_id.clone(),
        status: input.status,
        obligation_id: intent.obligation_id.clone(),
        pre_observation_id: intent.pre_observation_id.clone(),
        mutation_response_observation_id: input.mutation_response_observation_id,
        readback_observation_id: input.readback_observation_id.clone(),
        checks: input.checks,
        reasons: input.reasons,
        assessment: input.assessment,
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        extra: Extra::new(),
    };
    store.put_readback_receipt(&receipt)?;
    store.attach_readback_receipt(
        &intent.session_id,
        &intent.obligation_id,
        input.readback_observation_id.as_deref(),
        &receipt.receipt_id,
    )?;
    Ok(receipt)
}

fn ensure_safe_readback_operation(
    profile: &SourceProfile,
    operation: &OperationProfile,
) -> Result<()> {
    let effects = &operation.effects;
    let protocol_proves_get = profile.kind == prog_core::SourceKind::Http
        && operation
            .extra
            .get("invocation")
            .and_then(|value| value.pointer("/http/method"))
            .and_then(Value::as_str)
            .is_some_and(|method| method.eq_ignore_ascii_case("GET"));
    let descriptor_proves_read =
        prog_core::EvidenceGrade::from_extra(&effects.extra) == prog_core::EvidenceGrade::Proven;
    if !effects.read_only
        || effects.mutating
        || effects.shell
        || effects.sensitive
        || !(protocol_proves_get || descriptor_proves_read)
    {
        return Err(verification_error(
            "the read-back operation must be protocol- or descriptor-proven read-only, non-mutating, non-shell, and non-sensitive",
        ));
    }
    Ok(())
}

fn precondition_status(status: &str) -> bool {
    status == "409" || status == "412" || status.starts_with("409 ") || status.starts_with("412 ")
}

fn ensure_redaction_safe(policy: &RedactionPolicy, value: &Value, label: &str) -> Result<()> {
    let (redacted, paths) = policy.apply_persistence(value);
    if redacted != *value || !paths.is_empty() {
        return Err(verification_error(format!(
            "{label} contains data that would be redacted and cannot be persisted in an action intent"
        )));
    }
    Ok(())
}

fn verification_error(reason: impl Into<String>) -> CoreError {
    CoreError::BadArgs {
        operation: "read-back verification".to_string(),
        reason: reason.into(),
    }
}

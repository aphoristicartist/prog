//! MCP task lifecycle command.

use crate::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct McpTaskCommandOutput {
    pub(crate) schema: &'static str,
    pub(crate) observation_id: String,
    pub(crate) availability: EvidenceAvailability,
    pub(crate) payload: Value,
}

pub(crate) async fn mcp_task_command(
    store: &Store,
    command: &McpTaskCommand,
) -> Result<McpTaskCommandOutput> {
    match command {
        McpTaskCommand::Start(args) => {
            let profile = mcp_task_profile(store, &args.source_id)?;
            let operation = profile_operation(&profile, &args.operation)?.clone();
            let invocation = invocation_config(&operation, "mcp")?;
            if required_profile_string(invocation, "kind")? != "tool" {
                return Err(CoreError::BadArgs {
                    operation: args.operation.clone(),
                    reason: "only MCP tool operations can be started as tasks".to_string(),
                });
            }
            check_call(&operation, CallFlags { yes: args.yes }, &profile.trust)?;
            let tool_name = required_profile_string(invocation, "name")?;
            let call_args = parse_json_argument(&args.args, "mcp-task start --args")?;
            validate_call_args(&operation, &call_args)?;
            let result = mcp_source_from_profile(&profile)?
                .call_tool_as_task(&tool_name, &call_args, args.ttl_ms)
                .await?;
            record_mcp_task_observation(
                store,
                &profile,
                "mcp_task.start",
                serde_json::to_value(&result)?,
                Some(task_status(&result)?),
                Some(result.provenance.duration_ms),
                Some(&result.task.task_id),
                args.parent_observation.clone(),
            )
        }
        McpTaskCommand::Get(args) => {
            let profile = mcp_task_profile(store, &args.source_id)?;
            match mcp_source_from_profile(&profile)?
                .get_task(&args.task_id)
                .await
            {
                Ok(result) => record_mcp_task_observation(
                    store,
                    &profile,
                    "mcp_task.get",
                    serde_json::to_value(&result)?,
                    Some(task_status(&result)?),
                    Some(result.provenance.duration_ms),
                    Some(&args.task_id),
                    args.parent_observation.clone(),
                ),
                Err(error) if mcp_task_result_unavailable(&error) => {
                    record_mcp_task_unavailable_observation(
                        store,
                        &profile,
                        "mcp_task.get",
                        &error,
                        &args.task_id,
                        args.parent_observation.clone(),
                    )
                }
                Err(error) => Err(error),
            }
        }
        McpTaskCommand::Result(args) => {
            let profile = mcp_task_profile(store, &args.source_id)?;
            match mcp_source_from_profile(&profile)?
                .get_task_result(&args.task_id)
                .await
            {
                Ok(result) => record_mcp_task_observation(
                    store,
                    &profile,
                    "mcp_task.result",
                    serde_json::to_value(&result)?,
                    None,
                    Some(result.provenance.duration_ms),
                    Some(&args.task_id),
                    args.parent_observation.clone(),
                ),
                Err(error) if mcp_task_result_unavailable(&error) => {
                    record_mcp_task_unavailable_observation(
                        store,
                        &profile,
                        "mcp_task.result",
                        &error,
                        &args.task_id,
                        args.parent_observation.clone(),
                    )
                }
                Err(error) => Err(error),
            }
        }
        McpTaskCommand::Cancel(args) => {
            let profile = mcp_task_profile(store, &args.source_id)?;
            match mcp_source_from_profile(&profile)?
                .cancel_task(&args.task_id)
                .await
            {
                Ok(result) => record_mcp_task_observation(
                    store,
                    &profile,
                    "mcp_task.cancel",
                    serde_json::to_value(&result)?,
                    Some(task_status(&result)?),
                    Some(result.provenance.duration_ms),
                    Some(&args.task_id),
                    args.parent_observation.clone(),
                ),
                Err(error) if mcp_task_result_unavailable(&error) => {
                    record_mcp_task_unavailable_observation(
                        store,
                        &profile,
                        "mcp_task.cancel",
                        &error,
                        &args.task_id,
                        args.parent_observation.clone(),
                    )
                }
                Err(error) => Err(error),
            }
        }
    }
}

/// A task reference can cease to resolve independently of the original tool
/// call. Preserve that attempted lifecycle transition as unavailable evidence
/// instead of treating a protocol, transport, or timeout failure as a result.
pub(crate) fn mcp_task_result_unavailable(error: &CoreError) -> bool {
    matches!(
        error,
        CoreError::McpTimeout { .. }
            | CoreError::McpTransport { .. }
            | CoreError::McpProtocol { .. }
    )
}

fn mcp_task_profile(store: &Store, source_id: &str) -> Result<SourceProfile> {
    let profile = store
        .read_profile(source_id)?
        .ok_or_else(|| CoreError::UnknownSource(source_id.to_string()))?;
    if profile.kind != prog_core::SourceKind::Mcp {
        return Err(CoreError::BadArgs {
            operation: "mcp-task".to_string(),
            reason: format!("source '{source_id}' is not an MCP source"),
        });
    }
    apply_profile_disclosure_budget(&profile)?;
    Ok(profile)
}

fn task_status(result: &McpTaskResult) -> Result<String> {
    serde_json::to_value(result.task.status)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| CoreError::BadArgs {
            operation: "mcp-task".to_string(),
            reason: "task status was not serializable".to_string(),
        })
}

#[allow(clippy::too_many_arguments)]
fn record_mcp_task_observation(
    store: &Store,
    profile: &SourceProfile,
    operation: &str,
    value: Value,
    status: Option<String>,
    duration_ms: Option<u64>,
    task_id: Option<&str>,
    parent_id: Option<String>,
) -> Result<McpTaskCommandOutput> {
    record_mcp_task_observation_with_availability(
        store,
        profile,
        operation,
        value,
        status,
        duration_ms,
        task_id,
        parent_id,
        false,
    )
}

pub(crate) fn record_mcp_task_unavailable_observation(
    store: &Store,
    profile: &SourceProfile,
    operation: &str,
    error: &CoreError,
    task_id: &str,
    parent_id: Option<String>,
) -> Result<McpTaskCommandOutput> {
    record_mcp_task_observation_with_availability(
        store,
        profile,
        operation,
        json!({
            "status": "unavailable",
            "error": {
                "kind": error.kind(),
                "message": error.to_string(),
                "hint": error.hint(),
            },
        }),
        Some("unavailable".to_string()),
        None,
        Some(task_id),
        parent_id,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn record_mcp_task_observation_with_availability(
    store: &Store,
    profile: &SourceProfile,
    operation: &str,
    value: Value,
    status: Option<String>,
    duration_ms: Option<u64>,
    task_id: Option<&str>,
    parent_id: Option<String>,
    unavailable: bool,
) -> Result<McpTaskCommandOutput> {
    let redacted = RawPayload::new(value).redact(&resolve_redaction(Some(profile)));
    let payload = redacted.payload;
    let payload_bytes = json_len_u64(payload.as_value())?;
    let payload_hash = store.put_payload(&payload)?;
    let (availability, mut capture) = if unavailable {
        (
            EvidenceAvailability::Unavailable,
            CaptureCompleteness::unavailable(payload_bytes),
        )
    } else {
        complete_capture(payload_bytes, true, !redacted.redacted_paths.is_empty())
    };
    if !unavailable {
        capture.budget = CaptureBudget::default();
    }
    let task_ref = task_id
        .map(|task_id| {
            Store::cache_key(
                &profile.id,
                "mcp_task_reference",
                &json!({"task_id": task_id}),
            )
        })
        .transpose()?;
    let invocation_fingerprint = Store::cache_key(
        &profile.id,
        operation,
        &json!({"task_ref": task_ref, "parent": parent_id.clone()}),
    )?;
    let mut lineage = prog_core::ObservationLineage {
        parent_id,
        ..Default::default()
    };
    if let Some(task_ref) = &task_ref {
        lineage
            .extra
            .insert("mcp_task_ref".to_string(), json!(task_ref));
    }
    let observation_id = store
        .record_observation(NewObservation {
            payload_hash,
            availability,
            invocation_fingerprint,
            source_id: profile.id.clone(),
            operation: operation.to_string(),
            duration_ms,
            status,
            capture,
            redacted: !redacted.redacted_paths.is_empty(),
            lineage,
            provenance: Some(call_provenance(
                "mcp-task",
                None,
                duration_ms,
                json!({"kind": "mcp_task", "task_ref": task_ref}),
            )),
            ..NewObservation::default()
        })?
        .observation_id;
    Ok(McpTaskCommandOutput {
        schema: DISCLOSURE_SCHEMA,
        observation_id,
        availability,
        payload: payload.as_value().clone(),
    })
}

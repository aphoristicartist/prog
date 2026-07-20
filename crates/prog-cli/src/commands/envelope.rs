//! Shared evidence capture, envelope construction, and disclosure compaction.

use crate::*;

pub(crate) fn evidence_ref(input: EvidenceRefInput<'_>) -> EvidenceRef {
    let omitted_in_scope = input
        .omitted
        .iter()
        .filter(|region| omission_intersects_path(input.path, &region.path))
        .collect::<Vec<_>>();
    let availability = input
        .observation
        .map(|observation| observation.availability)
        .unwrap_or(EvidenceAvailability::Unavailable);
    let capture = input
        .observation
        .map(|observation| observation.capture.clone())
        .unwrap_or_else(|| CaptureCompleteness::unavailable(0));
    let redacted = input.redacted_paths > 0
        || value_contains_redaction(input.value)
        || omitted_in_scope
            .iter()
            .any(|region| region.reason == OmissionReason::Redacted);
    let lossy = omitted_in_scope
        .iter()
        .any(|region| region.reason != OmissionReason::Redacted);
    let redacted_slice_sha256 = canonical_json(input.value)
        .ok()
        .map(|bytes| hex_sha256(bytes.as_slice()));
    let cache_status = input.cache.map(|cache| cache.status);
    let age_seconds = input.cache.and_then(|cache| cache.age_seconds);
    let stale = cache_is_stale(input.cache);
    EvidenceRef {
        schema: "prog.evidence_ref".to_string(),
        source_id: input.source_id.to_string(),
        operation: input.operation.to_string(),
        cursor: input.cursor.map(str::to_string),
        path: input.path.to_string(),
        uri: input
            .cursor
            .map(|cursor| format!("prog://{cursor}#{}", input.path)),
        captured_at: input
            .provenance
            .map(|provenance| provenance.captured_at.clone()),
        cache_status,
        age_seconds,
        expires_at: input.cache.and_then(|cache| cache.expires_at.clone()),
        stale,
        availability,
        capture,
        redacted,
        lossy,
        redacted_slice_sha256,
        extra: Extra::new(),
    }
}

fn omission_intersects_path(path: &str, omitted_path: &str) -> bool {
    prog_core::pointer::is_within(path, omitted_path).unwrap_or(false)
        || prog_core::pointer::is_within(omitted_path, path).unwrap_or(false)
}

fn value_contains_redaction(value: &Value) -> bool {
    match value {
        Value::String(value) => value.contains("[REDACTED:"),
        Value::Array(values) => values.iter().any(value_contains_redaction),
        Value::Object(map) => map.values().any(value_contains_redaction),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_capture(
    store: &Store,
    payload_hash: String,
    availability: EvidenceAvailability,
    capture: CaptureCompleteness,
    invocation_fingerprint: String,
    source_id: String,
    operation: String,
    comparison_family: Option<String>,
    selection: SelectionCoverage,
    provenance: Option<CallProvenance>,
    cache_key: Option<String>,
    redacted: bool,
    provider: Option<String>,
    parser: Option<String>,
    lens: Option<&LensManifest>,
    source_state: Option<SourceStateToken>,
) -> Result<String> {
    let duration_ms = provenance.as_ref().and_then(|item| item.duration_ms);
    let status = provenance.as_ref().and_then(|item| item.status.clone());
    let captured_at = provenance.as_ref().map(|item| item.captured_at.clone());
    Ok(store
        .record_observation(NewObservation {
            payload_hash,
            availability,
            invocation_fingerprint,
            source_id,
            operation,
            comparison_family,
            selection,
            captured_at,
            duration_ms,
            status,
            capture,
            redacted,
            provider,
            parser,
            lens: lens.map(|item| item.id.clone()),
            source_state,
            provenance,
            cache_key,
            ..NewObservation::default()
        })?
        .observation_id)
}

/// Transport/adapter identity for the [`SourceKind`] that produced a capture.
/// Distinct from `parser` (which format interpreted the bytes) and `lens`
/// (which view was applied): this is which adapter fetched them, and it is
/// the coarsest normalization-compatibility signal available before a
/// registered-provider system (#135) exists.
pub(crate) fn source_kind_provider(kind: prog_core::SourceKind) -> String {
    match kind {
        prog_core::SourceKind::Http => "http",
        prog_core::SourceKind::Cli => "cli",
        prog_core::SourceKind::Mcp => "mcp",
    }
    .to_string()
}

pub(crate) fn selection_coverage(scopes: &[String], exhaustive: bool) -> SelectionCoverage {
    let scopes = scopes
        .iter()
        .map(|scope| scope.trim())
        .filter(|scope| !scope.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    SelectionCoverage {
        scopes,
        exhaustive,
        extra: Extra::new(),
    }
}

pub(crate) fn complete_capture(
    stored_bytes: u64,
    persisted: bool,
    redacted: bool,
) -> (EvidenceAvailability, CaptureCompleteness) {
    let availability = if !persisted {
        EvidenceAvailability::MetadataOnly
    } else if redacted {
        EvidenceAvailability::Redacted
    } else {
        EvidenceAvailability::Recoverable
    };
    let mut capture = CaptureCompleteness::complete(stored_bytes);
    if availability != EvidenceAvailability::Recoverable {
        capture.can_prove_absence = false;
        capture.stop_reason = if redacted {
            CaptureStopReason::Redacted
        } else {
            CaptureStopReason::StorageLimit
        };
    }
    (availability, capture)
}

pub(crate) fn adapter_capture(
    provenance: Option<&CallProvenance>,
    payload: &Value,
    stored_bytes: u64,
    persisted: bool,
    redacted: bool,
) -> (EvidenceAvailability, CaptureCompleteness) {
    let adapter = provenance
        .and_then(|item| item.extra.get("adapter"))
        .and_then(Value::as_object);
    let generic_truncated = adapter
        .and_then(|item| item.get("truncated"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cli_truncated = adapter.is_some_and(|item| {
        ["stdout_truncated", "stderr_truncated"]
            .into_iter()
            .any(|field| item.get(field).and_then(Value::as_bool).unwrap_or(false))
    });
    if cli_truncated {
        return cli_adapter_capture(
            adapter.expect("CLI truncation requires adapter provenance"),
            payload,
            stored_bytes,
        );
    }
    if generic_truncated {
        let response_bytes = adapter
            .and_then(|item| item.get("response_bytes"))
            .and_then(Value::as_u64);
        let mcp_response = adapter.is_some_and(|item| item.contains_key("server_command"));
        let (total_bytes, captured_bytes, stop_reason) = if mcp_response {
            // MCP reports the complete response size before it projects the
            // bounded preview, so this is retention loss rather than a
            // transport capture limit.
            (
                response_bytes,
                response_bytes.unwrap_or(stored_bytes),
                CaptureStopReason::StorageLimit,
            )
        } else {
            // HTTP reports bytes read from the bounded body, but has no
            // trustworthy total once the body limit interrupts the stream.
            (
                None,
                response_bytes.unwrap_or(stored_bytes),
                CaptureStopReason::ByteLimit,
            )
        };
        return (
            EvidenceAvailability::CaptureTruncated,
            CaptureCompleteness {
                total_bytes,
                captured_bytes,
                stored_bytes,
                stop_reason,
                budget: CaptureBudget::default(),
                affected: vec![CaptureScope {
                    scope: "body".to_string(),
                    total_bytes,
                    captured_bytes,
                    stop_reason,
                    extra: Extra::new(),
                }],
                can_prove_absence: false,
                extra: Extra::new(),
            },
        );
    }
    complete_capture(stored_bytes, persisted, redacted)
}

pub(crate) fn cli_adapter_capture(
    adapter: &Map<String, Value>,
    payload: &Value,
    stored_bytes: u64,
) -> (EvidenceAvailability, CaptureCompleteness) {
    let mut total_bytes = 0u64;
    let mut captured_bytes = 0u64;
    let mut affected = Vec::new();
    for stream in ["stdout", "stderr"] {
        let total = adapter
            .get(&format!("{stream}_bytes"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let truncated = adapter
            .get(&format!("{stream}_truncated"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let captured = if truncated {
            cli_stream_captured_bytes(adapter, payload, stream).unwrap_or(0)
        } else {
            total
        };
        total_bytes = total_bytes.saturating_add(total);
        captured_bytes = captured_bytes.saturating_add(captured);
        if truncated {
            affected.push(CaptureScope {
                scope: stream.to_string(),
                total_bytes: Some(total),
                captured_bytes: captured,
                stop_reason: CaptureStopReason::ByteLimit,
                extra: Extra::new(),
            });
        }
    }
    (
        EvidenceAvailability::CaptureTruncated,
        CaptureCompleteness {
            total_bytes: Some(total_bytes),
            captured_bytes,
            stored_bytes,
            stop_reason: CaptureStopReason::ByteLimit,
            budget: CaptureBudget::default(),
            affected,
            can_prove_absence: false,
            extra: Extra::new(),
        },
    )
}

pub(crate) fn cli_stream_captured_bytes(
    adapter: &Map<String, Value>,
    payload: &Value,
    stream: &str,
) -> Option<u64> {
    let output = payload
        .get(stream)
        .or_else(|| (stream == "stdout").then_some(payload));
    output
        .and_then(|value| value.get("byte_count"))
        .and_then(Value::as_u64)
        .or_else(|| {
            adapter
                .get("diagnostics")
                .and_then(|value| value.get(stream))
                .and_then(|value| value.get("byte_count"))
                .and_then(Value::as_u64)
        })
}

pub(crate) fn run_capture_completeness(
    stdout: &RunCapture,
    stderr: &RunCapture,
    stored_bytes: u64,
    redacted: bool,
    status: &RunProcessStatus,
) -> (EvidenceAvailability, CaptureCompleteness) {
    let truncated = stdout.truncated || stderr.truncated;
    let captured_bytes = stdout.bytes.len().saturating_add(stderr.bytes.len()) as u64;
    let total_bytes = stdout.total_bytes.saturating_add(stderr.total_bytes) as u64;
    let reason = if matches!(status, RunProcessStatus::TimedOut) {
        CaptureStopReason::Timeout
    } else if truncated {
        CaptureStopReason::ByteLimit
    } else if redacted {
        CaptureStopReason::Redacted
    } else {
        CaptureStopReason::Complete
    };
    let availability = if matches!(status, RunProcessStatus::TimedOut) || truncated {
        EvidenceAvailability::CaptureTruncated
    } else if redacted {
        EvidenceAvailability::Redacted
    } else {
        EvidenceAvailability::Recoverable
    };
    (
        availability,
        CaptureCompleteness {
            total_bytes: Some(total_bytes),
            captured_bytes,
            stored_bytes,
            stop_reason: reason,
            budget: CaptureBudget::default(),
            affected: vec![
                CaptureScope {
                    scope: "stdout".to_string(),
                    total_bytes: Some(stdout.total_bytes as u64),
                    captured_bytes: stdout.bytes.len() as u64,
                    stop_reason: if stdout.truncated {
                        CaptureStopReason::ByteLimit
                    } else {
                        CaptureStopReason::Complete
                    },
                    extra: Extra::new(),
                },
                CaptureScope {
                    scope: "stderr".to_string(),
                    total_bytes: Some(stderr.total_bytes as u64),
                    captured_bytes: stderr.bytes.len() as u64,
                    stop_reason: if stderr.truncated {
                        CaptureStopReason::ByteLimit
                    } else {
                        CaptureStopReason::Complete
                    },
                    extra: Extra::new(),
                },
            ],
            can_prove_absence: !matches!(status, RunProcessStatus::TimedOut)
                && !truncated
                && !redacted,
            extra: Extra::new(),
        },
    )
}

pub(crate) fn source_state_from_provenance(
    kind: prog_core::SourceKind,
    source_id: &str,
    operation: &str,
    invocation: &Value,
    provenance: &CallProvenance,
) -> Result<Option<SourceStateToken>> {
    if kind != prog_core::SourceKind::Http {
        return Ok(None);
    }
    let headers = provenance
        .extra
        .get("adapter")
        .and_then(|adapter| adapter.get("selected_headers"))
        .and_then(Value::as_object)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .as_str()
                        .map(|value| (name.to_ascii_lowercase(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    http_source_state(
        source_id,
        operation,
        invocation,
        &headers,
        &provenance.captured_at,
    )
}

pub(crate) fn cursor_lens_extra(lens: Option<&LensManifest>) -> Extra {
    let mut extra = Extra::new();
    if let Some(lens) = lens {
        extra.insert("lens_id".to_string(), json!(lens.id));
    }
    extra
}

pub(crate) fn cursor_for_projection(
    store: &Store,
    input: CursorInput<'_>,
) -> Result<Option<String>> {
    if !input.may_cache {
        return Ok(None);
    }
    // Validate the projected root before minting the cursor. Cacheable calls
    // always get a cursor so inspect/search/evidence work even when the first
    // preview happens to contain the entire small payload.
    project_with_lens(
        input.payload,
        input.root_path,
        input.slice,
        &PreviewPolicy::default(),
        input.lens,
    )?;
    Ok(Some(store.create_cursor_with_extra(
        input.cache_key,
        input.source_id,
        input.operation,
        input.root_path,
        ttl_seconds(input.cache),
        cursor_lens_extra(input.lens),
    )?))
}

pub(crate) fn envelope_for_payload(
    store: &Store,
    input: EnvelopeInput,
    cursor: Option<String>,
    max_envelope_bytes: usize,
) -> Result<DisclosureEnvelope> {
    let observation_record = input
        .observation_id
        .as_deref()
        .map(|id| store.get_observation(id))
        .transpose()?
        .flatten();
    let mut policy = PreviewPolicy {
        max_envelope_bytes,
        ..PreviewPolicy::default()
    };
    let mut last = None;
    let findings = ranked_findings_with_lens(
        input.payload.as_value(),
        &FindingOptions {
            goal: None,
            cursor: cursor.clone(),
            scope_path: Some(input.root_path.clone()),
            limit: 3,
            hints: CommandHintConfig::NAV_ALL,
            workspace_root: std::env::current_dir().ok(),
            identity: FindingIdentityContext {
                provider: observation_record
                    .as_ref()
                    .and_then(|observation| observation.provider.clone()),
                parser: observation_record
                    .as_ref()
                    .and_then(|observation| observation.parser.clone()),
                lens: observation_record
                    .as_ref()
                    .and_then(|observation| observation.lens.clone()),
            },
        },
        input.lens.as_ref(),
    )
    .unwrap_or_default();
    for _ in 0..16 {
        let lens_projection = project_with_lens(
            &input.payload,
            &input.root_path,
            &input.slice,
            &policy,
            input.lens.as_ref(),
        )?;
        let mut envelope = make_envelope(
            &input,
            lens_projection,
            cursor.clone(),
            findings.clone(),
            observation_record.as_ref(),
        );
        let bytes = finalize_envelope_bytes(&mut envelope)?;
        if bytes <= policy.max_envelope_bytes {
            return Ok(envelope);
        }
        last = Some(envelope);
        let next = shrink_policy(&policy);
        if next == policy {
            break;
        }
        policy = next;
    }
    let mut envelope = last.expect("envelope loop always builds at least once");
    if serde_json::to_vec(&envelope)?.len() > policy.max_envelope_bytes {
        envelope.schema_hints.clear();
        envelope.provenance = None;
        envelope.findings.truncate(1);
        envelope.next_actions.truncate(4);
        envelope.omitted.truncate(8);
        envelope.warnings.truncate(4);
        envelope
            .warnings
            .push("envelope metadata compacted to enforce max_envelope_bytes".to_string());
        finalize_envelope_bytes(&mut envelope)?;
    }
    if serde_json::to_vec(&envelope)?.len() > policy.max_envelope_bytes {
        envelope.data_preview =
            Value::String("«preview omitted to enforce envelope budget»".to_string());
        envelope.omitted.clear();
        envelope.next_actions.clear();
        envelope.warnings.truncate(1);
        finalize_envelope_bytes(&mut envelope)?;
    }
    compact_envelope_to_budget(&mut envelope, max_envelope_bytes)?;
    Ok(envelope)
}

pub(crate) fn make_envelope(
    input: &EnvelopeInput,
    lens_projection: prog_core::LensProjection,
    cursor: Option<String>,
    findings: Vec<prog_core::Finding>,
    observation_record: Option<&prog_core::ObservationRecord>,
) -> DisclosureEnvelope {
    let projection = lens_projection.projection;
    let preview = projection.preview;
    let omitted = projection.omitted;
    let observation = observation_metadata(input, &omitted, cursor.as_deref(), observation_record);
    let mut next_actions = lens_projection
        .next_actions
        .into_iter()
        .filter(|action| cursor.is_some() || action.kind != "expand")
        .collect::<Vec<_>>();
    let generated_next_actions = cursor
        .as_ref()
        .map(|cursor| {
            expansion_next_actions(
                Some(cursor.as_str()),
                input.next_action_operation.as_deref(),
                &omitted,
                10,
            )
        })
        .unwrap_or_default();
    for action in generated_next_actions {
        let duplicate = next_actions
            .iter()
            .any(|existing| existing.kind == action.kind && existing.path == action.path);
        if !duplicate {
            next_actions.push(action);
        }
    }
    for action in input.additional_next_actions.clone() {
        let duplicate = next_actions
            .iter()
            .any(|existing| existing.kind == action.kind && existing.path == action.path);
        if !duplicate {
            next_actions.push(action);
        }
    }
    next_actions.truncate(10);
    let mut extra = Extra::new();
    if let Some(lens) = &input.lens {
        extra.insert(
            "lens".to_string(),
            json!({
                "id": lens.id
            }),
        );
    }
    if let Some(cursor) = cursor.as_deref() {
        let evidence_path = input.slice.path.as_deref().unwrap_or(&input.root_path);
        extra.insert(
            "evidence_ref".to_string(),
            serde_json::to_value(evidence_ref(EvidenceRefInput {
                source_id: &input.source_id,
                operation: &input.operation,
                cursor: Some(cursor),
                path: evidence_path,
                value: &preview,
                observation: observation_record,
                provenance: input.provenance.as_ref(),
                cache: input.cache.as_ref(),
                omitted: &omitted,
                redacted_paths: input.redacted_paths,
            }))
            .unwrap_or(Value::Null),
        );
    }
    DisclosureEnvelope {
        schema: DISCLOSURE_SCHEMA.to_string(),
        source_id: Some(input.source_id.clone()),
        operation: Some(input.operation.clone()),
        summary: Summary {
            kind: value_kind(input.payload.as_value()).to_string(),
            item_count: item_count(input.payload.as_value()),
            preview_count: item_count(&preview),
            payload_bytes: input.payload_bytes,
            approx_tokens: 0,
            envelope_bytes: None,
            extra: Extra::new(),
        },
        data_preview: preview,
        schema_hints: input.schema_hints.clone(),
        omitted,
        findings,
        cursor,
        next_actions,
        provenance: input.provenance.clone(),
        cache: input.cache.clone(),
        observation: Some(observation),
        warnings: input.warnings.clone(),
        extra,
    }
}

pub(crate) fn observation_metadata(
    input: &EnvelopeInput,
    omitted: &[OmittedRegion],
    cursor: Option<&str>,
    observation_record: Option<&prog_core::ObservationRecord>,
) -> ObservationMetadata {
    let redacted_omissions = omitted
        .iter()
        .filter(|region| region.reason == OmissionReason::Redacted)
        .count();
    let redacted_count = input.redacted_paths.max(redacted_omissions);
    let truncated = omitted
        .iter()
        .any(|region| region.reason != OmissionReason::Redacted);
    let effective_root_path = input
        .slice
        .path
        .as_deref()
        .unwrap_or(&input.root_path)
        .to_string();
    let path_scoped = !effective_root_path.is_empty()
        || input.slice.path.is_some()
        || !input.slice.fields.is_empty()
        || !input.slice.omit.is_empty();
    let preview_complete = omitted.is_empty();
    let completeness_status = if truncated {
        "truncated"
    } else if redacted_count > 0 {
        "redacted"
    } else if !omitted.is_empty() {
        "partial"
    } else {
        "complete"
    };
    let cache_status = input.cache.as_ref().map(|cache| cache.status);
    let cached = matches!(cache_status, Some(CacheStatus::Stored | CacheStatus::Hit));
    let age_seconds = input.cache.as_ref().and_then(|cache| cache.age_seconds);
    let stale = cache_is_stale(input.cache.as_ref());
    let sensitive_cache_disabled = matches!(cache_status, Some(CacheStatus::Skipped))
        && input
            .effects
            .as_ref()
            .is_some_and(|effects| effects.sensitive);
    let mut metadata_extra = Extra::new();
    // Surface value-scan lossiness: when low-confidence secret-like shapes were
    // observed (and, by default, preserved verbatim), OR-fold that uncertainty
    // into the parser's `lossy`/`confidence` AND emit a disambiguating
    // `value_scan` extra entry so the cause is inspectable. When nothing was
    // observed, behavior is byte-identical to today.
    let parser_value = match (&input.observation_parser, &input.value_scan) {
        (Some(parser), Some(scan)) if scan.lossy() => {
            let mut folded = parser.clone();
            if let Some(obj) = folded.as_object_mut() {
                obj.insert("lossy".to_string(), Value::Bool(true));
                if let Some(confidence) = obj.get("confidence").and_then(Value::as_f64) {
                    obj.insert("confidence".to_string(), Value::from(confidence.min(0.6)));
                }
            }
            Some(folded)
        }
        (Some(parser), _) => Some(parser.clone()),
        _ => None,
    };
    if let Some(parser) = parser_value {
        metadata_extra.insert("parser".to_string(), parser);
    }
    if let Some(scan) = input.value_scan.as_ref().filter(|scan| scan.lossy()) {
        metadata_extra.insert(
            "value_scan".to_string(),
            json!({
                "lossy": true,
                "high_confidence_count": scan.high_confidence_redactions,
                "low_confidence_count": scan.low_confidence_observations,
            }),
        );
    }
    ObservationMetadata {
        observation_id: input.observation_id.clone(),
        completeness: ObservationCompleteness {
            status: completeness_status.to_string(),
            preview_complete,
            path_scoped,
            truncated,
            redacted: redacted_count > 0,
            omitted_count: omitted.len().try_into().unwrap_or(u64::MAX),
            redacted_count: redacted_count.try_into().unwrap_or(u64::MAX),
            root_path: effective_root_path,
            extra: Extra::new(),
        },
        freshness: ObservationFreshness {
            captured_at: input
                .provenance
                .as_ref()
                .map(|provenance| provenance.captured_at.clone()),
            age_seconds,
            expires_at: input
                .cache
                .as_ref()
                .and_then(|cache| cache.expires_at.clone()),
            stale_after_seconds: input.cache.as_ref().and_then(|cache| cache.ttl_seconds),
            stale,
            refresh_recommended: stale,
            extra: Extra::new(),
        },
        trust: ObservationTrust {
            profile_backed: !matches!(input.source_id.as_str(), "observe" | "prog"),
            source_kind: input.source_kind.clone(),
            adapter_provenance: input
                .provenance
                .as_ref()
                .is_some_and(|provenance| provenance.extra.contains_key("adapter")),
            provenance_status: input
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.status.clone()),
            extra: {
                let mut trust_extra = Extra::new();
                // Surface the graded-evidence auto-upgrade provenance: when a
                // *proven* read-only op had its confirmation relaxed for this
                // call, record the evidence chain (grade + reason) so the
                // decision is inspectable. The relaxed EffectSet (carrying its
                // own extra["auto_upgrade"] stamp) flows to safety.effects.
                if let Some(reason) = &input.auto_upgrade_audit {
                    let grade = input
                        .effects
                        .as_ref()
                        .map(|effects| EvidenceGrade::from_extra(&effects.extra).as_str())
                        .unwrap_or("proven");
                    trust_extra.insert(
                        "auto_upgrade".to_string(),
                        json!({
                            "grade": grade,
                            "relaxed_requires_confirmation": true,
                            "reason": reason,
                        }),
                    );
                }
                trust_extra
            },
        },
        safety: ObservationSafety {
            redacted_before_persistence: redacted_count > 0,
            redacted_paths: redacted_count.try_into().unwrap_or(u64::MAX),
            sensitive_cache_disabled,
            cache_disabled_reason: input.cache_disabled_reason.clone(),
            effects: input.effects.clone(),
            extra: Extra::new(),
        },
        payload: ObservationPayloadStatus {
            cache_status,
            cached,
            expandable: cursor.is_some(),
            payload_bytes: input.payload_bytes,
            extra: Extra::new(),
        },
        availability: observation_record
            .map(|record| record.availability)
            .unwrap_or(EvidenceAvailability::Unavailable),
        capture: Some(observation_record.map_or_else(
            || CaptureCompleteness {
                total_bytes: None,
                captured_bytes: 0,
                stored_bytes: input.payload_bytes,
                stop_reason: CaptureStopReason::Unavailable,
                budget: CaptureBudget::unavailable(),
                affected: Vec::new(),
                can_prove_absence: false,
                extra: Extra::new(),
            },
            |record| record.capture.clone(),
        )),
        extra: metadata_extra,
    }
}

pub(crate) fn finalize_envelope_bytes(envelope: &mut DisclosureEnvelope) -> Result<usize> {
    // Both fields describe the delivered JSON, including their own encoded
    // digits. Iterate to the small fixed point rather than estimating from
    // the much larger cached payload.
    for _ in 0..8 {
        let bytes = serde_json::to_vec(envelope)?.len();
        let envelope_bytes = bytes.try_into().unwrap_or(u64::MAX);
        let approx_tokens = envelope_bytes.saturating_add(3) / 4;
        if envelope.summary.envelope_bytes == Some(envelope_bytes)
            && envelope.summary.approx_tokens == approx_tokens
        {
            return Ok(bytes);
        }
        envelope.summary.envelope_bytes = Some(envelope_bytes);
        envelope.summary.approx_tokens = approx_tokens;
    }
    Err(CoreError::Storage(
        "envelope size accounting did not converge".to_string(),
    ))
}

pub(crate) fn compact_envelope_to_budget(
    envelope: &mut DisclosureEnvelope,
    max_envelope_bytes: usize,
) -> Result<()> {
    let budget = max_envelope_bytes;
    while serde_json::to_vec(envelope)?.len() > budget && !envelope.findings.is_empty() {
        envelope.findings.pop();
    }
    if serde_json::to_vec(envelope)?.len() > budget
        && let Some(recipe) = envelope
            .extra
            .get_mut("recipe")
            .and_then(Value::as_object_mut)
    {
        recipe.remove("expanded_commands");
    }
    if serde_json::to_vec(envelope)?.len() > budget {
        envelope.data_preview = json!("preview omitted to enforce envelope budget");
        envelope.omitted.truncate(4);
        envelope.next_actions.truncate(4);
        envelope.warnings.truncate(2);
    }
    if serde_json::to_vec(envelope)?.len() > budget {
        // Keep the observation identity and cursor, which are the recovery
        // path for the payload, while dropping derivable presentation detail.
        envelope.provenance = None;
        envelope.cache = None;
        envelope.schema_hints.clear();
        envelope.extra.clear();
        envelope.omitted.truncate(1);
        envelope.next_actions.truncate(1);
        envelope.warnings.truncate(1);
    }
    finalize_envelope_bytes(envelope)?;
    Ok(())
}

/// Re-enforce `max_envelope_bytes` after the pagination `extra` block is
/// appended. The per-page `pages[]` index and the `merged_shape` grow with page
/// count and schema width, so a many-page or wide-shape call could push the
/// final envelope past the 16 KiB ceiling even though page 1 was bounded.
/// Progressively drop `pages[]` then `merged_shape` (keeping the tiny scalar
/// counters) until the serialized envelope fits, recording a warning each time
/// (invariant I11: pagination never escapes the envelope budget).
pub(crate) fn compact_pagination_extra_to_budget(
    envelope: &mut DisclosureEnvelope,
    max_envelope_bytes: usize,
) -> Result<()> {
    let budget = max_envelope_bytes;
    if serde_json::to_vec(envelope)?.len() <= budget {
        return Ok(());
    }
    let dropped_pages = envelope
        .extra
        .get_mut("pagination")
        .and_then(Value::as_object_mut)
        .is_some_and(|pagination| pagination.remove("pages").is_some());
    if dropped_pages {
        envelope
            .warnings
            .push("pagination page index compacted to enforce max_envelope_bytes".to_string());
    }
    if serde_json::to_vec(envelope)?.len() <= budget {
        finalize_envelope_bytes(envelope)?;
        return Ok(());
    }
    let dropped_shape = envelope
        .extra
        .get_mut("pagination")
        .and_then(Value::as_object_mut)
        .is_some_and(|pagination| pagination.remove("merged_shape").is_some());
    if dropped_shape {
        envelope
            .warnings
            .push("pagination merged shape compacted to enforce max_envelope_bytes".to_string());
    }
    finalize_envelope_bytes(envelope)?;
    Ok(())
}

pub(crate) fn shrink_policy(policy: &PreviewPolicy) -> PreviewPolicy {
    PreviewPolicy {
        array_items: halve_to_zero(policy.array_items),
        object_fields: halve_to_zero(policy.object_fields),
        string_chars: halve_to_zero(policy.string_chars).max(16),
        depth: policy.depth.saturating_sub(1),
        node_budget: halve_to_zero(policy.node_budget).max(1),
        max_envelope_bytes: policy.max_envelope_bytes,
    }
}

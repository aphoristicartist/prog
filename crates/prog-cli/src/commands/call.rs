//! Source call execution command.

use crate::*;

pub(crate) async fn call_source(
    store: &Store,
    lens_dir: &Path,
    args: &CallArgs,
) -> Result<CallSourceResult> {
    let profile = store
        .read_profile(&args.source_id)?
        .ok_or_else(|| CoreError::UnknownSource(args.source_id.clone()))?;
    apply_profile_disclosure_budget(&profile)?;
    let operation = profile_operation(&profile, &args.operation)?.clone();
    let call_args = parse_json_argument(&args.args, "call --args")?;
    validate_call_args(&operation, &call_args)?;
    // check_call runs trust auto-upgrade internally and returns the EFFECTIVE
    // (possibly relaxed) effect set plus the audit note; both flow into the
    // envelope so the recorded observation reflects the policy actually applied
    // and the upgrade is inspectable.
    let (effective_effects, auto_upgrade_audit) =
        check_call(&operation, CallFlags { yes: args.yes }, &profile.trust)?;
    let requested_view = parse_view(args.view.as_deref())?;
    let lens = match &args.lens {
        Some(id) => {
            let lens = load_lens(lens_dir, id, "call --lens")?;
            validate_lens_matches_call(&lens, &profile, &operation)?;
            Some(lens)
        }
        None => None,
    };
    let view = match &lens {
        Some(lens) => lens_slice_request(lens, &requested_view)?,
        None => requested_view,
    };
    let root_path = view.path.clone().unwrap_or_default();
    let effective_cache = effective_cache_policy(&profile, &operation);
    let may_cache = !args.no_cache && cache_allowed(&operation, &effective_cache);
    let cache_key = Store::cache_key(&args.source_id, &args.operation, &call_args)?;

    let cached_entry = if may_cache {
        store.get_entry(&cache_key)?
    } else {
        None
    };
    if !args.refresh
        && let Some(entry) = cached_entry.as_ref()
    {
        let cached_pagination = entry.extra.get("pagination").cloned();
        let cache_satisfies_request = args.pages <= 1
            || cached_pagination
                .as_ref()
                .is_some_and(|value| cached_pagination_satisfies(value, args.pages));
        if cache_satisfies_request {
            if let Some(observation_id) = &entry.observation_id
                && let Some(observation) = store.get_observation(observation_id)?
            {
                set_response_capture_budget(observation.capture.budget);
            }
            let payload = store
                .get_payload(&entry.payload_hash)?
                .ok_or_else(|| CoreError::CacheMiss(cache_key.clone()))?
                .into_redacted();
            let cache_info = cache_info(
                CacheStatus::Hit,
                entry,
                Some(age_seconds(&entry.created_at)?),
            );
            let cursor = cursor_for_projection(
                store,
                CursorInput {
                    cache_key: &cache_key,
                    source_id: &args.source_id,
                    operation: &args.operation,
                    root_path: &root_path,
                    payload: &payload,
                    slice: &view,
                    cache: &effective_cache,
                    may_cache,
                    lens: lens.as_ref(),
                },
            )?;
            let mut envelope = envelope_for_payload(
                store,
                EnvelopeInput {
                    value_scan: None,
                    source_id: args.source_id.clone(),
                    operation: args.operation.clone(),
                    source_kind: Some(profile_source_kind_name(profile.kind).to_string()),
                    payload,
                    root_path: root_path.clone(),
                    slice: view,
                    payload_bytes: entry.payload_bytes,
                    observation_id: entry.observation_id.clone(),
                    provenance: entry.provenance.clone(),
                    cache: Some(cache_info),
                    effects: Some(effective_effects.clone()),
                    auto_upgrade_audit: auto_upgrade_audit.clone(),
                    redacted_paths: 0,
                    cache_disabled_reason: None,
                    warnings: Vec::new(),
                    schema_hints: operation
                        .output_shape
                        .as_ref()
                        .map(|shape| render_hints(shape, ""))
                        .unwrap_or_default(),
                    next_action_operation: Some(args.operation.clone()),
                    additional_next_actions: Vec::new(),
                    observation_parser: None,
                    lens,
                },
                cursor,
            )?;
            if let Some(pagination) = cached_pagination {
                envelope.extra.insert("pagination".to_string(), pagination);
                if let Some(actions) = entry.extra.get("pagination_next_actions") {
                    let actions: Vec<NextAction> = serde_json::from_value(actions.clone())?;
                    envelope.next_actions.extend(actions);
                }
                compact_pagination_extra_to_budget(&mut envelope)?;
            }
            let received_error = entry.provenance.as_ref().is_some_and(|provenance| {
                provenance.extra.get("received_error") == Some(&Value::Bool(true))
            });
            return Ok(CallSourceResult {
                envelope,
                received_error,
            });
        }
    }

    let source = callable_source_from_profile(&profile)?;
    let revalidation = if args.refresh {
        match cached_entry
            .as_ref()
            .and_then(|entry| entry.observation_id.as_deref())
        {
            Some(observation_id) => store
                .get_observation(observation_id)?
                .and_then(|observation| observation.source_state),
            None => None,
        }
    } else {
        None
    };
    let adapter_call =
        execute_callable_conditional(&source, &operation, &call_args, revalidation.as_ref())
            .await?;
    if adapter_call.not_modified {
        let prior = cached_entry.as_ref().ok_or_else(|| CoreError::BadArgs {
            operation: "call --refresh".to_string(),
            reason: "received HTTP 304 without a reusable cached observation".to_string(),
        })?;
        let prior_id = prior
            .observation_id
            .as_ref()
            .ok_or_else(|| CoreError::BadArgs {
                operation: "call --refresh".to_string(),
                reason: "received HTTP 304 but cached evidence has no observation identity"
                    .to_string(),
            })?;
        let prior_observation =
            store
                .get_observation(prior_id)?
                .ok_or_else(|| CoreError::BadArgs {
                    operation: "call --refresh".to_string(),
                    reason: "received HTTP 304 but cached observation metadata is unavailable"
                        .to_string(),
                })?;
        set_response_capture_budget(prior_observation.capture.budget.clone());
        let payload = store
            .get_payload(&prior.payload_hash)?
            .ok_or_else(|| CoreError::CacheMiss(cache_key.clone()))?
            .into_redacted();
        let provenance = call_provenance(
            &cache_key,
            adapter_call.status.clone(),
            adapter_call.duration_ms,
            adapter_call.provenance,
        );
        let observation_id = store
            .record_observation(NewObservation {
                payload_hash: prior.payload_hash.clone(),
                availability: prior_observation.availability,
                invocation_fingerprint: cache_key.clone(),
                source_id: args.source_id.clone(),
                operation: args.operation.clone(),
                comparison_family: args.comparison_family.clone(),
                selection: selection_coverage(&args.selection_scopes, args.selection_exhaustive),
                captured_at: Some(provenance.captured_at.clone()),
                duration_ms: provenance.duration_ms,
                status: provenance.status.clone(),
                capture: prior_observation.capture.clone(),
                redacted: prior_observation.redacted,
                source_state: prior_observation.source_state.clone(),
                lineage: prog_core::ObservationLineage {
                    revalidates_id: Some(prior_id.clone()),
                    ..prog_core::ObservationLineage::default()
                },
                provenance: Some(provenance.clone()),
                cache_key: Some(cache_key.clone()),
                ..NewObservation::default()
            })?
            .observation_id;
        let mut entry = prior.clone();
        entry.observation_id = Some(observation_id.clone());
        entry.provenance = Some(provenance.clone());
        let cache_retained = store.put_entry(&cache_key, &entry)?;
        let cursor = cursor_for_projection(
            store,
            CursorInput {
                cache_key: &cache_key,
                source_id: &args.source_id,
                operation: &args.operation,
                root_path: &root_path,
                payload: &payload,
                slice: &view,
                cache: &effective_cache,
                may_cache: cache_retained,
                lens: lens.as_ref(),
            },
        )?;
        let retention_warning =
            "cache retention policy evicted this payload before it could be reused".to_string();
        let mut envelope = envelope_for_payload(
            store,
            EnvelopeInput {
                value_scan: None,
                source_id: args.source_id.clone(),
                operation: args.operation.clone(),
                source_kind: Some(profile_source_kind_name(profile.kind).to_string()),
                payload,
                root_path,
                slice: view,
                payload_bytes: entry.payload_bytes,
                observation_id: Some(observation_id),
                provenance: Some(provenance),
                cache: Some(if cache_retained {
                    cache_info(
                        CacheStatus::Hit,
                        &entry,
                        Some(age_seconds(&entry.created_at)?),
                    )
                } else {
                    CacheInfo {
                        status: CacheStatus::Skipped,
                        ttl_seconds: None,
                        expires_at: None,
                        age_seconds: None,
                    }
                }),
                effects: Some(effective_effects),
                auto_upgrade_audit,
                redacted_paths: 0,
                cache_disabled_reason: (!cache_retained).then_some(retention_warning.clone()),
                warnings: {
                    let mut warnings = vec![
                        "HTTP validator confirmed the source is unchanged (304 Not Modified)"
                            .to_string(),
                    ];
                    if !cache_retained {
                        warnings.push(retention_warning);
                    }
                    warnings
                },
                schema_hints: operation
                    .output_shape
                    .as_ref()
                    .map(|shape| render_hints(shape, ""))
                    .unwrap_or_default(),
                next_action_operation: Some(args.operation.clone()),
                additional_next_actions: Vec::new(),
                observation_parser: None,
                lens,
            },
            cursor,
        )?;
        envelope
            .extra
            .insert("source_validity".to_string(), json!("confirmed_unchanged"));
        compact_envelope_to_budget(&mut envelope)?;
        return Ok(CallSourceResult {
            envelope,
            received_error: false,
        });
    }
    let received_error = adapter_call.received_error;
    let first_pagination = adapter_call.pagination.clone();
    let redaction = resolve_redaction(Some(&profile));
    let redacted = RawPayload::new(adapter_call.data).redact(&redaction);
    let redacted_paths = redacted.redacted_paths;
    let value_scan = redacted.value_scan;
    let payload = redacted.payload;
    let payload_bytes = json_len_u64(payload.as_value())?;
    let observed = infer(payload.as_value());

    let mut provenance = call_provenance(
        &cache_key,
        adapter_call.status,
        adapter_call.duration_ms,
        adapter_call.provenance,
    );
    provenance
        .extra
        .insert("received_error".to_string(), Value::Bool(received_error));
    let mut warnings = adapter_call.warnings;
    warnings.extend(call_effect_warnings(&operation));
    if args.no_cache {
        warnings.push("profile learning skipped because --no-cache was requested".to_string());
    } else if operation.effects.sensitive {
        warnings.push(
            "profile learning skipped because the operation may handle sensitive data".to_string(),
        );
    } else {
        update_profile_from_call(
            store,
            &profile,
            &operation.id,
            &call_args,
            payload.as_value(),
            &observed,
        )?;
    }
    if !redacted_paths.is_empty() {
        warnings.push(format!(
            "redacted {} sensitive path(s) before inference and persistence",
            redacted_paths.len()
        ));
    }
    if let Some(pagination) = adapter_call.pagination {
        warnings.push(format!(
            "pagination hints available: {}",
            compact_json(&pagination)?
        ));
    }

    let payload_hash = if may_cache {
        store.put_payload(&payload)?
    } else {
        Store::payload_hash(&payload)?
    };
    if may_cache {
        provenance.cache_key = Some(cache_key.clone());
    } else {
        provenance.cache_key = None;
    }
    let (availability, mut capture) = adapter_capture(
        Some(&provenance),
        payload.as_value(),
        payload_bytes,
        may_cache,
        !redacted_paths.is_empty(),
    );
    capture.budget = capture_budget_for_call(&profile, &operation);
    set_response_capture_budget(capture.budget.clone());
    let observation_id = record_capture(
        store,
        payload_hash.clone(),
        availability,
        capture,
        cache_key.clone(),
        args.source_id.clone(),
        args.operation.clone(),
        args.comparison_family.clone(),
        selection_coverage(&args.selection_scopes, args.selection_exhaustive),
        Some(provenance.clone()),
        may_cache.then(|| cache_key.clone()),
        !redacted_paths.is_empty(),
        None,
        lens.as_ref(),
        source_state_from_provenance(
            profile.kind,
            &args.source_id,
            &args.operation,
            &call_args,
            &provenance,
        )?,
    )?;

    let mut cache_disabled_reason = None;
    let cache_retained = if may_cache {
        let ttl = ttl_seconds(&effective_cache);
        let mut entry = new_cache_entry(
            cache_key.clone(),
            payload_hash,
            args.source_id.clone(),
            args.operation.clone(),
            payload_bytes,
            ttl,
        );
        entry.observation_id = Some(observation_id.clone());
        entry.provenance = Some(provenance.clone());
        let retained = store.put_entry(&cache_key, &entry)?;
        if !retained {
            let reason =
                "cache retention policy evicted this payload before it could be reused".to_string();
            warnings.push(reason.clone());
            cache_disabled_reason = Some(reason);
        }
        retained
    } else {
        false
    };
    let cache_status = if cache_retained {
        let entry = store
            .get_entry(&cache_key)?
            .ok_or_else(|| CoreError::CacheMiss(cache_key.clone()))?;
        Some(cache_info(CacheStatus::Stored, &entry, Some(0)))
    } else if !may_cache {
        let reason = cache_skip_warning(args.no_cache, &operation);
        warnings.push(reason.clone());
        cache_disabled_reason = Some(reason);
        Some(CacheInfo {
            status: CacheStatus::Skipped,
            ttl_seconds: None,
            expires_at: None,
            age_seconds: None,
        })
    } else {
        Some(CacheInfo {
            status: CacheStatus::Skipped,
            ttl_seconds: None,
            expires_at: None,
            age_seconds: None,
        })
    };

    let cursor = cursor_for_projection(
        store,
        CursorInput {
            cache_key: &cache_key,
            source_id: &args.source_id,
            operation: &args.operation,
            root_path: &root_path,
            payload: &payload,
            slice: &view,
            cache: &effective_cache,
            may_cache: cache_retained,
            lens: lens.as_ref(),
        },
    )?;
    let mut envelope = envelope_for_payload(
        store,
        EnvelopeInput {
            value_scan: Some(value_scan),
            source_id: args.source_id.clone(),
            operation: args.operation.clone(),
            source_kind: Some(profile_source_kind_name(profile.kind).to_string()),
            payload,
            root_path: root_path.clone(),
            slice: view,
            payload_bytes,
            observation_id: Some(observation_id),
            provenance: Some(provenance),
            cache: cache_status,
            effects: Some(effective_effects),
            auto_upgrade_audit,
            redacted_paths: redacted_paths.len(),
            cache_disabled_reason,
            warnings,
            schema_hints: render_hints(&observed, ""),
            next_action_operation: Some(args.operation.clone()),
            additional_next_actions: Vec::new(),
            observation_parser: None,
            lens: lens.clone(),
        },
        cursor,
    )?;
    if args.refresh {
        let validity = if received_error {
            "refresh_failed"
        } else if revalidation.is_some() {
            "source_changed"
        } else {
            "validator_unavailable"
        };
        envelope
            .extra
            .insert("source_validity".to_string(), json!(validity));
    }

    // Auto-pagination: when --pages > 1 on a read-only operation, prefetch up
    // to N pages into the cache under hard page/byte/time caps (I10). The
    // envelope stays the bounded view of page 1; additional pages are each
    // redacted -> inferred -> stored -> projected (I2/I8), their shapes merged
    // monotonically (I5), and each is reachable via its own pc1_ page cursor
    // (I9) or the surfaced continuation NextAction.
    if args.pages > 1 && !received_error {
        if prog_core::pagination_allowed(&operation.effects) {
            let caps = prog_core::PageCaps {
                max_pages: args.pages.min(50),
                ..prog_core::PageCaps::default()
            };
            let mut current_args = call_args.clone();
            // Live hints win; fall back to the discover-time pagination shape
            // stored on the operation profile when the live response carries none.
            let mut hints = first_pagination
                .clone()
                .or_else(|| operation.pagination.clone());
            let mut pages_fetched = 1usize;
            let mut total_bytes = payload_bytes;
            let mut stop = prog_core::StopReason::NoMore;
            let started = std::time::Instant::now();
            let mut prefetch_warnings: Vec<String> = Vec::new();
            // Per-page shape accumulation (I5) seeded with page 1.
            let mut merged_shape = observed.clone();
            // Page summaries (page 1 first). `envelope.omitted` stays page-1
            // scoped so an expand against the page-1 cursor can never reach a
            // page-2 path (I3 containment / I9 fail-closed).
            let mut page_summaries: Vec<Value> = Vec::new();
            page_summaries.push(json!({
                "page": 1,
                "cache_key": cache_key.clone(),
                "cursor": envelope.cursor.clone(),
                "bytes": payload_bytes,
                "omitted_count": envelope.omitted.len(),
                "omitted_paths": envelope.omitted.iter().take(8)
                    .map(|region| region.path.clone()).collect::<Vec<_>>(),
            }));
            while pages_fetched < caps.max_pages {
                let Some(target) = hints
                    .as_ref()
                    .and_then(|value| prog_core::next_args_from_hints(value, &current_args))
                else {
                    stop = prog_core::StopReason::NoMore;
                    break;
                };
                if started.elapsed().as_millis() as u64 > caps.max_wall_ms {
                    stop = prog_core::StopReason::TimeCap;
                    break;
                }
                // Resolve the target into a fetched page + the args used for
                // the cache key. URL continuation (Link rel="next") now follows
                // the same-host guard inside HttpSource::execute_url.
                let (page_call, page_key_args) = match target {
                    prog_core::PageTarget::Args(page_args) => {
                        let call = match execute_callable(&source, &operation, &page_args).await {
                            Ok(call) => call,
                            Err(error) => {
                                prefetch_warnings.push(format!(
                                    "pagination prefetch stopped at page {}: {error}",
                                    pages_fetched + 1
                                ));
                                stop = prog_core::StopReason::NoMore;
                                break;
                            }
                        };
                        let key_args = page_args.clone();
                        (call, key_args)
                    }
                    prog_core::PageTarget::Url(url) => {
                        match execute_callable_url(&source, &operation, &url, &current_args).await {
                            Ok(Some(call)) => {
                                // Distinct, deterministic cache key per URL page.
                                (call, json!({ "__url__": url }))
                            }
                            Ok(None) => {
                                prefetch_warnings.push(
                                    "pagination prefetch stopped: the next page is a URL \
                                     continuation (Link rel=\"next\") but this source kind has no \
                                     URL model"
                                        .to_string(),
                                );
                                stop = prog_core::StopReason::NoMore;
                                break;
                            }
                            Err(error) => {
                                prefetch_warnings.push(format!(
                                    "pagination prefetch stopped at page {}: {error}",
                                    pages_fetched + 1
                                ));
                                stop = prog_core::StopReason::NoMore;
                                break;
                            }
                        }
                    }
                };
                // redact -> infer -> store -> project, per page (I2/I8).
                let page_payload = RawPayload::new(page_call.data).redact(&redaction).payload;
                let page_bytes = json_len_u64(page_payload.as_value())?;
                if total_bytes + page_bytes > caps.max_total_bytes {
                    stop = prog_core::StopReason::ByteCap;
                    break;
                }
                total_bytes += page_bytes;
                prefetch_warnings.extend(page_call.warnings);

                let page_shape = infer(page_payload.as_value());
                merged_shape = prog_core::merge_page_shapes(Some(&merged_shape), &page_shape);
                // Project with a coarsened policy to obtain THIS page's omitted
                // regions; previews for N>=2 never enter envelope.data_preview
                // (page 1 stays the bounded view), only counts + top-K paths.
                let page_projection = project(
                    page_payload.as_value(),
                    &shrink_policy(&PreviewPolicy::default()),
                    "",
                );
                let omitted_paths: Vec<String> = page_projection
                    .omitted
                    .iter()
                    .take(8)
                    .map(|region| region.path.clone())
                    .collect();

                let page_cache_key =
                    Store::cache_key(&args.source_id, &args.operation, &page_key_args)?;
                let page_hash = if may_cache {
                    store.put_payload(&page_payload)?
                } else {
                    Store::payload_hash(&page_payload)?
                };
                let page_provenance = call_provenance(
                    &page_cache_key,
                    page_call.status.clone(),
                    page_call.duration_ms,
                    page_call.provenance.clone(),
                );
                let (availability, mut capture) = adapter_capture(
                    Some(&page_provenance),
                    page_payload.as_value(),
                    page_bytes,
                    may_cache,
                    false,
                );
                capture.budget = capture_budget_for_call(&profile, &operation);
                set_response_capture_budget(capture.budget.clone());
                let page_observation_id = record_capture(
                    store,
                    page_hash.clone(),
                    availability,
                    capture,
                    page_cache_key.clone(),
                    args.source_id.clone(),
                    args.operation.clone(),
                    args.comparison_family.clone(),
                    selection_coverage(&args.selection_scopes, args.selection_exhaustive),
                    Some(page_provenance.clone()),
                    may_cache.then(|| page_cache_key.clone()),
                    false,
                    None,
                    lens.as_ref(),
                    source_state_from_provenance(
                        profile.kind,
                        &args.source_id,
                        &args.operation,
                        &page_key_args,
                        &page_provenance,
                    )?,
                )?;
                let page_cursor = if may_cache {
                    let ttl = ttl_seconds(&effective_cache);
                    let mut entry = new_cache_entry(
                        page_cache_key.clone(),
                        page_hash,
                        args.source_id.clone(),
                        args.operation.clone(),
                        page_bytes,
                        ttl,
                    );
                    entry.observation_id = Some(page_observation_id.clone());
                    entry.provenance = Some(page_provenance);
                    let page_retained = store.put_entry(&page_cache_key, &entry)?;
                    if !page_retained {
                        prefetch_warnings.push(format!(
                            "page {} was not retained because the cache retention policy evicted it",
                            pages_fetched + 1
                        ));
                    }
                    // Mint a pc1_ cursor carrying page metadata (I9 fail-closed
                    // reuse; extra is observability only).
                    let mut cursor_extra = Map::new();
                    cursor_extra.insert("kind".to_string(), json!("page"));
                    cursor_extra.insert("page".to_string(), json!(pages_fetched + 1));
                    cursor_extra.insert(
                        "args".to_string(),
                        redacted_profile_args(&operation, &page_key_args),
                    );
                    page_retained
                        .then(|| {
                            store.create_cursor_with_extra(
                                &page_cache_key,
                                &args.source_id,
                                &args.operation,
                                &root_path,
                                ttl,
                                cursor_extra,
                            )
                        })
                        .transpose()?
                } else {
                    None
                };

                // Profile learning: each page's shape joins the operation's
                // output_shape (monotonic via the store, same as across calls).
                if !args.no_cache && !operation.effects.sensitive {
                    update_profile_from_call(
                        store,
                        &profile,
                        &args.operation,
                        &page_key_args,
                        page_payload.as_value(),
                        &page_shape,
                    )?;
                }

                page_summaries.push(json!({
                    "page": pages_fetched + 1,
                    "cache_key": page_cache_key,
                    "cursor": page_cursor,
                    "bytes": page_bytes,
                    "omitted_count": page_projection.omitted.len(),
                    "omitted_paths": omitted_paths,
                }));

                pages_fetched += 1;
                current_args = page_key_args;
                hints = page_call.pagination.clone();
            }
            if pages_fetched >= caps.max_pages {
                stop = prog_core::StopReason::PageCap;
            }

            // Reconcile the stop reason with reality: the next-page target is
            // computed from the LAST fetched page's hints. If no next page
            // remains, the chain ended naturally (NoMore) regardless of which
            // exit path the loop took (a page cap reached exactly at the end of
            // a finite chain is NoMore, not PageCap). This target is also the
            // resume point surfaced below when paused at a real cap.
            let resume_target = hints
                .as_ref()
                .and_then(|value| prog_core::next_args_from_hints(value, &current_args));
            if resume_target.is_none() {
                stop = prog_core::StopReason::NoMore;
            }

            // Continuation: when paused at a cap (not NoMore) with a concrete
            // next target, surface a resume NextAction. NoMore never surfaces one.
            if !stop.is_terminal()
                && let Some(resume) = resume_target
            {
                let reason = format!(
                    "pagination paused at {}; {} page(s) fetched; resume with the next page",
                    stop.as_str(),
                    pages_fetched
                );
                let next_action = match resume {
                    prog_core::PageTarget::Args(resume_args) => NextAction {
                        kind: "call".to_string(),
                        operation: Some(args.operation.clone()),
                        path: None,
                        reason: Some(reason),
                        extra: {
                            let mut map = Map::new();
                            map.insert("args".to_string(), resume_args);
                            map.insert(
                                "source_id".to_string(),
                                Value::String(args.source_id.clone()),
                            );
                            map
                        },
                        ..NextAction::default()
                    },
                    prog_core::PageTarget::Url(url) => NextAction {
                        kind: "call_url".to_string(),
                        operation: Some(args.operation.clone()),
                        path: None,
                        reason: Some(reason),
                        extra: {
                            let mut map = Map::new();
                            map.insert("url".to_string(), Value::String(url));
                            map.insert(
                                "source_id".to_string(),
                                Value::String(args.source_id.clone()),
                            );
                            map
                        },
                        ..NextAction::default()
                    },
                };
                envelope.next_actions.push(next_action);
            }

            envelope.warnings.extend(prefetch_warnings);
            envelope.extra.insert(
                "pagination".to_string(),
                json!({
                    "pages_fetched": pages_fetched,
                    "total_bytes": total_bytes,
                    "stop_reason": stop.as_str(),
                    "max_pages": caps.max_pages,
                    "merged_shape": serde_json::to_value(&merged_shape)?,
                    "pages": page_summaries,
                }),
            );
            // The pagination extra (uncapped `merged_shape` + per-page `pages[]`)
            // is appended AFTER `envelope_for_payload`'s budget loop, so re-enforce
            // `max_envelope_bytes` here: compact the pagination metadata if the
            // final envelope would otherwise exceed the budget (invariant I11).
            compact_pagination_extra_to_budget(&mut envelope)?;
            if may_cache
                && let Some(pagination) = envelope.extra.get("pagination").cloned()
                && let Some(mut entry) = store.get_entry(&cache_key)?
            {
                entry.extra.insert("pagination".to_string(), pagination);
                let pagination_next_actions = envelope
                    .next_actions
                    .iter()
                    .filter(|action| matches!(action.kind.as_str(), "call" | "call_url"))
                    .cloned()
                    .collect::<Vec<_>>();
                entry.extra.insert(
                    "pagination_next_actions".to_string(),
                    serde_json::to_value(pagination_next_actions)?,
                );
                store.put_entry(&cache_key, &entry)?;
            }
        } else {
            envelope.warnings.push(
                "--pages requested but the operation is not auto-pagination-safe \
                 (it is not read-only); fetched a single page"
                    .to_string(),
            );
        }
    }

    if received_error {
        envelope
            .extra
            .insert("received_error".to_string(), Value::Bool(true));
    }
    Ok(CallSourceResult {
        envelope,
        received_error,
    })
}

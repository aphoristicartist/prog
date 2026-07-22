//! Local command execution, capture, and failure analysis.

use crate::*;

struct RunProcessResult {
    stdout: RunCapture,
    stderr: RunCapture,
    combined: Vec<RunChunk>,
    status: RunProcessStatus,
}

pub(crate) enum RunProcessStatus {
    Exited {
        success: bool,
        code: Option<i32>,
        signal: Option<i32>,
    },
    TimedOut,
    SpawnError(String),
}

pub(crate) async fn run_command(
    store: &Store,
    lens_dir: &Path,
    args: &RunArgs,
    ctx: &mut InvocationContext,
) -> Result<RunEnvelopeResult> {
    let cwd = std::env::current_dir()?;
    let started_at = Utc::now();
    let started_instant = Instant::now();
    let argv = args.command.clone();
    let run_sequence = RUN_CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let started_at_nanos = started_at
        .timestamp_nanos_opt()
        .map(|value| value.to_string())
        .unwrap_or_else(|| started_at.timestamp_micros().to_string());
    let run_id = format!(
        "run_{}_{}_{}",
        std::process::id(),
        started_at_nanos,
        run_sequence
    );
    let operation = run_operation_name(&argv);
    let lens = match &args.lens {
        Some(id) => {
            let lens = load_lens(lens_dir, id, "run --lens")?;
            validate_lens_matches_run(&lens, &operation)?;
            Some(lens)
        }
        None => None,
    };
    let requested_view = SliceRequest {
        path: None,
        limit: None,
        depth: None,
        fields: Vec::new(),
        omit: Vec::new(),
        extra: Extra::new(),
    };
    let slice = match &lens {
        Some(lens) => lens_slice_request(lens, &requested_view)?,
        None => requested_view,
    };
    let root_path = slice.path.clone().unwrap_or_default();
    let redacted_argv = redact_run_argv(&argv);
    let cache_args = json!({
        "run_id": &run_id,
        "argv": argv,
        "cwd": cwd.to_string_lossy(),
        "started_at": started_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
    });
    let cache_key = Store::cache_key("run", &operation, &cache_args)?;
    let invocation_fingerprint = Store::cache_key(
        "run",
        &operation,
        &json!({
            "argv": argv,
            "cwd": cwd.to_string_lossy(),
        }),
    )?;

    let mut command = TokioCommand::new(&args.command[0]);
    command
        .args(&args.command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_run_process_group(&mut command);

    let run = match command.spawn() {
        Ok(child) => {
            run_spawned_child(
                child,
                args.timeout_ms,
                args.max_stdout_bytes,
                args.max_stderr_bytes,
            )
            .await?
        }
        Err(error) => RunProcessResult {
            stdout: empty_run_capture("stdout"),
            stderr: empty_run_capture("stderr"),
            combined: Vec::new(),
            status: RunProcessStatus::SpawnError(error.to_string()),
        },
    };

    let ended_at = Utc::now();
    let duration_ms = started_instant
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    let stdout_text = run_text_from_capture(&run.stdout);
    let stderr_text = run_text_from_capture(&run.stderr);
    let combined = run
        .combined
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let text = redact_run_output_bytes(&chunk.bytes).text;
            json!({
                "index": index,
                "stream": chunk.stream,
                "text": text,
                "byte_count": chunk.bytes.len()
            })
        })
        .collect::<Vec<_>>();
    let failure_sections = detect_run_failure_sections(&run.status, &stdout_text, &stderr_text);
    let payload = run_payload(RunPayloadInput {
        run_id: &run_id,
        argv: &args.command,
        redacted_argv: &redacted_argv,
        cwd: &cwd,
        started_at,
        ended_at,
        duration_ms,
        status: &run.status,
        stdout: &stdout_text,
        stderr: &stderr_text,
        combined,
        failure_sections: &failure_sections,
        out: args.out.as_ref(),
    });
    let redaction = RedactionPolicy::default();
    let redacted = RawPayload::new(payload).redact(&redaction);
    let policy_redactions = redacted.redacted_paths;
    let value_scan = redacted.value_scan;
    let redacted_payload = redacted.payload;
    if let Some(path) = &args.out {
        write_private_file(
            path,
            &serde_json::to_vec_pretty(redacted_payload.as_value())?,
        )?;
    }
    let payload_hash = store.put_payload(&redacted_payload)?;
    let payload_bytes = json_len_u64(redacted_payload.as_value())?;
    let ttl: i64 = args
        .ttl_seconds
        .try_into()
        .map_err(|_| CoreError::BadArgs {
            operation: "run".to_string(),
            reason: "ttl_seconds is too large".to_string(),
        })?;
    let mut provenance = run_provenance(
        &run_id,
        &cache_key,
        &redacted_argv,
        &cwd,
        duration_ms,
        &run.status,
        args,
    );
    let mut entry = new_cache_entry(
        cache_key.clone(),
        payload_hash.clone(),
        "run".to_string(),
        operation.clone(),
        payload_bytes,
        ttl,
    );
    entry.provenance = Some(provenance.clone());
    let stdout_windowed = stdout_text.line_count > stdout_text.head.len() + stdout_text.tail.len();
    let stderr_windowed = stderr_text.line_count > stderr_text.head.len() + stderr_text.tail.len();
    let (availability, mut capture) = run_capture_completeness(
        &run.stdout,
        &run.stderr,
        payload_bytes,
        !policy_redactions.is_empty(),
        &run.status,
        stdout_windowed,
        stderr_windowed,
    );
    capture.budget = capture_budget_for_run(args);
    ctx.set_capture(capture.budget.clone());
    let observation_id = record_capture(
        store,
        payload_hash.clone(),
        availability,
        capture,
        invocation_fingerprint,
        "run".to_string(),
        operation.clone(),
        args.comparison_family.clone(),
        selection_coverage(&args.selection_scopes, args.selection_exhaustive),
        Some(provenance.clone()),
        Some(cache_key.clone()),
        !policy_redactions.is_empty(),
        Some("cli".to_string()),
        None,
        lens.as_ref(),
        None,
        // A local `run` executes a subprocess directly; there is no external
        // upstream source separate from the command itself that could have
        // drifted between capture and now, so this observation's source
        // state is confirmed unchanged by construction, not merely unknown.
        prog_core::SourceValidity::ConfirmedUnchanged,
    )?;
    entry.observation_id = Some(observation_id.clone());
    let cache_retained = store.put_entry(&cache_key, &entry)?;
    let cursor = cache_retained
        .then(|| {
            store.create_cursor_with_extra(
                &cache_key,
                "run",
                &operation,
                &root_path,
                ttl,
                cursor_lens_extra(lens.as_ref()),
            )
        })
        .transpose()?;
    provenance.cache_key = Some(cache_key.clone());

    let mut warnings = run_warnings(&run.status, args, &run.stdout, &run.stderr);
    let text_redactions = stdout_text
        .redactions
        .saturating_add(stderr_text.redactions)
        .saturating_add(
            redacted_argv
                .iter()
                .filter(|arg| arg.contains("[REDACTED"))
                .count(),
        );
    let redacted_paths = policy_redactions.len().saturating_add(text_redactions);
    if redacted_paths > 0 {
        warnings.push(format!(
            "redacted {redacted_paths} sensitive value(s) before persistence"
        ));
    }
    if args.out.is_some() {
        warnings.push("wrote redacted structured run capture to --out path".to_string());
    }
    if !cache_retained {
        warnings.push(
            "cache retention policy evicted this payload before it could be reused".to_string(),
        );
    }
    let mut next_actions = run_next_actions(cursor.as_deref(), &failure_sections);
    next_actions.extend(targeted_rerun_actions(
        store,
        &args.command,
        &failure_sections,
    ));
    let envelope = envelope_for_payload(
        store,
        EnvelopeInput {
            value_scan: Some(value_scan),
            source_id: "run".to_string(),
            operation,
            source_kind: Some("cli".to_string()),
            payload: redacted_payload,
            root_path,
            slice,
            payload_bytes,
            observation_id: Some(observation_id),
            provenance: Some(provenance),
            cache: Some(if cache_retained {
                cache_info(CacheStatus::Stored, &entry, Some(0))
            } else {
                CacheInfo {
                    status: CacheStatus::Skipped,
                    ttl_seconds: None,
                    expires_at: None,
                    age_seconds: None,
                }
            }),
            effects: Some(run_effects()),
            auto_upgrade_audit: None,
            redacted_paths,
            cache_disabled_reason: None,
            warnings,
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: next_actions,
            observation_parser: None,
            lens,
        },
        cursor,
        ctx.max_envelope_bytes(),
    )?;

    Ok(RunEnvelopeResult {
        envelope,
        exit_code: run_exit_code(&run.status),
    })
}

async fn run_spawned_child(
    mut child: tokio::process::Child,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<RunProcessResult> {
    let stdout = child.stdout.take().ok_or_else(|| CoreError::CliTransport {
        operation: "run".to_string(),
        message: "failed to capture stdout".to_string(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| CoreError::CliTransport {
        operation: "run".to_string(),
        message: "failed to capture stderr".to_string(),
    })?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let stdout_task = tokio::spawn(read_run_stream(
        "stdout",
        stdout,
        max_stdout_bytes,
        tx.clone(),
    ));
    let stderr_task = tokio::spawn(read_run_stream(
        "stderr",
        stderr,
        max_stderr_bytes,
        tx.clone(),
    ));
    drop(tx);

    let wait = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await;
    let status = match wait {
        Ok(result) => {
            let status = result.map_err(|error| CoreError::CliTransport {
                operation: "run".to_string(),
                message: error.to_string(),
            })?;
            RunProcessStatus::Exited {
                success: status.success(),
                code: status.code(),
                signal: exit_signal(&status),
            }
        }
        Err(_) => {
            kill_run_process_group(&mut child).await;
            let _ = tokio::join!(
                finish_run_reader_or_abort(stdout_task),
                finish_run_reader_or_abort(stderr_task)
            );
            let mut combined = Vec::new();
            while let Ok(chunk) = rx.try_recv() {
                combined.push(chunk);
            }
            return Ok(RunProcessResult {
                stdout: empty_run_capture("stdout"),
                stderr: empty_run_capture("stderr"),
                combined,
                status: RunProcessStatus::TimedOut,
            });
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|error| CoreError::CliTransport {
            operation: "run".to_string(),
            message: error.to_string(),
        })?
        .map_err(|error| CoreError::CliTransport {
            operation: "run".to_string(),
            message: error.to_string(),
        })?;
    let stderr = stderr_task
        .await
        .map_err(|error| CoreError::CliTransport {
            operation: "run".to_string(),
            message: error.to_string(),
        })?
        .map_err(|error| CoreError::CliTransport {
            operation: "run".to_string(),
            message: error.to_string(),
        })?;
    let mut combined = Vec::new();
    while let Ok(chunk) = rx.try_recv() {
        combined.push(chunk);
    }
    Ok(RunProcessResult {
        stdout,
        stderr,
        combined,
        status,
    })
}

#[cfg(unix)]
fn configure_run_process_group(command: &mut TokioCommand) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_run_process_group(_command: &mut TokioCommand) {}

async fn kill_run_process_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
            let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_millis(100), child.wait()).await;
}

async fn finish_run_reader_or_abort(mut task: JoinHandle<std::io::Result<RunCapture>>) {
    tokio::select! {
        _ = &mut task => {}
        _ = tokio::time::sleep(Duration::from_millis(25)) => {
            task.abort();
            let _ = task.await;
        }
    }
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

async fn read_run_stream<R: AsyncRead + Unpin>(
    stream: &'static str,
    mut reader: R,
    cap: usize,
    tx: mpsc::UnboundedSender<RunChunk>,
) -> std::io::Result<RunCapture> {
    let mut output = Vec::new();
    let mut total_bytes = 0usize;
    let mut truncated = false;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read);
        let remaining = cap.saturating_sub(output.len());
        if remaining > 0 {
            let stored = read.min(remaining);
            let bytes = buffer[..stored].to_vec();
            output.extend_from_slice(&bytes);
            let _ = tx.send(RunChunk { stream, bytes });
        }
        if read > remaining || total_bytes > cap {
            truncated = true;
        }
    }
    Ok(RunCapture {
        stream,
        bytes: output,
        total_bytes,
        truncated,
    })
}

fn empty_run_capture(stream: &'static str) -> RunCapture {
    RunCapture {
        stream,
        bytes: Vec::new(),
        total_bytes: 0,
        truncated: false,
    }
}

fn run_operation_name(argv: &[String]) -> String {
    Path::new(&argv[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&argv[0])
        .to_string()
}

fn run_text_from_capture(capture: &RunCapture) -> RunText {
    let mut text = redact_run_output_bytes(&capture.bytes);
    text.byte_count = capture.total_bytes;
    text.captured_bytes = capture.bytes.len();
    text.truncated = capture.truncated;
    text
}

fn redact_run_output_bytes(bytes: &[u8]) -> RunText {
    let utf8_valid = std::str::from_utf8(bytes).is_ok();
    let text = String::from_utf8_lossy(bytes);
    let mut redactions = 0usize;
    let lines = text
        .lines()
        .map(|line| {
            let (redacted, count) = prog_core::redact_sensitive_text(line);
            redactions = redactions.saturating_add(count);
            redacted
        })
        .collect::<Vec<_>>();
    let line_count = lines.len();
    let head = lines.iter().take(10).cloned().collect::<Vec<_>>();
    let tail_start = lines.len().saturating_sub(10).max(head.len());
    let tail = lines.iter().skip(tail_start).cloned().collect::<Vec<_>>();
    RunText {
        text: lines.join("\n"),
        head,
        tail,
        line_count,
        byte_count: bytes.len(),
        captured_bytes: bytes.len(),
        truncated: false,
        utf8_valid,
        redactions,
    }
}

fn run_payload(input: RunPayloadInput<'_>) -> Value {
    json!({
        "format": "run",
        "command": {
            "capture_id": input.run_id,
            "argv": input.redacted_argv,
            "argv_count": input.argv.len(),
            "cwd": input.cwd.to_string_lossy(),
            "started_at": input.started_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "ended_at": input.ended_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "duration_ms": input.duration_ms,
            "success": matches!(input.status, RunProcessStatus::Exited { success: true, .. }),
            "exit_code": match input.status {
                RunProcessStatus::Exited { code, .. } => json!(code),
                _ => Value::Null,
            },
            "signal": match input.status {
                RunProcessStatus::Exited { signal, .. } => json!(signal),
                _ => Value::Null,
            },
            "timed_out": matches!(input.status, RunProcessStatus::TimedOut),
            "spawn_error": match input.status {
                RunProcessStatus::SpawnError(message) => json!(message),
                _ => Value::Null,
            },
            "out": input.out.map(|path| path.to_string_lossy().to_string())
        },
        "stdout": run_stream_value(input.stdout),
        "stderr": run_stream_value(input.stderr),
        "combined": input.combined,
        "failure_sections": input.failure_sections
            .iter()
            .enumerate()
            .map(|(index, section)| {
                json!({
                    "index": index,
                    "kind": section.kind,
                    "stream": section.stream,
                    "line_start": section.line_start,
                    "line_end": section.line_end,
                    "reason": section.reason,
                    "priority": section.priority,
                    "lines": section.lines
                })
            })
            .collect::<Vec<_>>()
    })
}

fn run_stream_value(text: &RunText) -> Value {
    json!({
        "format": "text",
        "text": text.text,
        "head": text.head,
        "tail": text.tail,
        "line_count": text.line_count,
        "byte_count": text.byte_count,
        "captured_bytes": text.captured_bytes,
        "truncated": text.truncated,
        "utf8_valid": text.utf8_valid
    })
}

fn detect_run_failure_sections(
    status: &RunProcessStatus,
    stdout: &RunText,
    stderr: &RunText,
) -> Vec<RunFailureSection> {
    let allow_generic = !matches!(status, RunProcessStatus::Exited { success: true, .. });
    let mut sections = Vec::new();
    collect_failure_sections("stderr", &stderr.text, allow_generic, &mut sections);
    collect_failure_sections("stdout", &stdout.text, allow_generic, &mut sections);
    if sections.is_empty() {
        match status {
            RunProcessStatus::Exited { success: false, .. } => {
                let lines = stderr
                    .text
                    .lines()
                    .chain(stdout.text.lines())
                    .rev()
                    .take(8)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if !lines.is_empty() {
                    sections.push(RunFailureSection {
                        kind: "generic",
                        stream: "stderr",
                        line_start: 1,
                        line_end: lines.len(),
                        lines: lines.into_iter().rev().collect(),
                        reason: "command exited unsuccessfully; inspect captured diagnostics"
                            .to_string(),
                        priority: 50,
                    });
                }
            }
            RunProcessStatus::TimedOut => sections.push(RunFailureSection {
                kind: "timeout",
                stream: "stderr",
                line_start: 1,
                line_end: 1,
                lines: vec!["command timed out".to_string()],
                reason: "command exceeded --timeout-ms".to_string(),
                priority: 95,
            }),
            RunProcessStatus::SpawnError(message) => sections.push(RunFailureSection {
                kind: "spawn_error",
                stream: "stderr",
                line_start: 1,
                line_end: 1,
                lines: vec![message.clone()],
                reason: "command could not be started".to_string(),
                priority: 100,
            }),
            RunProcessStatus::Exited { success: true, .. } => {}
        }
    }
    sections.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.stream.cmp(right.stream))
            .then_with(|| left.line_start.cmp(&right.line_start))
    });
    sections.truncate(10);
    sections
}

fn collect_failure_sections(
    stream: &'static str,
    text: &str,
    allow_generic: bool,
    sections: &mut Vec<RunFailureSection>,
) {
    let lines = text.lines().map(str::to_string).collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        let detected = if line.trim_start().starts_with("--- FAIL:") {
            Some(("go", 90, "Go test failure"))
        } else if line.trim_start().starts_with("FAIL ") {
            Some(("jest_vitest", 85, "Jest or Vitest failure"))
        } else if line.contains("error[") || line.contains("panicked at") {
            Some(("rust", 90, "Rust compiler or test failure"))
        } else if line.contains("Traceback (most recent call last):") {
            Some(("python", 90, "Python traceback"))
        } else if line.contains("npm ERR!")
            || line.starts_with("Error:")
            || line.starts_with("node:")
        {
            Some(("node", 85, "Node.js or npm error"))
        } else if allow_generic
            && (lower.contains("error")
                || lower.contains("failed")
                || lower.contains("exception")
                || lower.contains("not found"))
        {
            Some(("generic", 60, "generic failure diagnostic"))
        } else {
            None
        };
        if let Some((kind, priority, reason)) = detected {
            let start = index.saturating_sub(2);
            let end = (index + 6).min(lines.len());
            if let Some(existing) = sections
                .iter_mut()
                .rev()
                .find(|section| section.stream == stream && section.line_end > start)
            {
                if priority > existing.priority {
                    existing.kind = kind;
                    existing.reason = reason.to_string();
                    existing.priority = priority;
                    existing.line_start = start + 1;
                    existing.line_end = end;
                    existing.lines = lines[start..end].to_vec();
                    continue;
                }
                existing.line_end = existing.line_end.max(end);
                existing.lines = lines[existing.line_start - 1..existing.line_end].to_vec();
                continue;
            }
            sections.push(RunFailureSection {
                kind,
                stream,
                line_start: start + 1,
                line_end: end,
                lines: lines[start..end].to_vec(),
                reason: reason.to_string(),
                priority,
            });
        }
    }
}

fn run_next_actions(cursor: Option<&str>, sections: &[RunFailureSection]) -> Vec<NextAction> {
    let Some(cursor) = cursor else {
        return Vec::new();
    };
    sections
        .iter()
        .take(6)
        .enumerate()
        .map(|(index, section)| {
            let path = format!("/failure_sections/{index}");
            let mut extra = Extra::new();
            extra.insert("priority".to_string(), json!(section.priority));
            extra.insert("stream".to_string(), json!(section.stream));
            // `extra` is flattened into NextAction; avoid colliding with the
            // typed `kind` field and producing duplicate JSON object keys.
            extra.insert("section_kind".to_string(), json!(section.kind));
            NextAction {
                kind: "expand".to_string(),
                operation: None,
                path: Some(path),
                reason: Some(section.reason.clone()),
                argv: Some(vec![
                    "prog".to_string(),
                    "expand".to_string(),
                    cursor.to_string(),
                    "--path".to_string(),
                    format!("/failure_sections/{index}"),
                ]),
                scope: Some("failure_section".to_string()),
                exactness: Some(prog_core::ActionExactness::Exact),
                derived_from: Some("run.failure_section".to_string()),
                extra,
                ..NextAction::default()
            }
        })
        .collect()
}

type RerunActionEmitter = fn(&[String], &[RunFailureSection], &[String]) -> Vec<NextAction>;

const RERUN_ACTION_EMITTERS: &[RerunActionEmitter] = &[
    pytest_target_actions,
    go_test_target_actions,
    cargo_test_target_actions,
    jest_vitest_target_actions,
];

pub(crate) fn targeted_rerun_actions(
    store: &Store,
    command: &[String],
    sections: &[RunFailureSection],
) -> Vec<NextAction> {
    let does_not_satisfy = store
        .list_obligations(None)
        .map(|list| {
            list.obligations
                .into_iter()
                .filter(|obligation| obligation.required && obligation.required_scope != "target")
                .map(|obligation| obligation.id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    RERUN_ACTION_EMITTERS
        .iter()
        .flat_map(|emitter| emitter(command, sections, &does_not_satisfy))
        .collect()
}

fn pytest_target_actions(
    command: &[String],
    sections: &[RunFailureSection],
    does_not_satisfy: &[String],
) -> Vec<NextAction> {
    // This is deliberately narrower than command equivalence: only the
    // literal pytest executable and a complete node ID printed by pytest are
    // evidence enough for an exact argv recommendation.
    if command.first().map(String::as_str) != Some("pytest") {
        return Vec::new();
    }
    let mut node_ids = BTreeSet::new();
    for section in sections {
        for line in &section.lines {
            let Some(candidate) = line.trim_start().strip_prefix("FAILED ") else {
                continue;
            };
            let Some(node_id) = candidate.split_whitespace().next() else {
                continue;
            };
            if node_id.contains("::")
                && !node_id.starts_with('-')
                && !node_id.contains('\0')
                && !node_id.contains(char::is_whitespace)
            {
                node_ids.insert(node_id.to_string());
            }
        }
    }
    node_ids
        .into_iter()
        .take(3)
        .map(|node_id| NextAction {
            kind: "rerun".to_string(),
            operation: None,
            path: None,
            reason: Some("rerun this exact pytest node ID for a focused diagnostic".to_string()),
            argv: Some(vec!["pytest".to_string(), node_id.clone()]),
            scope: Some("target_test".to_string()),
            exactness: Some(prog_core::ActionExactness::Exact),
            derived_from: Some("pytest.failed_node_id".to_string()),
            does_not_satisfy: does_not_satisfy.to_vec(),
            cwd: None,
            extra: Extra::new(),
        })
        .collect()
}

pub(crate) fn go_test_target_actions(
    command: &[String],
    sections: &[RunFailureSection],
    does_not_satisfy: &[String],
) -> Vec<NextAction> {
    if command.first().map(String::as_str) != Some("go")
        || command.get(1).map(String::as_str) != Some("test")
    {
        return Vec::new();
    }
    let Some(package) = go_test_package(command, sections) else {
        return Vec::new();
    };
    let test_names = sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .filter_map(|line| go_test_name(line))
        .collect::<BTreeSet<_>>();
    test_names
        .into_iter()
        .take(3)
        .map(|name| NextAction {
            kind: "rerun".to_string(),
            operation: None,
            path: None,
            reason: Some("rerun this exact Go test in its known package scope".to_string()),
            argv: Some(vec![
                "go".to_string(),
                "test".to_string(),
                package.clone(),
                "-run".to_string(),
                anchored_go_test_pattern(&name),
            ]),
            scope: Some("target_test".to_string()),
            exactness: Some(prog_core::ActionExactness::Exact),
            derived_from: Some("go_test.failed_name_and_package".to_string()),
            does_not_satisfy: does_not_satisfy.to_vec(),
            cwd: None,
            extra: Extra::new(),
        })
        .collect()
}

pub(crate) fn cargo_test_target_actions(
    command: &[String],
    sections: &[RunFailureSection],
    does_not_satisfy: &[String],
) -> Vec<NextAction> {
    if command.first().map(String::as_str) != Some("cargo")
        || command.get(1).map(String::as_str) != Some("test")
    {
        return Vec::new();
    }
    let test_names = sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .filter_map(|line| cargo_test_name(line))
        .collect::<BTreeSet<_>>();
    let exact_target = cargo_exact_target_args(command);
    test_names
        .into_iter()
        .take(3)
        .filter(|name| safe_positional_identity(name))
        .map(|name| {
            let (argv, exactness, derived_from, reason) = if let Some(target) = &exact_target {
                let mut argv = vec!["cargo".to_string(), "test".to_string()];
                argv.extend(target.clone());
                argv.extend([name.clone(), "--".to_string(), "--exact".to_string()]);
                (
                    argv,
                    prog_core::ActionExactness::Exact,
                    "cargo_test.failed_name_and_harness",
                    "rerun this exact libtest name in the known Cargo harness",
                )
            } else {
                (
                    vec!["cargo".to_string(), "test".to_string(), name.clone()],
                    prog_core::ActionExactness::Filter,
                    "cargo_test.failed_name_filter",
                    "filter Cargo tests by this failed name; the harness scope is not proven exact",
                )
            };
            NextAction {
                kind: "rerun".to_string(),
                operation: None,
                path: None,
                reason: Some(reason.to_string()),
                argv: Some(argv),
                scope: Some("target_test".to_string()),
                exactness: Some(exactness),
                derived_from: Some(derived_from.to_string()),
                does_not_satisfy: does_not_satisfy.to_vec(),
                cwd: None,
                extra: Extra::new(),
            }
        })
        .collect()
}

pub(crate) fn jest_vitest_target_actions(
    command: &[String],
    sections: &[RunFailureSection],
    does_not_satisfy: &[String],
) -> Vec<NextAction> {
    let Some(runner) = command
        .first()
        .filter(|item| matches!(item.as_str(), "jest" | "vitest"))
    else {
        return Vec::new();
    };
    let paths = sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .filter_map(|line| jest_vitest_path(line))
        .collect::<BTreeSet<_>>();
    let names = sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .filter_map(|line| jest_vitest_name(line))
        .collect::<BTreeSet<_>>();
    let mut actions = Vec::new();
    if paths.len() == 1 && names.len() == 1 {
        let path = paths.first().expect("checked exactly one path");
        let name = names.first().expect("checked exactly one name");
        if safe_positional_identity(path) && safe_flag_value(name) {
            actions.push(NextAction {
                kind: "rerun".to_string(),
                operation: None,
                path: None,
                reason: Some("rerun this exact test name in its failed test file".to_string()),
                argv: Some(vec![
                    runner.clone(),
                    path.clone(),
                    "--testNamePattern".to_string(),
                    anchored_regex(name),
                ]),
                scope: Some("target_test".to_string()),
                exactness: Some(prog_core::ActionExactness::Exact),
                derived_from: Some("jest_vitest.failed_path_and_name".to_string()),
                does_not_satisfy: does_not_satisfy.to_vec(),
                cwd: None,
                extra: Extra::new(),
            });
        }
        return actions;
    }
    if paths.is_empty() && names.len() == 1 {
        let name = names.first().expect("checked exactly one name");
        if safe_flag_value(name) {
            actions.push(NextAction {
                kind: "rerun".to_string(),
                operation: None,
                path: None,
                reason: Some(
                    "filter test names by this failed title; file scope is unknown".to_string(),
                ),
                argv: Some(vec![
                    runner.clone(),
                    "--testNamePattern".to_string(),
                    anchored_regex(name),
                ]),
                scope: Some("target_test".to_string()),
                exactness: Some(prog_core::ActionExactness::Filter),
                derived_from: Some("jest_vitest.failed_name_filter".to_string()),
                does_not_satisfy: does_not_satisfy.to_vec(),
                cwd: None,
                extra: Extra::new(),
            });
        }
        return actions;
    }
    if paths.len() == 1 && names.is_empty() {
        let path = paths.first().expect("checked exactly one path");
        if safe_positional_identity(path) {
            actions.push(NextAction {
                kind: "rerun".to_string(),
                operation: None,
                path: None,
                reason: Some(
                    "rerun this failed test file; no individual test name was proven".to_string(),
                ),
                argv: Some(vec![runner.clone(), path.clone()]),
                scope: Some("target_file".to_string()),
                exactness: Some(prog_core::ActionExactness::Approximate),
                derived_from: Some("jest_vitest.failed_path".to_string()),
                does_not_satisfy: does_not_satisfy.to_vec(),
                cwd: None,
                extra: Extra::new(),
            });
        }
    }
    actions
}

fn go_test_package(command: &[String], sections: &[RunFailureSection]) -> Option<String> {
    let mut supplied = BTreeSet::new();
    let mut skip_value = false;
    for argument in command
        .iter()
        .skip(2)
        .take_while(|argument| argument.as_str() != "--")
    {
        if skip_value {
            skip_value = false;
            continue;
        }
        if go_test_flag_takes_value(argument) {
            skip_value = true;
            continue;
        }
        if argument.starts_with('-') {
            continue;
        }
        if safe_positional_identity(argument) {
            supplied.insert(argument);
        }
    }
    if supplied.len() == 1 {
        let package = supplied.first()?.to_string();
        if package != "./..." && !package.contains("...") {
            return Some(package);
        }
    }
    let output_packages = sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .filter_map(|line| line.trim_start().strip_prefix("FAIL\t"))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|package| safe_positional_identity(package) && !package.contains("..."))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    (output_packages.len() == 1)
        .then(|| output_packages.into_iter().next())
        .flatten()
}

fn go_test_flag_takes_value(argument: &str) -> bool {
    matches!(
        argument,
        "-run"
            | "-bench"
            | "-count"
            | "-cpu"
            | "-timeout"
            | "-parallel"
            | "-shuffle"
            | "-tags"
            | "-mod"
    )
}

fn go_test_name(line: &str) -> Option<String> {
    let name = line.trim_start().strip_prefix("--- FAIL: ")?;
    let name = name.split(" (").next()?.trim();
    safe_flag_value(name).then(|| name.to_string())
}

fn cargo_test_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let name = trimmed
        .strip_prefix("test ")
        .and_then(|line| line.strip_suffix(" ... FAILED"))
        .or_else(|| {
            trimmed
                .strip_prefix("---- ")
                .and_then(|line| line.strip_suffix(" stdout ----"))
        })?
        .trim();
    safe_positional_identity(name).then(|| name.to_string())
}

fn cargo_exact_target_args(command: &[String]) -> Option<Vec<String>> {
    let args = command
        .get(2..)?
        .iter()
        .take_while(|argument| argument.as_str() != "--");
    let args = args.cloned().collect::<Vec<_>>();
    let mut target = Vec::new();
    let mut exact_harness = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--lib" => {
                exact_harness = true;
                target.push(args[index].clone());
            }
            "--bin" | "--example" | "--test" | "--bench" => {
                let value = args.get(index + 1)?;
                if !safe_positional_identity(value) {
                    return None;
                }
                exact_harness = true;
                target.extend([args[index].clone(), value.clone()]);
                index += 1;
            }
            "-p" | "--package" => {
                let value = args.get(index + 1)?;
                if !safe_positional_identity(value) {
                    return None;
                }
                target.extend([args[index].clone(), value.clone()]);
                index += 1;
            }
            _ => return None,
        }
        index += 1;
    }
    exact_harness.then_some(target)
}

fn jest_vitest_path(line: &str) -> Option<String> {
    let path = line.trim_start().strip_prefix("FAIL ")?.trim();
    let path = path.split_whitespace().next().unwrap_or_default();
    safe_positional_identity(path).then(|| path.to_string())
}

fn jest_vitest_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let name = ["\u{25cf} ", "\u{2715} ", "\u{00d7} "]
        .iter()
        .find_map(|marker| trimmed.strip_prefix(marker))?
        .trim();
    let name = name
        .rsplit_once(" (")
        .filter(|(_, suffix)| suffix.ends_with("ms)"))
        .map(|(name, _)| name)
        .unwrap_or(name)
        .trim();
    safe_flag_value(name).then(|| name.to_string())
}

fn anchored_go_test_pattern(name: &str) -> String {
    name.split('/')
        .map(anchored_regex)
        .collect::<Vec<_>>()
        .join("/")
}

fn anchored_regex(value: &str) -> String {
    format!("^{}$", regex_escape(value))
}

fn regex_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn safe_positional_identity(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.contains('\0')
        && !value.contains(char::is_whitespace)
}

fn safe_flag_value(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains('\0')
}

fn run_provenance(
    run_id: &str,
    cache_key: &str,
    redacted_argv: &[String],
    cwd: &Path,
    duration_ms: u64,
    status: &RunProcessStatus,
    args: &RunArgs,
) -> CallProvenance {
    let mut extra = Extra::new();
    extra.insert(
        "run".to_string(),
        json!({
            "argv": redacted_argv,
            "cwd": cwd.to_string_lossy(),
            "timeout_ms": args.timeout_ms,
            "max_stdout_bytes": args.max_stdout_bytes,
            "max_stderr_bytes": args.max_stderr_bytes,
            "preserve_exit_code": args.preserve_exit_code,
            "exit_code": match status {
                RunProcessStatus::Exited { code, .. } => json!(code),
                _ => Value::Null,
            },
            "signal": match status {
                RunProcessStatus::Exited { signal, .. } => json!(signal),
                _ => Value::Null,
            },
            "timed_out": matches!(status, RunProcessStatus::TimedOut)
        }),
    );
    CallProvenance {
        source_call_id: run_id.to_string(),
        cache_key: Some(cache_key.to_string()),
        captured_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        status: Some(run_status_name(status).to_string()),
        duration_ms: Some(duration_ms),
        extra,
    }
}

fn run_warnings(
    status: &RunProcessStatus,
    args: &RunArgs,
    stdout: &RunCapture,
    stderr: &RunCapture,
) -> Vec<String> {
    let mut warnings = Vec::new();
    match status {
        RunProcessStatus::Exited {
            success: false,
            code,
            signal,
        } => {
            warnings.push(format!(
                "child command exited unsuccessfully: exit_code={code:?}, signal={signal:?}; envelope still returned successfully"
            ));
        }
        RunProcessStatus::TimedOut => warnings.push(format!(
            "child command timed out after {} ms and was killed",
            args.timeout_ms
        )),
        RunProcessStatus::SpawnError(message) => {
            warnings.push(format!("child command could not be started: {message}"));
        }
        RunProcessStatus::Exited { success: true, .. } => {}
    }
    if stdout.truncated {
        warnings.push(format!(
            "{} exceeded max_stdout_bytes ({}); captured output was truncated",
            stdout.stream, args.max_stdout_bytes
        ));
    }
    if stderr.truncated {
        warnings.push(format!(
            "{} exceeded max_stderr_bytes ({}); captured diagnostics were truncated",
            stderr.stream, args.max_stderr_bytes
        ));
    }
    warnings
}

fn run_status_name(status: &RunProcessStatus) -> &'static str {
    match status {
        RunProcessStatus::Exited { success: true, .. } => "success",
        RunProcessStatus::Exited { success: false, .. } => "exit_nonzero",
        RunProcessStatus::TimedOut => "timeout",
        RunProcessStatus::SpawnError(_) => "spawn_error",
    }
}

fn run_exit_code(status: &RunProcessStatus) -> RunExitCode {
    match status {
        RunProcessStatus::Exited { success: true, .. } => RunExitCode::Success,
        RunProcessStatus::Exited {
            code: Some(code), ..
        } => RunExitCode::Code(*code),
        RunProcessStatus::Exited {
            signal: Some(signal),
            ..
        } => RunExitCode::Signal(*signal),
        RunProcessStatus::Exited { .. } => RunExitCode::Code(1),
        RunProcessStatus::TimedOut => RunExitCode::Timeout,
        RunProcessStatus::SpawnError(_) => RunExitCode::SpawnError,
    }
}

pub(crate) fn child_exit_code(code: RunExitCode) -> ExitCode {
    let raw = match code {
        RunExitCode::Success => 0,
        RunExitCode::Code(code) => code.clamp(1, 255),
        RunExitCode::Signal(signal) => (128 + signal).clamp(1, 255),
        RunExitCode::Timeout => 124,
        RunExitCode::SpawnError => 127,
    };
    ExitCode::from(raw as u8)
}

fn run_effects() -> EffectSet {
    EffectSet {
        read_only: false,
        mutating: true,
        network: true,
        shell: true,
        sensitive: false,
        cacheable: true,
        requires_confirmation: false,
        extra: Extra::new(),
    }
}

pub(crate) fn redact_run_argv(argv: &[String]) -> Vec<String> {
    let mut redact_next = false;
    argv.iter()
        .map(|arg| {
            if redact_next {
                redact_next = false;
                return "[REDACTED:run_arg_secret]".to_string();
            }
            // Inline form first: `--access-token=JWT` or `--access-token:JWT`.
            // Checking this before the bare-flag path ensures the secret value
            // embedded in the same element is redacted rather than leaked.
            if let Some(redacted) = redact_inline_secret(arg) {
                return redacted;
            }
            // Bare flag form: `--access-token` marks the *next* element as the
            // value to redact.
            if is_sensitive_flag(arg) {
                redact_next = true;
                return arg.clone();
            }
            redact_observed_text(arg)
        })
        .collect()
}

fn is_sensitive_flag(arg: &str) -> bool {
    let trimmed = arg.trim_start_matches('-');
    prog_core::is_sensitive_name(trimmed)
}

/// If `arg` is an inline `name<sep>value` whose name is sensitive, return the
/// redacted form; otherwise `None`. Catches compound flag names like
/// `--access-token=...` and `--passwd=...` that the bare-flag path would miss.
fn redact_inline_secret(arg: &str) -> Option<String> {
    for separator in ['=', ':'] {
        if let Some((name, value)) = arg.split_once(separator)
            && !value.is_empty()
            && prog_core::is_sensitive_name(name.trim_start_matches('-'))
        {
            return Some(format!("{name}{separator}[REDACTED:run_arg_secret]"));
        }
    }
    None
}

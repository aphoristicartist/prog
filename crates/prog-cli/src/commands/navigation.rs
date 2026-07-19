//! Cursor navigation commands: inspect, evidence, and search.

use crate::*;

struct CursorContext {
    record: ValidatedCursor,
    entry: CacheEntryMeta,
    observation: Option<prog_core::ObservationRecord>,
    payload: PersistedPayload,
    target_path: String,
    age_seconds: u64,
    cache: CacheInfo,
}

fn cursor_context(store: &Store, cursor: &str, requested_path: &str) -> Result<CursorContext> {
    let record = store.get_cursor(cursor)?;
    let entry = store
        .get_entry(&record.cache_key)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let observation = record
        .observation_id
        .as_deref()
        .map(|observation_id| store.get_observation(observation_id))
        .transpose()?
        .flatten();
    let payload = store
        .get_payload(&entry.payload_hash)?
        .ok_or_else(|| CoreError::CacheMiss(record.cache_key.clone()))?;
    let target_path = if requested_path.is_empty() {
        record.root_path.clone()
    } else {
        requested_path.to_string()
    };
    let scoped = ScopedSlice::new(
        ExpansionScope::from_cursor(&record)?,
        SliceRequest {
            path: Some(target_path.clone()),
            limit: None,
            depth: None,
            fields: Vec::new(),
            omit: Vec::new(),
            extra: Extra::new(),
        },
    )?;
    let target_path = scoped.target_path().as_str().to_string();
    if prog_core::pointer::get(payload.as_value(), &target_path)?.is_none() {
        return Err(CoreError::PathNotFound {
            path: target_path,
            hint: prog_core::pointer::siblings_hint(payload.as_value(), requested_path),
        });
    }
    let age_seconds = age_seconds(&entry.created_at)?;
    let cache = cache_info(CacheStatus::Hit, &entry, Some(age_seconds));
    Ok(CursorContext {
        record,
        entry,
        observation,
        payload,
        target_path,
        age_seconds,
        cache,
    })
}

pub(crate) fn inspect_cursor(
    store: &Store,
    lens_dir: &Path,
    args: &InspectArgs,
    ctx: &InvocationContext,
) -> Result<InspectResponse> {
    if args.goal.trim().is_empty() {
        return Err(CoreError::BadArgs {
            operation: "inspect".to_string(),
            reason: "--goal must not be empty".to_string(),
        });
    }
    if args.limit > 100 {
        return Err(CoreError::BadArgs {
            operation: "inspect".to_string(),
            reason: "--limit must be at most 100".to_string(),
        });
    }
    let context = cursor_context(store, &args.cursor, &args.path)?;
    let request = InspectRequest::builder(args.cursor.clone())
        .goal(args.goal.clone())
        .scope_path(context.target_path.clone())
        .limit(args.limit.saturating_mul(4).min(100))
        .hints(CommandHintConfig::NAV_ALL)
        .build();
    let mut response = build_inspect_response(context.payload.as_value(), &request)?;
    let lens = cursor_lens(lens_dir, context.record.record(), &mut response.warnings);
    let options = FindingOptions {
        goal: Some(args.goal.clone()),
        cursor: Some(args.cursor.clone()),
        scope_path: Some(context.target_path.clone()),
        limit: args.limit.saturating_mul(4).min(100),
        hints: CommandHintConfig::NAV_ALL,
        workspace_root: std::env::current_dir().ok(),
        identity: FindingIdentityContext {
            provider: context
                .observation
                .as_ref()
                .and_then(|observation| observation.provider.clone()),
            parser: context
                .observation
                .as_ref()
                .and_then(|observation| observation.parser.clone()),
            lens: context
                .observation
                .as_ref()
                .and_then(|observation| observation.lens.clone()),
        },
    };
    response.findings =
        ranked_findings_with_lens(context.payload.as_value(), &options, lens.as_ref())?;
    if let Some(kind) = &args.kind {
        let kind = normalize_finding_kind(kind);
        response
            .findings
            .retain(|finding| finding_matches_kind(finding, &kind));
    }
    response.findings.truncate(args.limit);
    for (index, finding) in response.findings.iter_mut().enumerate() {
        finding.rank = index as u64 + 1;
        if let Some(value) = prog_core::pointer::get(context.payload.as_value(), &finding.path)? {
            finding.evidence_ref = Some(evidence_ref(EvidenceRefInput {
                source_id: &context.record.source_id,
                operation: &context.record.operation,
                cursor: Some(&args.cursor),
                path: &finding.path,
                value,
                observation: context.observation.as_ref(),
                provenance: context.entry.provenance.as_ref(),
                cache: Some(&context.cache),
                omitted: &[],
                redacted_paths: 0,
            }));
        }
    }
    let cache_stale = cache_is_stale(Some(&context.cache));
    response.cache = Some(context.cache);
    let scoped_value = prog_core::pointer::get(context.payload.as_value(), &context.target_path)?
        .expect("cursor_context validated the target path");
    if exceeds_node_budget(scoped_value, 10_000, 64) {
        response.omitted.push(OmittedRegion {
            path: context.target_path.clone(),
            reason: OmissionReason::NodeBudget,
            detail: Some("inspect finding traversal reached the 10000-node budget".to_string()),
            extra: Extra::new(),
        });
        response.warnings.push(
            "inspect findings are partial; narrow --path to traverse a smaller subtree".to_string(),
        );
    }
    if cache_stale {
        response.warnings.push(format!(
            "cached payload age_seconds={}; inspect did not contact upstream",
            context.age_seconds
        ));
    }
    bound_inspect_response(&mut response, ctx.max_envelope_bytes())?;
    Ok(response)
}

pub(crate) fn evidence_cursor(
    store: &Store,
    lens_dir: &Path,
    args: &EvidenceArgs,
    ctx: &InvocationContext,
) -> Result<EvidenceBlock> {
    let context = cursor_context(store, &args.cursor, &args.path)?;
    let value = prog_core::pointer::get(context.payload.as_value(), &context.target_path)?
        .expect("cursor_context validated the target path");
    let mut block = evidence_block(
        context.payload.as_value(),
        args.cursor.clone(),
        &context.target_path,
    )?;
    let mut lens_warnings = Vec::new();
    let lens = cursor_lens(lens_dir, context.record.record(), &mut lens_warnings);
    if let Some(finding) = ranked_findings_with_lens(
        context.payload.as_value(),
        &FindingOptions {
            cursor: Some(args.cursor.clone()),
            scope_path: Some(context.target_path.clone()),
            limit: 20,
            hints: CommandHintConfig::NAV_ALL,
            ..FindingOptions::default()
        },
        lens.as_ref(),
    )?
    .into_iter()
    .find(|finding| finding.path == context.target_path)
    {
        block.kind = finding.kind;
        if let Some(lens_id) = finding.lens_id {
            block.extra.insert("lens_id".to_string(), json!(lens_id));
        }
    }
    block.warnings.extend(lens_warnings);
    block.evidence_ref = Some(evidence_ref(EvidenceRefInput {
        source_id: &context.record.source_id,
        operation: &context.record.operation,
        cursor: Some(&args.cursor),
        path: &context.target_path,
        value,
        observation: context.observation.as_ref(),
        provenance: context.entry.provenance.as_ref(),
        cache: Some(&context.cache),
        omitted: &[],
        redacted_paths: 0,
    }));
    block.source_command = source_command_from_provenance(context.entry.provenance.as_ref());
    block.provenance = context.entry.provenance;
    let cache_stale = cache_is_stale(Some(&context.cache));
    block.cache = Some(context.cache);
    if cache_stale {
        block.warnings.push(format!(
            "cached payload age_seconds={}; evidence did not contact upstream",
            context.age_seconds
        ));
    }
    bound_evidence_block(&mut block, ctx.max_envelope_bytes())?;
    Ok(block)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn search_cursor(
    store: &Store,
    lens_dir: &Path,
    cursor: &str,
    query: Option<String>,
    kind: Option<String>,
    path: &str,
    limit: usize,
    case_sensitive: bool,
    regex: bool,
    ctx: &InvocationContext,
) -> Result<SearchResponse> {
    if limit > 200 {
        return Err(CoreError::BadArgs {
            operation: "search".to_string(),
            reason: "--limit must be at most 200".to_string(),
        });
    }
    let context = cursor_context(store, cursor, path)?;
    let mut lens_warnings = Vec::new();
    let lens = cursor_lens(lens_dir, context.record.record(), &mut lens_warnings);
    let mut response = search_payload_with_lens(
        context.payload.as_value(),
        cursor.to_string(),
        &SearchOptions {
            query,
            kind,
            scope_path: Some(context.target_path),
            limit,
            case_sensitive,
            regex,
            workspace_root: std::env::current_dir().ok(),
            ..SearchOptions::default()
        },
        lens.as_ref(),
    )?;
    response.warnings.extend(lens_warnings);
    let cache_stale = cache_is_stale(Some(&context.cache));
    response.cache = Some(context.cache);
    if cache_stale {
        response.warnings.push(format!(
            "cached payload age_seconds={}; search did not contact upstream",
            context.age_seconds
        ));
    }
    bound_search_response(&mut response, ctx.max_envelope_bytes())?;
    Ok(response)
}

fn cursor_lens(
    lens_dir: &Path,
    record: &prog_core::CursorRecord,
    warnings: &mut Vec<String>,
) -> Option<LensManifest> {
    let id = record.extra.get("lens_id").and_then(Value::as_str)?;
    match load_lens(lens_dir, id, "inspect") {
        Ok(lens) => Some(lens),
        Err(error) => {
            warnings.push(format!(
                "lens '{id}' recorded on the cursor could not be loaded; used generic findings: {error}"
            ));
            None
        }
    }
}

fn normalize_finding_kind(kind: &str) -> String {
    kind.trim().to_ascii_lowercase().replace('-', "_")
}

fn finding_matches_kind(finding: &prog_core::Finding, kind: &str) -> bool {
    normalize_finding_kind(&finding.kind) == kind
        || (kind == "error"
            && (finding.severity.as_deref() == Some("error")
                || finding.kind.contains("error")
                || finding.kind.contains("failure")
                || finding.kind.contains("exception")
                || finding.kind.contains("panic")
                || finding.kind.contains("timeout")))
        || (kind == "warning" && finding.severity.as_deref() == Some("warning"))
}

fn source_command_from_provenance(provenance: Option<&CallProvenance>) -> Option<String> {
    let argv = provenance?
        .extra
        .get("run")?
        .get("argv")?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?;
    Some(
        argv.into_iter()
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn bound_inspect_response(response: &mut InspectResponse, max_envelope_bytes: usize) -> Result<()> {
    let budget = max_envelope_bytes;
    let original_len = response.findings.len();
    while serde_json::to_vec(response)?.len() > budget && !response.findings.is_empty() {
        response.findings.pop();
    }
    if response.findings.len() < original_len
        && response
            .omitted
            .iter()
            .all(|region| region.reason != OmissionReason::NodeBudget)
    {
        response.omitted.push(OmittedRegion {
            path: response.scope_path.clone().unwrap_or_default(),
            reason: OmissionReason::NodeBudget,
            detail: Some("inspect findings were compacted to the disclosure budget".to_string()),
            extra: Extra::new(),
        });
        response.warnings.push(format!(
            "inspect retained {} of {original_len} findings to enforce the disclosure budget",
            response.findings.len()
        ));
    }
    while serde_json::to_vec(response)?.len() > budget && !response.findings.is_empty() {
        response.findings.pop();
    }
    if serde_json::to_vec(response)?.len() > budget {
        response.warnings.truncate(1);
        response.omitted.truncate(1);
    }
    Ok(())
}

fn bound_search_response(response: &mut SearchResponse, max_envelope_bytes: usize) -> Result<()> {
    let budget = max_envelope_bytes;
    let original_len = response.hits.len();
    while serde_json::to_vec(response)?.len() > budget && !response.hits.is_empty() {
        response.hits.pop();
    }
    if response.hits.len() < original_len {
        response.omitted.push(OmittedRegion {
            path: response.scope_path.clone().unwrap_or_default(),
            reason: OmissionReason::NodeBudget,
            detail: Some("search hits were compacted to the disclosure budget".to_string()),
            extra: Extra::new(),
        });
        response.warnings.push(format!(
            "search retained {} of {original_len} hits to enforce the disclosure budget",
            response.hits.len()
        ));
    }
    while serde_json::to_vec(response)?.len() > budget && !response.hits.is_empty() {
        response.hits.pop();
    }
    for (index, hit) in response.hits.iter_mut().enumerate() {
        hit.rank = index as u64 + 1;
    }
    Ok(())
}

fn bound_evidence_block(block: &mut EvidenceBlock, max_envelope_bytes: usize) -> Result<()> {
    let budget = max_envelope_bytes;
    if serde_json::to_vec(block)?.len() > budget {
        block.citations.truncate(1);
        if let Some(citation) = block.citations.first_mut() {
            citation.excerpt = json!("excerpt compacted; use commands.expand");
        }
        block.excerpt = json!("excerpt compacted; use commands.expand");
        block.warnings.truncate(2);
        block
            .warnings
            .push("evidence excerpt compacted to the disclosure budget".to_string());
    }
    if serde_json::to_vec(block)?.len() > budget {
        block.provenance = None;
        block.citations.clear();
        block.extra.clear();
        block.warnings.truncate(1);
        block.summary = block.summary.chars().take(180).collect();
        block.commands.inspect = None;
        block.commands.search = None;
        block.commands.evidence = None;
    }
    Ok(())
}

fn exceeds_node_budget(value: &Value, max_nodes: usize, max_depth: usize) -> bool {
    fn visit(
        value: &Value,
        depth: usize,
        max_depth: usize,
        max_nodes: usize,
        visited: &mut usize,
    ) -> bool {
        if depth > max_depth || *visited >= max_nodes {
            return true;
        }
        *visited += 1;
        match value {
            Value::Array(items) => items
                .iter()
                .any(|item| visit(item, depth + 1, max_depth, max_nodes, visited)),
            Value::Object(map) => map
                .values()
                .any(|item| visit(item, depth + 1, max_depth, max_nodes, visited)),
            _ => false,
        }
    }
    let mut visited = 0usize;
    visit(value, 0, max_depth, max_nodes, &mut visited)
}

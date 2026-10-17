//! Collection moves: `select` (filter, project, page) and `coverage`.

use std::collections::BTreeSet;

use crate::commands::navigation::cursor_context;
use crate::*;

/// Rows a single select may request, before budget fitting.
const MAX_SELECT_LIMIT: usize = 500;
/// Unreturned index ranges listed by `coverage`.
const MAX_COVERAGE_RANGES: usize = 20;
/// Session-event key holding the collection and item ranges a response returned.
const COVERED_KEY: &str = "covered";

/// Where a payload's records live: parsed stdout for a run, otherwise the
/// requested root. A run without parsed JSON has no record collection; its
/// arrays (`combined`, `failure_sections`) are capture bookkeeping.
pub(crate) fn collection_search_root(payload: &Value, start: &str) -> Option<String> {
    let node = prog_core::pointer::get(payload, start).ok()??;
    if node.get("format").and_then(Value::as_str) == Some("run") {
        return node
            .pointer("/stdout/json")
            .map(|_| prog_core::pointer::push(&prog_core::pointer::push(start, "stdout"), "json"));
    }
    Some(start.to_string())
}

/// The collection a cursor-level command addresses when no path is given.
fn default_collection(payload: &Value, root: &str) -> Option<String> {
    let start = collection_search_root(payload, root)?;
    prog_core::find_collection(payload, &start)
}

pub(crate) fn select_cursor(
    store: &Store,
    args: &SelectArgs,
    ctx: &InvocationContext,
) -> Result<SelectResponse> {
    let context = cursor_context(store, &args.cursor, args.path.as_deref().unwrap_or(""))?;
    let path = match &args.path {
        Some(_) => context.target_path.clone(),
        None => default_collection(context.payload.as_value(), &context.target_path).ok_or_else(
            || CoreError::BadArgs {
                operation: "select".to_string(),
                reason: format!(
                    "no record collection under {}; pass --path to name an array or object",
                    display_path(&context.target_path)
                ),
            },
        )?,
    };
    let filters = args
        .filters
        .iter()
        .map(|filter| prog_core::SelectFilter::parse(filter))
        .collect::<Result<Vec<_>>>()?;
    if args.limit == 0 || args.limit > MAX_SELECT_LIMIT {
        return Err(CoreError::BadArgs {
            operation: "select".to_string(),
            reason: format!("--limit must be between 1 and {MAX_SELECT_LIMIT}"),
        });
    }
    let fields = args
        .fields
        .iter()
        .map(|field| field.trim().to_string())
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();

    let mut limit = args.limit;
    let mut string_chars = args.max_chars.max(16);
    loop {
        let request = prog_core::SelectRequest {
            filters: &filters,
            fields: &fields,
            offset: args.offset,
            limit,
            string_chars,
        };
        let result = prog_core::select_rows(context.payload.as_value(), &path, &request)?;
        let mut response = select_response(args, &context, &path, &fields, result.clone());
        if limit < args.limit && !response.rows.is_empty() {
            response.warnings.push(format!(
                "returned {} of the {} requested record(s) to fit the response budget; continue with --offset {}",
                response.rows.len(),
                args.limit,
                response.next_offset.unwrap_or(response.offset + response.rows.len() as u64)
            ));
        }
        let bytes = serde_json::to_vec(&response)?.len();
        let fits = bytes <= ctx.max_envelope_bytes();
        if fits || (limit == 1 && string_chars <= 16) {
            record_select_event(store, &args.cursor, &path, &result);
            return Ok(response);
        }
        if limit > 1 {
            limit = (limit / 2).max(1);
        } else {
            string_chars = (string_chars / 2).max(16);
        }
    }
}

fn select_response(
    args: &SelectArgs,
    context: &crate::commands::navigation::CursorContext,
    path: &str,
    fields: &[String],
    result: prog_core::SelectResult,
) -> SelectResponse {
    let mut warnings = Vec::new();
    if result.unknown > 0 {
        warnings.push(format!(
            "{} record(s) have a redacted filter field; they are counted as unknown, neither matching nor excluded",
            result.unknown
        ));
    }
    if cache_is_stale(Some(&context.cache)) {
        warnings.push(format!(
            "cached payload age_seconds={}; rerun the capture to refresh it",
            context.cache.age_seconds.unwrap_or(0)
        ));
    }
    SelectResponse {
        schema: prog_core::SELECT_SCHEMA.to_string(),
        cursor: args.cursor.clone(),
        path: path.to_string(),
        total: result.total,
        matched: result.matched,
        unknown: result.unknown,
        offset: args.offset as u64,
        rows: result.rows,
        next_offset: result.next_offset,
        fields: fields.to_vec(),
        filters: args.filters.clone(),
        omitted: result.omitted,
        cache: Some(context.cache.clone()),
        warnings,
        extra: Extra::new(),
    }
}

fn record_select_event(store: &Store, cursor: &str, path: &str, result: &prog_core::SelectResult) {
    let indices = result
        .returned
        .iter()
        .filter_map(|segment| segment.parse::<u64>().ok())
        .collect::<BTreeSet<_>>();
    let mut extra = Extra::new();
    if !indices.is_empty() {
        extra.insert(
            COVERED_KEY.to_string(),
            json!({"path": path, "items": prog_core::index_ranges(&indices)}),
        );
    }
    let _ = store.record_session_event(NewSessionEvent {
        kind: "select".to_string(),
        cursor: Some(cursor.to_string()),
        path: Some(path.to_string()),
        evidence_ref: None,
        summary: Some(format!(
            "selected {} of {} matching record(s)",
            result.rows.len(),
            result.matched
        )),
        extra,
    });
    let _ = store.release();
}

/// Session-event metadata naming the collection items an envelope preview
/// returned, so `coverage` can count them later.
pub(crate) fn envelope_covered_extra(envelope: &DisclosureEnvelope) -> Option<Value> {
    let overview = envelope.extra.get("overview")?;
    let path = overview.get("path")?.as_str()?;
    let root = envelope
        .observation
        .as_ref()
        .map(|observation| observation.completeness.root_path.as_str())
        .unwrap_or("");
    let shown = visible_items(&envelope.data_preview, root, path)?;
    if shown == 0 {
        return None;
    }
    let indices = (0..shown as u64).collect::<BTreeSet<_>>();
    Some(json!({"path": path, "items": prog_core::index_ranges(&indices)}))
}

/// Items of the collection at absolute `path` that a preview rooted at
/// `root` shows in full or in part. `None` when the preview does not reach it.
pub(crate) fn visible_items(preview: &Value, root: &str, path: &str) -> Option<usize> {
    let relative = path.strip_prefix(root)?;
    if !(relative.is_empty() || relative.starts_with('/')) {
        return None;
    }
    let node = if relative.is_empty() {
        Some(preview)
    } else {
        preview.pointer(relative)
    };
    match node {
        Some(Value::Array(items)) => Some(items.len()),
        _ => Some(0),
    }
}

pub(crate) fn coverage_cursor(store: &Store, args: &CoverageArgs) -> Result<CoverageReport> {
    let context = cursor_context(store, &args.cursor, args.path.as_deref().unwrap_or(""))?;
    let path = match &args.path {
        Some(_) => context.target_path.clone(),
        None => default_collection(context.payload.as_value(), &context.target_path).ok_or_else(
            || CoreError::BadArgs {
                operation: "coverage".to_string(),
                reason: format!(
                    "no record collection under {}; pass --path to name an array",
                    display_path(&context.target_path)
                ),
            },
        )?,
    };
    let Some(Value::Array(items)) = prog_core::pointer::get(context.payload.as_value(), &path)?
    else {
        return Err(CoreError::BadArgs {
            operation: "coverage".to_string(),
            reason: format!("{path} is not an array"),
        });
    };
    let total = items.len() as u64;
    let trail = store.get_session(None)?;
    let mut returned = BTreeSet::new();
    let mut warnings = vec![
        "returned counts records prog sent in this session, in full or in part; it does not show what a reader used"
            .to_string(),
    ];
    if let Some(trail) = &trail {
        for event in trail
            .events
            .iter()
            .filter(|event| event.cursor.as_deref() == Some(args.cursor.as_str()))
        {
            if let Some(covered) = event.extra.get(COVERED_KEY)
                && covered.get("path").and_then(Value::as_str) == Some(path.as_str())
            {
                returned.extend(prog_core::parse_index_ranges(
                    covered.get("items").and_then(Value::as_str).unwrap_or(""),
                ));
            }
            if matches!(event.kind.as_str(), "expand" | "evidence")
                && let Some(index) = event
                    .path
                    .as_deref()
                    .and_then(|event_path| item_index(event_path, &path))
            {
                returned.insert(index);
            }
        }
        if trail
            .warnings
            .iter()
            .any(|warning| warning.contains("1000-event cap"))
        {
            warnings.push(
                "the session trail dropped its oldest events; earlier returns are not counted"
                    .to_string(),
            );
        }
    }
    returned.retain(|index| *index < total);
    let missing = prog_core::missing_index_ranges(total, &returned);
    let ranges_truncated = missing.len() > MAX_COVERAGE_RANGES;
    let report = CoverageReport {
        schema: prog_core::COVERAGE_SCHEMA.to_string(),
        cursor: args.cursor.clone(),
        path,
        items: total,
        returned: returned.len() as u64,
        not_returned: total - returned.len() as u64,
        not_returned_ranges: missing.into_iter().take(MAX_COVERAGE_RANGES).collect(),
        ranges_truncated,
        warnings,
    };
    Ok(report)
}

/// Index of the collection item that `event_path` points into, if any.
fn item_index(event_path: &str, collection: &str) -> Option<u64> {
    let rest = event_path.strip_prefix(collection)?.strip_prefix('/')?;
    rest.split('/').next()?.parse().ok()
}

fn display_path(path: &str) -> &str {
    if path.is_empty() {
        "the cursor root"
    } else {
        path
    }
}

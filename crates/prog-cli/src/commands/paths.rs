//! Cursor-path navigation command.

use std::collections::BTreeSet;

use crate::*;

pub(crate) fn collect_paths(
    value: &Value,
    path: &str,
    depth: usize,
    limit: usize,
    out: &mut Vec<PathEntry>,
) -> bool {
    let mut truncated = false;
    collect_paths_inner(value, path, depth, limit, out, &mut truncated);
    truncated
}

fn collect_paths_inner(
    value: &Value,
    path: &str,
    depth: usize,
    limit: usize,
    out: &mut Vec<PathEntry>,
    truncated: &mut bool,
) {
    if out.len() >= limit {
        *truncated = true;
        return;
    }

    out.push(PathEntry {
        path: path.to_string(),
        kind: value_kind(value).to_string(),
        expandable: matches!(value, Value::Array(_) | Value::Object(_)),
        omitted_reason: None,
        detail: None,
        evidence_ref: None,
    });

    if depth == 0 {
        if matches!(value, Value::Array(items) if !items.is_empty())
            || matches!(value, Value::Object(map) if !map.is_empty())
        {
            *truncated = true;
        }
        return;
    }

    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                if out.len() >= limit {
                    *truncated = true;
                    break;
                }
                let child_path = prog_core::pointer::push(path, &index.to_string());
                collect_paths_inner(item, &child_path, depth - 1, limit, out, truncated);
            }
        }
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if out.len() >= limit {
                    *truncated = true;
                    break;
                }
                let child_path = prog_core::pointer::push(path, key);
                collect_paths_inner(&map[key], &child_path, depth - 1, limit, out, truncated);
            }
        }
        _ => {}
    }
}

pub(crate) fn annotate_path_omissions(paths: &mut [PathEntry], omitted: &[OmittedRegion]) {
    let omitted_by_path = omitted
        .iter()
        .map(|region| (region.path.as_str(), region))
        .collect::<BTreeMap<_, _>>();

    for path in paths {
        if let Some(region) = omitted_by_path.get(path.path.as_str()) {
            path.expandable = true;
            path.omitted_reason = Some(region.reason);
            path.detail.clone_from(&region.detail);
        }
    }
}

pub(crate) fn append_missing_omitted_paths(
    paths: &mut Vec<PathEntry>,
    omitted: &[OmittedRegion],
    limit: usize,
) {
    let mut seen = paths
        .iter()
        .map(|path| path.path.clone())
        .collect::<BTreeSet<_>>();
    for region in omitted {
        if paths.len() >= limit {
            break;
        }
        if !seen.insert(region.path.clone()) {
            continue;
        }
        paths.push(PathEntry {
            path: region.path.clone(),
            kind: "omitted".to_string(),
            expandable: true,
            omitted_reason: Some(region.reason),
            detail: region.detail.clone(),
            evidence_ref: None,
        });
    }
}

fn attach_path_evidence_refs(
    paths: &mut [PathEntry],
    payload: &Value,
    context: PathEvidenceContext<'_>,
) -> Result<()> {
    for path in paths {
        if !path.expandable && path.omitted_reason.is_none() {
            continue;
        }
        if let Some(value) = prog_core::pointer::get(payload, &path.path)? {
            path.evidence_ref = Some(evidence_ref(EvidenceRefInput {
                source_id: &context.record.source_id,
                operation: &context.record.operation,
                cursor: Some(context.cursor),
                path: &path.path,
                value,
                observation: context.observation,
                provenance: context.entry.provenance.as_ref(),
                cache: Some(context.cache),
                omitted: context.omitted,
                redacted_paths: 0,
            }));
        }
    }
    Ok(())
}

pub(crate) fn expansion_next_actions(
    cursor: Option<&str>,
    operation: Option<&str>,
    omitted: &[OmittedRegion],
    limit: usize,
) -> Vec<NextAction> {
    let Some(cursor) = cursor else {
        return Vec::new();
    };
    let mut ranked = omitted.iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        omission_priority(right.reason)
            .cmp(&omission_priority(left.reason))
            .then_with(|| left.path.cmp(&right.path))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|region| expansion_next_action(cursor, operation, region))
        .collect()
}

fn expansion_next_action(
    cursor: &str,
    operation: Option<&str>,
    region: &OmittedRegion,
) -> NextAction {
    let action_kind = if region.reason == OmissionReason::LargeString {
        "evidence"
    } else {
        "expand"
    };
    let mut extra = Extra::new();
    extra.insert(
        "priority".to_string(),
        json!(omission_priority(region.reason)),
    );
    extra.insert(
        "omitted_reason".to_string(),
        json!(omission_reason_name(region.reason)),
    );
    if let Some(detail) = &region.detail {
        extra.insert("detail".to_string(), json!(detail));
    }
    extra.insert(
        "offline".to_string(),
        json!("uses cached redacted payload; does not contact upstream"),
    );
    NextAction {
        kind: action_kind.to_string(),
        operation: operation.map(str::to_string),
        path: Some(region.path.clone()),
        reason: Some(omission_action_reason(region)),
        argv: Some(match region.reason {
            OmissionReason::LargeString => vec![
                "prog".to_string(),
                "evidence".to_string(),
                cursor.to_string(),
                "--path".to_string(),
                region.path.clone(),
            ],
            _ => vec![
                "prog".to_string(),
                "expand".to_string(),
                cursor.to_string(),
                "--path".to_string(),
                region.path.clone(),
            ],
        }),
        scope: Some("cached_evidence".to_string()),
        exactness: Some(prog_core::ActionExactness::Exact),
        derived_from: Some("omitted_region".to_string()),
        extra,
        ..NextAction::default()
    }
}

fn omission_priority(reason: OmissionReason) -> u8 {
    match reason {
        OmissionReason::LargeString => 90,
        OmissionReason::DeepObject => 80,
        OmissionReason::ManyFields => 70,
        OmissionReason::LongArray => 60,
        OmissionReason::NodeBudget => 50,
        OmissionReason::Redacted => 10,
    }
}

fn omission_reason_name(reason: OmissionReason) -> &'static str {
    match reason {
        OmissionReason::LargeString => "large_string",
        OmissionReason::LongArray => "long_array",
        OmissionReason::ManyFields => "many_fields",
        OmissionReason::DeepObject => "deep_object",
        OmissionReason::NodeBudget => "node_budget",
        OmissionReason::Redacted => "redacted",
    }
}

fn omission_action_reason(region: &OmittedRegion) -> String {
    match region.reason {
        OmissionReason::LargeString => format!(
            "{} is a large string; emit a bounded evidence excerpt, or use expand --out for the full stored redacted value",
            region.path
        ),
        OmissionReason::LongArray => format!(
            "{} is a long array; expand with --limit to inspect selected items",
            region.path
        ),
        OmissionReason::ManyFields => format!(
            "{} has many fields; expand with --fields or --omit to inspect selected fields",
            region.path
        ),
        OmissionReason::DeepObject => format!(
            "{} was omitted by depth; expand with --depth to inspect nested structure",
            region.path
        ),
        OmissionReason::NodeBudget => format!(
            "{} was omitted by the global node budget; expand a narrower prefix",
            region.path
        ),
        OmissionReason::Redacted => format!(
            "{} is redacted before persistence; expansion will not reveal the original secret",
            region.path
        ),
    }
}

struct PathFilters {
    reason: Option<OmissionReason>,
    fields: BTreeSet<String>,
    omitted_only: bool,
    expandable_only: bool,
}

pub(crate) fn paths_cursor(store: &Store, args: &PathsArgs) -> Result<PathsResponse> {
    let filters = path_filters(args)?;
    let record = store.get_cursor(&args.cursor)?;
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
    let requested_prefix = if args.prefix.is_empty() {
        record.root_path.clone()
    } else {
        args.prefix.clone()
    };
    let scoped = ScopedSlice::new(
        ExpansionScope::from_cursor(&record)?,
        SliceRequest {
            path: Some(requested_prefix),
            limit: None,
            depth: None,
            fields: Vec::new(),
            omit: Vec::new(),
            extra: Extra::new(),
        },
    )?;
    let prefix = scoped.target_path().as_str().to_string();
    let value = payload.as_value();
    let target =
        prog_core::pointer::get(value, &prefix)?.ok_or_else(|| CoreError::PathNotFound {
            path: prefix.clone(),
            hint: prog_core::pointer::siblings_hint(value, &prefix),
        })?;
    let projection = project(target, &PreviewPolicy::default(), &prefix);
    let projected_omitted = projection.omitted.clone();
    let mut paths = Vec::new();
    let truncated = collect_paths(target, &prefix, args.depth, args.limit, &mut paths);
    annotate_path_omissions(&mut paths, &projected_omitted);
    append_missing_omitted_paths(&mut paths, &projected_omitted, args.limit);
    paths.retain(|path| path_matches_filters(path, &filters));
    let omitted = projection
        .omitted
        .into_iter()
        .filter(|region| omitted_matches_filters(region, &filters))
        .collect::<Vec<_>>();
    let next_actions = expansion_next_actions(
        Some(args.cursor.as_str()),
        Some(record.operation.as_str()),
        &omitted,
        args.limit.min(10),
    );
    let age = age_seconds(&entry.created_at)?;
    let mut warnings = Vec::new();
    if truncated {
        warnings.push(format!(
            "path listing reached --limit {}; use --prefix to narrow the result",
            args.limit
        ));
    }
    let cache = cache_info(CacheStatus::Hit, &entry, Some(age));
    if cache_is_stale(Some(&cache)) {
        warnings.push(format!(
            "cached payload age_seconds={age}; re-run the original observation or call to refresh"
        ));
    }
    attach_path_evidence_refs(
        &mut paths,
        value,
        PathEvidenceContext {
            record: record.record(),
            entry: &entry,
            observation: observation.as_ref(),
            cache: &cache,
            omitted: &projected_omitted,
            cursor: &args.cursor,
        },
    )?;

    Ok(PathsResponse {
        schema: DISCLOSURE_SCHEMA,
        cursor: args.cursor.clone(),
        source_id: record.source_id.clone(),
        operation: record.operation.clone(),
        root_path: record.root_path.clone(),
        prefix,
        paths,
        omitted,
        next_actions,
        cache,
        warnings,
    })
}

fn path_filters(args: &PathsArgs) -> Result<PathFilters> {
    let reason = args
        .reason
        .as_deref()
        .map(parse_omission_reason)
        .transpose()?;
    Ok(PathFilters {
        reason,
        fields: args.field.iter().cloned().collect(),
        omitted_only: args.omitted_only || reason.is_some(),
        expandable_only: args.expandable_only,
    })
}

fn parse_omission_reason(raw: &str) -> Result<OmissionReason> {
    let normalized = raw.replace('-', "_").to_ascii_lowercase();
    match normalized.as_str() {
        "large_string" => Ok(OmissionReason::LargeString),
        "long_array" => Ok(OmissionReason::LongArray),
        "many_fields" => Ok(OmissionReason::ManyFields),
        "deep_object" => Ok(OmissionReason::DeepObject),
        "node_budget" => Ok(OmissionReason::NodeBudget),
        "redacted" => Ok(OmissionReason::Redacted),
        _ => Err(CoreError::BadArgs {
            operation: "paths --reason".to_string(),
            reason: format!(
                "unknown omission reason '{raw}'; expected one of large_string, long_array, many_fields, deep_object, node_budget, redacted"
            ),
        }),
    }
}

fn path_matches_filters(path: &PathEntry, filters: &PathFilters) -> bool {
    if filters.expandable_only && !path.expandable {
        return false;
    }
    if filters.omitted_only && path.omitted_reason.is_none() {
        return false;
    }
    if let Some(reason) = filters.reason
        && path.omitted_reason != Some(reason)
    {
        return false;
    }
    if !filters.fields.is_empty() && !path_has_any_field(&path.path, &filters.fields) {
        return false;
    }
    true
}

fn omitted_matches_filters(region: &OmittedRegion, filters: &PathFilters) -> bool {
    if let Some(reason) = filters.reason
        && region.reason != reason
    {
        return false;
    }
    if !filters.fields.is_empty() && !path_has_any_field(&region.path, &filters.fields) {
        return false;
    }
    true
}

fn path_has_any_field(path: &str, fields: &BTreeSet<String>) -> bool {
    prog_core::pointer::parse(path)
        .map(|segments| segments.iter().any(|segment| fields.contains(segment)))
        .unwrap_or(false)
}

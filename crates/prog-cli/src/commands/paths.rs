//! Cursor-path navigation command.

use std::collections::BTreeSet;

use crate::*;

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

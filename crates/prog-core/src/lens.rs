use std::{cmp::Ordering, collections::BTreeMap};

use serde_json::{Map, Value, json};

use crate::{
    CoreError, ExpandablePayload, ExpansionScope, Extra, Finding, FindingCommandHints,
    FindingOptions, LENS_MANIFEST_VERSION, LensFindingRule, LensManifest, LensOmission, NextAction,
    OmittedRegion, PreviewPolicy, Projection, RedactionPolicy, RedactionState, Result, ScopedSlice,
    SliceRequest,
    disclosure::{expand, project},
    pointer::{get, is_within, parse},
    ranked_findings,
};

#[derive(Debug, Clone, PartialEq)]
pub struct LensProjection {
    pub projection: Projection,
    pub next_actions: Vec<NextAction>,
}

pub fn validate_lens_manifest(manifest: &LensManifest) -> Result<()> {
    if manifest.schema_version != LENS_MANIFEST_VERSION {
        return Err(lens_error(
            manifest,
            format!(
                "schema_version must be '{LENS_MANIFEST_VERSION}', got '{}'",
                manifest.schema_version
            ),
        ));
    }
    if !safe_identifier(&manifest.id, 128) {
        return Err(lens_error(
            manifest,
            "id must contain 1-128 ASCII letters, digits, '.', '_', or '-'",
        ));
    }
    if manifest.version == 0 {
        return Err(lens_error(manifest, "version must be greater than zero"));
    }
    if manifest.findings.len() > 100 {
        return Err(lens_error(
            manifest,
            "a lens may declare at most 100 finding rules",
        ));
    }
    if let Some(root) = &manifest.view.root {
        validate_pointer(root, false, manifest, "view.root")?;
    }
    for (name, selector) in &manifest.view.fields {
        if name.trim().is_empty() {
            return Err(lens_error(
                manifest,
                "view.fields keys must not be empty strings",
            ));
        }
        validate_pointer(selector, true, manifest, &format!("view.fields.{name}"))?;
    }
    for omission in &manifest.omit {
        validate_omission(manifest, omission)?;
    }
    for action in &manifest.next_actions {
        if let Some(path) = &action.path
            && !path.contains('{')
        {
            validate_pointer(path, true, manifest, "next_actions[].path")?;
        }
    }
    for rule in &manifest.findings {
        validate_finding_rule(manifest, rule)?;
    }
    Ok(())
}

/// Merge deterministic generic findings with declarative lens-provided
/// candidates. Lens rules only select paths that exist in the redacted payload;
/// they never execute code or replace generic ranking wholesale.
pub fn ranked_findings_with_lens(
    payload: &Value,
    options: &FindingOptions,
    manifest: Option<&LensManifest>,
) -> Result<Vec<Finding>> {
    let mut findings = ranked_findings(payload, options)?;
    let Some(manifest) = manifest else {
        return Ok(findings);
    };
    validate_lens_manifest(manifest)?;
    let scope = options.scope_path.as_deref().unwrap_or("");
    for rule in &manifest.findings {
        let mut matches = Vec::new();
        resolve_rule_paths(payload, &parse(&rule.path)?, "", &mut matches, 1_000);
        for (path, value) in matches {
            if !is_within(scope, &path)? || !rule_matches_value(rule, value) {
                continue;
            }
            findings.push(Finding {
                rank: 0,
                kind: rule.kind.clone(),
                path: path.clone(),
                confidence: round_confidence(rule.confidence),
                reason: sanitize_manifest_text(&rule.reason, 180),
                title: rule
                    .title
                    .as_deref()
                    .map(|title| sanitize_manifest_text(title, 120)),
                severity: rule.severity.clone(),
                source: Some("lens.finding_provider".to_string()),
                lens_id: Some(manifest.id.clone()),
                evidence_ref: None,
                line_range: object_line_range(value),
                byte_range: None,
                redaction_state: redaction_state(value),
                commands: lens_command_hints(options, &path, &rule.kind),
                extra: sanitize_manifest_extra(&rule.extra),
            });
        }
    }

    let mut best = BTreeMap::<(String, String), Finding>::new();
    for finding in findings {
        let key = (finding.path.clone(), finding.kind.clone());
        match best.get(&key) {
            Some(existing) if existing.confidence >= finding.confidence => {}
            _ => {
                best.insert(key, finding);
            }
        }
    }
    let mut findings = best.into_values().collect::<Vec<_>>();
    findings.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                severity_priority(&right.severity).cmp(&severity_priority(&left.severity))
            })
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    findings.truncate(options.limit);
    for (index, finding) in findings.iter_mut().enumerate() {
        finding.rank = index as u64 + 1;
    }
    Ok(findings)
}

pub fn lens_slice_request(
    manifest: &LensManifest,
    fallback: &SliceRequest,
) -> Result<SliceRequest> {
    validate_lens_manifest(manifest)?;
    Ok(SliceRequest {
        path: manifest.view.root.clone().or_else(|| fallback.path.clone()),
        limit: manifest.view.limit.or(fallback.limit),
        depth: manifest.view.depth.or(fallback.depth),
        fields: if manifest.view.fields.is_empty() {
            fallback.fields.clone()
        } else {
            Vec::new()
        },
        omit: fallback.omit.clone(),
        extra: Extra::new(),
    })
}

pub fn project_with_lens(
    payload: &impl ExpandablePayload,
    root_path: &str,
    slice: &SliceRequest,
    policy: &PreviewPolicy,
    manifest: Option<&LensManifest>,
) -> Result<LensProjection> {
    let Some(manifest) = manifest else {
        let scoped = ScopedSlice::new(ExpansionScope::new(root_path)?, slice.clone())?;
        return Ok(LensProjection {
            projection: expand(payload, &scoped, policy)?,
            next_actions: Vec::new(),
        });
    };

    validate_lens_manifest(manifest)?;
    let mut effective_policy = policy.with_limit_and_depth(slice.limit, slice.depth);
    if let Some(limit) = manifest.view.limit {
        effective_policy.array_items = limit;
    }
    if let Some(depth) = manifest.view.depth {
        effective_policy.depth = depth;
    }

    let projection = if manifest.view.fields.is_empty() {
        let scoped = ScopedSlice::new(ExpansionScope::new(root_path)?, slice.clone())?;
        expand(payload, &scoped, &effective_policy)?
    } else {
        let value = payload.expansion_value();
        let target = get(value, root_path)?.ok_or_else(|| CoreError::PathNotFound {
            path: root_path.to_string(),
            hint: crate::pointer::siblings_hint(value, root_path),
        })?;
        let selected = select_fields_with_pointers(target, &manifest.view.fields)?;
        project(&selected, &effective_policy, root_path)
    };

    let mut omitted = projection.omitted;
    omitted.extend(manifest_omissions(manifest));
    dedupe_omitted(&mut omitted);

    Ok(LensProjection {
        projection: Projection {
            preview: projection.preview,
            omitted,
        },
        next_actions: manifest.next_actions.clone(),
    })
}

fn validate_pointer(
    pointer: &str,
    allow_wildcards: bool,
    manifest: &LensManifest,
    field: &str,
) -> Result<()> {
    let segments =
        parse(pointer).map_err(|error| lens_error(manifest, format!("{field}: {error}")))?;
    if !allow_wildcards && segments.iter().any(|segment| segment == "*") {
        return Err(lens_error(
            manifest,
            format!("{field}: wildcards are not allowed here"),
        ));
    }
    Ok(())
}

fn validate_omission(manifest: &LensManifest, omission: &LensOmission) -> Result<()> {
    validate_pointer(&omission.path, true, manifest, "omit[].path")?;
    if let Some(root) = &manifest.view.root
        && !omission.path.contains('*')
        && !is_within(root, &omission.path)?
    {
        return Err(lens_error(
            manifest,
            format!(
                "omit path '{}' is outside view.root '{}'",
                omission.path, root
            ),
        ));
    }
    Ok(())
}

fn validate_finding_rule(manifest: &LensManifest, rule: &LensFindingRule) -> Result<()> {
    if !safe_identifier(&rule.kind, 64) {
        return Err(lens_error(
            manifest,
            "findings[].kind must contain 1-64 ASCII letters, digits, '.', '_', or '-'",
        ));
    }
    if rule.reason.trim().is_empty() {
        return Err(lens_error(manifest, "findings[].reason must not be empty"));
    }
    if rule.reason.chars().count() > 2_000
        || rule
            .title
            .as_deref()
            .is_some_and(|title| title.chars().count() > 500)
    {
        return Err(lens_error(
            manifest,
            "findings[] reason/title metadata exceeds the 2000/500 character limit",
        ));
    }
    if !rule.confidence.is_finite() || !(0.0..=1.0).contains(&rule.confidence) {
        return Err(lens_error(
            manifest,
            "findings[].confidence must be finite and between 0 and 1",
        ));
    }
    validate_pointer(&rule.path, true, manifest, "findings[].path")?;
    if let Some(root) = &manifest.view.root
        && !is_within(root, &rule.path)?
    {
        return Err(lens_error(
            manifest,
            format!(
                "finding path '{}' is outside view.root '{}'",
                rule.path, root
            ),
        ));
    }
    if rule.contains_any.len() > 32
        || rule
            .contains_any
            .iter()
            .any(|term| term.trim().is_empty() || term.chars().count() > 128)
    {
        return Err(lens_error(
            manifest,
            "findings[].contains_any allows at most 32 non-empty terms of at most 128 characters",
        ));
    }
    if rule.severity.as_deref().is_some_and(|severity| {
        !matches!(
            severity,
            "critical" | "fatal" | "error" | "warning" | "info"
        )
    }) {
        return Err(lens_error(
            manifest,
            "findings[].severity must be critical, fatal, error, warning, or info",
        ));
    }
    Ok(())
}

fn safe_identifier(value: &str, max_chars: usize) -> bool {
    let count = value.chars().count();
    count > 0
        && count <= max_chars
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn sanitize_manifest_text(value: &str, max_chars: usize) -> String {
    let (redacted, _) =
        RedactionPolicy::default().apply_persistence(&Value::String(value.to_string()));
    truncate_chars(
        redacted.as_str().unwrap_or("[REDACTED:lens_metadata]"),
        max_chars,
    )
}

fn sanitize_manifest_extra(extra: &Extra) -> Extra {
    let (redacted, _) = RedactionPolicy::default().apply_persistence(&Value::Object(extra.clone()));
    redacted.as_object().cloned().unwrap_or_default()
}

fn resolve_rule_paths<'a>(
    value: &'a Value,
    segments: &[String],
    path: &str,
    out: &mut Vec<(String, &'a Value)>,
    limit: usize,
) {
    if out.len() >= limit {
        return;
    }
    let Some((head, tail)) = segments.split_first() else {
        out.push((path.to_string(), value));
        return;
    };
    if head == "*" {
        match value {
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    resolve_rule_paths(
                        item,
                        tail,
                        &crate::pointer::push(path, &index.to_string()),
                        out,
                        limit,
                    );
                    if out.len() >= limit {
                        break;
                    }
                }
            }
            Value::Object(map) => {
                let mut keys = map.keys().collect::<Vec<_>>();
                keys.sort();
                for key in keys {
                    resolve_rule_paths(
                        &map[key],
                        tail,
                        &crate::pointer::push(path, key),
                        out,
                        limit,
                    );
                    if out.len() >= limit {
                        break;
                    }
                }
            }
            _ => {}
        }
        return;
    }
    match value {
        Value::Object(map) => {
            if let Some(child) = map.get(head) {
                resolve_rule_paths(child, tail, &crate::pointer::push(path, head), out, limit);
            }
        }
        Value::Array(items) => {
            if let Some(child) = head
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get(index))
            {
                resolve_rule_paths(child, tail, &crate::pointer::push(path, head), out, limit);
            }
        }
        _ => {}
    }
}

fn rule_matches_value(rule: &LensFindingRule, value: &Value) -> bool {
    if rule.contains_any.is_empty() {
        return true;
    }
    let terms = rule
        .contains_any
        .iter()
        .map(|term| term.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut visited = 0usize;
    value_contains_any(value, &terms, 0, &mut visited)
}

fn value_contains_any(value: &Value, terms: &[String], depth: usize, visited: &mut usize) -> bool {
    if *visited >= 10_000 || depth > 64 {
        return false;
    }
    *visited += 1;
    match value {
        Value::String(text) => {
            let text = text.to_ascii_lowercase();
            terms.iter().any(|term| text.contains(term))
        }
        Value::Array(items) => items
            .iter()
            .any(|item| value_contains_any(item, terms, depth + 1, visited)),
        Value::Object(map) => map.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            terms.iter().any(|term| key.contains(term))
                || value_contains_any(value, terms, depth + 1, visited)
        }),
        _ => false,
    }
}

fn lens_command_hints(options: &FindingOptions, path: &str, kind: &str) -> FindingCommandHints {
    let Some(cursor) = options.cursor.as_deref() else {
        return FindingCommandHints::default();
    };
    let cursor = shell_arg(cursor);
    let path = shell_arg(path);
    let goal = shell_arg(&format!("investigate {kind}"));
    let kind = shell_arg(kind);
    FindingCommandHints {
        inspect: options
            .hints
            .inspect
            .then(|| format!("prog inspect {cursor} --goal {goal} --path {path}")),
        expand: options
            .hints
            .expand
            .then(|| format!("prog expand {cursor} --path {path}")),
        evidence: options
            .hints
            .evidence
            .then(|| format!("prog evidence {cursor} --path {path}")),
        search: options
            .hints
            .search
            .then(|| format!("prog find {cursor} --kind {kind} --path {path}")),
        extra: Extra::new(),
    }
}

fn shell_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn object_line_range(value: &Value) -> Option<crate::LineRange> {
    let map = value.as_object()?;
    Some(crate::LineRange {
        start: map.get("line_start")?.as_u64()?,
        end: map.get("line_end")?.as_u64()?,
        extra: Extra::new(),
    })
}

fn redaction_state(value: &Value) -> Option<RedactionState> {
    let mut visited = 0usize;
    let count = count_redactions(value, 0, &mut visited);
    (count > 0).then(|| RedactionState {
        redacted: true,
        redacted_paths: count,
        lossy: false,
        redaction_version: None,
        extra: Extra::new(),
    })
}

fn count_redactions(value: &Value, depth: usize, visited: &mut usize) -> u64 {
    if *visited >= 10_000 || depth > 64 {
        return 0;
    }
    *visited += 1;
    match value {
        Value::String(text)
            if text.contains("[REDACTED:") || text.contains("\u{00ab}redacted\u{00bb}") =>
        {
            1
        }
        Value::Array(items) => items
            .iter()
            .map(|value| count_redactions(value, depth + 1, visited))
            .sum(),
        Value::Object(map) => map
            .values()
            .map(|value| count_redactions(value, depth + 1, visited))
            .sum(),
        _ => 0,
    }
}

fn severity_priority(severity: &Option<String>) -> u8 {
    match severity.as_deref() {
        Some("critical" | "fatal") => 3,
        Some("error") => 2,
        Some("warning") => 1,
        _ => 0,
    }
}

fn round_confidence(value: f64) -> f64 {
    (value.clamp(0.0, 1.0) * 100.0).round() / 100.0
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>();
    output.push_str("...");
    output
}

fn select_fields_with_pointers(value: &Value, fields: &BTreeMap<String, String>) -> Result<Value> {
    match value {
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|item| select_object_fields_with_pointers(item, fields))
                .collect::<Result<Vec<_>>>()?,
        )),
        _ => select_object_fields_with_pointers(value, fields),
    }
}

fn select_object_fields_with_pointers(
    value: &Value,
    fields: &BTreeMap<String, String>,
) -> Result<Value> {
    let mut selected = Map::new();
    for (name, selector) in fields {
        if let Some(field_value) = select_pointer_with_wildcards(value, selector)? {
            selected.insert(name.clone(), field_value);
        }
    }
    Ok(Value::Object(selected))
}

fn select_pointer_with_wildcards(value: &Value, pointer: &str) -> Result<Option<Value>> {
    let segments = parse(pointer)?;
    select_segments(value, &segments)
}

fn select_segments(value: &Value, segments: &[String]) -> Result<Option<Value>> {
    let Some((head, tail)) = segments.split_first() else {
        return Ok(Some(value.clone()));
    };

    if head == "*" {
        let mut selected = Vec::new();
        match value {
            Value::Array(items) => {
                for item in items {
                    if let Some(value) = select_segments(item, tail)? {
                        selected.push(value);
                    }
                }
            }
            Value::Object(map) => {
                for item in map.values() {
                    if let Some(value) = select_segments(item, tail)? {
                        selected.push(value);
                    }
                }
            }
            _ => return Ok(None),
        }
        return Ok(Some(Value::Array(selected)));
    }

    match value {
        Value::Object(map) => match map.get(head) {
            Some(next) => select_segments(next, tail),
            None => Ok(None),
        },
        Value::Array(items) => match head
            .parse::<usize>()
            .ok()
            .and_then(|index| items.get(index))
        {
            Some(next) => select_segments(next, tail),
            None => Ok(None),
        },
        _ => Ok(None),
    }
}

fn manifest_omissions(manifest: &LensManifest) -> Vec<OmittedRegion> {
    manifest
        .omit
        .iter()
        .map(|omission| {
            let mut extra = omission.extra.clone();
            extra.insert("expandable".to_string(), json!(omission.expandable));
            OmittedRegion {
                path: omission.path.clone(),
                reason: omission.reason,
                detail: omission.detail.clone(),
                extra,
            }
        })
        .collect()
}

fn dedupe_omitted(omitted: &mut Vec<OmittedRegion>) {
    let mut seen = std::collections::BTreeSet::new();
    omitted.retain(|entry| seen.insert((entry.path.clone(), entry.reason)));
}

fn lens_error(manifest: &LensManifest, reason: impl Into<String>) -> CoreError {
    CoreError::BadArgs {
        operation: format!("lens {}", manifest.id),
        reason: reason.into(),
    }
}

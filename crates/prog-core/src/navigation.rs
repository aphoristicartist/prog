use std::{cmp::Ordering, collections::BTreeMap};

use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};

use crate::{
    ByteRange, CoreError, EVIDENCE_BLOCK_SCHEMA, EvidenceBlock, EvidenceCitation, Extra,
    FindingCommandHints, FindingOptions, LensManifest, LineRange, OmissionReason, OmittedRegion,
    PreviewPolicy, RedactionState, Result, SEARCH_SCHEMA, SearchHit, SearchResponse, pointer,
    project, ranked_findings, ranked_findings_with_lens,
};

const DEFAULT_SEARCH_LIMIT: usize = 20;
const DEFAULT_MAX_NODES: usize = 10_000;
const DEFAULT_MAX_DEPTH: usize = 64;
const MAX_PREVIEW_CHARS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    pub query: Option<String>,
    pub kind: Option<String>,
    pub scope_path: Option<String>,
    pub limit: usize,
    pub case_sensitive: bool,
    pub regex: bool,
    pub max_nodes: usize,
    pub max_depth: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: None,
            kind: None,
            scope_path: None,
            limit: DEFAULT_SEARCH_LIMIT,
            case_sensitive: false,
            regex: false,
            max_nodes: DEFAULT_MAX_NODES,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

pub fn search_payload(
    payload: &Value,
    cursor: impl Into<String>,
    options: &SearchOptions,
) -> Result<SearchResponse> {
    search_payload_with_lens(payload, cursor, options, None)
}

pub fn search_payload_with_lens(
    payload: &Value,
    cursor: impl Into<String>,
    options: &SearchOptions,
    lens: Option<&LensManifest>,
) -> Result<SearchResponse> {
    if options.query.as_deref().is_none_or(str::is_empty) && options.kind.is_none() {
        return Err(CoreError::BadArgs {
            operation: "search".to_string(),
            reason: "provide a non-empty query or --kind".to_string(),
        });
    }
    if options.regex && options.query.as_deref().is_none_or(str::is_empty) {
        return Err(CoreError::BadArgs {
            operation: "search --regex".to_string(),
            reason: "--regex requires a non-empty query".to_string(),
        });
    }

    let scope_path = options.scope_path.as_deref().unwrap_or("");
    let target = pointer::get(payload, scope_path)?.ok_or_else(|| CoreError::PathNotFound {
        path: scope_path.to_string(),
        hint: pointer::siblings_hint(payload, scope_path),
    })?;
    let matcher = QueryMatcher::new(
        options.query.as_deref(),
        options.case_sensitive,
        options.regex,
    )?;
    let semantic = semantic_findings(payload, options, lens)?;
    let mut traversal = SearchTraversal {
        matcher,
        requested_kind: options.kind.as_deref().map(normalize_kind),
        semantic,
        max_nodes: options.max_nodes.max(1),
        max_depth: options.max_depth,
        visited: 0,
        truncated: false,
        hits: Vec::new(),
        cursor: cursor.into(),
    };
    traversal.visit(target, scope_path, None, 0);

    let mut best_by_path: BTreeMap<String, SearchHit> = BTreeMap::new();
    for hit in traversal.hits {
        match best_by_path.get(&hit.path) {
            Some(existing) if existing.score >= hit.score => {}
            _ => {
                best_by_path.insert(hit.path.clone(), hit);
            }
        }
    }
    let mut hits = best_by_path.into_values().collect::<Vec<_>>();
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.match_kind.cmp(&right.match_kind))
    });
    let result_truncated = hits.len() > options.limit;
    hits.truncate(options.limit);
    for (index, hit) in hits.iter_mut().enumerate() {
        hit.rank = index as u64 + 1;
    }

    let truncated = traversal.truncated || result_truncated;
    let mut warnings = Vec::new();
    let omitted = if truncated {
        warnings.push(format!(
            "search was bounded after {} visited node(s) and {} retained hit(s); narrow --path or increase --limit",
            traversal.visited,
            hits.len()
        ));
        vec![OmittedRegion {
            path: scope_path.to_string(),
            reason: OmissionReason::NodeBudget,
            detail: Some("cached search traversal or result limit was reached".to_string()),
            extra: Extra::new(),
        }]
    } else {
        Vec::new()
    };

    Ok(SearchResponse {
        schema: SEARCH_SCHEMA.to_string(),
        cursor: traversal.cursor,
        query: options.query.clone(),
        kind: options.kind.clone(),
        scope_path: options.scope_path.clone(),
        hits,
        omitted,
        cache: None,
        warnings,
        extra: Extra::new(),
    })
}

pub fn evidence_block(
    payload: &Value,
    cursor: impl Into<String>,
    path: &str,
) -> Result<EvidenceBlock> {
    let cursor = cursor.into();
    let value = pointer::get(payload, path)?.ok_or_else(|| CoreError::PathNotFound {
        path: path.to_string(),
        hint: pointer::siblings_hint(payload, path),
    })?;
    let policy = PreviewPolicy {
        array_items: 6,
        object_fields: 12,
        string_chars: 800,
        depth: 4,
        node_budget: 120,
        max_envelope_bytes: PreviewPolicy::default().max_envelope_bytes,
    };
    let projection = project(value, &policy, path);
    let finding = ranked_findings(
        payload,
        &FindingOptions {
            scope_path: Some(path.to_string()),
            limit: 1,
            ..FindingOptions::default()
        },
    )?
    .into_iter()
    .next();
    let kind = finding
        .as_ref()
        .map(|finding| finding.kind.clone())
        .unwrap_or_else(|| value_kind(value).to_string());
    let redaction_state = Some(redaction_state(value).unwrap_or_else(|| RedactionState {
        redacted: false,
        redacted_paths: 0,
        lossy: false,
        redaction_version: None,
        extra: Extra::new(),
    }));
    let line_range = object_line_range(value);
    let byte_range = object_byte_range(value);
    let commands = navigation_commands(&cursor, path, Some(&kind));
    let excerpt = projection.preview;

    Ok(EvidenceBlock {
        schema: EVIDENCE_BLOCK_SCHEMA.to_string(),
        cursor,
        path: path.to_string(),
        kind,
        summary: evidence_summary(
            value,
            finding.as_ref().map(|finding| finding.reason.as_str()),
        ),
        excerpt: excerpt.clone(),
        citations: vec![EvidenceCitation {
            path: path.to_string(),
            label: Some("cached redacted evidence".to_string()),
            excerpt,
            line_range: line_range.clone(),
            byte_range: byte_range.clone(),
            redaction_state: redaction_state.clone(),
            extra: Extra::new(),
        }],
        evidence_ref: None,
        line_range,
        byte_range,
        source_command: None,
        provenance: None,
        redaction_state,
        commands,
        cache: None,
        warnings: if projection.omitted.is_empty() {
            Vec::new()
        } else {
            vec![
                "evidence excerpt is bounded; use commands.expand for a larger cached slice"
                    .to_string(),
            ]
        },
        extra: {
            let mut extra = Extra::new();
            if !projection.omitted.is_empty() {
                extra.insert("omitted".to_string(), json!(projection.omitted));
            }
            extra
        },
    })
}

#[derive(Debug)]
struct SearchTraversal {
    matcher: QueryMatcher,
    requested_kind: Option<String>,
    semantic: BTreeMap<String, SemanticMatch>,
    max_nodes: usize,
    max_depth: usize,
    visited: usize,
    truncated: bool,
    hits: Vec<SearchHit>,
    cursor: String,
}

impl SearchTraversal {
    fn visit(&mut self, value: &Value, path: &str, field: Option<&str>, depth: usize) {
        if self.visited >= self.max_nodes || depth > self.max_depth {
            self.truncated = true;
            return;
        }
        self.visited += 1;

        let semantic = self.semantic.get(path);
        let kind_matches = requested_kind_matches(self.requested_kind.as_deref(), value, semantic);
        let query_match = self.matcher.matches(path, field, value);
        if kind_matches && query_match.matched {
            let score = (query_match.score + semantic.map_or(0.0, |item| item.confidence * 0.5))
                .clamp(0.0, 1.0);
            self.hits.push(SearchHit {
                rank: 0,
                path: path.to_string(),
                score: round_score(score),
                match_kind: query_match.kind.to_string(),
                preview: search_preview(value),
                field: field.map(str::to_string),
                finding_kind: semantic.map(|item| item.kind.clone()),
                line_range: object_line_range(value),
                byte_range: object_byte_range(value),
                redaction_state: redaction_state(value),
                commands: navigation_commands(
                    &self.cursor,
                    path,
                    semantic.map(|semantic| semantic.kind.as_str()),
                ),
                extra: Extra::new(),
            });
        }

        match value {
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    self.visit(
                        child,
                        &pointer::push(path, &index.to_string()),
                        Some(&index.to_string()),
                        depth + 1,
                    );
                    if self.visited >= self.max_nodes {
                        self.truncated = true;
                        break;
                    }
                }
            }
            Value::Object(map) => {
                let mut keys = map.keys().collect::<Vec<_>>();
                keys.sort();
                for key in keys {
                    self.visit(&map[key], &pointer::push(path, key), Some(key), depth + 1);
                    if self.visited >= self.max_nodes {
                        self.truncated = true;
                        break;
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
struct SemanticMatch {
    kind: String,
    severity: Option<String>,
    confidence: f64,
}

fn semantic_findings(
    payload: &Value,
    options: &SearchOptions,
    lens: Option<&LensManifest>,
) -> Result<BTreeMap<String, SemanticMatch>> {
    let finding_options = FindingOptions {
        scope_path: options.scope_path.clone(),
        limit: options.max_nodes.min(1_000),
        ..FindingOptions::default()
    };
    let findings = ranked_findings_with_lens(payload, &finding_options, lens)?;
    let mut semantic = BTreeMap::<String, SemanticMatch>::new();
    for finding in findings {
        let candidate = SemanticMatch {
            kind: finding.kind,
            severity: finding.severity,
            confidence: finding.confidence,
        };
        match semantic.get(&finding.path) {
            Some(existing) if existing.confidence >= candidate.confidence => {}
            _ => {
                semantic.insert(finding.path, candidate);
            }
        }
    }
    Ok(semantic)
}

#[derive(Debug)]
struct QueryMatcher {
    raw: Option<String>,
    normalized: Option<String>,
    regex: Option<Regex>,
    case_sensitive: bool,
}

impl QueryMatcher {
    fn new(query: Option<&str>, case_sensitive: bool, regex: bool) -> Result<Self> {
        let raw = query.map(str::to_string);
        let compiled = if regex {
            Some(
                RegexBuilder::new(query.unwrap_or_default())
                    .case_insensitive(!case_sensitive)
                    .size_limit(1 << 20)
                    .dfa_size_limit(1 << 20)
                    .build()
                    .map_err(|error| CoreError::BadArgs {
                        operation: "search --regex".to_string(),
                        reason: error.to_string(),
                    })?,
            )
        } else {
            None
        };
        Ok(Self {
            normalized: raw.as_ref().map(|query| {
                if case_sensitive {
                    query.clone()
                } else {
                    query.to_lowercase()
                }
            }),
            raw,
            regex: compiled,
            case_sensitive,
        })
    }

    fn matches(&self, path: &str, field: Option<&str>, value: &Value) -> QueryMatch {
        if self.raw.is_none() {
            return QueryMatch {
                matched: true,
                score: 0.5,
                kind: "kind",
            };
        }
        if field.is_some_and(|field| self.is_match(field)) {
            return QueryMatch {
                matched: true,
                score: 0.95,
                kind: "key",
            };
        }
        if self.is_match(path) {
            return QueryMatch {
                matched: true,
                score: 0.8,
                kind: "path",
            };
        }
        if let Value::String(text) = value
            && self.is_match(text)
        {
            return QueryMatch {
                matched: true,
                score: if self.equals(text) { 1.0 } else { 0.9 },
                kind: "value",
            };
        }
        QueryMatch {
            matched: false,
            score: 0.0,
            kind: "none",
        }
    }

    fn is_match(&self, candidate: &str) -> bool {
        if let Some(regex) = &self.regex {
            return regex.is_match(candidate);
        }
        let Some(query) = &self.normalized else {
            return true;
        };
        if self.case_sensitive {
            candidate.contains(query)
        } else {
            candidate.to_lowercase().contains(query)
        }
    }

    fn equals(&self, candidate: &str) -> bool {
        let Some(query) = &self.normalized else {
            return false;
        };
        if self.case_sensitive {
            candidate == query
        } else {
            candidate.to_lowercase() == *query
        }
    }
}

#[derive(Debug)]
struct QueryMatch {
    matched: bool,
    score: f64,
    kind: &'static str,
}

fn requested_kind_matches(
    requested: Option<&str>,
    value: &Value,
    semantic: Option<&SemanticMatch>,
) -> bool {
    let Some(requested) = requested else {
        return true;
    };
    if requested == value_kind(value) {
        return true;
    }
    let Some(semantic) = semantic else {
        return false;
    };
    requested == normalize_kind(&semantic.kind)
        || (requested == "error"
            && (semantic.severity.as_deref() == Some("error")
                || semantic.kind.contains("error")
                || semantic.kind.contains("failure")
                || semantic.kind.contains("exception")
                || semantic.kind.contains("panic")
                || semantic.kind.contains("timeout")))
        || (requested == "warning"
            && (semantic.severity.as_deref() == Some("warning")
                || semantic.kind.contains("warning")))
}

fn normalize_kind(kind: &str) -> String {
    kind.trim().to_ascii_lowercase().replace('-', "_")
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn search_preview(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(truncate_chars(text, MAX_PREVIEW_CHARS)),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::Array(items) => json!({"kind": "array", "item_count": items.len()}),
        Value::Object(map) => json!({
            "kind": "object",
            "field_count": map.len(),
            "fields": map.keys().take(12).collect::<Vec<_>>()
        }),
    }
}

fn evidence_summary(value: &Value, finding_reason: Option<&str>) -> String {
    if let Some(reason) = finding_reason {
        return truncate_chars(reason, 180);
    }
    match value {
        Value::Array(items) => format!("cached redacted array with {} item(s)", items.len()),
        Value::Object(map) => format!("cached redacted object with {} field(s)", map.len()),
        Value::String(text) => format!(
            "cached redacted string with {} character(s)",
            text.chars().count()
        ),
        Value::Null => "cached null value".to_string(),
        Value::Bool(_) => "cached boolean value".to_string(),
        Value::Number(_) => "cached numeric value".to_string(),
    }
}

fn navigation_commands(cursor: &str, path: &str, kind: Option<&str>) -> FindingCommandHints {
    let cursor = shell_arg(cursor);
    let path = shell_arg(path);
    let goal = shell_arg(&format!("investigate {}", kind.unwrap_or("evidence")));
    FindingCommandHints {
        inspect: Some(format!("prog inspect {cursor} --goal {goal} --path {path}")),
        expand: Some(format!("prog expand {cursor} --path {path}")),
        evidence: Some(format!("prog evidence {cursor} --path {path}")),
        search: kind.map(|kind| {
            let kind = shell_arg(kind);
            format!("prog find {cursor} --kind {kind} --path {path}")
        }),
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

fn redaction_state(value: &Value) -> Option<RedactionState> {
    let count = count_redactions(value);
    (count > 0).then(|| RedactionState {
        redacted: true,
        redacted_paths: count,
        lossy: false,
        redaction_version: None,
        extra: Extra::new(),
    })
}

fn count_redactions(value: &Value) -> u64 {
    match value {
        Value::String(text)
            if text.contains("[REDACTED:") || text.contains("\u{00ab}redacted\u{00bb}") =>
        {
            1
        }
        Value::Array(items) => items.iter().map(count_redactions).sum(),
        Value::Object(map) => map.values().map(count_redactions).sum(),
        _ => 0,
    }
}

fn object_line_range(value: &Value) -> Option<LineRange> {
    let map = value.as_object()?;
    Some(LineRange {
        start: map.get("line_start")?.as_u64()?,
        end: map.get("line_end")?.as_u64()?,
        extra: Extra::new(),
    })
}

fn object_byte_range(value: &Value) -> Option<ByteRange> {
    let map = value.as_object()?;
    Some(ByteRange {
        start: map.get("byte_start")?.as_u64()?,
        end: map.get("byte_end")?.as_u64()?,
        extra: Extra::new(),
    })
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

fn round_score(score: f64) -> f64 {
    (score * 100.0).round() / 100.0
}

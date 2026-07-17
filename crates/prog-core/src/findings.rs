use std::{
    cmp::Ordering,
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    Extra, Finding, FindingCommandHints, INSPECT_SCHEMA, InspectResponse, LineRange,
    RedactionState, Result, SourceSpan, SourceSpanExactness, pointer,
};

const DEFAULT_LIMIT: usize = 10;
const MAX_REASON_CHARS: usize = 180;
const MAX_FINDING_NODES: usize = 10_000;
const MAX_FINDING_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingOptions {
    pub goal: Option<String>,
    pub cursor: Option<String>,
    pub scope_path: Option<String>,
    pub limit: usize,
    pub hints: CommandHintConfig,
    /// Proven workspace root used solely to convert an absolute producer path
    /// into a workspace-relative source span. Without it, absolute paths fail
    /// closed rather than leaking host-specific locations.
    pub workspace_root: Option<PathBuf>,
    /// Immutable parser/provider metadata of the observation being projected.
    /// These are identity components, not model-visible finding prose.
    pub identity: FindingIdentityContext,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindingIdentityContext {
    pub provider: Option<String>,
    pub parser: Option<String>,
    pub lens: Option<String>,
}

impl Default for FindingOptions {
    fn default() -> Self {
        Self {
            goal: None,
            cursor: None,
            scope_path: None,
            limit: DEFAULT_LIMIT,
            hints: CommandHintConfig::NAV_EXPAND_ONLY,
            workspace_root: None,
            identity: FindingIdentityContext::default(),
        }
    }
}

/// Which navigation command hints `command_hints` should emit on each [`Finding`].
///
/// The minimal default emits only `prog expand` for compatibility-oriented
/// library callers. CLI envelopes opt into [`CommandHintConfig::NAV_ALL`] so
/// every advertised navigation command is directly runnable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandHintConfig {
    pub expand: bool,
    pub inspect: bool,
    pub evidence: bool,
    pub search: bool,
}

impl CommandHintConfig {
    /// Minimal compatibility mode: emit only `prog expand`.
    pub const NAV_EXPAND_ONLY: Self = Self {
        expand: true,
        inspect: false,
        evidence: false,
        search: false,
    };

    /// Emit every implemented navigation hint.
    pub const NAV_ALL: Self = Self {
        expand: true,
        inspect: true,
        evidence: true,
        search: true,
    };
}

impl Default for CommandHintConfig {
    fn default() -> Self {
        Self::NAV_EXPAND_ONLY
    }
}

/// Input boundary for [`build_inspect_response`].
///
/// This is NOT a contract type and is NOT serialized as part of
/// [`InspectResponse`]; it lives at the request edge so the required `cursor`
/// (the response field is a non-optional `String`) is enforced before assembly
/// rather than panic-recovered later. The engine never fabricates cursors
/// (fail-closed `pc1_` cursors, I9); cursor existence/freshness is validated by
/// the CLI layer before calling [`build_inspect_response`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectRequest {
    pub goal: Option<String>,
    pub cursor: String,
    pub scope_path: Option<String>,
    pub limit: usize,
    pub hints: CommandHintConfig,
}

impl Default for InspectRequest {
    fn default() -> Self {
        Self {
            goal: None,
            cursor: String::new(),
            scope_path: None,
            limit: DEFAULT_LIMIT,
            hints: CommandHintConfig::NAV_EXPAND_ONLY,
        }
    }
}

impl InspectRequest {
    /// Begin a builder, pinning the required `cursor` at construction time.
    pub fn builder(cursor: impl Into<String>) -> InspectRequestBuilder {
        InspectRequestBuilder {
            request: Self {
                cursor: cursor.into(),
                ..Self::default()
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct InspectRequestBuilder {
    request: InspectRequest,
}

impl InspectRequestBuilder {
    pub fn goal(mut self, goal: impl Into<String>) -> Self {
        self.request.goal = Some(goal.into());
        self
    }

    pub fn scope_path(mut self, scope_path: impl Into<String>) -> Self {
        self.request.scope_path = Some(scope_path.into());
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.request.limit = limit;
        self
    }

    pub fn hints(mut self, hints: CommandHintConfig) -> Self {
        self.request.hints = hints;
        self
    }

    pub fn build(self) -> InspectRequest {
        self.request
    }
}

pub fn normalized_goal(goal: Option<&str>) -> Option<String> {
    let goal = goal?.trim();
    if goal.is_empty() {
        return None;
    }
    Some(GoalIntent::from_text(goal).as_str().to_string())
}

pub fn ranked_findings(payload: &Value, options: &FindingOptions) -> Result<Vec<Finding>> {
    let scope_path = options.scope_path.as_deref().unwrap_or("");
    let Some(scoped) = pointer::get(payload, scope_path)? else {
        return Ok(Vec::new());
    };
    if options.limit == 0 {
        return Ok(Vec::new());
    }

    let intent = GoalIntent::from_text(options.goal.as_deref().unwrap_or(""));
    let mut candidates = Vec::new();
    collect_run_signals(
        scoped,
        scope_path,
        options.workspace_root.as_deref(),
        &mut candidates,
    );
    let mut visited = 0usize;
    collect_generic_signals(
        scoped,
        scope_path,
        options.workspace_root.as_deref(),
        &mut candidates,
        0,
        &mut visited,
    );
    for candidate in &mut candidates {
        candidate.fingerprint = fingerprint_finding(
            &candidate.kind,
            &candidate.source,
            &candidate.identity_value,
            candidate.primary_span.as_ref(),
            None,
            &options.identity,
        );
    }

    let mut best_by_path_kind: BTreeMap<(String, String), Candidate> = BTreeMap::new();
    for mut candidate in candidates {
        candidate.score = score_candidate(&candidate, intent);
        let key = (candidate.path.clone(), candidate.kind.clone());
        match best_by_path_kind.get(&key) {
            Some(existing) if compare_candidates(&candidate, existing) != Ordering::Less => {}
            _ => {
                best_by_path_kind.insert(key, candidate);
            }
        }
    }

    let mut candidates = best_by_path_kind.into_values().collect::<Vec<_>>();
    candidates.sort_by(compare_candidates);
    candidates.truncate(options.limit);

    Ok(candidates
        .into_iter()
        .enumerate()
        .map(|(index, candidate)| {
            let ordinal = index as u64 + 1;
            candidate.into_finding(ordinal, ordinal, options)
        })
        .collect())
}

/// Assemble a full [`InspectResponse`] over an already-redacted, stored payload.
///
/// This is the single boundary the `prog inspect` CLI command calls.
/// The engine is pure and store-less: it projects a ranked view over `payload`
/// (consumed AFTER redact -> infer -> store -> project), stamps `schema`
/// from [`INSPECT_SCHEMA`], and derives `normalized_goal` via
/// [`normalized_goal`]. `omitted` / `cache` / `warnings` are left default; the
/// bounded 16 KiB envelope is preserved via `request.limit` (default
/// [`DEFAULT_LIMIT`]) and [`MAX_REASON_CHARS`] truncation. Each [`Finding`]
/// carries `redaction_state` derived from the projected payload, preserving
/// redaction visibility (I2/I4).
pub fn build_inspect_response(
    payload: &Value,
    request: &InspectRequest,
) -> Result<InspectResponse> {
    let options = FindingOptions {
        goal: request.goal.clone(),
        cursor: Some(request.cursor.clone()),
        scope_path: request.scope_path.clone(),
        limit: request.limit,
        hints: request.hints,
        ..FindingOptions::default()
    };
    let findings = ranked_findings(payload, &options)?;
    Ok(InspectResponse {
        schema: INSPECT_SCHEMA.to_string(),
        cursor: request.cursor.clone(),
        goal: request.goal.clone().unwrap_or_default(),
        normalized_goal: normalized_goal(request.goal.as_deref()),
        scope_path: request.scope_path.clone(),
        findings,
        omitted: Vec::new(),
        cache: None,
        warnings: Vec::new(),
        extra: Extra::new(),
    })
}

#[derive(Debug, Clone)]
struct Candidate {
    kind: String,
    path: String,
    confidence: f64,
    score: f64,
    reason: String,
    title: Option<String>,
    severity: Option<String>,
    source: String,
    line_range: Option<LineRange>,
    primary_span: Option<SourceSpan>,
    related_spans: Vec<SourceSpan>,
    redaction_state: Option<RedactionState>,
    fingerprint: Option<String>,
    identity_value: Value,
    extra: Extra,
}

impl Candidate {
    fn from_signal(
        path: String,
        value: &Value,
        signal: Signal,
        workspace_root: Option<&Path>,
    ) -> Self {
        let (primary_span, related_spans) =
            extract_source_spans_with_workspace_root(value, workspace_root);
        let fingerprint = fingerprint_finding(
            signal.kind,
            signal.source,
            value,
            primary_span.as_ref(),
            None,
            &FindingIdentityContext::default(),
        );
        Self {
            kind: signal.kind.to_string(),
            path,
            confidence: signal.confidence,
            score: 0.0,
            reason: signal.reason.to_string(),
            title: Some(signal.title.to_string()),
            severity: signal.severity.map(str::to_string),
            source: signal.source.to_string(),
            line_range: None,
            primary_span,
            related_spans,
            redaction_state: redaction_state(value),
            fingerprint,
            identity_value: value.clone(),
            extra: Extra::new(),
        }
    }

    fn inherit_source_spans(&mut self, context: &Value, workspace_root: Option<&Path>) {
        if self.primary_span.is_some() || !self.related_spans.is_empty() {
            return;
        }
        let (primary_span, related_spans) =
            extract_source_spans_with_workspace_root(context, workspace_root);
        self.primary_span = primary_span;
        self.related_spans = related_spans;
    }

    fn use_identity_context(&mut self, context: &Value) {
        self.identity_value = context.clone();
        self.fingerprint = fingerprint_finding(
            &self.kind,
            &self.source,
            context,
            self.primary_span.as_ref(),
            None,
            &FindingIdentityContext::default(),
        );
    }

    fn into_finding(self, rank: u64, occurrence: u64, options: &FindingOptions) -> Finding {
        let commands = command_hints(
            options.cursor.as_deref(),
            &self.path,
            &self.kind,
            options.hints,
        );
        Finding {
            // This is observation-local, never an alias for the cross-run
            // fingerprint: equal findings in one observation remain distinct.
            occurrence_id: Some(format!("fo_{occurrence:08}")),
            fingerprint: self.fingerprint,
            rank,
            kind: self.kind,
            path: self.path.clone(),
            confidence: round_confidence(self.confidence),
            reason: truncate_reason(&self.reason),
            title: self.title,
            severity: self.severity,
            source: Some(self.source),
            lens_id: None,
            evidence_ref: None,
            line_range: self.line_range,
            byte_range: None,
            primary_span: self.primary_span,
            related_spans: self.related_spans,
            redaction_state: self.redaction_state,
            commands,
            extra: self.extra,
        }
    }
}

const MAX_SOURCE_SPANS: usize = 16;

/// Extract source locations without a workspace root.
///
/// Relative paths remain usable; absolute producer paths fail closed. Call
/// [`extract_source_spans_with_workspace_root`] when a trusted root is known.
pub fn extract_source_spans(value: &Value) -> (Option<SourceSpan>, Vec<SourceSpan>) {
    extract_source_spans_with_workspace_root(value, None)
}

/// Extract source locations only from explicit structured location fields.
///
/// It never reads a referenced file. Apart from explicit structured location
/// objects, it accepts only strict, domain-specific records: Go/Jest location
/// lines and unified-diff headers/hunks. Arbitrary tracebacks and log prose do
/// not become source locations. Input is expected to have crossed the
/// redaction-before-persistence boundary already.
pub fn extract_source_spans_with_workspace_root(
    value: &Value,
    workspace_root: Option<&Path>,
) -> (Option<SourceSpan>, Vec<SourceSpan>) {
    let Some(map) = value.as_object() else {
        return (None, Vec::new());
    };

    let mut spans = Vec::new();
    // rustc/Cargo JSON diagnostics: the primary span is explicit and related
    // spans retain the producer's original order.
    if let Some(items) = map.get("spans").and_then(Value::as_array) {
        for item in items.iter().take(MAX_SOURCE_SPANS) {
            let Some(item) = item.as_object() else {
                continue;
            };
            let role = if item
                .get("is_primary")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "primary"
            } else if item
                .get("is_generated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "generated"
            } else {
                "related"
            };
            push_span(
                &mut spans,
                build_span(item, role, "structured.rustc", workspace_root),
            );
        }
    }

    // SARIF results use locations/relatedLocations with a physicalLocation
    // wrapper; we retain URI locations without resolving them locally.
    if let Some(items) = map.get("locations").and_then(Value::as_array) {
        for (index, item) in items.iter().take(MAX_SOURCE_SPANS).enumerate() {
            let Some(item) = item.as_object() else {
                continue;
            };
            let mut span = build_span(
                item.get("physicalLocation")
                    .and_then(Value::as_object)
                    .unwrap_or(item),
                if index == 0 { "primary" } else { "related" },
                "structured.sarif",
                workspace_root,
            );
            if let Some(span) = span.as_mut()
                && span.label.is_none()
            {
                span.label = span_label(map);
            }
            push_span(&mut spans, span);
        }
    }
    for key in ["related_locations", "relatedLocations"] {
        if let Some(items) = map.get(key).and_then(Value::as_array) {
            for item in items.iter().take(MAX_SOURCE_SPANS) {
                let Some(item) = item.as_object() else {
                    continue;
                };
                push_span(
                    &mut spans,
                    build_span(
                        item.get("physicalLocation")
                            .and_then(Value::as_object)
                            .unwrap_or(item),
                        "related",
                        "structured.related_location",
                        workspace_root,
                    ),
                );
            }
        }
    }

    // Common structured diagnostic wrappers. Do not recurse through arbitrary
    // object fields: a name such as `path` is not source evidence by itself.
    for key in ["primary_location", "primaryLocation", "location", "span"] {
        if let Some(location) = map.get(key).and_then(Value::as_object) {
            push_span(
                &mut spans,
                build_span(location, "primary", "structured.location", workspace_root),
            );
        }
    }
    extract_typescript_spans(map, workspace_root, &mut spans);
    extract_go_test_spans(map, workspace_root, &mut spans);
    extract_jest_vitest_spans(map, workspace_root, &mut spans);
    extract_unified_diff_spans(map, workspace_root, &mut spans);
    push_span(
        &mut spans,
        build_span(map, "primary", "structured.direct", workspace_root),
    );

    let mut primary = None;
    let mut related = Vec::new();
    for mut span in spans {
        if primary.is_none() && span.role == "primary" {
            primary = Some(span);
        } else {
            if span.role == "primary" {
                span.role = "related".to_string();
            }
            related.push(span);
        }
    }
    if primary.is_none()
        && let Some(index) = related.iter().position(|span| span.role == "related")
    {
        let mut span = related.remove(index);
        span.role = "primary".to_string();
        primary = Some(span);
    }
    related.truncate(MAX_SOURCE_SPANS.saturating_sub(primary.is_some() as usize));
    (primary, related)
}

fn push_span(target: &mut Vec<SourceSpan>, span: Option<SourceSpan>) {
    let Some(span) = span else {
        return;
    };
    if target.len() >= MAX_SOURCE_SPANS || target.iter().any(|existing| existing == &span) {
        return;
    }
    target.push(span);
}

fn build_span(
    map: &Map<String, Value>,
    role: &str,
    origin: &str,
    workspace_root: Option<&Path>,
) -> Option<SourceSpan> {
    let region = map.get("region").and_then(Value::as_object).unwrap_or(map);
    let start_line = field_u64(region, &["start_line", "startLine", "line_start", "line"])?;
    if start_line == 0 {
        return None;
    }
    let start_column = field_u64(
        region,
        &["start_column", "startColumn", "column_start", "column"],
    );
    let end_line = field_u64(region, &["end_line", "endLine", "line_end"]);
    let end_column = field_u64(region, &["end_column", "endColumn", "column_end"]);
    if start_column == Some(0)
        || end_column == Some(0)
        || end_line.is_some_and(|end| end < start_line)
        || (end_line == Some(start_line)
            && start_column
                .zip(end_column)
                .is_some_and(|(start, end)| end < start))
    {
        return None;
    }

    let locator_map = map
        .get("artifactLocation")
        .and_then(Value::as_object)
        .unwrap_or(map);
    let path = ["file_name", "file", "path", "filename"]
        .into_iter()
        .find_map(|key| locator_map.get(key).and_then(Value::as_str))
        .and_then(|raw| normalize_workspace_path(raw, workspace_root));
    let uri = if path.is_none() {
        ["uri", "url"]
            .into_iter()
            .find_map(|key| locator_map.get(key).and_then(Value::as_str))
            .and_then(safe_external_uri)
    } else {
        None
    };
    if path.is_none() && uri.is_none() {
        return None;
    }
    let exactness = if end_line.is_some() && start_column.is_some() && end_column.is_some() {
        SourceSpanExactness::Exact
    } else if end_line.is_some() || start_column.is_some() {
        SourceSpanExactness::Range
    } else {
        SourceSpanExactness::Approximate
    };
    Some(SourceSpan {
        path,
        uri,
        start_line,
        start_column,
        end_line,
        end_column,
        role: role.to_string(),
        label: span_label(map),
        origin: origin.to_string(),
        exactness,
        redaction_state: redaction_state(&Value::Object(map.clone())),
        extra: Extra::new(),
    })
}

fn extract_typescript_spans(
    map: &Map<String, Value>,
    workspace_root: Option<&Path>,
    spans: &mut Vec<SourceSpan>,
) {
    let Some(file) = map.get("file").and_then(Value::as_object) else {
        return;
    };
    let Some(path) = file
        .get("fileName")
        .or_else(|| file.get("file_name"))
        .and_then(Value::as_str)
    else {
        return;
    };
    let start = map.get("start").and_then(Value::as_object).unwrap_or(map);
    // TypeScript's `line` and `character` values are zero-based. Only its
    // structured form is accepted; textual compiler renderings are ignored.
    let Some(line) = start
        .get("line")
        .or_else(|| start.get("lineNumber"))
        .and_then(Value::as_u64)
        .and_then(|line| line.checked_add(1))
    else {
        return;
    };
    let column = start
        .get("character")
        .or_else(|| start.get("column"))
        .and_then(Value::as_u64)
        .and_then(|column| column.checked_add(1));
    push_span(
        spans,
        text_span(
            path,
            line,
            column,
            "primary",
            "structured.typescript",
            span_label(map),
            workspace_root,
        ),
    );
}

fn extract_go_test_spans(
    map: &Map<String, Value>,
    workspace_root: Option<&Path>,
    spans: &mut Vec<SourceSpan>,
) {
    let Some(lines) = map.get("lines").and_then(Value::as_array) else {
        return;
    };
    for line in lines
        .iter()
        .filter_map(Value::as_str)
        .take(MAX_SOURCE_SPANS)
    {
        let Some((path, line_number, column)) = parse_colon_location(line) else {
            continue;
        };
        if !path.ends_with(".go") {
            continue;
        }
        push_span(
            spans,
            text_span(
                path,
                line_number,
                Some(column),
                "primary",
                "structured.go_test",
                None,
                workspace_root,
            ),
        );
    }
}

fn extract_jest_vitest_spans(
    map: &Map<String, Value>,
    workspace_root: Option<&Path>,
    spans: &mut Vec<SourceSpan>,
) {
    let Some(lines) = map.get("lines").and_then(Value::as_array) else {
        return;
    };
    for line in lines
        .iter()
        .filter_map(Value::as_str)
        .take(MAX_SOURCE_SPANS)
    {
        let candidate = line
            .rsplit_once('(')
            .and_then(|(_, location)| location.strip_suffix(')'))
            .unwrap_or(line);
        let Some((path, line_number, column)) = parse_colon_location(candidate) else {
            continue;
        };
        if !matches!(
            path.rsplit('.').next(),
            Some("js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs")
        ) {
            continue;
        }
        push_span(
            spans,
            text_span(
                path,
                line_number,
                Some(column),
                "primary",
                "structured.jest_vitest",
                None,
                workspace_root,
            ),
        );
    }
}

fn extract_unified_diff_spans(
    map: &Map<String, Value>,
    workspace_root: Option<&Path>,
    spans: &mut Vec<SourceSpan>,
) {
    let Some(lines) = map
        .get("lines")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .or_else(|| {
            map.get("text")
                .and_then(Value::as_str)
                .map(|text| text.lines().collect())
        })
    else {
        return;
    };
    let mut old_path = None;
    let mut new_path = None;
    for line in lines.into_iter().take(MAX_SOURCE_SPANS * 8) {
        if let Some(path) = line.strip_prefix("--- ") {
            old_path = diff_path(path);
        } else if let Some(path) = line.strip_prefix("+++ ") {
            new_path = diff_path(path);
        } else if let Some((old_line, new_line)) = diff_hunk_lines(line) {
            push_span(
                spans,
                new_path.as_deref().and_then(|path| {
                    text_span(
                        path,
                        new_line,
                        None,
                        "primary",
                        "structured.unified_diff",
                        Some("new".to_string()),
                        workspace_root,
                    )
                }),
            );
            push_span(
                spans,
                old_path.as_deref().and_then(|path| {
                    text_span(
                        path,
                        old_line,
                        None,
                        "related",
                        "structured.unified_diff",
                        Some("old".to_string()),
                        workspace_root,
                    )
                }),
            );
        }
    }
}

fn text_span(
    raw_path: &str,
    start_line: u64,
    start_column: Option<u64>,
    role: &str,
    origin: &str,
    label: Option<String>,
    workspace_root: Option<&Path>,
) -> Option<SourceSpan> {
    let path = normalize_workspace_path(raw_path, workspace_root)?;
    Some(SourceSpan {
        path: Some(path),
        uri: None,
        start_line,
        start_column,
        end_line: None,
        end_column: None,
        role: role.to_string(),
        label,
        origin: origin.to_string(),
        exactness: if start_column.is_some() {
            SourceSpanExactness::Range
        } else {
            SourceSpanExactness::Approximate
        },
        redaction_state: None,
        extra: Extra::new(),
    })
}

fn parse_colon_location(value: &str) -> Option<(&str, u64, u64)> {
    let value = value.trim();
    let (prefix, tail) = value.rsplit_once(':')?;
    let (path, line, column) = match (
        prefix.rsplit_once(':'),
        tail.parse::<u64>().ok().filter(|column| *column > 0),
    ) {
        (Some((path, line)), Some(column)) => {
            let line = line.parse::<u64>().ok().filter(|line| *line > 0)?;
            (path, line, column)
        }
        _ => {
            let (prefix, column) = prefix.rsplit_once(':')?;
            let (path, line) = prefix.rsplit_once(':')?;
            let line = line.parse::<u64>().ok().filter(|line| *line > 0)?;
            let column = column.parse::<u64>().ok().filter(|column| *column > 0)?;
            (path, line, column)
        }
    };
    (!path.is_empty() && !path.contains(char::is_whitespace)).then_some((path, line, column))
}

fn diff_path(value: &str) -> Option<String> {
    let path = value.split_whitespace().next()?;
    (!matches!(path, "/dev/null" | "a/dev/null" | "b/dev/null"))
        .then(|| {
            path.strip_prefix("a/")
                .or_else(|| path.strip_prefix("b/"))
                .unwrap_or(path)
        })
        .map(str::to_string)
}

fn diff_hunk_lines(value: &str) -> Option<(u64, u64)> {
    let hunk = value.strip_prefix("@@ ")?.split_once(" @@")?.0;
    let mut ranges = hunk.split_whitespace();
    let old = ranges.next()?.strip_prefix('-')?;
    let new = ranges.next()?.strip_prefix('+')?;
    Some((
        range_start(old)
            .parse::<u64>()
            .ok()
            .filter(|line| *line > 0)?,
        range_start(new)
            .parse::<u64>()
            .ok()
            .filter(|line| *line > 0)?,
    ))
}

fn range_start(range: &str) -> &str {
    range.split_once(',').map_or(range, |(start, _)| start)
}

fn span_label(map: &Map<String, Value>) -> Option<String> {
    let label = ["label", "message", "text"]
        .into_iter()
        .find_map(|key| map.get(key))
        .and_then(|value| match value {
            Value::String(text) => Some(text.as_str()),
            Value::Object(object) => object
                .get("text")
                .or_else(|| object.get("message"))
                .and_then(Value::as_str),
            _ => None,
        })?;
    (!contains_redaction_marker(label)).then(|| truncate_span_label(label))
}

fn contains_redaction_marker(value: &str) -> bool {
    value.contains("[REDACTED:") || value.contains("\u{00ab}redacted\u{00bb}")
}

fn truncate_span_label(value: &str) -> String {
    const LIMIT: usize = 500;
    if value.chars().count() <= LIMIT {
        return value.to_string();
    }
    let mut output = value.chars().take(LIMIT - 3).collect::<String>();
    output.push_str("...");
    output
}

fn field_u64(map: &Map<String, Value>, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| map.get(*name).and_then(Value::as_u64))
}

fn normalize_workspace_path(raw: &str, workspace_root: Option<&Path>) -> Option<String> {
    if raw.is_empty()
        || raw.contains('\0')
        || raw.contains("[REDACTED:")
        || raw.contains("\u{00ab}redacted\u{00bb}")
    {
        return None;
    }
    let normalized = raw.replace('\\', "/");
    let absolute = normalized.starts_with('/')
        || normalized.starts_with("//")
        || normalized.as_bytes().get(1) == Some(&b':');
    let relative = if absolute {
        let root = workspace_root?.to_string_lossy().replace('\\', "/");
        let root = normalize_path_components(&root, true)?;
        let absolute = normalize_path_components(&normalized, true)?;
        absolute
            .strip_prefix(&format!("{root}/"))
            .or_else(|| (absolute == root).then_some(""))?
            .to_string()
    } else {
        normalized
    };
    normalize_path_components(&relative, false)
}

fn normalize_path_components(raw: &str, absolute: bool) -> Option<String> {
    let mut output = Vec::new();
    for component in raw.split('/') {
        match component {
            "" | "." => {}
            ".." => return None,
            value => output.push(value),
        }
    }
    let joined = output.join("/");
    if absolute {
        (!joined.is_empty()).then(|| format!("/{joined}"))
    } else {
        (!joined.is_empty()).then_some(joined)
    }
}

fn safe_external_uri(raw: &str) -> Option<String> {
    if raw.is_empty()
        || raw.len() > 2_048
        || raw.contains(char::is_whitespace)
        || raw.contains("[REDACTED:")
        || raw.contains("\u{00ab}redacted\u{00bb}")
    {
        return None;
    }
    let colon = raw.find(':')?;
    let scheme = &raw[..colon];
    if scheme.is_empty()
        || scheme == "file"
        || !scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    {
        return None;
    }
    Some(raw.to_string())
}

/// Generic identities intentionally preserve every non-whitespace character in
/// the selected message. Domain packs may introduce proven templates later;
/// generic value stripping would merge distinct failures.
const MAX_IDENTITY_CHARS: usize = 1_024;

/// Compute a cross-observation fingerprint from an already-redacted value.
/// The hashed bytes are canonical JSON with explicitly tagged components;
/// delimiter concatenation cannot make component boundaries ambiguous.
pub fn fingerprint_finding(
    kind: &str,
    provider: &str,
    value: &Value,
    primary_span: Option<&SourceSpan>,
    selectors: Option<&BTreeMap<String, String>>,
    identity: &FindingIdentityContext,
) -> Option<String> {
    let message = identity_component(value, selectors, "message_template")
        .or_else(|| message_component(value))?;
    let message = canonical_identity_text(&message);
    if message.is_empty() {
        return None;
    }

    let subject = identity_component(value, selectors, "subject").or_else(|| {
        named_identity_component(
            value,
            &[
                "nodeid",
                "node_id",
                "test_id",
                "ruleId",
                "rule_id",
                "diagnostic_code",
                "code",
            ],
        )
    });
    let diagnostic_type = identity_component(value, selectors, "diagnostic_type").or_else(|| {
        named_identity_component(
            value,
            &["exception_type", "diagnostic_type", "error_type", "type"],
        )
    });
    // File is a fallback subject only; raw line/column values never take part
    // in cross-run equivalence.
    let file = if subject.is_none() {
        identity_component(value, selectors, "file").or_else(|| {
            primary_span.and_then(|span| span.path.clone().or_else(|| span.uri.clone()))
        })
    } else {
        None
    };

    let mut components = Vec::new();
    push_identity_component(&mut components, "finding_kind", kind);
    push_identity_component(&mut components, "classifier", provider);
    if let Some(value) = identity.provider.as_deref() {
        push_identity_component(&mut components, "provider", value);
    }
    if let Some(value) = identity.parser.as_deref() {
        push_identity_component(&mut components, "parser", value);
    }
    if let Some(value) = identity.lens.as_deref() {
        push_identity_component(&mut components, "lens", value);
    }
    if let Some(value) = subject
        .as_deref()
        .map(canonical_identity_text)
        .filter(|value| !value.is_empty())
    {
        push_identity_component(&mut components, "subject", &value);
    }
    if let Some(value) = diagnostic_type
        .as_deref()
        .map(canonical_identity_text)
        .filter(|value| !value.is_empty())
    {
        push_identity_component(&mut components, "diagnostic_type", &value);
    }
    if let Some(value) = file
        .as_deref()
        .map(canonical_identity_text)
        .filter(|value| !value.is_empty())
    {
        push_identity_component(&mut components, "file", &value);
    }
    push_identity_component(&mut components, "message_template", &message);

    let mut document = BTreeMap::new();
    document.insert(
        "algorithm".to_string(),
        Value::String("prog.finding_identity.v1".to_string()),
    );
    document.insert("components".to_string(), Value::Array(components));
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(&document).expect("identity document serializes"));
    Some(format!("sha256:{:x}", hash.finalize()))
}

fn push_identity_component(components: &mut Vec<Value>, tag: &str, value: &str) {
    let mut component = BTreeMap::new();
    component.insert("tag".to_string(), Value::String(tag.to_string()));
    component.insert("value".to_string(), Value::String(value.to_string()));
    components.push(serde_json::to_value(component).expect("identity component serializes"));
}

fn identity_component(
    value: &Value,
    selectors: Option<&BTreeMap<String, String>>,
    key: &str,
) -> Option<String> {
    let pointer = selectors?.get(key)?;
    pointer::get(value, pointer)
        .ok()
        .flatten()
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn message_component(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Object(object) => ["message", "reason", "summary"]
            .into_iter()
            .find_map(|field| {
                object
                    .get(field)
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            }),
        _ => None,
    }
}

fn named_identity_component(value: &Value, names: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    names.iter().find_map(|name| {
        object
            .get(*name)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn canonical_identity_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_IDENTITY_CHARS)
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct Signal {
    kind: &'static str,
    confidence: f64,
    reason: &'static str,
    title: &'static str,
    severity: Option<&'static str>,
    source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoalIntent {
    RootCause,
    TestFailure,
    SummarizeIssues,
    Security,
    Logs,
    DiffReview,
    General,
}

impl GoalIntent {
    fn from_text(goal: &str) -> Self {
        let normalized = normalize_text(goal);
        if normalized.contains("root cause")
            || normalized.contains("why")
            || normalized.contains("debug")
            || normalized.contains("fix")
        {
            Self::RootCause
        } else if normalized.contains("test")
            || normalized.contains("pytest")
            || normalized.contains("cargo")
            || normalized.contains("fail")
        {
            Self::TestFailure
        } else if normalized.contains("issue")
            || normalized.contains("summar")
            || normalized.contains("triage")
        {
            Self::SummarizeIssues
        } else if normalized.contains("security")
            || normalized.contains("secret")
            || normalized.contains("vulnerab")
            || normalized.contains("cve")
        {
            Self::Security
        } else if normalized.contains("log") {
            Self::Logs
        } else if normalized.contains("diff") || normalized.contains("review") {
            Self::DiffReview
        } else {
            Self::General
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::RootCause => "root_cause",
            Self::TestFailure => "test_failure",
            Self::SummarizeIssues => "summarize_issues",
            Self::Security => "security",
            Self::Logs => "logs",
            Self::DiffReview => "diff_review",
            Self::General => "general",
        }
    }
}

fn collect_run_signals(
    value: &Value,
    path: &str,
    workspace_root: Option<&Path>,
    out: &mut Vec<Candidate>,
) {
    let Value::Object(map) = value else {
        return;
    };

    if let Some(command) = map.get("command").and_then(Value::as_object) {
        collect_command_signals(command, pointer::push(path, "command"), workspace_root, out);
    }

    if let Some(sections) = map.get("failure_sections").and_then(Value::as_array) {
        collect_failure_sections(
            sections,
            pointer::push(path, "failure_sections"),
            workspace_root,
            out,
        );
    }
}

fn collect_command_signals(
    command: &Map<String, Value>,
    path: String,
    workspace_root: Option<&Path>,
    out: &mut Vec<Candidate>,
) {
    if command
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let signal = Signal {
            kind: "command_timeout",
            confidence: 0.98,
            reason: "command metadata reports a timeout",
            title: "command timed out",
            severity: Some("error"),
            source: "generic.run.command",
        };
        out.push(Candidate::from_signal(
            path.clone(),
            &Value::Object(command.clone()),
            signal,
            workspace_root,
        ));
    }

    if command
        .get("spawn_error")
        .and_then(Value::as_str)
        .is_some_and(|message| !message.is_empty())
    {
        let signal = Signal {
            kind: "command_spawn_error",
            confidence: 0.98,
            reason: "command metadata reports a spawn error",
            title: "command spawn error",
            severity: Some("error"),
            source: "generic.run.command",
        };
        out.push(Candidate::from_signal(
            pointer::push(&path, "spawn_error"),
            command.get("spawn_error").unwrap_or(&Value::Null),
            signal,
            workspace_root,
        ));
    }

    if command
        .get("success")
        .and_then(Value::as_bool)
        .is_some_and(|success| !success)
    {
        let signal = Signal {
            kind: "nonzero_exit",
            confidence: 0.72,
            reason: "command metadata reports an unsuccessful exit",
            title: "command exited unsuccessfully",
            severity: Some("error"),
            source: "generic.run.command",
        };
        out.push(Candidate::from_signal(
            path,
            &Value::Object(command.clone()),
            signal,
            workspace_root,
        ));
    }
}

/// Known-safe stream identifiers that may be echoed into finding metadata.
/// Anything else is omitted so a payload-controlled stream value cannot carry
/// a secret the persistence value-redactor did not classify as high-confidence
/// into the agent-facing `extra`/`reason`.
const SAFE_SECTION_STREAMS: &[&str] = &[
    "stdout",
    "stderr",
    "combined",
    "all",
    "stdout_head",
    "stderr_head",
    "stdout_tail",
    "stderr_tail",
    "combined_head",
    "combined_tail",
];

/// Known-safe upstream section-kind labels that may be echoed into finding
/// metadata. Exotic or attacker-controlled kinds are omitted for the same
/// reason as `SAFE_SECTION_STREAMS`.
const SAFE_SECTION_KINDS: &[&str] = &[
    "generic",
    "timeout",
    "spawn_error",
    "build",
    "compile",
    "rust_compile",
    "test",
    "python",
    "rust",
    "go",
    "c",
    "cpp",
    "csharp",
    "java",
    "javascript",
    "typescript",
    "ruby",
    "kotlin",
    "swift",
    "php",
    "scala",
    "exception",
    "diagnostic",
    "warning",
    "error",
];

/// Return the identifier only when it exactly matches a known-safe label, so
/// payload-controlled text is never echoed verbatim into agent-facing metadata.
fn allowlisted_identifier<'a>(value: &'a str, allowed: &[&'static str]) -> Option<&'a str> {
    allowed.contains(&value).then_some(value)
}

fn collect_failure_sections(
    sections: &[Value],
    path: String,
    workspace_root: Option<&Path>,
    out: &mut Vec<Candidate>,
) {
    for (index, section) in sections.iter().take(MAX_FINDING_NODES).enumerate() {
        let section_path = pointer::push(&path, &index.to_string());
        let Value::Object(map) = section else {
            continue;
        };
        let text = section_text(map);
        let kind = map.get("kind").and_then(Value::as_str).unwrap_or("generic");
        let signal = failure_section_signal(kind, &text);
        let priority = map.get("priority").and_then(Value::as_u64).unwrap_or(60);
        let confidence = (0.78 + priority.min(100) as f64 * 0.002).clamp(signal.confidence, 0.99);

        let mut candidate = Candidate::from_signal(section_path, section, signal, workspace_root);
        candidate.confidence = confidence;
        candidate.reason = failure_section_reason(map, signal.reason);
        candidate.line_range = line_range(map);
        if let Some(safe_kind) = allowlisted_identifier(kind, SAFE_SECTION_KINDS) {
            candidate
                .extra
                .insert("section_kind".to_string(), json!(safe_kind));
        }
        if let Some(stream) = map
            .get("stream")
            .and_then(Value::as_str)
            .and_then(|s| allowlisted_identifier(s, SAFE_SECTION_STREAMS))
        {
            candidate.extra.insert("stream".to_string(), json!(stream));
        }
        candidate
            .extra
            .insert("priority".to_string(), json!(priority));
        out.push(candidate);
    }
}

fn failure_section_signal(kind: &str, text: &str) -> Signal {
    let lower = normalize_text(text);
    if kind == "timeout" {
        Signal {
            kind: "command_timeout",
            confidence: 0.98,
            reason: "run failure section reports a timeout",
            title: "command timed out",
            severity: Some("error"),
            source: "generic.run.failure_sections",
        }
    } else if kind == "spawn_error" {
        Signal {
            kind: "command_spawn_error",
            confidence: 0.98,
            reason: "run failure section reports a spawn error",
            title: "command spawn error",
            severity: Some("error"),
            source: "generic.run.failure_sections",
        }
    } else if lower.contains("error[") {
        Signal {
            kind: "rust_compile_error",
            confidence: 0.94,
            reason: "run failure section contains a Rust compiler diagnostic",
            title: "Rust compiler diagnostic",
            severity: Some("error"),
            source: "generic.run.failure_sections",
        }
    } else if lower.contains("panicked at") {
        Signal {
            kind: "rust_panic",
            confidence: 0.92,
            reason: "run failure section contains a Rust panic",
            title: "Rust panic",
            severity: Some("error"),
            source: "generic.run.failure_sections",
        }
    } else if lower.contains("traceback (most recent call last)") {
        Signal {
            kind: "python_traceback",
            confidence: 0.92,
            reason: "run failure section contains a Python traceback",
            title: "Python traceback",
            severity: Some("error"),
            source: "generic.run.failure_sections",
        }
    } else if lower.contains("assertionerror") || lower.contains("assertion failed") {
        Signal {
            kind: "test_failure",
            confidence: 0.9,
            reason: "run failure section contains an assertion failure",
            title: "test assertion failure",
            severity: Some("error"),
            source: "generic.run.failure_sections",
        }
    } else if is_compile_error(&lower) {
        // Generic build/compile framing (cargo/rustc/gmake/ninja). Placed AFTER
        // rust_compile_error so an `error[E0308]` diagnostic is not double-counted.
        Signal {
            kind: "compile_error",
            confidence: 0.9,
            reason: "run failure section contains a build or compile error",
            title: "compile error",
            severity: Some("error"),
            source: "generic.run.failure_sections",
        }
    } else if is_test_name(&lower) {
        Signal {
            kind: "test_name",
            confidence: 0.82,
            reason: "run failure section references a failing test",
            title: "failing test",
            severity: Some("error"),
            source: "generic.run.failure_sections",
        }
    } else if lower.contains("exception") {
        Signal {
            kind: "exception",
            confidence: 0.84,
            reason: "run failure section contains an exception",
            title: "exception",
            severity: Some("error"),
            source: "generic.run.failure_sections",
        }
    } else {
        Signal {
            kind: "stderr_error",
            confidence: 0.78,
            reason: "run failure section contains captured diagnostics",
            title: "failure diagnostics",
            severity: Some("error"),
            source: "generic.run.failure_sections",
        }
    }
}

fn failure_section_reason(section: &Map<String, Value>, fallback: &str) -> String {
    // Only interpolate a stream identifier when it is a known-safe label; a
    // payload-controlled stream value could otherwise echo a secret the value
    // redactor did not classify as high-confidence into the agent-facing reason.
    let stream = section
        .get("stream")
        .and_then(Value::as_str)
        .and_then(|s| allowlisted_identifier(s, SAFE_SECTION_STREAMS));
    let line_start = section.get("line_start").and_then(Value::as_u64);
    let line_end = section.get("line_end").and_then(Value::as_u64);
    match (stream, line_start, line_end) {
        (Some(stream), Some(start), Some(end)) => {
            format!("{fallback}; inspect {stream} lines {start}-{end}")
        }
        _ => fallback.to_string(),
    }
}

fn section_text(section: &Map<String, Value>) -> String {
    section
        .get("lines")
        .and_then(Value::as_array)
        .map(|lines| {
            lines
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn line_range(map: &Map<String, Value>) -> Option<LineRange> {
    Some(LineRange {
        start: map.get("line_start")?.as_u64()?,
        end: map.get("line_end")?.as_u64()?,
        extra: Extra::new(),
    })
}

fn collect_generic_signals(
    value: &Value,
    path: &str,
    workspace_root: Option<&Path>,
    out: &mut Vec<Candidate>,
    depth: usize,
    visited: &mut usize,
) {
    if *visited >= MAX_FINDING_NODES || depth > MAX_FINDING_DEPTH {
        return;
    }
    *visited += 1;
    match value {
        Value::Object(map) => {
            collect_object_level_signal(map, path, value, workspace_root, out);
            for (key, child) in map {
                let child_path = pointer::push(path, key);
                if let Some(signal) = key_signal(key, child) {
                    let mut candidate =
                        Candidate::from_signal(child_path.clone(), child, signal, workspace_root);
                    candidate.inherit_source_spans(value, workspace_root);
                    candidate.use_identity_context(value);
                    out.push(candidate);
                }
                collect_generic_signals(
                    child,
                    &child_path,
                    workspace_root,
                    out,
                    depth + 1,
                    visited,
                );
                if *visited >= MAX_FINDING_NODES {
                    break;
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_generic_signals(
                    child,
                    &pointer::push(path, &index.to_string()),
                    workspace_root,
                    out,
                    depth + 1,
                    visited,
                );
                if *visited >= MAX_FINDING_NODES {
                    break;
                }
            }
        }
        Value::String(text) => {
            // Command argv is provenance, not observed output. Treating a
            // shell snippet containing words like "error" as causal evidence
            // creates false findings and can outrank the actual stream data.
            if !is_command_argv_path(path)
                && let Some(signal) = string_signal(text)
            {
                out.push(Candidate::from_signal(
                    path.to_string(),
                    value,
                    signal,
                    workspace_root,
                ));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn collect_object_level_signal(
    map: &Map<String, Value>,
    path: &str,
    value: &Value,
    workspace_root: Option<&Path>,
    out: &mut Vec<Candidate>,
) {
    let severity = map
        .get("severity")
        .or_else(|| map.get("level"))
        .or_else(|| map.get("status"))
        .and_then(Value::as_str)
        .map(normalize_text);
    if severity
        .as_deref()
        .is_some_and(|value| matches!(value, "error" | "failed" | "failure" | "critical" | "fatal"))
    {
        out.push(Candidate::from_signal(
            path.to_string(),
            value,
            Signal {
                kind: "diagnostic",
                confidence: 0.68,
                reason: "object severity indicates an error or failure",
                title: "error diagnostic",
                severity: Some("error"),
                source: "generic.object.severity",
            },
            workspace_root,
        ));
    }
}

fn key_signal(key: &str, value: &Value) -> Option<Signal> {
    if key == "failure_sections" || is_absent_signal_value(value) {
        return None;
    }
    let normalized = normalize_key(key);
    if normalized.contains("error") {
        Some(Signal {
            kind: "generic_error_field",
            confidence: 0.72,
            reason: "field name indicates error evidence",
            title: "error field",
            severity: Some("error"),
            source: "generic.field_name",
        })
    } else if normalized.contains("failure") || normalized.contains("failed") {
        Some(Signal {
            kind: "test_failure",
            confidence: 0.7,
            reason: "field name indicates failure evidence",
            title: "failure field",
            severity: Some("error"),
            source: "generic.field_name",
        })
    } else if normalized.contains("exception") {
        Some(Signal {
            kind: "exception",
            confidence: 0.72,
            reason: "field name indicates exception evidence",
            title: "exception field",
            severity: Some("error"),
            source: "generic.field_name",
        })
    } else if normalized.contains("diagnostic") {
        Some(Signal {
            kind: "diagnostic",
            confidence: 0.62,
            reason: "field name indicates diagnostic evidence",
            title: "diagnostic field",
            severity: Some("error"),
            source: "generic.field_name",
        })
    } else if normalized.contains("warning") {
        Some(Signal {
            kind: "warning",
            confidence: 0.42,
            reason: "field name indicates warning evidence",
            title: "warning field",
            severity: Some("warning"),
            source: "generic.field_name",
        })
    } else if (normalized.contains("diff")
        || normalized.contains("patch")
        || normalized.contains("hunks"))
        && value.as_str().is_some_and(is_diff_hunk)
    {
        // Field name + value shape together indicate a unified diff hunk; a diff
        // is evidence to review, not an error, so severity stays None.
        value.as_str().map(|_| Signal {
            kind: "diff_hunk",
            confidence: 0.6,
            reason: "field name and value indicate a unified diff hunk",
            title: "diff hunk",
            severity: None,
            source: "generic.field_name",
        })
    } else if matches!(normalized.as_str(), "message" | "reason" | "summary") {
        value.as_str().and_then(string_signal)
    } else {
        None
    }
}

fn string_signal(text: &str) -> Option<Signal> {
    let normalized = normalize_text(text);
    if normalized.contains("error[") {
        Some(Signal {
            kind: "rust_compile_error",
            confidence: 0.86,
            reason: "string contains a Rust compiler diagnostic marker",
            title: "Rust compiler diagnostic",
            severity: Some("error"),
            source: "generic.string_pattern",
        })
    } else if normalized.contains("panicked at") {
        Some(Signal {
            kind: "rust_panic",
            confidence: 0.84,
            reason: "string contains a Rust panic marker",
            title: "Rust panic",
            severity: Some("error"),
            source: "generic.string_pattern",
        })
    } else if normalized.contains("traceback (most recent call last)") {
        Some(Signal {
            kind: "stack_trace",
            confidence: 0.84,
            reason: "string contains a traceback marker",
            title: "stack trace",
            severity: Some("error"),
            source: "generic.string_pattern",
        })
    } else if normalized.contains("assertionerror") || normalized.contains("assertion failed") {
        Some(Signal {
            kind: "test_failure",
            confidence: 0.8,
            reason: "string contains an assertion failure marker",
            title: "assertion failure",
            severity: Some("error"),
            source: "generic.string_pattern",
        })
    } else if is_compile_error(&normalized) {
        // Generic build/compile framing (cargo/rustc/gmake/ninja). Placed AFTER
        // rust_compile_error (`error[`) so a rustc diagnostic is not double-counted,
        // and BEFORE the generic `error:`/` failed` stderr branch.
        Some(Signal {
            kind: "compile_error",
            confidence: 0.82,
            reason: "string contains a build or compile error marker",
            title: "compile error",
            severity: Some("error"),
            source: "generic.string_pattern",
        })
    } else if is_test_name(&normalized) {
        Some(Signal {
            kind: "test_name",
            confidence: 0.74,
            reason: "string references a failing test",
            title: "failing test",
            severity: Some("error"),
            source: "generic.string_pattern",
        })
    } else if has_explicit_log_error_line(text) {
        Some(Signal {
            kind: "log_error",
            confidence: 0.72,
            reason: "string contains an explicit ERROR or FATAL log line",
            title: "log error",
            severity: Some("error"),
            source: "generic.string_pattern",
        })
    } else if normalized.contains("exception") {
        Some(Signal {
            kind: "exception",
            confidence: 0.7,
            reason: "string contains an exception marker",
            title: "exception",
            severity: Some("error"),
            source: "generic.string_pattern",
        })
    } else if normalized.starts_with("error:")
        || normalized.contains(" error:")
        || contains_failure_word(&normalized)
    {
        Some(Signal {
            kind: "stderr_error",
            confidence: 0.58,
            reason: "string contains generic error text",
            title: "error text",
            severity: Some("error"),
            source: "generic.string_pattern",
        })
    } else if is_diff_hunk(text) {
        // Unified-diff hunk markers. Diffs are evidence to review, not errors,
        // so severity stays None and confidence is intentionally moderate.
        Some(Signal {
            kind: "diff_hunk",
            confidence: 0.6,
            reason: "string contains unified diff hunk markers",
            title: "diff hunk",
            severity: None,
            source: "generic.string_pattern",
        })
    } else if normalized.contains("warning:") || normalized.contains(" warning") {
        Some(Signal {
            kind: "warning",
            confidence: 0.34,
            reason: "string contains warning text",
            title: "warning text",
            severity: Some("warning"),
            source: "generic.string_pattern",
        })
    } else {
        None
    }
}

fn is_empty_container(value: &Value) -> bool {
    matches!(value, Value::Array(items) if items.is_empty())
        || matches!(value, Value::Object(map) if map.is_empty())
}

fn is_absent_signal_value(value: &Value) -> bool {
    matches!(value, Value::Null | Value::Bool(false))
        || value.as_str().is_some_and(str::is_empty)
        || is_empty_container(value)
}

fn is_command_argv_path(path: &str) -> bool {
    pointer::parse(path).is_ok_and(|segments| {
        segments
            .windows(2)
            .any(|window| window[0] == "command" && window[1] == "argv")
    })
}

fn score_candidate(candidate: &Candidate, intent: GoalIntent) -> f64 {
    let mut score = candidate.confidence;
    score += kind_bonus(&candidate.kind, intent);
    if candidate.source == "generic.run.failure_sections" {
        score += 0.08;
    }
    if candidate.path.ends_with("/text") {
        score -= 0.04;
    }
    score.clamp(0.0, 1.25)
}

fn kind_bonus(kind: &str, intent: GoalIntent) -> f64 {
    match intent {
        GoalIntent::RootCause => match kind {
            "rust_compile_error"
            | "compile_error"
            | "rust_panic"
            | "python_traceback"
            | "command_timeout"
            | "command_spawn_error"
            | "test_failure" => 0.12,
            "test_name" => 0.04,
            "diff_hunk" => -0.05,
            "nonzero_exit" => 0.02,
            "warning" => -0.08,
            _ => 0.04,
        },
        GoalIntent::TestFailure => match kind {
            "test_failure" | "rust_panic" | "python_traceback" | "rust_compile_error"
            | "compile_error" => 0.14,
            "test_name" => 0.10,
            "warning" => -0.08,
            _ => 0.02,
        },
        GoalIntent::SummarizeIssues => match kind {
            "generic_error_field" | "diagnostic" | "warning" => 0.06,
            _ => 0.0,
        },
        GoalIntent::Security => match kind {
            "generic_error_field" | "diagnostic" => 0.04,
            _ => 0.0,
        },
        GoalIntent::Logs => match kind {
            "stack_trace" | "stderr_error" | "exception" | "warning" => 0.08,
            _ => 0.0,
        },
        GoalIntent::DiffReview => match kind {
            "diff_hunk" => 0.14,
            "diagnostic" | "warning" => 0.04,
            _ => 0.0,
        },
        GoalIntent::General => 0.0,
    }
}

fn compare_candidates(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            right
                .confidence
                .partial_cmp(&left.confidence)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| left.kind.cmp(&right.kind))
}

fn command_hints(
    cursor: Option<&str>,
    path: &str,
    kind: &str,
    hints: CommandHintConfig,
) -> FindingCommandHints {
    let Some(cursor) = cursor else {
        return FindingCommandHints::default();
    };
    let cursor = shell_arg(cursor);
    let path = shell_arg(path);
    let goal = shell_arg(&format!("investigate {kind}"));
    let kind = shell_arg(kind);
    FindingCommandHints {
        inspect: hints
            .inspect
            .then(|| format!("prog inspect {cursor} --goal {goal} --path {path}")),
        expand: hints
            .expand
            .then(|| format!("prog expand {cursor} --path {path}")),
        evidence: hints
            .evidence
            .then(|| format!("prog evidence {cursor} --path {path}")),
        search: hints
            .search
            .then(|| format!("prog find {cursor} --kind {kind} --path {path}")),
        extra: Extra::new(),
    }
}

fn redaction_state(value: &Value) -> Option<RedactionState> {
    let mut visited = 0usize;
    let count = count_redacted_values(value, 0, &mut visited);
    (count > 0).then(|| RedactionState {
        redacted: true,
        redacted_paths: count,
        lossy: false,
        extra: Extra::new(),
    })
}

fn count_redacted_values(value: &Value, depth: usize, visited: &mut usize) -> u64 {
    if *visited >= MAX_FINDING_NODES || depth > MAX_FINDING_DEPTH {
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
            .map(|value| count_redacted_values(value, depth + 1, visited))
            .sum(),
        Value::Object(map) => map
            .values()
            .map(|value| count_redacted_values(value, depth + 1, visited))
            .sum(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 0,
    }
}

fn round_confidence(value: f64) -> f64 {
    (value.clamp(0.0, 1.0) * 100.0).round() / 100.0
}

fn truncate_reason(reason: &str) -> String {
    if reason.chars().count() <= MAX_REASON_CHARS {
        return reason.to_string();
    }
    let mut output = reason
        .chars()
        .take(MAX_REASON_CHARS.saturating_sub(1))
        .collect::<String>();
    output.push_str("...");
    output
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|ch| *ch != '-' && *ch != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

fn normalize_text(text: &str) -> String {
    text.to_ascii_lowercase().replace(['_', '-'], " ")
}

/// Detect generic build/compile error framing (cargo, rustc, gmake, ninja).
/// `error[E0nnn]` rustc diagnostics are matched earlier as `rust_compile_error`
/// so a real rustc diagnostic is never double-counted here.
fn is_compile_error(normalized: &str) -> bool {
    normalized.contains("could not compile")
        || normalized.contains("cargo: error")
        || normalized.contains("rustc: error")
        || (normalized.contains("gmake") && normalized.contains("error"))
        || (normalized.contains("ninja") && normalized.contains("error"))
}

/// Detect a reference to a failing test: pytest nodeids (`test_x.py::test_case`),
/// cargo result lines (`test foo ... FAILED`), gtest filters, and mocha/jest
/// summary counts (`2 passing`, `1 failing`).
fn is_test_name(normalized: &str) -> bool {
    normalized.contains(".py::")
        || normalized.contains("::test")
        // "test filter" catches both `--gtest_filter` (underscore -> space) and
        // the runtime banner "Google Test filter = ...".
        || normalized.contains("test filter")
        || (normalized.contains("test ") && normalized.contains("... failed"))
        || has_counted_label(normalized, "passing")
        || has_counted_label(normalized, "failing")
}

fn contains_failure_word(normalized: &str) -> bool {
    let mut start = 0;
    while let Some(relative) = normalized[start..].find("failed") {
        let position = start + relative;
        let before = normalized[..position].trim_end();
        let is_word_boundary = before
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_ascii_alphabetic());
        let after = &normalized[position + "failed".len()..];
        let ends_at_boundary = after
            .chars()
            .next()
            .is_none_or(|character| !character.is_ascii_alphabetic());
        if is_word_boundary && ends_at_boundary {
            let preceding = before.split_whitespace().next_back().unwrap_or_default();
            if preceding.parse::<u64>() != Ok(0) {
                return true;
            }
        }
        start = position + "failed".len();
    }
    false
}

fn has_explicit_log_error_line(text: &str) -> bool {
    text.lines().any(|line| {
        let normalized = line.trim_start().to_ascii_lowercase();
        ["error ", "error:", "fatal ", "fatal:"]
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
            || line.split_whitespace().any(|token| {
                matches!(
                    token.trim_matches(|character: char| !character.is_ascii_alphabetic()),
                    "ERROR" | "FATAL"
                )
            })
    })
}

/// True when `label` (`"passing"`/`"failing"`) is preceded by a decimal count,
/// matching mocha/jest summary lines like `"  2 passing (3s)"`.
fn has_counted_label(normalized: &str, label: &str) -> bool {
    let mut start = 0;
    while let Some(rel) = normalized[start..].find(label) {
        let pos = start + rel;
        let before = normalized[..pos].trim_end();
        if before
            .bytes()
            .next_back()
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            return true;
        }
        start = pos + label.len();
    }
    false
}

/// Detect unified-diff hunk markers on the raw (un-normalized) text. Normalization
/// would erase the leading `+`/`-` runes, so `@@` hunk headers and the git header
/// are the load-bearing signals.
fn is_diff_hunk(text: &str) -> bool {
    text.contains("@@")
        || text.contains("diff --git")
        || text.contains("\n+++ ")
        || text.contains("\n--- ")
}

fn shell_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':' | '~'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

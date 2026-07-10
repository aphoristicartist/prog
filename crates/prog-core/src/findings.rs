use std::{cmp::Ordering, collections::BTreeMap};

use serde_json::{Map, Value, json};

use crate::{
    Extra, Finding, FindingCommandHints, INSPECT_VERSION, InspectResponse, LineRange,
    RedactionState, Result, pointer,
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
}

impl Default for FindingOptions {
    fn default() -> Self {
        Self {
            goal: None,
            cursor: None,
            scope_path: None,
            limit: DEFAULT_LIMIT,
            hints: CommandHintConfig::NAV_EXPAND_ONLY,
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
    collect_run_signals(scoped, scope_path, &mut candidates);
    let mut visited = 0usize;
    collect_generic_signals(scoped, scope_path, &mut candidates, 0, &mut visited);

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
        .map(|(index, candidate)| candidate.into_finding(index as u64 + 1, options))
        .collect())
}

/// Assemble a full [`InspectResponse`] over an already-redacted, stored payload.
///
/// This is the single boundary the `prog inspect` CLI command calls.
/// The engine is pure and store-less: it projects a ranked view over `payload`
/// (consumed AFTER redact -> infer -> store -> project), stamps `schema_version`
/// from [`INSPECT_VERSION`], and derives `normalized_goal` via
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
    };
    let findings = ranked_findings(payload, &options)?;
    Ok(InspectResponse {
        schema_version: INSPECT_VERSION.to_string(),
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
    redaction_state: Option<RedactionState>,
    extra: Extra,
}

impl Candidate {
    fn from_signal(path: String, value: &Value, signal: Signal) -> Self {
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
            redaction_state: redaction_state(value),
            extra: Extra::new(),
        }
    }

    fn into_finding(self, rank: u64, options: &FindingOptions) -> Finding {
        let commands = command_hints(
            options.cursor.as_deref(),
            &self.path,
            &self.kind,
            options.hints,
        );
        Finding {
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
            redaction_state: self.redaction_state,
            commands,
            extra: self.extra,
        }
    }
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

fn collect_run_signals(value: &Value, path: &str, out: &mut Vec<Candidate>) {
    let Value::Object(map) = value else {
        return;
    };

    if let Some(command) = map.get("command").and_then(Value::as_object) {
        collect_command_signals(command, pointer::push(path, "command"), out);
    }

    if let Some(sections) = map.get("failure_sections").and_then(Value::as_array) {
        collect_failure_sections(sections, pointer::push(path, "failure_sections"), out);
    }
}

fn collect_command_signals(command: &Map<String, Value>, path: String, out: &mut Vec<Candidate>) {
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

fn collect_failure_sections(sections: &[Value], path: String, out: &mut Vec<Candidate>) {
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

        let mut candidate = Candidate::from_signal(section_path, section, signal);
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
            collect_object_level_signal(map, path, value, out);
            for (key, child) in map {
                let child_path = pointer::push(path, key);
                if let Some(signal) = key_signal(key, child) {
                    out.push(Candidate::from_signal(child_path.clone(), child, signal));
                }
                collect_generic_signals(child, &child_path, out, depth + 1, visited);
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
                out.push(Candidate::from_signal(path.to_string(), value, signal));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn collect_object_level_signal(
    map: &Map<String, Value>,
    path: &str,
    value: &Value,
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
        || normalized.contains(" failed")
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
        redaction_version: None,
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

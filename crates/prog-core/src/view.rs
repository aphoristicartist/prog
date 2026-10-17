//! Compact response view: the same answer with the bookkeeping removed.
//!
//! The full JSON contracts carry everything a script or audit needs
//! (budgets, verdicts, provenance, templates, per-item evidence refs). An
//! agent reading a response needs far less: the data, where to move next, and
//! every reason the data might be partial. This view keeps all three and
//! drops the rest. It never adds a value: everything it emits is copied from
//! the full response, cut with a label, or a caveat derived from it.
//!
//! A caveat is never dropped. Truncation, redaction, staleness, incomplete
//! capture, and warnings all survive into `caveats` or `omitted`.

use serde_json::{Map, Value, json};

/// Omitted regions listed before the rest are counted.
const MAX_OMITTED: usize = 8;
/// Findings listed in a compact envelope.
const MAX_FINDINGS: usize = 5;
/// Items kept in a data preview whose collection is described by an overview.
const OVERVIEW_PREVIEW_ITEMS: usize = 2;
/// Comparability reasons listed in a compact delta.
const MAX_REASONS: usize = 6;

/// Response shape requested by the caller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ResponseView {
    /// The full JSON contract, as published by `prog meta`.
    #[default]
    Full,
    /// Data, moves, and caveats only.
    Compact,
}

impl ResponseView {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "compact" => Some(Self::Compact),
            _ => None,
        }
    }
}

/// Rewrite a full response into its compact form. Unknown schemas pass
/// through unchanged apart from transport budgets.
pub fn compact_view(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let schema = object.get("schema").and_then(Value::as_str).unwrap_or("");
    let compact = match schema {
        "prog.disclosure" if object.contains_key("paths") => compact_paths(object),
        "prog.disclosure" => compact_disclosure(object),
        "prog.evidence" => compact_evidence(object),
        "prog.search" => compact_search(object),
        "prog.inspect" => compact_inspect(object),
        "prog.observation_delta" => compact_delta(object),
        "prog.select" => compact_select(object),
        _ => {
            let mut passthrough = object.clone();
            for key in ["disclosure_budget", "capture_budget", "storage_budget"] {
                passthrough.remove(key);
            }
            passthrough
        }
    };
    Value::Object(compact)
}

fn compact_disclosure(full: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    copy(full, &mut out, "cursor");
    if let Some(id) = full.pointer_str("/observation/observation_id") {
        out.insert("observation_id".to_string(), json!(id));
    }
    let completeness = full.get("observation").and_then(|o| o.get("completeness"));
    if let Some(status) = completeness.and_then(|c| c.get("status")) {
        out.insert("status".to_string(), status.clone());
    }
    if let Some(summary) = full.get("summary") {
        let mut size = Map::new();
        copy(
            summary.as_object().unwrap_or(&Map::new()),
            &mut size,
            "kind",
        );
        copy(
            summary.as_object().unwrap_or(&Map::new()),
            &mut size,
            "item_count",
        );
        copy(
            summary.as_object().unwrap_or(&Map::new()),
            &mut size,
            "payload_bytes",
        );
        size.retain(|_, value| !value.is_null());
        if !size.is_empty() {
            out.insert("size".to_string(), Value::Object(size));
        }
    }
    let overview = full.get("overview");
    if let Some(overview) = overview {
        out.insert("overview".to_string(), compact_overview(overview));
    }
    let root = completeness
        .and_then(|c| c.get("root_path"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut omitted = compact_omitted(full.get("omitted"));
    if let Some(data) = full.get("data_preview") {
        let mut data = data.clone();
        drop_run_duplicates(&mut data);
        if let Some(path) = overview.and_then(|o| o.get("path")).and_then(Value::as_str)
            && let Some(note) = trim_described_collection(&mut data, root, path)
        {
            omitted.insert(0, json!(note));
        }
        out.insert("data".to_string(), data);
    }
    if !omitted.is_empty() {
        out.insert("omitted".to_string(), Value::Array(omitted));
    }
    if let Some(findings) = full.get("findings").and_then(Value::as_array)
        && !findings.is_empty()
    {
        out.insert(
            "findings".to_string(),
            Value::Array(
                findings
                    .iter()
                    .take(MAX_FINDINGS)
                    .map(compact_finding)
                    .collect(),
            ),
        );
        if findings.len() > MAX_FINDINGS {
            out.insert(
                "findings_omitted".to_string(),
                json!(findings.len() - MAX_FINDINGS),
            );
        }
    }
    if let Some(changes) = full.get("changes_since").and_then(Value::as_object) {
        out.insert(
            "changes_since".to_string(),
            Value::Object(compact_delta(changes)),
        );
    }
    let moves = compact_moves(full.get("next_actions"));
    if !moves.is_empty() {
        out.insert("next".to_string(), Value::Array(moves));
    }
    let caveats = disclosure_caveats(full);
    if !caveats.is_empty() {
        out.insert("caveats".to_string(), Value::Array(caveats));
    }
    out
}

/// `combined` interleaves the same bytes as `stdout` and `stderr`; the
/// compact view keeps the per-stream copies only.
fn drop_run_duplicates(data: &mut Value) {
    if let Value::Object(map) = data
        && map.get("format").and_then(Value::as_str) == Some("run")
    {
        map.remove("combined");
    }
}

/// When an overview already counts a collection, two sample items are enough
/// to show the record shape. Returns the omission note for the trimmed items.
fn trim_described_collection(data: &mut Value, root: &str, path: &str) -> Option<String> {
    let relative = path.strip_prefix(root)?;
    if !(relative.is_empty() || relative.starts_with('/')) {
        return None;
    }
    let target = if relative.is_empty() {
        Some(data)
    } else {
        data.pointer_mut(relative)
    }?;
    let Value::Array(items) = target else {
        return None;
    };
    if items.len() <= OVERVIEW_PREVIEW_ITEMS {
        return None;
    }
    let shown = items.len();
    items.truncate(OVERVIEW_PREVIEW_ITEMS);
    let label = if path.is_empty() { "(root)" } else { path };
    Some(format!(
        "{label}: compact view shows {OVERVIEW_PREVIEW_ITEMS} of the {shown} previewed items; see overview"
    ))
}

fn compact_overview(overview: &Value) -> Value {
    let items = overview.get("items").and_then(Value::as_u64).unwrap_or(0);
    let mut out = Map::new();
    copy_value(overview, &mut out, "path");
    out.insert("items".to_string(), json!(items));
    if let Some(kinds) = overview.get("item_kinds").and_then(Value::as_object)
        && !(kinds.len() == 1 && kinds.contains_key("object"))
    {
        out.insert("item_kinds".to_string(), Value::Object(kinds.clone()));
    }
    let mut fields = Map::new();
    for field in overview
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = field.get("field").and_then(Value::as_str) else {
            continue;
        };
        fields.insert(name.to_string(), json!(describe_field(field, items)));
    }
    out.insert("fields".to_string(), Value::Object(fields));
    if let Some(omitted) = overview.get("fields_omitted").and_then(Value::as_u64)
        && omitted > 0
    {
        out.insert("fields_omitted".to_string(), json!(omitted));
    }
    if let Some(keys) = overview.get("key_candidates").and_then(Value::as_array)
        && !keys.is_empty()
    {
        out.insert("keys".to_string(), Value::Array(keys.clone()));
    }
    Value::Object(out)
}

/// `string; open=197, closed=103` or `integer; 1..300` or
/// `object|null (12/300)`: kinds, presence when partial, then values or range.
fn describe_field(field: &Value, items: u64) -> String {
    let kinds = field
        .get("kinds")
        .and_then(Value::as_array)
        .map(|kinds| {
            kinds
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default();
    let mut text = kinds;
    let present = field
        .get("present")
        .and_then(Value::as_u64)
        .unwrap_or(items);
    if present != items {
        text.push_str(&format!(" ({present}/{items})"));
    }
    if let Some(values) = field.get("values").and_then(Value::as_array)
        && !values.is_empty()
    {
        let listed = values
            .iter()
            .map(|entry| {
                format!(
                    "{}={}",
                    value_text(entry.get("value").unwrap_or(&Value::Null)),
                    entry.get("count").and_then(Value::as_u64).unwrap_or(0)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        text.push_str("; ");
        text.push_str(&listed);
    } else if let (Some(min), Some(max)) = (field.get("min"), field.get("max")) {
        text.push_str(&format!("; {}..{}", value_text(min), value_text(max)));
    }
    text
}

fn compact_paths(full: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    copy(full, &mut out, "cursor");
    copy(full, &mut out, "prefix");
    let paths = full
        .get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|entry| {
            let mut row = Map::new();
            for key in ["path", "kind", "detail"] {
                copy_value(entry, &mut row, key);
            }
            Value::Object(row)
        })
        .collect();
    out.insert("paths".to_string(), Value::Array(paths));
    let omitted = compact_omitted(full.get("omitted"));
    if !omitted.is_empty() {
        out.insert("omitted".to_string(), Value::Array(omitted));
    }
    insert_warnings(full, &mut out);
    out
}

fn compact_evidence(full: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for key in [
        "cursor",
        "path",
        "kind",
        "summary",
        "excerpt",
        "line_range",
        "source_command",
    ] {
        copy(full, &mut out, key);
    }
    if let Some(uri) = full.pointer_str("/evidence_ref/uri") {
        out.insert("ref".to_string(), json!(uri));
    }
    if let Some(state) = full.get("redaction_state")
        && state.get("redacted").and_then(Value::as_bool) == Some(true)
    {
        out.insert("redaction".to_string(), state.clone());
    }
    let omitted = compact_omitted(full.get("omitted"));
    if !omitted.is_empty() {
        out.insert("omitted".to_string(), Value::Array(omitted));
    }
    insert_warnings(full, &mut out);
    out
}

fn compact_search(full: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for key in ["cursor", "query", "kind", "scope_path"] {
        copy(full, &mut out, key);
    }
    out.retain(|_, value| !value.is_null());
    let hits = full
        .get("hits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|hit| {
            let mut row = Map::new();
            for key in [
                "path",
                "preview",
                "finding_kind",
                "line_range",
                "primary_span",
            ] {
                copy_value(hit, &mut row, key);
            }
            Value::Object(row)
        })
        .collect();
    out.insert("hits".to_string(), Value::Array(hits));
    let omitted = compact_omitted(full.get("omitted"));
    if !omitted.is_empty() {
        out.insert("omitted".to_string(), Value::Array(omitted));
    }
    insert_warnings(full, &mut out);
    out
}

fn compact_inspect(full: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for key in ["cursor", "goal", "scope_path"] {
        copy(full, &mut out, key);
    }
    out.retain(|_, value| !value.is_null());
    let findings = full
        .get("findings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(compact_finding)
        .collect();
    out.insert("findings".to_string(), Value::Array(findings));
    let omitted = compact_omitted(full.get("omitted"));
    if !omitted.is_empty() {
        out.insert("omitted".to_string(), Value::Array(omitted));
    }
    insert_warnings(full, &mut out);
    out
}

fn compact_select(full: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for key in [
        "cursor",
        "path",
        "total",
        "matched",
        "offset",
        "next_offset",
        "rows",
    ] {
        copy(full, &mut out, key);
    }
    if full.get("unknown").and_then(Value::as_u64).unwrap_or(0) > 0 {
        copy(full, &mut out, "unknown");
    }
    out.retain(|_, value| !value.is_null());
    let omitted = compact_omitted(full.get("omitted"));
    if !omitted.is_empty() {
        out.insert("omitted".to_string(), Value::Array(omitted));
    }
    insert_warnings(full, &mut out);
    out
}

fn compact_delta(full: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(id) = full.get("baseline_observation_id") {
        out.insert("baseline".to_string(), id.clone());
    }
    if let Some(id) = full.get("subject_observation_id") {
        out.insert("subject".to_string(), id.clone());
    }
    let assessment = full.get("assessment");
    if let Some(proof) = assessment.and_then(|a| a.get("can_prove_absence")) {
        out.insert("can_prove_absence".to_string(), proof.clone());
    }
    if let Some(counts) = full.get("counts").and_then(Value::as_object)
        && !counts.is_empty()
    {
        out.insert("counts".to_string(), Value::Object(counts.clone()));
    }
    let findings = full
        .get("findings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|finding| {
            let mut row = Map::new();
            copy_value(finding, &mut row, "status");
            copy_value(finding, &mut row, "title");
            let path = finding
                .get("subject_path")
                .filter(|path| !path.is_null())
                .or_else(|| finding.get("baseline_path"));
            if let Some(path) = path.filter(|path| !path.is_null()) {
                row.insert("path".to_string(), path.clone());
            }
            if let Some(reasons) = finding.get("reasons").and_then(Value::as_array)
                && !reasons.is_empty()
            {
                row.insert("reasons".to_string(), Value::Array(reasons.clone()));
            }
            Value::Object(row)
        })
        .collect::<Vec<_>>();
    if !findings.is_empty() {
        out.insert("findings".to_string(), Value::Array(findings));
    }
    if full.get("truncated").and_then(Value::as_bool) == Some(true) {
        out.insert("findings_truncated".to_string(), json!(true));
    }
    if let Some(records) = full.get("records").and_then(Value::as_object) {
        out.insert(
            "records".to_string(),
            Value::Object(compact_records(records)),
        );
    }
    if let Some(reasons) = assessment
        .and_then(|a| a.get("reasons"))
        .and_then(Value::as_array)
        && !reasons.is_empty()
    {
        out.insert(
            "reasons".to_string(),
            Value::Array(reasons.iter().take(MAX_REASONS).cloned().collect()),
        );
        if reasons.len() > MAX_REASONS {
            out.insert(
                "reasons_omitted".to_string(),
                json!(reasons.len() - MAX_REASONS),
            );
        }
    }
    out
}

fn compact_records(full: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for key in ["status", "path", "key", "counts", "added", "removed"] {
        copy(full, &mut out, key);
    }
    if let Some(changed) = full.get("changed").and_then(Value::as_array) {
        let rows = changed
            .iter()
            .map(|change| {
                let mut row = Map::new();
                copy_value(change, &mut row, "key");
                if let Some(path) = change.get("subject_path") {
                    row.insert("path".to_string(), path.clone());
                }
                copy_value(change, &mut row, "fields");
                if change
                    .get("fields_omitted")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    > 0
                {
                    copy_value(change, &mut row, "fields_omitted");
                }
                Value::Object(row)
            })
            .collect();
        out.insert("changed".to_string(), Value::Array(rows));
    }
    if full.get("truncated").and_then(Value::as_bool) == Some(true) {
        out.insert("truncated".to_string(), json!(true));
    }
    if let Some(reasons) = full.get("reasons").and_then(Value::as_array)
        && !reasons.is_empty()
    {
        out.insert("reasons".to_string(), Value::Array(reasons.clone()));
    }
    out.retain(|_, value| !value.is_null());
    out
}

fn compact_finding(finding: &Value) -> Value {
    let mut row = Map::new();
    for key in [
        "kind",
        "path",
        "title",
        "severity",
        "line_range",
        "primary_span",
    ] {
        copy_value(finding, &mut row, key);
    }
    Value::Object(row)
}

fn compact_omitted(omitted: Option<&Value>) -> Vec<Value> {
    let regions = omitted
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = regions
        .iter()
        .take(MAX_OMITTED)
        .map(|region| {
            let path = region.get("path").and_then(Value::as_str).unwrap_or("");
            let shown_path = if path.is_empty() { "(root)" } else { path };
            let detail = region
                .get("detail")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    region
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            json!(format!("{shown_path}: {detail}"))
        })
        .collect::<Vec<_>>();
    if regions.len() > MAX_OMITTED {
        out.push(json!(format!(
            "+{} more omitted region(s)",
            regions.len() - MAX_OMITTED
        )));
    }
    out
}

/// Moves that cached navigation cannot express: reruns and other actions
/// outside the stored payload, as argv arrays (never shell strings).
fn compact_moves(actions: Option<&Value>) -> Vec<Value> {
    actions
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|action| {
            action.get("scope").and_then(Value::as_str) != Some("cached_evidence")
                && action.get("argv").is_some_and(Value::is_array)
        })
        .take(3)
        .map(|action| {
            let mut row = Map::new();
            copy_value(action, &mut row, "argv");
            copy_value(action, &mut row, "reason");
            copy_value(action, &mut row, "exactness");
            Value::Object(row)
        })
        .collect()
}

fn disclosure_caveats(full: &Map<String, Value>) -> Vec<Value> {
    let mut caveats = Vec::new();
    let observation = full.get("observation");
    if let Some(capture) = observation.and_then(|o| o.get("capture")) {
        let stop = capture
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !matches!(stop, "complete" | "derivation_windowed" | "redacted" | "") {
            caveats.push(json!(format!("capture incomplete: {stop}")));
        }
        if let (Some(total), Some(captured)) = (
            capture.get("total_bytes").and_then(Value::as_u64),
            capture.get("captured_bytes").and_then(Value::as_u64),
        ) && captured < total
        {
            caveats.push(json!(format!("captured {captured} of {total} bytes")));
        }
    }
    if let Some(safety) = observation.and_then(|o| o.get("safety")) {
        let redacted = safety
            .get("redacted_paths")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if redacted > 0 {
            caveats.push(json!(format!(
                "{redacted} value(s) redacted before storage"
            )));
        }
    }
    if let Some(freshness) = observation.and_then(|o| o.get("freshness"))
        && freshness.get("stale").and_then(Value::as_bool) == Some(true)
    {
        let age = freshness
            .get("age_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        caveats.push(json!(format!(
            "stale: captured {age}s ago; rerun to refresh"
        )));
    }
    if let Some(status) = full.pointer_str("/cache/status")
        && status == "skipped"
    {
        caveats.push(json!("not cached: the cursor cannot be expanded later"));
    }
    if let Some(warnings) = full.get("warnings").and_then(Value::as_array) {
        caveats.extend(warnings.iter().cloned());
    }
    caveats
}

fn insert_warnings(full: &Map<String, Value>, out: &mut Map<String, Value>) {
    if let Some(warnings) = full.get("warnings").and_then(Value::as_array)
        && !warnings.is_empty()
    {
        out.insert("warnings".to_string(), Value::Array(warnings.clone()));
    }
}

fn copy(from: &Map<String, Value>, to: &mut Map<String, Value>, key: &str) {
    if let Some(value) = from.get(key) {
        to.insert(key.to_string(), value.clone());
    }
}

fn copy_value(from: &Value, to: &mut Map<String, Value>, key: &str) {
    if let Some(value) = from.get(key).filter(|value| !value.is_null()) {
        to.insert(key.to_string(), value.clone());
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

trait PointerStr {
    fn pointer_str(&self, pointer: &str) -> Option<&str>;
}

impl PointerStr for Map<String, Value> {
    fn pointer_str(&self, pointer: &str) -> Option<&str> {
        let (first, rest) = pointer
            .trim_start_matches('/')
            .split_once('/')
            .unwrap_or((pointer.trim_start_matches('/'), ""));
        let value = self.get(first)?;
        if rest.is_empty() {
            value.as_str()
        } else {
            value.pointer(&format!("/{rest}"))?.as_str()
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn envelope() -> Value {
        json!({
            "schema": "prog.disclosure",
            "source_id": "observe",
            "summary": {"kind": "array", "item_count": 300, "payload_bytes": 595281},
            "disclosure_verdict": {"result": "bounded_win"},
            "data_preview": [{"n": 1}, {"n": 2}, {"n": 3}, {"n": 4}, {"n": 5}],
            "overview": {
                "path": "",
                "items": 300,
                "item_kinds": {"object": 300},
                "fields": [
                    {"field": "/state", "kinds": ["string"], "present": 300, "values": [{"value": "open", "count": 197}, {"value": "closed", "count": 103}]},
                    {"field": "/number", "kinds": ["integer"], "present": 300, "min": 1, "max": 300},
                    {"field": "/milestone", "kinds": ["null", "object"], "present": 12}
                ],
                "fields_omitted": 0,
                "key_candidates": ["/number"]
            },
            "omitted": [{"path": "", "reason": "long_array", "detail": "300 items, showing 5"}],
            "findings": [{"occurrence_id": "fo_1", "fingerprint": "sha256:x", "rank": 1, "kind": "log_error", "path": "/110/body", "confidence": 0.7, "reason": "r", "title": "log error", "severity": "error", "commands": {"available": ["expand"]}}],
            "cursor": "pc1_abc",
            "next_actions": [
                {"kind": "expand", "path": "", "scope": "cached_evidence"},
                {"kind": "rerun", "argv": ["cargo", "test", "x"], "scope": "target_test", "exactness": "exact"}
            ],
            "action_templates": {"expand": {"argv": ["prog"], "scope": "cached_evidence", "exactness": "exact"}},
            "provenance": {"source_call_id": "x"},
            "observation": {
                "observation_id": "obs_1",
                "completeness": {"status": "truncated", "root_path": ""},
                "freshness": {"stale": true, "age_seconds": 90000},
                "safety": {"redacted_paths": 2},
                "capture": {"total_bytes": 10, "captured_bytes": 8, "stop_reason": "byte_limit"}
            },
            "warnings": ["redacted 2 sensitive value(s) before persistence"],
            "disclosure_budget": {"actual_bytes": 1},
            "capture_budget": {},
            "storage_budget": {}
        })
    }

    #[test]
    fn compact_envelope_keeps_data_moves_and_every_caveat() {
        let compact = compact_view(&envelope());
        assert_eq!(compact["cursor"], "pc1_abc");
        assert_eq!(compact["observation_id"], "obs_1");
        assert_eq!(compact["status"], "truncated");
        assert_eq!(
            compact["overview"]["fields"]["/state"],
            "string; open=197, closed=103"
        );
        assert_eq!(compact["overview"]["fields"]["/number"], "integer; 1..300");
        assert_eq!(
            compact["overview"]["fields"]["/milestone"],
            "null|object (12/300)"
        );
        assert_eq!(compact["overview"]["keys"], json!(["/number"]));
        assert_eq!(compact["data"], json!([{"n": 1}, {"n": 2}]));
        let omitted = compact["omitted"].as_array().unwrap();
        assert_eq!(
            omitted[0],
            "(root): compact view shows 2 of the 5 previewed items; see overview"
        );
        assert_eq!(omitted[1], "(root): 300 items, showing 5");
        assert_eq!(
            compact["findings"][0],
            json!({"kind": "log_error", "path": "/110/body", "title": "log error", "severity": "error"})
        );
        assert_eq!(
            compact["next"],
            json!([{"argv": ["cargo", "test", "x"], "exactness": "exact"}])
        );
        let caveats = compact["caveats"].as_array().unwrap();
        for expected in [
            "capture incomplete: byte_limit",
            "captured 8 of 10 bytes",
            "2 value(s) redacted before storage",
            "stale: captured 90000s ago; rerun to refresh",
            "redacted 2 sensitive value(s) before persistence",
        ] {
            assert!(
                caveats.contains(&json!(expected)),
                "missing caveat {expected}"
            );
        }
        for dropped in [
            "disclosure_verdict",
            "action_templates",
            "provenance",
            "disclosure_budget",
        ] {
            assert!(
                compact.get(dropped).is_none(),
                "{dropped} should be dropped"
            );
        }
    }

    #[test]
    fn compact_view_is_much_smaller_than_the_full_envelope() {
        let full = envelope();
        let compact = compact_view(&full);
        assert!(compact.to_string().len() * 2 < full.to_string().len());
    }

    #[test]
    fn compact_run_envelope_drops_the_interleaved_copy() {
        let full = json!({
            "schema": "prog.disclosure",
            "data_preview": {"format": "run", "combined": [{"text": "x"}], "stdout": {"text": "x"}},
            "observation": {"completeness": {"status": "complete", "root_path": ""}}
        });
        let compact = compact_view(&full);
        assert!(compact["data"].get("combined").is_none());
        assert_eq!(compact["data"]["stdout"]["text"], "x");
    }

    #[test]
    fn compact_delta_keeps_proof_status_reasons_and_records() {
        let full = json!({
            "schema": "prog.observation_delta",
            "baseline_observation_id": "obs_a",
            "subject_observation_id": "obs_b",
            "assessment": {"can_prove_absence": false, "reasons": ["subject capture stopped: timeout"]},
            "findings": [{"status": "unknown", "fingerprint": "sha256:x", "title": "t", "baseline_path": "/1", "subject_path": null, "evidence_ref": {"schema": "prog.evidence_ref"}}],
            "counts": {"unknown": 1},
            "truncated": false,
            "records": {"status": "compared", "path": "", "key": "/id", "counts": {"added": 1, "removed": 0, "changed": 0, "unchanged": 2}, "added": [{"key": 3, "path": "/2"}], "truncated": false, "reasons": ["captured collections only"]}
        });
        let compact = compact_view(&full);
        assert_eq!(compact["can_prove_absence"], false);
        assert_eq!(
            compact["findings"][0],
            json!({"status": "unknown", "title": "t", "path": "/1"})
        );
        assert_eq!(
            compact["reasons"],
            json!(["subject capture stopped: timeout"])
        );
        assert_eq!(
            compact["records"]["added"],
            json!([{"key": 3, "path": "/2"}])
        );
        assert_eq!(
            compact["records"]["reasons"],
            json!(["captured collections only"])
        );
    }

    #[test]
    fn unknown_schemas_pass_through_without_transport_budgets() {
        let full = json!({"schema": "prog.status", "ready": false, "disclosure_budget": {}, "storage_budget": {}});
        assert_eq!(
            compact_view(&full),
            json!({"schema": "prog.status", "ready": false})
        );
    }

    #[test]
    fn view_names_parse() {
        assert_eq!(ResponseView::parse("Compact"), Some(ResponseView::Compact));
        assert_eq!(ResponseView::parse("full"), Some(ResponseView::Full));
        assert_eq!(ResponseView::parse("tiny"), None);
    }
}

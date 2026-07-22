//! Shared bounded-parsing helpers used by more than one coding provider.

use std::path::Path;

use serde_json::Value;

use crate::{
    Extra, LineRange, SourceSpan, SourceSpanExactness, findings::normalize_workspace_path,
};

/// Hard bound on structured items (tests, diagnostics, test events) a single
/// provider call will normalize, so a pathological input cannot make
/// normalization unbounded work.
pub(super) const MAX_PROVIDER_ITEMS: usize = 500;

/// Hard bound on lines scanned from a captured text blob, whether parsed as
/// newline-delimited JSON or scanned for plain-text summary lines.
pub(super) const MAX_PROVIDER_TEXT_LINES: usize = 2_000;

/// Parse up to [`MAX_PROVIDER_TEXT_LINES`] newline-delimited JSON values from
/// `text`. Non-JSON or unparsable lines are skipped rather than treated as a
/// hard failure: a provider degrades to whatever lines *did* parse instead of
/// discarding the whole capture.
pub(super) fn parse_ndjson(text: &str) -> Vec<Value> {
    text.lines()
        .take(MAX_PROVIDER_TEXT_LINES)
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                None
            } else {
                serde_json::from_str::<Value>(trimmed).ok()
            }
        })
        .collect()
}

pub(super) fn line_range(start: u64, end: u64) -> LineRange {
    LineRange {
        start,
        end,
        extra: Extra::new(),
    }
}

/// Build a single-point (or range, when `column` is known) primary span from
/// a bare `path`/`line`/`column` location a provider extracted from
/// structured JSON or a panic message. Reuses the same workspace-relative
/// normalization (and fail-closed absolute-path handling) as the generic span
/// extractor so provider spans and generic spans agree on what counts as a
/// safe, addressable workspace path.
pub(super) fn point_span(
    raw_path: &str,
    line: u64,
    column: Option<u64>,
    origin: &'static str,
    workspace_root: Option<&Path>,
) -> Option<SourceSpan> {
    if line == 0 {
        return None;
    }
    let path = normalize_workspace_path(raw_path, workspace_root)?;
    Some(SourceSpan {
        path: Some(path),
        uri: None,
        start_line: line,
        start_column: column,
        end_line: None,
        end_column: None,
        role: "primary".to_string(),
        label: None,
        origin: origin.to_string(),
        exactness: if column.is_some() {
            SourceSpanExactness::Range
        } else {
            SourceSpanExactness::Approximate
        },
        redaction_state: None,
        extra: Extra::new(),
    })
}

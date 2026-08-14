//! Bounded, pure normalization for the first coding-loop providers.
//!
//! Providers add deterministic structure to an already captured command. They
//! never execute a tool and their failure never replaces the original bytes.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::SelectionCoverage;

const MAX_PROVIDER_BYTES: usize = 1024 * 1024;
const MAX_PROVIDER_LINES: usize = 10_000;
const MAX_PROVIDER_ITEMS: usize = 512;
const MAX_PROVIDER_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_MESSAGE_CHARS: usize = 2_048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CodingProviderLimits {
    pub max_input_bytes: u64,
    pub max_lines: u64,
    pub max_items: u64,
    pub max_output_bytes: u64,
    pub input_bytes: u64,
    pub lines_examined: u64,
    pub items_emitted: u64,
    pub output_bytes: u64,
    pub bound_hit: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CodingProviderResult {
    pub schema: String,
    pub provider: String,
    pub input_format: String,
    pub match_confidence: f64,
    pub complete: bool,
    pub selection: SelectionCoverage,
    pub limits: CodingProviderLimits,
    pub normalized: Value,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy)]
struct InputLine<'a> {
    stream: &'static str,
    number: usize,
    text: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct NormalizedTest {
    node_id: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    evidence_stream: &'static str,
    evidence_line: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct NormalizedSpan {
    path: String,
    line: u64,
    column: u64,
    primary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct NormalizedDiagnostic {
    severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic_code: Option<String>,
    message: String,
    spans: Vec<NormalizedSpan>,
    evidence_stream: &'static str,
    evidence_line: u64,
}

/// Normalize captured output for the two deliberately supported provider
/// families. The interface is pure over argv and captured text.
pub fn normalize_coding_output(
    argv: &[String],
    stdout: &str,
    stderr: &str,
    capture_complete: bool,
) -> Option<CodingProviderResult> {
    let (lines, input_bytes, bound_hit) = bounded_lines(stdout, stderr);
    match provider_kind(argv)? {
        ProviderKind::Pytest { args } => Some(normalize_pytest(
            args,
            &lines,
            input_bytes,
            bound_hit,
            capture_complete,
        )),
        ProviderKind::CargoRust { program, args } => Some(normalize_cargo_rust(
            program,
            args,
            &lines,
            input_bytes,
            bound_hit,
            capture_complete,
        )),
    }
}

enum ProviderKind<'a> {
    Pytest {
        args: &'a [String],
    },
    CargoRust {
        program: &'a str,
        args: &'a [String],
    },
}

fn provider_kind(argv: &[String]) -> Option<ProviderKind<'_>> {
    let program = argv
        .first()
        .and_then(|value| Path::new(value).file_name())?
        .to_str()?;
    if matches!(program, "pytest" | "py.test") {
        return Some(ProviderKind::Pytest { args: &argv[1..] });
    }
    if matches!(program, "python" | "python3")
        && argv.get(1).map(String::as_str) == Some("-m")
        && argv.get(2).map(String::as_str) == Some("pytest")
    {
        return Some(ProviderKind::Pytest { args: &argv[3..] });
    }
    if program == "rustc"
        || program == "cargo"
            && cargo_subcommand(&argv[1..]).is_some_and(|(_, subcommand)| {
                matches!(
                    subcommand,
                    "bench" | "build" | "check" | "clippy" | "rustc" | "test"
                )
            })
    {
        Some(ProviderKind::CargoRust {
            program,
            args: &argv[1..],
        })
    } else {
        None
    }
}

fn cargo_subcommand(args: &[String]) -> Option<(usize, &str)> {
    let index = usize::from(args.first()?.starts_with('+'));
    args.get(index).map(|value| (index, value.as_str()))
}

fn bounded_lines<'a>(stdout: &'a str, stderr: &'a str) -> (Vec<InputLine<'a>>, usize, bool) {
    let mut lines = Vec::new();
    let mut examined_bytes = 0usize;
    let mut bound_hit = stdout.len().saturating_add(stderr.len()) > MAX_PROVIDER_BYTES;
    'streams: for (stream, text) in [("stdout", stdout), ("stderr", stderr)] {
        let remaining = MAX_PROVIDER_BYTES.saturating_sub(examined_bytes);
        if remaining == 0 {
            bound_hit |= !text.is_empty();
            break;
        }
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let mut bounded = &text[..end];
        if end < text.len() && !bounded.ends_with('\n') {
            bounded = bounded
                .rfind('\n')
                .map_or("", |newline| &bounded[..=newline]);
        }
        examined_bytes = examined_bytes.saturating_add(bounded.len());
        for (index, line) in bounded.lines().enumerate() {
            if lines.len() >= MAX_PROVIDER_LINES {
                bound_hit = true;
                break 'streams;
            }
            lines.push(InputLine {
                stream,
                number: index + 1,
                text: line,
            });
        }
    }
    (lines, stdout.len().saturating_add(stderr.len()), bound_hit)
}

fn normalize_pytest(
    args: &[String],
    lines: &[InputLine<'_>],
    input_bytes: usize,
    mut bound_hit: bool,
    capture_complete: bool,
) -> CodingProviderResult {
    let mut tests = Vec::new();
    let mut summary_seen = false;
    let mut early_terminated = pytest_early_stop_args(args);
    for line in lines {
        let trimmed = line.text.trim();
        let lower = trimmed.to_ascii_lowercase();
        summary_seen |= pytest_summary_line(&lower);
        early_terminated |= lower.contains("stopping after")
            || lower.contains("interrupted:")
            || lower.contains("keyboardinterrupt");
        if let Some((node_id, status, message)) = pytest_test_line(trimmed) {
            if tests.len() >= MAX_PROVIDER_ITEMS {
                bound_hit = true;
                continue;
            }
            tests.push(NormalizedTest {
                node_id,
                status,
                message,
                evidence_stream: line.stream,
                evidence_line: line.number.try_into().unwrap_or(u64::MAX),
            });
        }
    }
    tests.sort();
    tests.dedup_by(|left, right| left.node_id == right.node_id && left.status == right.status);
    let targets = pytest_targets(args);
    let complete = capture_complete && !bound_hit && summary_seen && !early_terminated;
    let selection = SelectionCoverage {
        scopes: if targets.is_empty() {
            vec!["pytest:all".to_string()]
        } else {
            targets
                .iter()
                .map(|target| format!("pytest:{target}"))
                .collect()
        },
        exhaustive: complete,
        ..SelectionCoverage::default()
    };
    let mut warnings = Vec::new();
    if !summary_seen {
        warnings.push("pytest completion summary was not observed".to_string());
    }
    if early_terminated {
        warnings.push("pytest selection stopped early; broader absence is unprovable".to_string());
    }
    provider_result(
        "pytest.v1",
        "pytest_text",
        if tests.is_empty() { 0.72 } else { 0.96 },
        complete,
        selection,
        input_bytes,
        lines.len(),
        tests.len(),
        bound_hit,
        json!({
            "tests": tests,
            "summary_seen": summary_seen,
            "early_terminated": early_terminated,
            "targets": targets
        }),
        warnings,
    )
}

fn pytest_test_line(line: &str) -> Option<(String, String, Option<String>)> {
    for (prefix, status) in [
        ("FAILED ", "failed"),
        ("PASSED ", "passed"),
        ("ERROR ", "error"),
        ("SKIPPED ", "skipped"),
        ("XFAIL ", "xfail"),
        ("XPASS ", "xpass"),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let (node, message) = rest
                .split_once(" - ")
                .map_or((rest, None), |(node, message)| (node, Some(message)));
            let node = node.split_whitespace().next()?;
            if is_pytest_node_id(node) {
                return Some((
                    bounded_text(node),
                    status.to_string(),
                    message.map(bounded_text),
                ));
            }
        }
    }
    let node = line.split_whitespace().next()?;
    if !is_pytest_node_id(node) {
        return None;
    }
    let upper = line.to_ascii_uppercase();
    let status = if upper.contains(" FAILED") {
        "failed"
    } else if upper.contains(" PASSED") {
        "passed"
    } else if upper.contains(" SKIPPED") {
        "skipped"
    } else if upper.contains(" ERROR") {
        "error"
    } else {
        return None;
    };
    Some((bounded_text(node), status.to_string(), None))
}

fn is_pytest_node_id(value: &str) -> bool {
    value.contains(".py::") && !value.contains(char::is_whitespace)
}

fn pytest_summary_line(lower: &str) -> bool {
    (lower.contains(" passed")
        || lower.contains(" failed")
        || lower.contains(" error")
        || lower.contains(" deselected")
        || lower.contains("no tests ran"))
        && lower.contains(" in ")
}

fn pytest_early_stop_args(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "-x"
            || arg == "--exitfirst"
            || arg == "--lf"
            || arg == "--ff"
            || arg == "--sw"
            || arg.starts_with("--maxfail")
    })
}

fn pytest_targets(args: &[String]) -> Vec<String> {
    let mut targets = args
        .iter()
        .filter(|arg| !arg.starts_with('-') && (arg.contains(".py") || arg.contains("::")))
        .map(|arg| bounded_text(arg))
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    targets
}

fn normalize_cargo_rust(
    program: &str,
    args: &[String],
    lines: &[InputLine<'_>],
    input_bytes: usize,
    mut bound_hit: bool,
    capture_complete: bool,
) -> CodingProviderResult {
    let mut diagnostics = Vec::new();
    let mut tests = Vec::new();
    let mut malformed_structured = false;
    let mut structured_seen = false;
    let mut test_summary_seen = false;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.text.trim();
        if trimmed.starts_with('{') {
            match serde_json::from_str::<Value>(trimmed) {
                Ok(value) => {
                    structured_seen |=
                        value
                            .get("reason")
                            .and_then(Value::as_str)
                            .is_some_and(|reason| {
                                matches!(
                                    reason,
                                    "build-finished"
                                        | "build-script-executed"
                                        | "compiler-artifact"
                                        | "compiler-message"
                                )
                            });
                    if let Some(diagnostic) = rust_json_diagnostic(&value, line) {
                        if diagnostics.len() < MAX_PROVIDER_ITEMS {
                            diagnostics.push(diagnostic);
                        } else {
                            bound_hit = true;
                        }
                    }
                }
                Err(_) => malformed_structured = true,
            }
        }
        if let Some(test) = libtest_line(trimmed, line) {
            if tests.len() < MAX_PROVIDER_ITEMS {
                tests.push(test);
            } else {
                bound_hit = true;
            }
        }
        test_summary_seen |= trimmed.starts_with("test result:");
        if let Some(diagnostic) = rust_text_diagnostic(lines, index) {
            if diagnostics.len() < MAX_PROVIDER_ITEMS {
                diagnostics.push(diagnostic);
            } else {
                bound_hit = true;
            }
        }
    }
    diagnostics.sort();
    diagnostics.dedup();
    tests.sort();
    tests.dedup_by(|left, right| left.node_id == right.node_id && left.status == right.status);

    let test_invocation = program == "cargo"
        && cargo_subcommand(args).is_some_and(|(_, subcommand)| subcommand == "test");
    let single_harness = program == "rustc"
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--lib" | "--test" | "--bin"));
    let parser_complete = if test_invocation {
        !tests.is_empty() && test_summary_seen && single_harness
    } else {
        program == "rustc" || structured_seen || !diagnostics.is_empty()
    };
    let complete = capture_complete && !bound_hit && !malformed_structured && parser_complete;
    let input_format = if structured_seen {
        "cargo_rustc_json"
    } else if !tests.is_empty() {
        "cargo_libtest_text"
    } else {
        "cargo_rustc_text"
    };
    let selection = SelectionCoverage {
        scopes: cargo_rust_scopes(program, args),
        exhaustive: complete,
        ..SelectionCoverage::default()
    };
    let mut warnings = Vec::new();
    if malformed_structured {
        warnings.push(
            "malformed JSON-looking Cargo/rustc output fell back to retained text evidence"
                .to_string(),
        );
    }
    if !tests.is_empty() && !single_harness {
        warnings.push(
            "Cargo may run multiple test harnesses; broader absence is unprovable without an exact harness target"
                .to_string(),
        );
    }
    provider_result(
        "cargo_rustc.v1",
        input_format,
        if structured_seen { 0.99 } else { 0.9 },
        complete,
        selection,
        input_bytes,
        lines.len(),
        diagnostics.len().saturating_add(tests.len()),
        bound_hit,
        json!({
            "diagnostics": diagnostics,
            "tests": tests,
            "structured_seen": structured_seen,
            "test_summary_seen": test_summary_seen,
            "single_harness": single_harness
        }),
        warnings,
    )
}

fn cargo_rust_scopes(program: &str, args: &[String]) -> Vec<String> {
    if program == "rustc" {
        return vec![format!(
            "rustc:{}",
            args.iter()
                .find(|arg| !arg.starts_with('-'))
                .map_or("invocation", String::as_str)
        )];
    }
    let subcommand = cargo_subcommand(args).map_or("invocation", |(_, subcommand)| subcommand);
    let mut targets = args
        .windows(2)
        .filter_map(|pair| match pair[0].as_str() {
            "--bin" | "--example" | "--package" | "-p" | "--test" => {
                Some(format!("{}={}", pair[0], bounded_text(&pair[1])))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for flag in ["--all-targets", "--bins", "--examples", "--lib", "--tests"] {
        if args.iter().any(|arg| arg == flag) {
            targets.push(flag.to_string());
        }
    }
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        vec![format!("cargo:{subcommand}:default")]
    } else {
        targets
            .into_iter()
            .map(|target| format!("cargo:{subcommand}:{target}"))
            .collect()
    }
}

fn rust_json_diagnostic(value: &Value, line: &InputLine<'_>) -> Option<NormalizedDiagnostic> {
    let message = value.get("message").filter(|message| message.is_object())?;
    let severity = message.get("level")?.as_str()?;
    if !matches!(severity, "error" | "warning") {
        return None;
    }
    let diagnostic_code = message
        .get("code")
        .and_then(|code| code.get("code"))
        .and_then(Value::as_str)
        .map(bounded_text);
    let text = message.get("message")?.as_str().map(bounded_text)?;
    let mut spans = message
        .get("spans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|span| {
            Some(NormalizedSpan {
                path: bounded_text(span.get("file_name")?.as_str()?),
                line: span.get("line_start")?.as_u64()?,
                column: span.get("column_start")?.as_u64()?,
                primary: span
                    .get("is_primary")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                label: span.get("label").and_then(Value::as_str).map(bounded_text),
            })
        })
        .take(32)
        .collect::<Vec<_>>();
    spans.sort();
    Some(NormalizedDiagnostic {
        severity: severity.to_string(),
        diagnostic_code,
        message: text,
        spans,
        evidence_stream: line.stream,
        evidence_line: line.number.try_into().unwrap_or(u64::MAX),
    })
}

fn rust_text_diagnostic(lines: &[InputLine<'_>], index: usize) -> Option<NormalizedDiagnostic> {
    let line = lines[index];
    let trimmed = line.text.trim();
    let (severity, rest) = trimmed
        .strip_prefix("error")
        .map(|rest| ("error", rest))
        .or_else(|| {
            trimmed
                .strip_prefix("warning")
                .map(|rest| ("warning", rest))
        })?;
    let (diagnostic_code, rest) = if let Some(rest) = rest.strip_prefix('[') {
        let (code, rest) = rest.split_once(']')?;
        (Some(bounded_text(code)), rest)
    } else {
        (None, rest)
    };
    let message = rest.strip_prefix(':')?.trim();
    if message.is_empty() {
        return None;
    }
    let spans = lines
        .iter()
        .skip(index + 1)
        .take(4)
        .find_map(|candidate| parse_rust_arrow(candidate.text))
        .into_iter()
        .collect();
    Some(NormalizedDiagnostic {
        severity: severity.to_string(),
        diagnostic_code,
        message: bounded_text(message),
        spans,
        evidence_stream: line.stream,
        evidence_line: line.number.try_into().unwrap_or(u64::MAX),
    })
}

fn parse_rust_arrow(line: &str) -> Option<NormalizedSpan> {
    let location = line.trim().strip_prefix("-->")?.trim();
    let (prefix, column) = location.rsplit_once(':')?;
    let (path, line) = prefix.rsplit_once(':')?;
    Some(NormalizedSpan {
        path: bounded_text(path),
        line: line.parse().ok()?,
        column: column.parse().ok()?,
        primary: true,
        label: None,
    })
}

fn libtest_line(line: &str, input: &InputLine<'_>) -> Option<NormalizedTest> {
    let rest = line.strip_prefix("test ")?;
    let (node_id, status) = rest.rsplit_once(" ... ")?;
    let status = match status.trim() {
        "ok" => "passed",
        "FAILED" => "failed",
        "ignored" => "skipped",
        _ => return None,
    };
    Some(NormalizedTest {
        node_id: bounded_text(node_id),
        status: status.to_string(),
        message: None,
        evidence_stream: input.stream,
        evidence_line: input.number.try_into().unwrap_or(u64::MAX),
    })
}

#[allow(clippy::too_many_arguments)]
fn provider_result(
    provider: &str,
    input_format: &str,
    match_confidence: f64,
    complete: bool,
    mut selection: SelectionCoverage,
    input_bytes: usize,
    lines_examined: usize,
    items_emitted: usize,
    bound_hit: bool,
    normalized: Value,
    mut warnings: Vec<String>,
) -> CodingProviderResult {
    let (normalized, output_bytes, output_bound_hit, emitted) = bound_normalized_output(normalized);
    let bound_hit = bound_hit || output_bound_hit;
    selection.exhaustive &= !bound_hit;
    if bound_hit {
        warnings.push("provider work or output limit was reached".to_string());
    }
    CodingProviderResult {
        schema: "prog.coding_provider".to_string(),
        provider: provider.to_string(),
        input_format: input_format.to_string(),
        match_confidence,
        complete: complete && !bound_hit,
        selection,
        limits: CodingProviderLimits {
            max_input_bytes: MAX_PROVIDER_BYTES as u64,
            max_lines: MAX_PROVIDER_LINES as u64,
            max_items: MAX_PROVIDER_ITEMS as u64,
            max_output_bytes: MAX_PROVIDER_OUTPUT_BYTES as u64,
            input_bytes: input_bytes.try_into().unwrap_or(u64::MAX),
            lines_examined: lines_examined.try_into().unwrap_or(u64::MAX),
            items_emitted: items_emitted.min(emitted).try_into().unwrap_or(u64::MAX),
            output_bytes: output_bytes.try_into().unwrap_or(u64::MAX),
            bound_hit,
        },
        normalized,
        warnings,
    }
}

fn bound_normalized_output(mut normalized: Value) -> (Value, usize, bool, usize) {
    let mut pending = Vec::new();
    {
        let Some(map) = normalized.as_object_mut() else {
            let bytes = serde_json::to_vec(&normalized).map_or(0, |encoded| encoded.len());
            return (normalized, bytes, bytes > MAX_PROVIDER_OUTPUT_BYTES, 0);
        };
        for key in ["diagnostics", "tests"] {
            if let Some(items) = map.get_mut(key).and_then(Value::as_array_mut) {
                pending.push((key.to_string(), std::mem::take(items)));
            }
        }
    }

    let mut output_bytes = serde_json::to_vec(&normalized).map_or(0, |encoded| encoded.len());
    let mut emitted = 0usize;
    let mut bound_hit = false;
    for (key, items) in pending {
        let mut kept = Vec::new();
        for item in items {
            let item_bytes = serde_json::to_vec(&item).map_or(MAX_PROVIDER_OUTPUT_BYTES, |value| {
                value.len().saturating_add(1)
            });
            if emitted >= MAX_PROVIDER_ITEMS
                || output_bytes.saturating_add(item_bytes) > MAX_PROVIDER_OUTPUT_BYTES
            {
                bound_hit = true;
                continue;
            }
            output_bytes = output_bytes.saturating_add(item_bytes);
            emitted += 1;
            kept.push(item);
        }
        normalized
            .as_object_mut()
            .expect("provider normalized output stays an object")
            .insert(key, Value::Array(kept));
    }
    output_bytes = serde_json::to_vec(&normalized).map_or(output_bytes, |encoded| encoded.len());
    (normalized, output_bytes, bound_hit, emitted)
}

fn bounded_text(value: &str) -> String {
    if value.chars().count() <= MAX_MESSAGE_CHARS {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(MAX_MESSAGE_CHARS.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn pytest_identity_survives_reordering_unicode_and_line_shifts() {
        let argv = strings(&["pytest", "-q"]);
        let first = normalize_coding_output(
            &argv,
            "FAILED tests/test_api.py::test_total[€] - AssertionError: nope\n1 failed in 0.1s\n",
            "",
            true,
        )
        .unwrap();
        let shifted = normalize_coding_output(
            &argv,
            "noise\nnoise\nFAILED tests/test_api.py::test_total[€] - AssertionError: nope\n1 failed in 0.2s\n",
            "",
            true,
        )
        .unwrap();
        assert!(first.complete);
        assert_eq!(
            first.normalized["tests"][0]["node_id"],
            shifted.normalized["tests"][0]["node_id"]
        );
        assert_eq!(
            first.normalized["tests"][0]["message"],
            shifted.normalized["tests"][0]["message"]
        );
        assert_ne!(
            first.normalized["tests"][0]["evidence_line"],
            shifted.normalized["tests"][0]["evidence_line"]
        );
    }

    #[test]
    fn targeted_early_or_truncated_pytest_is_never_complete() {
        for (argv, capture_complete) in [
            (strings(&["pytest", "tests/test_api.py::test_total"]), true),
            (strings(&["pytest", "-x"]), true),
            (strings(&["pytest"]), false),
        ] {
            let result =
                normalize_coding_output(&argv, "1 passed in 0.1s\n", "", capture_complete).unwrap();
            if argv.iter().any(|arg| arg == "-x") || !capture_complete {
                assert!(!result.complete);
            }
            if argv.iter().any(|arg| arg.contains("::")) {
                assert_eq!(
                    result.selection.scopes[0],
                    "pytest:tests/test_api.py::test_total"
                );
            }
        }
    }

    #[test]
    fn structured_and_text_rust_diagnostics_share_identity_components() {
        let argv = strings(&["cargo", "check", "--message-format=json"]);
        let structured = normalize_coding_output(
            &argv,
            r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","code":{"code":"E0308"},"spans":[{"file_name":"src/lib.rs","line_start":20,"column_start":5,"is_primary":true,"label":"expected u8"},{"file_name":"src/lib.rs","line_start":7,"column_start":1,"is_primary":false,"label":"defined here"}]}}"#,
            "",
            true,
        )
        .unwrap();
        let text = normalize_coding_output(
            &strings(&["rustc", "src/lib.rs"]),
            "",
            "error[E0308]: mismatched types\n  --> src/lib.rs:99:5\n",
            true,
        )
        .unwrap();
        let structured = &structured.normalized["diagnostics"][0];
        let text = &text.normalized["diagnostics"][0];
        assert_eq!(structured["diagnostic_code"], text["diagnostic_code"]);
        assert_eq!(structured["message"], text["message"]);
        assert_eq!(structured["spans"].as_array().unwrap().len(), 2);
        assert_ne!(structured["spans"][0]["line"], text["spans"][0]["line"]);
    }

    #[test]
    fn malformed_structured_output_falls_back_without_claiming_completeness() {
        let result = normalize_coding_output(
            &strings(&["cargo", "test", "--lib"]),
            "{malformed}\ntest module::case ... FAILED\ntest result: FAILED. 0 passed; 1 failed\n",
            "",
            true,
        )
        .unwrap();
        assert!(!result.complete);
        assert_eq!(result.normalized["tests"][0]["node_id"], "module::case");
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("malformed"))
        );
    }

    #[test]
    fn unknown_commands_do_not_claim_a_provider() {
        assert!(
            normalize_coding_output(&strings(&["custom", "test"]), "error", "", true).is_none()
        );
    }

    #[test]
    fn very_long_line_and_item_flood_hit_bounds_without_partial_parsing() {
        let long_line = "x".repeat(MAX_PROVIDER_BYTES + 32);
        let long = normalize_coding_output(&strings(&["pytest"]), &long_line, "", true).unwrap();
        assert!(long.limits.bound_hit);
        assert_eq!(long.limits.lines_examined, 0);
        assert!(!long.complete);

        let mut flood = (0..MAX_PROVIDER_ITEMS + 20)
            .map(|index| format!("tests/test_many.py::test_{index} PASSED"))
            .collect::<Vec<_>>()
            .join("\n");
        flood.push_str("\n500 passed in 1.0s\n");
        let flooded = normalize_coding_output(&strings(&["pytest"]), &flood, "", true).unwrap();
        assert!(flooded.limits.bound_hit);
        assert_eq!(flooded.limits.items_emitted, MAX_PROVIDER_ITEMS as u64);
        assert!(!flooded.complete);
        assert!(!flooded.selection.exhaustive);
    }

    proptest! {
        #[test]
        fn normalization_is_deterministic_and_bounded_for_arbitrary_text(
            fragments in prop::collection::vec(".{0,80}", 0..500),
            cargo in any::<bool>(),
            capture_complete in any::<bool>(),
        ) {
            let text = fragments.join("\n");
            let argv = if cargo {
                strings(&["cargo", "check", "--message-format=json"])
            } else {
                strings(&["pytest", "-q"])
            };
            let first = normalize_coding_output(&argv, &text, "", capture_complete).unwrap();
            let second = normalize_coding_output(&argv, &text, "", capture_complete).unwrap();
            prop_assert_eq!(&first, &second);
            prop_assert!(first.limits.lines_examined <= first.limits.max_lines);
            prop_assert!(first.limits.items_emitted <= first.limits.max_items);
            prop_assert!(first.limits.output_bytes <= first.limits.max_output_bytes);
        }
    }
}

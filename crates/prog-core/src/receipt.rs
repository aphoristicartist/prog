//! Compact, model-facing receipts (#288).
//!
//! A receipt is a few hundred bytes of plain text built only from data an
//! observation already persisted (after redaction) plus, when a comparable
//! predecessor exists, the same [`ObservationDelta`] that `prog delta` reports.
//! It never claims a fix the delta cannot prove.

use serde_json::Value;

use crate::{
    DeltaFindingStatus, Extra, ObservationDelta, ObservationRecord, Receipt, ReceiptVerdict,
    TestPopulation,
};

const MAX_ARGV_CHARS: usize = 48;
const MAX_NAMED_TESTS: usize = 2;
const MAX_DETAIL_CHARS: usize = 96;
/// Hard ceiling; typical receipts are well under 300 bytes.
pub const MAX_RECEIPT_BYTES: usize = 480;

/// A captured observation and its persisted payload.
pub struct ReceiptSubject<'a> {
    pub observation: &'a ObservationRecord,
    pub payload: &'a Value,
    pub cursor: Option<&'a str>,
}

/// The earlier comparable observation and the delta from it to the subject.
pub struct ReceiptBaseline<'a> {
    pub observation: &'a ObservationRecord,
    pub payload: &'a Value,
    pub delta: &'a ObservationDelta,
}

pub fn build_receipt(
    subject: ReceiptSubject<'_>,
    baseline: Option<ReceiptBaseline<'_>>,
) -> Receipt {
    let payload = subject.payload;
    let tests = TestPopulation::from_payload(payload);
    let failing_tests = failing(tests.as_ref());
    let failing_now = run_failed(payload) || !failing_tests.is_empty();

    let mut lines = vec![headline(payload, &failing_tests)];
    if failing_now && let Some(detail) = first_failure(payload) {
        lines.push(format!("first failure: {detail}"));
    }

    let mut verdict = if failing_now {
        ReceiptVerdict::Failing
    } else {
        ReceiptVerdict::Clean
    };
    let mut compared_with = None;
    if let Some(baseline) = baseline {
        compared_with = Some(baseline.observation.observation_id.clone());
        let before = TestPopulation::from_payload(baseline.payload);
        let failed_before = run_failed(baseline.payload) || !failing(before.as_ref()).is_empty();
        if let Some(line) = comparison(baseline.delta, before.as_ref(), tests.as_ref()) {
            lines.push(line);
        }
        if !failing_now && failed_before {
            verdict = if baseline.delta.assessment.can_prove_absence {
                ReceiptVerdict::Fixed
            } else {
                ReceiptVerdict::NotProven
            };
        }
    }

    lines.push(match subject.cursor {
        Some(cursor) => format!(
            "evidence: prog evidence {cursor} --path {}",
            evidence_path(payload)
        ),
        None => format!("observation: {}", subject.observation.observation_id),
    });

    Receipt {
        observation_id: subject.observation.observation_id.clone(),
        cursor: subject.cursor.map(str::to_string),
        compared_with,
        verdict,
        text: bounded(lines.join("\n"), MAX_RECEIPT_BYTES),
        extra: Extra::new(),
    }
}

fn run_failed(payload: &Value) -> bool {
    let command = payload.get("command");
    let flag = |name: &str| {
        command
            .and_then(|command| command.get(name))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    flag("timed_out")
        || flag("cancelled")
        || command
            .and_then(|command| command.get("success"))
            .and_then(Value::as_bool)
            .is_some_and(|success| !success)
}

fn failing(tests: Option<&TestPopulation>) -> Vec<String> {
    tests
        .map(|tests| {
            tests
                .tests
                .iter()
                .filter(|(_, status)| matches!(status.as_str(), "failed" | "error"))
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn headline(payload: &Value, failing_tests: &[String]) -> String {
    let command = payload.get("command");
    let argv = command
        .and_then(|command| command.get("argv"))
        .and_then(Value::as_array)
        .map(|argv| {
            argv.iter()
                .enumerate()
                .filter_map(|(index, arg)| {
                    let arg = arg.as_str()?;
                    // Show the program name, not the absolute path it resolved to.
                    Some(if index == 0 {
                        arg.rsplit('/').next().unwrap_or(arg).to_string()
                    } else {
                        arg.to_string()
                    })
                })
                .collect::<Vec<_>>()
                .join(" ")
        });
    let mut parts = vec![match argv {
        Some(argv) => format!("prog: `{}`", truncate(&argv, MAX_ARGV_CHARS)),
        None => "prog:".to_string(),
    }];
    let field = |name: &str| command.and_then(|command| command.get(name));
    if field("timed_out").and_then(Value::as_bool) == Some(true) {
        parts.push("timed out".to_string());
    } else if let Some(signal) = field("signal").and_then(Value::as_i64) {
        parts.push(format!("signal {signal}"));
    } else if let Some(code) = field("exit_code").and_then(Value::as_i64) {
        parts.push(format!("exit {code}"));
    }
    if let Some(counts) = payload.pointer("/provider/normalized/counts") {
        let count = |name: &str| counts.get(name).and_then(Value::as_u64).unwrap_or(0);
        let failed = count("failed") + count("error");
        let mut tally = format!("{} passed, {failed} failed", count("passed"));
        if count("skipped") > 0 {
            tally.push_str(&format!(", {} skipped", count("skipped")));
        }
        if !failing_tests.is_empty() {
            tally.push_str(&format!(": {}", named(failing_tests)));
        }
        parts.push(tally);
    } else if !failing_tests.is_empty() {
        parts.push(format!("failed: {}", named(failing_tests)));
    }
    let lines = ["stdout", "stderr"]
        .iter()
        .filter_map(|stream| payload.pointer(&format!("/{stream}/line_count")))
        .filter_map(Value::as_u64)
        .sum::<u64>();
    if lines > 0 {
        parts.push(format!("full output kept ({lines} lines)"));
    }
    parts.join(" · ")
}

/// The most specific description of the first failure that the payload
/// already contains: a provider test message, a Rust panic location, or the
/// first meaningful line of the first failure section.
fn first_failure(payload: &Value) -> Option<String> {
    let tests_with_messages = payload
        .pointer("/provider/normalized/tests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for test in tests_with_messages {
        if matches!(test["status"].as_str(), Some("failed" | "error"))
            && let (Some(id), Some(message)) = (test["node_id"].as_str(), test["message"].as_str())
        {
            return Some(truncate(&format!("{id}: {message}"), MAX_DETAIL_CHARS));
        }
    }
    let section_lines = payload
        .pointer("/failure_sections/0/lines")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("----"))
        .collect::<Vec<_>>();
    if let Some(panic) = section_lines
        .iter()
        .find(|line| line.contains("panicked at"))
    {
        return Some(truncate(&strip_thread_id(panic), MAX_DETAIL_CHARS));
    }
    section_lines
        .first()
        .map(|line| truncate(line, MAX_DETAIL_CHARS))
}

/// `thread 'tests::adds' (266636439) panicked at src/lib.rs:6:25:` becomes
/// `tests::adds panicked at src/lib.rs:6:25`.
fn strip_thread_id(line: &str) -> String {
    let Some(rest) = line.strip_prefix("thread '") else {
        return line.trim_end_matches(':').to_string();
    };
    let Some((name, after)) = rest.split_once('\'') else {
        return line.to_string();
    };
    let after = after.trim_start();
    let after = match after.strip_prefix('(') {
        Some(inner) => inner
            .split_once(')')
            .map_or(after, |(_, tail)| tail.trim_start()),
        None => after,
    };
    format!("{name} {}", after.trim_end_matches(':'))
}

fn comparison(
    delta: &ObservationDelta,
    before: Option<&TestPopulation>,
    after: Option<&TestPopulation>,
) -> Option<String> {
    if !delta.assessment.can_prove_absence {
        let reason = delta
            .assessment
            .reasons
            .iter()
            .find(|reason| reason.contains("test"))
            .or_else(|| delta.assessment.reasons.first())
            .map(|reason| truncate(&actionable(reason), MAX_DETAIL_CHARS))
            .unwrap_or_else(|| "comparison is not provable".to_string());
        return Some(format!("vs previous run: NOT PROVEN ({reason})"));
    }
    if let (Some(before), Some(after)) = (before, after) {
        let mut fixed = Vec::new();
        let mut still = Vec::new();
        for (test, status) in &before.tests {
            if !matches!(status.as_str(), "failed" | "error") {
                continue;
            }
            match after.tests.get(test).map(String::as_str) {
                Some("failed" | "error") => still.push(test.clone()),
                // A provable comparison with an unchanged population means
                // an unnamed (quiet) baseline failure ran again and passed.
                _ => fixed.push(test.clone()),
            }
        }
        let new: Vec<String> = after
            .tests
            .iter()
            .filter(|(test, status)| {
                matches!(status.as_str(), "failed" | "error")
                    && !matches!(
                        before.tests.get(*test).map(String::as_str),
                        Some("failed" | "error")
                    )
            })
            .map(|(test, _)| test.clone())
            .collect();
        let mut parts = Vec::new();
        if !fixed.is_empty() {
            parts.push(format!("fixed: {}", named(&fixed)));
        }
        if !still.is_empty() {
            parts.push(format!("still failing: {}", named(&still)));
        }
        if !new.is_empty() {
            parts.push(format!("new failure: {}", named(&new)));
        }
        if !parts.is_empty() {
            return Some(format!("vs previous run: {}", parts.join("; ")));
        }
    }
    let count = |status: &str| delta.counts.get(status).copied().unwrap_or(0);
    let resolved = delta
        .findings
        .iter()
        .filter(|finding| finding.status == DeltaFindingStatus::Resolved)
        .count();
    if resolved == 0 && count("new") == 0 && count("persisting") == 0 {
        return None;
    }
    Some(format!(
        "vs previous run: {} resolved, {} new, {} persisting findings",
        count("resolved"),
        count("new"),
        count("persisting")
    ))
}

fn evidence_path(payload: &Value) -> &'static str {
    if payload
        .pointer("/failure_sections/0")
        .is_some_and(|section| !section.is_null())
    {
        "/failure_sections/0"
    } else if payload.get("stdout").is_some() {
        "/stdout"
    } else {
        ""
    }
}

fn named(tests: &[String]) -> String {
    let shown = tests
        .iter()
        .take(MAX_NAMED_TESTS)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match tests.len().saturating_sub(MAX_NAMED_TESTS) {
        0 => shown,
        more => format!("{shown} +{more} more"),
    }
}

/// A provider reason (`baseline provider: <why>; <what to do>`) is shown as
/// its advice, so the bounded receipt keeps the part an agent can act on.
fn actionable(reason: &str) -> String {
    ["baseline provider: ", "subject provider: "]
        .iter()
        .find_map(|prefix| reason.strip_prefix(prefix))
        .and_then(|warning| warning.rsplit_once("; "))
        .map_or_else(|| reason.to_string(), |(_, advice)| advice.to_string())
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out = text
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

fn bounded(text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_rust_thread_ids_from_panic_lines() {
        assert_eq!(
            strip_thread_id("thread 'tests::adds' (266636439) panicked at src/lib.rs:6:25:"),
            "tests::adds panicked at src/lib.rs:6:25"
        );
        assert_eq!(
            strip_thread_id("thread 'main' panicked at src/main.rs:2:5:"),
            "main panicked at src/main.rs:2:5"
        );
    }

    #[test]
    fn headline_names_program_exit_counts_and_failing_tests() {
        let payload = json!({
            "command": {"argv": ["/tmp/x/cargo", "test"], "exit_code": 101, "success": false},
            "provider": {"normalized": {
                "tests": [{"node_id": "t::a", "status": "failed"}],
                "counts": {"passed": 3, "failed": 1, "skipped": 0}
            }},
            "stdout": {"line_count": 20}, "stderr": {"line_count": 2}
        });
        assert_eq!(
            headline(&payload, &["t::a".to_string()]),
            "prog: `cargo test` · exit 101 · 3 passed, 1 failed: t::a · full output kept (22 lines)"
        );
    }

    #[test]
    fn many_failing_tests_are_bounded_with_a_count() {
        let tests = (0..7)
            .map(|index| format!("t::{index}"))
            .collect::<Vec<_>>();
        assert_eq!(named(&tests), "t::0, t::1 +5 more");
    }

    #[test]
    fn bounded_never_splits_a_character() {
        let text = "é".repeat(400);
        let out = bounded(text, 101);
        assert!(out.len() <= 101);
        assert!(out.ends_with('…'));
    }
}

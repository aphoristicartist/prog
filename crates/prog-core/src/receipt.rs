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
    /// The baseline is an older failing run of the same invocation whose
    /// failure no later run proved fixed, not the run just before the subject.
    /// A passing rerun after `NOT PROVEN` is still compared with that failure,
    /// so repeating a command cannot turn an unproven fix into `clean`.
    pub earlier_failure: bool,
}

/// Whether a persisted run payload reports a failure: the command failed,
/// timed out, or was cancelled, or a parsed test failed or errored.
pub fn payload_reports_failure(payload: &Value) -> bool {
    run_failed(payload) || !failing(TestPopulation::from_payload(payload).as_ref()).is_empty()
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
        let label = match run_kind_of(payload) {
            Some(RunKind::Build | RunKind::Lint) => "first error",
            _ => "first failure",
        };
        lines.push(format!("{label}: {detail}"));
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
            lines.push(if baseline.earlier_failure {
                line.replacen("vs previous run", "vs earlier failing run", 1)
            } else {
                line
            });
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

/// What a verification run checks (#313). A failing build or lint run is
/// reported as a build or lint failure, never as a failing test run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Test,
    Build,
    Lint,
}

/// Classify a verification argv; `None` when it is not a known test, build,
/// or lint invocation.
pub fn run_kind(argv: &[String]) -> Option<RunKind> {
    let program = argv.first()?.rsplit('/').next()?;
    let args = &argv[1..];
    let first = args.first().map(String::as_str);
    let nested = |rest: &[String]| {
        let start = rest.iter().position(|arg| !arg.starts_with('-'))?;
        run_kind(&rest[start..])
    };
    match program {
        "cargo" => {
            let subcommand = args
                .iter()
                .map(String::as_str)
                .find(|arg| !arg.starts_with('+'))?;
            match subcommand {
                "test" | "bench" | "nextest" => Some(RunKind::Test),
                "build" | "check" | "rustc" | "doc" => Some(RunKind::Build),
                "clippy" | "fmt" => Some(RunKind::Lint),
                _ => None,
            }
        }
        "pytest" | "py.test" | "jest" | "vitest" | "mocha" | "rspec" | "phpunit" | "ctest" => {
            Some(RunKind::Test)
        }
        "playwright" => (first == Some("test")).then_some(RunKind::Test),
        "go" => match first? {
            "test" => Some(RunKind::Test),
            "build" => Some(RunKind::Build),
            "vet" => Some(RunKind::Lint),
            _ => None,
        },
        "deno" => match first? {
            "test" => Some(RunKind::Test),
            "check" => Some(RunKind::Build),
            "lint" => Some(RunKind::Lint),
            _ => None,
        },
        "dotnet" => match first? {
            "test" => Some(RunKind::Test),
            "build" => Some(RunKind::Build),
            _ => None,
        },
        "rustc" | "tsc" => Some(RunKind::Build),
        "mypy" | "pyright" | "eslint" | "ruff" | "biome" => Some(RunKind::Lint),
        _ if program == "python"
            || program == "python3"
            || program
                .strip_prefix("python3.")
                .is_some_and(|minor| minor.chars().all(|ch| ch.is_ascii_digit())) =>
        {
            match (first, args.get(1).map(String::as_str)) {
                (Some("-m"), Some("pytest" | "unittest")) => Some(RunKind::Test),
                (Some("-m"), Some("mypy" | "ruff")) => Some(RunKind::Lint),
                _ => None,
            }
        }
        "npm" | "pnpm" | "yarn" | "bun" => match first? {
            "test" | "t" | "tst" => Some(RunKind::Test),
            "run" | "run-script" => script_kind(args.get(1)?),
            "exec" | "x" => nested(&args[1..]),
            _ if program != "npm" => nested(args).or_else(|| script_kind(first?)),
            _ => None,
        },
        "npx" | "bunx" | "pnpx" | "uvx" => nested(args),
        "uv" | "poetry" if first == Some("run") => nested(&args[1..]),
        _ => None,
    }
}

/// A package script by its conventional name: `test:unit`, `lint`, `build`.
fn script_kind(script: &str) -> Option<RunKind> {
    let script = script.to_ascii_lowercase();
    if ["test", "e2e", "spec"]
        .iter()
        .any(|word| script.contains(word))
    {
        Some(RunKind::Test)
    } else if script.contains("lint") {
        Some(RunKind::Lint)
    } else if ["build", "typecheck", "type-check", "check", "tsc"]
        .iter()
        .any(|word| script.contains(word))
    {
        Some(RunKind::Build)
    } else {
        None
    }
}

fn run_kind_of(payload: &Value) -> Option<RunKind> {
    let argv = payload
        .pointer("/command/argv")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    run_kind(&argv)
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
    let unfinished = unfinished_harnesses(payload);
    if let Some(counts) = payload
        .pointer("/provider/normalized/counts")
        .filter(|counts| counts.is_object())
    {
        let count = |name: &str| counts.get(name).and_then(Value::as_u64).unwrap_or(0);
        let failed = count("failed") + count("error");
        // A failed run whose failures no test owns (a crashed harness, a
        // collection or internal error, a timeout) never reads as `0 failed`.
        let unattributed = failed == 0 && failing_tests.is_empty() && run_failed(payload);
        let mut tally = if unattributed {
            format!(
                "{} passed, failure not attributed to a test",
                count("passed")
            )
        } else {
            format!("{} passed, {failed} failed", count("passed"))
        };
        if count("skipped") > 0 {
            tally.push_str(&format!(", {} skipped", count("skipped")));
        }
        if !failing_tests.is_empty() {
            tally.push_str(&format!(": {}", named(failing_tests)));
        }
        if unattributed && !unfinished.is_empty() {
            tally.push_str(&format!(" ({} did not finish)", named(&unfinished)));
        }
        parts.push(tally);
    } else if !failing_tests.is_empty() {
        parts.push(format!("failed: {}", named(failing_tests)));
    } else if !unfinished.is_empty() && run_failed(payload) {
        parts.push(format!("{} did not finish", named(&unfinished)));
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
/// already contains, in this order (#313):
///
/// 1. a compiler or lint error with its location;
/// 2. a crashed test binary and its signal;
/// 3. a provider test failure message;
/// 4. a Rust panic location;
/// 5. the most specific remaining output line: an exception, a Go panic, a
///    Node error, then any line that names an error.
///
/// Build progress (`Compiling`, `Checking`, `Running`), report headers
/// (`running 2 tests`, `rootdir:`), and passing-test lines are never chosen.
fn first_failure(payload: &Value) -> Option<String> {
    if let Some(diagnostic) = located_error(payload) {
        return Some(truncate(&diagnostic, MAX_DETAIL_CHARS));
    }
    let output = output_lines(payload);
    if let Some(crash) = crash(&output) {
        return Some(truncate(&crash, MAX_DETAIL_CHARS));
    }
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
    if let Some(panic) = panic(&output) {
        return Some(truncate(&panic, MAX_DETAIL_CHARS));
    }
    if let Some(line) = specific_line(&output) {
        return Some(truncate(&line, MAX_DETAIL_CHARS));
    }
    let section_lines = payload
        .pointer("/failure_sections/0/lines")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_progress(line))
        .collect::<Vec<_>>();
    section_lines
        .iter()
        .find(|line| names_an_error(line))
        .or_else(|| section_lines.first())
        .map(|line| truncate(line, MAX_DETAIL_CHARS))
}

/// The first error-level diagnostic a provider parsed with a source span:
/// `src/lib.rs:1:41: error: expected expression, found `}``.
fn located_error(payload: &Value) -> Option<String> {
    payload
        .pointer("/provider/normalized/diagnostics")?
        .as_array()?
        .iter()
        .filter(|diagnostic| diagnostic["severity"] == "error")
        .find_map(|diagnostic| {
            let span = diagnostic["spans"]
                .as_array()?
                .iter()
                .find(|span| span["primary"] != false)
                .or_else(|| diagnostic["spans"].get(0))?;
            let message = diagnostic["message"].as_str()?;
            let code = diagnostic["diagnostic_code"]
                .as_str()
                .map_or_else(String::new, |code| format!("[{code}]"));
            Some(format!(
                "{}:{}:{}: error{code}: {message}",
                span["path"].as_str()?,
                span["line"].as_u64()?,
                span["column"].as_u64()?
            ))
        })
}

/// Captured output lines, stderr first: Cargo and most runners write their
/// own errors there and test reports to stdout.
fn output_lines(payload: &Value) -> Vec<&str> {
    ["/stderr/text", "/stdout/text"]
        .iter()
        .filter_map(|pointer| payload.pointer(pointer).and_then(Value::as_str))
        .flat_map(str::lines)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

/// A test binary or interpreter that died from a signal rather than
/// reporting a failure. Cargo says which target (`--test a`) and how.
fn crash(lines: &[&str]) -> Option<String> {
    for (index, line) in lines.iter().enumerate() {
        if line.contains("didn't exit successfully")
            && let Some(start) = line.rfind("(signal: ")
        {
            let signal = line[start..].trim_end_matches(':');
            let target = lines[index.saturating_sub(6)..index]
                .iter()
                .rev()
                .find_map(|line| {
                    let rest = line
                        .strip_prefix("error: test failed, to rerun pass `")
                        .or_else(|| line.strip_prefix("error: doctest failed, to rerun pass `"))?;
                    rest.split_once('`').map(|(target, _)| target)
                });
            return Some(match target {
                Some(target) => format!("`{target}` crashed {signal}"),
                None => format!("test binary crashed {signal}"),
            });
        }
        if line.starts_with("Fatal Python error:")
            || line.starts_with("signal SIG")
            || [
                "Segmentation fault",
                "Abort trap",
                "Bus error",
                "Illegal instruction",
            ]
            .iter()
            .any(|marker| line.starts_with(marker))
        {
            return Some((*line).to_string());
        }
    }
    None
}

/// `tests::adds panicked at src/lib.rs:6:25`. A doctest panics inside a
/// temporary bundle, so it is named by its test and panic message instead.
fn panic(lines: &[&str]) -> Option<String> {
    let index = lines.iter().position(|line| line.contains("panicked at"))?;
    let line = lines[index];
    if line.contains("rustdoctest") || line.contains("doctest_bundle") {
        let name = lines[..index].iter().rev().find_map(|line| {
            line.strip_prefix("---- ")?
                .strip_suffix(" stdout ----")
                .map(str::to_string)
        });
        let message = lines.get(index + 1).copied();
        if let (Some(name), Some(message)) = (name, message) {
            return Some(format!("{name}: {message}"));
        }
    }
    Some(strip_thread_id(line))
}

/// The most specific failure line left once there is no parsed diagnostic,
/// crash, test message, or panic.
fn specific_line(lines: &[&str]) -> Option<String> {
    // pytest's `E   ModuleNotFoundError: …` detail lines.
    if let Some(detail) = lines
        .iter()
        .find_map(|line| line.strip_prefix("E ").map(str::trim_start))
        .filter(|detail| !detail.is_empty())
    {
        return Some(detail.to_string());
    }
    // A traceback ends with its exception; INTERNALERROR prefixes every line.
    if let Some(exception) = lines
        .iter()
        .map(|line| {
            line.strip_prefix("INTERNALERROR>")
                .map_or(*line, str::trim_start)
        })
        .rfind(|line| is_exception_line(line))
    {
        return Some(exception.to_string());
    }
    lines
        .iter()
        .find(|line| line.starts_with("panic: ") || line.starts_with("Error: "))
        .map(|line| (*line).to_string())
}

/// `ValueError: bad`, `pkg.CustomException: boom`.
fn is_exception_line(line: &str) -> bool {
    let Some((name, _)) = line.split_once(": ") else {
        return false;
    };
    (name.ends_with("Error") || name.ends_with("Exception"))
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.'))
}

fn names_an_error(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    ["error", "fail", "exception", "fatal", "panic", "traceback"]
        .iter()
        .any(|word| lower.contains(word))
}

/// Build progress, report headers, and passing results: never a failure.
fn is_progress(line: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "Compiling ",
        "Checking ",
        "Running ",
        "Finished ",
        "Doc-tests ",
        "Fresh ",
        "Downloaded ",
        "Downloading ",
        "Updating ",
        "Locking ",
        "Adding ",
        "Blocking ",
        "Building ",
        "Documenting ",
        "warning: build failed, waiting",
        "Caused by:",
        "note: run with `RUST_BACKTRACE",
        "test result: ok",
        "all doctests ran in",
        "platform ",
        "cachedir:",
        "rootdir:",
        "configfile:",
        "plugins:",
        "collecting ",
        "collected ",
    ];
    PREFIXES.iter().any(|prefix| line.starts_with(prefix))
        || line == "failures:"
        || line.ends_with(" ... ok")
        || line.ends_with(" PASSED")
        || line.contains("test session starts")
        || line
            .strip_prefix("running ")
            .is_some_and(|rest| rest.ends_with(" tests") || rest.ends_with(" test"))
        || line
            .chars()
            .all(|ch| matches!(ch, '=' | '-' | '_' | '*' | '~' | '!' | ' '))
}

/// Unfinished test harnesses a provider recorded (#313): Cargo started them
/// but their report never closed, so they crashed or were killed.
fn unfinished_harnesses(payload: &Value) -> Vec<String> {
    payload
        .pointer("/provider/normalized/harnesses/unfinished")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
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
        // Quote what blocks the proof, not context such as shared skips or
        // why the earlier run stopped (a baseline need not be exhaustive).
        let blocking = delta
            .assessment
            .reasons
            .iter()
            .filter(|reason| !is_context(reason))
            .collect::<Vec<_>>();
        // A narrowed population is the strongest evidence; then the rerun's
        // own advice for a provable run (`go test -v`, `jest --json`).
        let narrowed = |reason: &&&String| {
            [
                "did not run again",
                "population shrank",
                "cannot be matched",
                "recorded none",
            ]
            .iter()
            .any(|marker| reason.contains(marker))
        };
        let subject_advice = |reason: &&&String| {
            reason.starts_with("subject provider: ") && !reason.contains("skipped some tests")
        };
        let reason = blocking
            .iter()
            .find(narrowed)
            .or_else(|| blocking.iter().find(subject_advice))
            .or_else(|| blocking.iter().find(|reason| reason.contains("test")))
            .or_else(|| blocking.first())
            .copied()
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
    } else if payload
        .pointer("/stdout/line_count")
        .and_then(Value::as_u64)
        == Some(0)
        && payload
            .pointer("/stderr/line_count")
            .and_then(Value::as_u64)
            .is_some_and(|lines| lines > 0)
    {
        // `python -m unittest` writes its whole report to stderr.
        "/stderr"
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

fn is_context(reason: &str) -> bool {
    [
        "both runs skipped the same",
        "the subject skipped ",
        "baseline selection is not exhaustive",
        "baseline provider: ",
        "baseline has ",
        "subject has ",
    ]
    .iter()
    .any(|prefix| reason.starts_with(prefix))
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
    fn a_failed_run_without_a_failing_test_is_never_zero_failures() {
        let payload = json!({
            "command": {"argv": ["cargo", "test", "--no-fail-fast"], "exit_code": 101, "success": false},
            "provider": {"normalized": {
                "tests": [{"node_id": "tests::lib_ok", "status": "passed"}],
                "counts": {"passed": 2, "failed": 0, "skipped": 0},
                "harnesses": {"started": 4, "reports": 4, "finished": 3, "unfinished": ["tests/a.rs"]}
            }}
        });
        assert_eq!(
            headline(&payload, &[]),
            "prog: `cargo test --no-fail-fast` · exit 101 · 2 passed, failure not attributed to a test (tests/a.rs did not finish)"
        );
        // A passing run still reads as zero failures.
        let passing = json!({
            "command": {"argv": ["cargo", "test"], "exit_code": 0, "success": true},
            "provider": {"normalized": {"counts": {"passed": 2, "failed": 0, "skipped": 0}}}
        });
        assert_eq!(
            headline(&passing, &[]),
            "prog: `cargo test` · exit 0 · 2 passed, 0 failed"
        );
    }

    #[test]
    fn crashes_name_the_cargo_target_and_signal() {
        let lines = [
            "Running tests/a.rs (target/debug/deps/a-7c1c85b1797f517a)",
            "error: test failed, to rerun pass `--test a`",
            "Caused by:",
            "process didn't exit successfully: `target/debug/deps/a-7c1c85b1797f517a` (signal: 6, SIGABRT: process abort signal)",
        ];
        assert_eq!(
            crash(&lines).as_deref(),
            Some("`--test a` crashed (signal: 6, SIGABRT: process abort signal)")
        );
        assert_eq!(
            crash(&["Fatal Python error: Segmentation fault"]).as_deref(),
            Some("Fatal Python error: Segmentation fault")
        );
        assert_eq!(crash(&["error: test failed, to rerun pass `--lib`"]), None);
    }

    #[test]
    fn progress_headers_and_passing_results_are_never_failures() {
        for line in [
            "Compiling a v0.1.0 (/repo/a)",
            "Checking a v0.1.0 (/repo/a)",
            "Running unittests src/lib.rs (target/debug/deps/a-1)",
            "Finished `test` profile [unoptimized + debuginfo] target(s) in 0.41s",
            "running 2 tests",
            "running 1 test",
            "test tests::lib_ok ... ok",
            "test result: ok. 1 passed; 0 failed; 0 ignored",
            "rootdir: /repo",
            "cachedir: .pytest_cache",
            "collecting ... collected 1 item / 1 error",
            "============================= test session starts ==============================",
            "tests/test_ok.py::test_ok PASSED",
        ] {
            assert!(is_progress(line), "{line}");
        }
        for line in [
            "error: test failed, to rerun pass `--lib`",
            "panic: init boom",
            "FAILED tests/test_x.py::test_a - assert 1 == 2",
        ] {
            assert!(!is_progress(line), "{line}");
        }
    }

    #[test]
    fn specific_lines_prefer_exception_details() {
        assert_eq!(
            specific_line(&[
                "ImportError while importing test module '/repo/tests/test_coll.py'.",
                "E   ModuleNotFoundError: No module named 'nope'",
            ])
            .as_deref(),
            Some("ModuleNotFoundError: No module named 'nope'")
        );
        assert_eq!(
            specific_line(&[
                "INTERNALERROR> Traceback (most recent call last):",
                "INTERNALERROR>   File \"conftest.py\", line 2, in hook",
                "INTERNALERROR> RuntimeError: plugin boom",
            ])
            .as_deref(),
            Some("RuntimeError: plugin boom")
        );
    }

    #[test]
    fn run_kind_classifies_tests_builds_and_lints() {
        let kind = |argv: &[&str]| {
            run_kind(
                &argv
                    .iter()
                    .map(|arg| (*arg).to_string())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(kind(&["/x/cargo", "test"]), Some(RunKind::Test));
        assert_eq!(kind(&["cargo", "+1.89.0", "check"]), Some(RunKind::Build));
        assert_eq!(
            kind(&["cargo", "clippy", "--", "-D", "warnings"]),
            Some(RunKind::Lint)
        );
        assert_eq!(kind(&["python3", "-m", "unittest"]), Some(RunKind::Test));
        assert_eq!(kind(&["npm", "run", "lint"]), Some(RunKind::Lint));
        assert_eq!(kind(&["npm", "run", "build"]), Some(RunKind::Build));
        assert_eq!(
            kind(&["pnpm", "exec", "vitest", "run"]),
            Some(RunKind::Test)
        );
        assert_eq!(kind(&["uv", "run", "pytest", "-q"]), Some(RunKind::Test));
        assert_eq!(kind(&["go", "vet", "./..."]), Some(RunKind::Lint));
        assert_eq!(kind(&["cargo", "run"]), None);
        assert_eq!(kind(&["ls"]), None);
    }

    #[test]
    fn bounded_never_splits_a_character() {
        let text = "é".repeat(400);
        let out = bounded(text, 101);
        assert!(out.len() <= 101);
        assert!(out.ends_with('…'));
    }
}

//! Jest and Vitest. A fix is provable only from the JSON report on stdout
//! (`jest --json`, `vitest run --reporter=json --outputFile=/dev/stdout`):
//! both runners switch to a failures-only reporter when they detect an AI
//! agent (`CLAUDECODE`, `CODEX_THREAD_ID`, ...), so their text output never
//! names passing tests there, even with `--verbose`. Text output still yields
//! the summary counts and the failing tests.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::verification::Completion;
use super::{
    CodingProviderResult, InputLine, MAX_PROVIDER_ITEMS, NormalizedTest, bounded_text,
    provider_result,
};
use crate::SelectionCoverage;

pub(super) const SKIPPED: &str = "Jest or Vitest skipped some tests, so this run alone does not prove the suite passed; to prove a fix, rerun skipping the same tests, named by the JSON report";
const TEXT_ONLY: &str = "Jest and Vitest text output does not name passing tests; rerun with `--json` (Jest) or `--reporter=json --outputFile=/dev/stdout` (Vitest)";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Runner {
    Jest,
    Vitest,
}

/// How the runner was reached: directly, or through a package script whose
/// command line prog cannot see (`npm test`).
pub(super) enum Launch<'a> {
    Direct(Runner, &'a [String]),
    Script(&'a str, &'a [String]),
}

/// `jest`, `vitest`, `npx [flags] jest`, `pnpm exec vitest`, `yarn jest`,
/// `npm test [-- args]`, `pnpm test`, `yarn test`, `bun run test`.
pub(super) fn launch<'a>(program: &'a str, argv: &'a [String]) -> Option<Launch<'a>> {
    let runner = |name: &str| match name {
        "jest" => Some(Runner::Jest),
        "vitest" => Some(Runner::Vitest),
        _ => None,
    };
    if let Some(runner) = runner(program) {
        return Some(Launch::Direct(runner, &argv[1..]));
    }
    let args = &argv[1..];
    let first = args.first().map(String::as_str);
    match program {
        "npx" | "bunx" | "pnpx" => {
            let index = args.iter().position(|arg| !arg.starts_with('-'))?;
            let found = runner(args[index].rsplit('/').next()?)?;
            Some(Launch::Direct(found, &args[index + 1..]))
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            if matches!(first, Some("exec" | "x")) {
                let rest = &args[1..];
                let index = rest.iter().position(|arg| !arg.starts_with('-'))?;
                return Some(Launch::Direct(runner(&rest[index])?, &rest[index + 1..]));
            }
            if program != "npm"
                && program != "bun"
                && let Some(found) = first.and_then(runner)
            {
                return Some(Launch::Direct(found, &args[1..]));
            }
            // `bun test` is Bun's own runner, not a package script.
            let script = match (program, first) {
                ("bun", Some("test")) => return None,
                (_, Some("test" | "t" | "tst")) => &args[1..],
                (_, Some("run" | "run-script"))
                    if args.get(1).map(String::as_str) == Some("test") =>
                {
                    &args[2..]
                }
                _ => return None,
            };
            // npm and bun pass script arguments after `--`.
            let script = match script.iter().position(|arg| arg == "--") {
                Some(index) if matches!(program, "npm" | "bun") => &script[index + 1..],
                _ => script,
            };
            Some(Launch::Script(program, script))
        }
        _ => None,
    }
}

struct Invocation {
    runner: Option<Runner>,
    filters: Vec<String>,
    bail: bool,
    targets: Vec<String>,
    scopes: Vec<String>,
}

/// Options that take the next argument as their value.
const VALUE_OPTIONS: &[&str] = &[
    "-c",
    "--config",
    "--reporter",
    "--reporters",
    "--outputFile",
    "--outputfile",
    "--root",
    "--rootDir",
    "--dir",
    "--project",
    "--selectProjects",
    "--testTimeout",
    "--maxWorkers",
    "-w",
    "--pool",
    "--environment",
    "--env",
    "--coverageDirectory",
    "--seed",
    "--testPathIgnorePatterns",
    "--testPathPattern",
    "--testPathPatterns",
    "--retry",
    "--mode",
];

fn invocation(launch: &Launch<'_>) -> Invocation {
    let (runner, args, base) = match launch {
        Launch::Direct(runner, args) => (
            Some(*runner),
            *args,
            match runner {
                Runner::Jest => "jest".to_string(),
                Runner::Vitest => "vitest".to_string(),
            },
        ),
        Launch::Script(program, args) => (None, *args, format!("{program}:test")),
    };
    // `vitest run`, `vitest related <files>`.
    let (args, related) = match (runner, args.first().map(String::as_str)) {
        (Some(Runner::Vitest), Some("run")) => (&args[1..], false),
        (Some(Runner::Vitest), Some("related")) => (&args[1..], true),
        _ => (args, false),
    };
    let mut filters = Vec::new();
    if related {
        filters.push("related".to_string());
    }
    let mut bail = false;
    let mut targets = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].as_str();
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg, false), |(name, _)| (name, true));
        match name {
            "-t" | "--testNamePattern" | "-o" | "--onlyChanged" | "--changedSince"
            | "--lastCommit" | "--findRelatedTests" | "--related" | "--onlyFailures" | "-f"
            | "--changed" | "--shard" | "--listTests" | "--testPathPattern"
            | "--testPathPatterns" | "--project" | "--selectProjects" => {
                filters.push(name.to_string());
                let takes_value = !matches!(
                    name,
                    "-o" | "--onlyChanged"
                        | "--lastCommit"
                        | "--findRelatedTests"
                        | "--onlyFailures"
                        | "-f"
                        | "--listTests"
                        | "--changed"
                        | "--related"
                );
                if takes_value && !inline {
                    index += 1;
                }
            }
            "-b" | "--bail" => bail = true,
            _ if VALUE_OPTIONS.contains(&name) && !inline => index += 1,
            _ if arg.starts_with('-') => {}
            _ => targets.push(bounded_text(arg)),
        }
        index += 1;
    }
    targets.sort();
    targets.dedup();
    let scopes = if targets.is_empty() {
        vec![format!("{base}:all")]
    } else {
        targets
            .iter()
            .map(|target| format!("{base}:{target}"))
            .collect()
    };
    Invocation {
        runner,
        filters,
        bail,
        targets,
        scopes,
    }
}

fn strip_ansi(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.next() == Some('[') {
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Passed,
    Skipped,
    Failed,
    /// A test file that failed to load or run.
    Error,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Error => "error",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Passed => 0,
            Self::Skipped => 1,
            Self::Failed => 2,
            Self::Error => 3,
        }
    }
}

struct Record {
    status: Status,
    message: Option<String>,
    stream: &'static str,
    line: Option<usize>,
    pointer: Option<String>,
}

#[derive(Default)]
struct Parsed {
    records: BTreeMap<String, Record>,
    bound_hit: bool,
    malformed: bool,
}

impl Parsed {
    fn record(&mut self, id: String, record: Record) {
        if let Some(existing) = self.records.get_mut(&id) {
            if record.status.rank() > existing.status.rank() {
                *existing = record;
            }
            return;
        }
        if self.records.len() >= MAX_PROVIDER_ITEMS {
            self.bound_hit = true;
            return;
        }
        self.records.insert(id, record);
    }

    fn count(&self, status: Status) -> u64 {
        self.records
            .values()
            .filter(|record| record.status == status)
            .count() as u64
    }
}

fn first_line(text: &str) -> Option<String> {
    strip_ansi(text)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('●'))
        .map(bounded_text)
}

/// Package managers print a banner around the report (`> pkg@1.0.0 test`,
/// `$ jest --json`, `Done in 1.2s.`); nothing else may share stdout with it.
fn is_frame_line(text: &str) -> bool {
    text.is_empty()
        || text.starts_with("> ")
        || text.starts_with("$ ")
        || text.starts_with("yarn run v")
        || text.starts_with("Done in ")
        || text.starts_with(VITEST_NOTICE)
}

/// Vitest prints this after writing its JSON report, on the report's own
/// line when the report goes to stdout without a trailing newline.
const VITEST_NOTICE: &str = "JSON report written to ";

fn without_vitest_notice(text: &str) -> &str {
    match text.rfind(VITEST_NOTICE) {
        Some(index) if text[..index].ends_with('}') => &text[..index],
        _ => text,
    }
}

enum Report {
    None,
    One(Value),
    /// A report shares stdout with other output, or appears twice.
    Embedded,
}

fn json_report(stdout: &str) -> Report {
    let is_report = |value: &Value| {
        value.get("testResults").is_some_and(Value::is_array)
            && value.get("numTotalTests").is_some_and(Value::is_u64)
    };
    if let Ok(value) = serde_json::from_str::<Value>(without_vitest_notice(stdout.trim()))
        && is_report(&value)
    {
        return Report::One(value);
    }
    let mut reports = Vec::new();
    let mut other_output = false;
    for line in stdout.lines() {
        let text = without_vitest_notice(line.trim());
        if text.starts_with('{')
            && let Ok(value) = serde_json::from_str::<Value>(text)
            && is_report(&value)
        {
            reports.push(value);
        } else if !is_frame_line(text) {
            other_output = true;
        }
    }
    match (reports.len(), other_output) {
        (0, _) => Report::None,
        (1, false) => Report::One(reports.pop().expect("one report")),
        _ => Report::Embedded,
    }
}

struct ReportFacts {
    success: Option<bool>,
    interrupted: bool,
    runtime_error_suites: u64,
    totals: BTreeMap<&'static str, Option<u64>>,
    assertion_statuses: BTreeMap<&'static str, u64>,
}

fn relative<'a>(path: &'a str, cwd: Option<&str>) -> &'a str {
    cwd.map(|cwd| cwd.trim_end_matches('/'))
        .filter(|cwd| !cwd.is_empty())
        .and_then(|cwd| path.strip_prefix(cwd))
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(path)
}

fn parse_report(report: &Value, cwd: Option<&str>) -> (Parsed, ReportFacts) {
    let mut parsed = Parsed::default();
    let mut assertion_statuses = BTreeMap::new();
    let files = report["testResults"].as_array().expect("validated report");
    if files.len() > MAX_PROVIDER_ITEMS {
        parsed.bound_hit = true;
    }
    for (file_index, file) in files.iter().enumerate().take(MAX_PROVIDER_ITEMS) {
        let Some(name) = file.get("name").and_then(Value::as_str) else {
            parsed.malformed = true;
            continue;
        };
        let name = bounded_text(relative(name, cwd));
        let assertions = file
            .get("assertionResults")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let mut failed_assertion = false;
        for (index, assertion) in assertions.iter().enumerate() {
            let Some(raw) = assertion.get("status").and_then(Value::as_str) else {
                parsed.malformed = true;
                continue;
            };
            let (status, key) = match raw {
                "passed" => (Status::Passed, "passed"),
                "failed" => (Status::Failed, "failed"),
                "pending" | "skipped" | "disabled" => (Status::Skipped, "pending"),
                "todo" => (Status::Skipped, "todo"),
                _ => {
                    parsed.malformed = true;
                    continue;
                }
            };
            *assertion_statuses.entry(key).or_insert(0u64) += 1;
            failed_assertion |= status == Status::Failed;
            let titles = assertion
                .get("ancestorTitles")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .chain(assertion.get("title").and_then(Value::as_str))
                .collect::<Vec<_>>();
            if titles.is_empty() {
                parsed.malformed = true;
                continue;
            }
            let message = (status == Status::Failed)
                .then(|| {
                    assertion
                        .get("failureMessages")
                        .and_then(Value::as_array)
                        .and_then(|messages| messages.first())
                        .and_then(Value::as_str)
                        .and_then(first_line)
                })
                .flatten();
            parsed.record(
                bounded_text(&format!("{name} > {}", titles.join(" > "))),
                Record {
                    status,
                    message,
                    stream: "stdout",
                    line: None,
                    pointer: Some(format!(
                        "/testResults/{file_index}/assertionResults/{index}"
                    )),
                },
            );
        }
        // RTK #3935, #3588: a file that failed to load fails the run even
        // when no test inside it is counted as failed.
        if file.get("status").and_then(Value::as_str) == Some("failed") && !failed_assertion {
            parsed.record(
                name,
                Record {
                    status: Status::Error,
                    message: file
                        .get("message")
                        .and_then(Value::as_str)
                        .and_then(first_line)
                        .or_else(|| Some("test file failed to run".to_string())),
                    stream: "stdout",
                    line: None,
                    pointer: Some(format!("/testResults/{file_index}")),
                },
            );
        }
    }
    let total = |key: &str| report.get(key).and_then(Value::as_u64);
    let facts = ReportFacts {
        success: report.get("success").and_then(Value::as_bool),
        interrupted: report.get("wasInterrupted").and_then(Value::as_bool) == Some(true),
        runtime_error_suites: total("numRuntimeErrorTestSuites").unwrap_or(0),
        totals: BTreeMap::from([
            ("total", total("numTotalTests")),
            ("passed", total("numPassedTests")),
            ("failed", total("numFailedTests")),
            ("pending", total("numPendingTests")),
            ("todo", total("numTodoTests")),
        ]),
        assertion_statuses,
    };
    (parsed, facts)
}

/// `Tests:       1 failed, 1 skipped, 1 todo, 2 passed, 5 total` (Jest) or
/// `Tests  1 failed | 2 passed | 1 skipped | 1 todo (5)` (Vitest).
fn text_summary(text: &str) -> Option<BTreeMap<String, u64>> {
    let (rest, separator) = if let Some(rest) = text.strip_prefix("Tests:") {
        (rest, ',')
    } else {
        (text.strip_prefix("Tests  ")?, '|')
    };
    let mut counts = BTreeMap::new();
    for part in rest.split(separator) {
        let part = part.trim();
        let part = part.split_once(" (").map_or(part, |(head, _)| head);
        let (number, label) = part.split_once(' ')?;
        counts.insert(label.trim().to_string(), number.parse().ok()?);
    }
    (!counts.is_empty()).then_some(counts)
}

struct TextFacts {
    summaries: Vec<BTreeMap<String, u64>>,
    failed_files: u64,
}

fn parse_text(lines: &[InputLine<'_>]) -> (Parsed, TextFacts) {
    let mut parsed = Parsed::default();
    let mut facts = TextFacts {
        summaries: Vec::new(),
        failed_files: 0,
    };
    let mut jest_file: Option<String> = None;
    for line in lines {
        let text = strip_ansi(line.text);
        let trimmed = text.trim();
        if let Some(summary) = text_summary(trimmed) {
            facts.summaries.push(summary);
            continue;
        }
        if let Some(files) = trimmed
            .strip_prefix("Test Suites:")
            .or_else(|| trimmed.strip_prefix("Test Files  "))
        {
            facts.failed_files = files
                .split([',', '|'])
                .filter_map(|part| part.trim().strip_suffix(" failed"))
                .filter_map(|number| number.trim().parse::<u64>().ok())
                .sum();
            continue;
        }
        let record = |status| Record {
            status,
            message: None,
            stream: line.stream,
            line: Some(line.number),
            pointer: None,
        };
        // Jest: `FAIL src/calc.test.js`, then `● calc › adds`.
        if let Some(file) = trimmed.strip_prefix("FAIL ")
            && !file.starts_with(' ')
            && !file.contains(" > ")
            && !file.contains(" [ ")
        {
            jest_file = Some(bounded_text(file.trim()));
            continue;
        }
        if trimmed.starts_with("PASS ") {
            jest_file = None;
            continue;
        }
        if let Some(title) = trimmed.strip_prefix("● ") {
            let Some(file) = &jest_file else { continue };
            if title == "Test suite failed to run" {
                parsed.record(file.clone(), record(Status::Error));
            } else if !title.starts_with("Console") {
                let id = format!(
                    "{file} > {}",
                    title.split(" › ").collect::<Vec<_>>().join(" > ")
                );
                parsed.record(bounded_text(&id), record(Status::Failed));
            }
            continue;
        }
        // Vitest: ` FAIL  calc.test.js > calc > adds`, or
        // ` FAIL  broken.test.js [ broken.test.js ]` for a file that failed.
        if let Some(rest) = trimmed.strip_prefix("FAIL  ") {
            let rest = rest.trim();
            if let Some((file, _)) = rest.split_once(" [ ") {
                parsed.record(bounded_text(file.trim()), record(Status::Error));
            } else if rest.contains(" > ") {
                parsed.record(bounded_text(rest), record(Status::Failed));
            }
        }
    }
    (parsed, facts)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn normalize(
    launch: &Launch<'_>,
    cwd: Option<&str>,
    stdout: &str,
    lines: &[InputLine<'_>],
    input_bytes: usize,
    bound_hit: bool,
    capture_complete: bool,
    exit_code: Option<i32>,
) -> Option<(CodingProviderResult, Completion)> {
    let invocation = invocation(launch);
    let report = if bound_hit {
        Report::None
    } else {
        json_report(stdout)
    };
    let (parsed, completion, counts, input_format, summary_seen, runner) = match report {
        Report::One(report) => {
            let (parsed, facts) = parse_report(&report, cwd);
            let runner =
                invocation
                    .runner
                    .unwrap_or(if report.get("numRuntimeErrorTestSuites").is_some() {
                        Runner::Jest
                    } else {
                        Runner::Vitest
                    });
            let passed = parsed.count(Status::Passed);
            let failed = parsed.count(Status::Failed);
            let counts = json!({
                "passed": passed,
                "failed": failed,
                "error": parsed.count(Status::Error),
                "skipped": parsed.count(Status::Skipped),
                "xfailed": 0,
                "xpassed": 0,
                "executed": passed + failed
            });
            let completion = report_completion(&parsed, &facts, exit_code);
            (
                parsed,
                completion,
                Some(counts),
                "js_test_json_report",
                true,
                Some(runner),
            )
        }
        Report::Embedded => (
            Parsed::default(),
            Completion::Conflicting(
                "Jest/Vitest JSON report shares stdout with other output or appears twice; completion is unproven",
            ),
            None,
            "js_test_json_report",
            false,
            invocation.runner,
        ),
        Report::None => {
            let (parsed, facts) = parse_text(lines);
            // A package script is claimed only when its output is recognized.
            if invocation.runner.is_none() && facts.summaries.is_empty() {
                return None;
            }
            let summary = (facts.summaries.len() == 1).then(|| &facts.summaries[0]);
            let counts = summary.map(|summary| {
                let get = |key: &str| summary.get(key).copied().unwrap_or(0);
                json!({
                    "passed": get("passed"),
                    "failed": get("failed"),
                    "error": parsed.count(Status::Error),
                    "skipped": get("skipped") + get("todo") + get("pending"),
                    "xfailed": 0,
                    "xpassed": 0,
                    "executed": get("passed") + get("failed")
                })
            });
            let completion = text_completion(&parsed, &facts, exit_code);
            let format = "js_test_text";
            (
                parsed,
                completion,
                counts,
                format,
                summary.is_some(),
                invocation.runner,
            )
        }
    };
    let bound_hit = bound_hit || parsed.bound_hit;
    let complete = capture_complete && !bound_hit && !parsed.malformed;
    let failing = parsed
        .records
        .values()
        .any(|record| record.status.rank() >= Status::Failed.rank());
    let selection = SelectionCoverage {
        scopes: invocation.scopes.clone(),
        exhaustive: complete
            && summary_seen
            && invocation.filters.is_empty()
            && !(invocation.bail && failing),
        ..SelectionCoverage::default()
    };
    let mut warnings = Vec::new();
    if !summary_seen
        && stdout
            .lines()
            .any(|line| line.trim().starts_with(VITEST_NOTICE) && !line.contains("/dev/stdout"))
    {
        warnings.push(
            "Vitest wrote its JSON report to a file prog does not read; add `--outputFile=/dev/stdout`"
                .to_string(),
        );
    }
    if parsed.malformed {
        warnings.push("Jest/Vitest JSON report has records prog cannot read".to_string());
    }
    if !summary_seen {
        warnings.push(
            "Jest/Vitest completion summary was not observed; selection exhaustiveness is unproven"
                .to_string(),
        );
    }
    if !invocation.filters.is_empty() {
        warnings.push(format!(
            "{} runs a subset of the tests; broader absence is unprovable",
            invocation.filters.join(" ")
        ));
    }
    if invocation.bail && failing {
        warnings.push(
            "--bail stops after the first failing test file, so later files may not have run; rerun without --bail to prove selection exhaustion"
                .to_string(),
        );
    }
    let tests = parsed
        .records
        .iter()
        .map(|(id, record)| NormalizedTest {
            node_id: id.clone(),
            status: record.status.as_str().to_string(),
            message: record.message.clone(),
            evidence_stream: record.stream,
            evidence_line: record.line.map(|line| line.try_into().unwrap_or(u64::MAX)),
            report_pointer: record.pointer.clone(),
        })
        .collect::<Vec<_>>();
    let item_count = tests.len();
    let result = provider_result(
        "js_test.v1",
        input_format,
        if input_format == "js_test_json_report" {
            0.99
        } else {
            0.9
        },
        complete,
        selection,
        input_bytes,
        lines.len(),
        item_count,
        bound_hit,
        json!({
            "tests": tests,
            "summary_seen": summary_seen,
            "counts": counts,
            "runner": runner.map(|runner| match runner {
                Runner::Jest => "jest",
                Runner::Vitest => "vitest",
            }),
            "filters": invocation.filters,
            "bail": invocation.bail,
            "targets": invocation.targets,
            // #308: only a consistent JSON report lists every test that ran;
            // text output names failures only.
            "names_every_test_that_ran": input_format == "js_test_json_report"
                && complete
                && matches!(completion, Completion::Consistent | Completion::Skipped(_))
        }),
        warnings,
    );
    Some((result, completion))
}

fn report_completion(parsed: &Parsed, facts: &ReportFacts, exit_code: Option<i32>) -> Completion {
    let Some(exit_code) = exit_code else {
        return Completion::Unproven(
            "Jest/Vitest completion requires an observed process exit code",
        );
    };
    let Some(success) = facts.success else {
        return Completion::Unproven("Jest/Vitest JSON report omits success");
    };
    if success != (exit_code == 0) {
        return Completion::Conflicting(
            "Jest/Vitest JSON success disagrees with the observed process exit",
        );
    }
    let statuses = |key: &str| facts.assertion_statuses.get(key).copied().unwrap_or(0);
    let records = statuses("passed") + statuses("failed") + statuses("pending") + statuses("todo");
    let disagrees = [
        ("total", records),
        ("passed", statuses("passed")),
        ("failed", statuses("failed")),
        ("pending", statuses("pending")),
        ("todo", statuses("todo")),
    ]
    .iter()
    .any(|(key, observed)| facts.totals[key].is_some_and(|total| total != *observed))
        || facts.totals["total"].is_none();
    if disagrees {
        return Completion::Conflicting("Jest/Vitest JSON summary disagrees with its test records");
    }
    let failing = parsed.count(Status::Failed) + parsed.count(Status::Error);
    if success && failing > 0 {
        return Completion::Conflicting(
            "Jest/Vitest JSON reports success despite failing tests or files",
        );
    }
    if facts.interrupted {
        return Completion::Unproven("the Jest run was interrupted");
    }
    if parsed.count(Status::Error) > 0 || facts.runtime_error_suites > 0 {
        return Completion::Unproven("a test file failed to load or run, so its tests never ran");
    }
    if parsed.count(Status::Passed) + parsed.count(Status::Failed) == 0 {
        return Completion::Unproven("Jest/Vitest ran no tests");
    }
    if parsed.count(Status::Skipped) > 0 {
        return Completion::Skipped(SKIPPED);
    }
    Completion::Consistent
}

fn text_completion(parsed: &Parsed, facts: &TextFacts, exit_code: Option<i32>) -> Completion {
    if facts.summaries.len() != 1 {
        return Completion::Unproven("Jest/Vitest requires one unambiguous completion summary");
    }
    let Some(exit_code) = exit_code else {
        return Completion::Unproven(
            "Jest/Vitest completion requires an observed process exit code",
        );
    };
    let failed = facts.summaries[0].get("failed").copied().unwrap_or(0);
    let failing = failed > 0 || facts.failed_files > 0 || !parsed.records.is_empty();
    if failing != (exit_code != 0) {
        return Completion::Conflicting(
            "Jest/Vitest summary disagrees with the observed process exit",
        );
    }
    if parsed.count(Status::Failed) > failed {
        return Completion::Conflicting(
            "Jest/Vitest summary contradicts the failing tests it names",
        );
    }
    Completion::Unproven(TEXT_ONLY)
}

#[cfg(test)]
mod tests {
    use super::super::normalize_coding_output;
    use super::*;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn statuses(result: &CodingProviderResult) -> Vec<(String, String)> {
        result.normalized["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|test| {
                (
                    test["node_id"].as_str().unwrap().to_string(),
                    test["status"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn assertion(titles: &[&str], status: &str) -> Value {
        let (title, ancestors) = titles.split_last().unwrap();
        json!({
            "ancestorTitles": ancestors,
            "title": title,
            "fullName": titles.join(" "),
            "status": status,
            "failureMessages": if status == "failed" {
                vec!["Error: expect(received).toBe(expected)\n    at Object.<anonymous>"]
            } else {
                vec![]
            }
        })
    }

    /// The shape `jest --json` (Jest 30) prints for one failing, one
    /// passing, one skipped, and one todo test.
    fn jest_report(calc: &[(&[&str], &str)], success: bool) -> Value {
        let assertions = calc
            .iter()
            .map(|(titles, status)| assertion(titles, status))
            .collect::<Vec<_>>();
        let count = |wanted: &[&str]| {
            calc.iter()
                .filter(|(_, status)| wanted.contains(status))
                .count()
        };
        json!({
            "numFailedTestSuites": u64::from(count(&["failed"]) > 0),
            "numFailedTests": count(&["failed"]),
            "numPassedTests": count(&["passed"]),
            "numPendingTests": count(&["pending"]),
            "numRuntimeErrorTestSuites": 0,
            "numTodoTests": count(&["todo"]),
            "numTotalTestSuites": 1,
            "numTotalTests": calc.len(),
            "success": success,
            "wasInterrupted": false,
            "testResults": [{
                "name": "/repo/src/calc.test.js",
                "status": if count(&["failed"]) > 0 { "failed" } else { "passed" },
                "message": "",
                "assertionResults": assertions
            }]
        })
    }

    #[test]
    fn jest_json_report_names_every_test_by_file_and_describe_path() {
        let report = jest_report(
            &[
                (&["calc", "adds"], "failed"),
                (&["calc", "zero"], "passed"),
                (&["calc", "network"], "pending"),
                (&["calc", "later"], "todo"),
            ],
            false,
        );
        let stdout = format!("\n> app@1.0.0 test\n> jest --json\n\n{report}\n");
        let result = normalize_coding_output(
            &argv(&["npm", "test", "--", "--json"]),
            &stdout,
            "",
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(result.provider, "js_test.v1");
        assert_eq!(result.input_format, "js_test_json_report");
        assert!(result.complete);
        assert_eq!(
            statuses(&result),
            [
                ("/repo/src/calc.test.js > calc > adds", "failed"),
                ("/repo/src/calc.test.js > calc > later", "skipped"),
                ("/repo/src/calc.test.js > calc > network", "skipped"),
                ("/repo/src/calc.test.js > calc > zero", "passed"),
            ]
            .iter()
            .map(|(id, status)| ((*id).to_string(), (*status).to_string()))
            .collect::<Vec<_>>()
        );
        assert_eq!(
            result.normalized["tests"][0]["message"],
            "Error: expect(received).toBe(expected)"
        );
        assert_eq!(result.normalized["counts"]["executed"], 2);
        assert_eq!(result.normalized["runner"], "jest");
        assert_eq!(result.selection.scopes, vec!["npm:test:all"]);
        assert_eq!(
            result.selection.extra[crate::SELECTION_EXCLUDES_SKIPPED_TESTS],
            true,
            "{:?}",
            result.warnings
        );

        let green = jest_report(&[(&["calc", "adds"], "passed")], true);
        let result = normalize_coding_output(
            &argv(&["npx", "jest", "--json"]),
            &green.to_string(),
            "",
            true,
            Some(0),
        )
        .unwrap();
        assert!(result.selection.exhaustive, "{:?}", result.warnings);
        assert_eq!(result.selection.scopes, vec!["jest:all"]);
    }

    #[test]
    fn reports_that_cannot_prove_a_fix_say_why() {
        let green = jest_report(&[(&["calc", "adds"], "passed")], true);
        let mut load_failure = green.clone();
        // RTK #3935/#3588: a file that failed to load, counted nowhere else.
        load_failure["testResults"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "name": "/repo/src/broken.test.js",
                "status": "failed",
                "message": "  ● Test suite failed to run\n\n    Cannot find module './nope'",
                "assertionResults": []
            }));
        let mut lying = green.clone();
        lying["numFailedTests"] = json!(0);
        lying["testResults"][0]["assertionResults"][0]["status"] = json!("failed");
        let mut interrupted = green.clone();
        interrupted["wasInterrupted"] = json!(true);
        let cases: &[(&[&str], String, Option<i32>, &str)] = &[
            (
                &["jest", "--json", "-t", "adds"],
                green.to_string(),
                Some(0),
                "-t runs a subset",
            ),
            (
                &["vitest", "run", "--changed"],
                green.to_string(),
                Some(0),
                "--changed runs a subset",
            ),
            (
                &["jest", "--json"],
                green.to_string(),
                Some(1),
                "disagrees with the observed process exit",
            ),
            (
                &["jest", "--json"],
                green.to_string(),
                None,
                "requires an observed process exit",
            ),
            (
                &["jest", "--json"],
                load_failure.to_string(),
                Some(0),
                "despite failing tests or files",
            ),
            (
                &["jest", "--json"],
                {
                    let mut failed = load_failure.clone();
                    failed["success"] = json!(false);
                    failed.to_string()
                },
                Some(1),
                "failed to load or run",
            ),
            (
                &["jest", "--json"],
                lying.to_string(),
                Some(0),
                "disagrees with its test records",
            ),
            (
                &["jest", "--json"],
                interrupted.to_string(),
                Some(0),
                "interrupted",
            ),
            (
                &["jest", "--json"],
                format!("console.log output\n{green}"),
                Some(0),
                "shares stdout",
            ),
            (
                &["jest", "--json"],
                String::new(),
                Some(1),
                "summary was not observed",
            ),
        ];
        for (args, stdout, exit_code, reason) in cases {
            let result =
                normalize_coding_output(&argv(args), stdout, "", true, *exit_code).unwrap();
            assert!(!result.selection.exhaustive, "{args:?} {reason}");
            assert!(
                result
                    .warnings
                    .iter()
                    .any(|warning| warning.contains(reason)),
                "{args:?}: {reason}: {:?}",
                result.warnings
            );
        }
        let failed = normalize_coding_output(
            &argv(&["jest", "--json"]),
            &{
                let mut failed = load_failure;
                failed["success"] = json!(false);
                failed.to_string()
            },
            "",
            true,
            Some(1),
        )
        .unwrap();
        assert!(
            statuses(&failed)
                .contains(&("/repo/src/broken.test.js".to_string(), "error".to_string()))
        );
    }

    /// Vitest 5 appends its notice to the report line on stdout, and both
    /// runners report absolute file paths.
    #[test]
    fn vitest_stdout_report_and_cwd_relative_ids() {
        let report = jest_report(&[(&["calc", "adds"], "passed")], true);
        let mut vitest = report.clone();
        vitest
            .as_object_mut()
            .unwrap()
            .remove("numRuntimeErrorTestSuites");
        vitest.as_object_mut().unwrap().remove("wasInterrupted");
        let stdout = format!("{vitest}JSON report written to /dev/stdout\n");
        let result = super::super::normalize_coding_output_in(
            Some("/repo/"),
            &argv(&[
                "npx",
                "vitest",
                "run",
                "--reporter=json",
                "--outputFile=/dev/stdout",
            ]),
            &stdout,
            "",
            true,
            Some(0),
        )
        .unwrap();
        assert!(result.selection.exhaustive, "{:?}", result.warnings);
        assert_eq!(result.normalized["runner"], "vitest");
        assert_eq!(
            statuses(&result),
            vec![(
                "src/calc.test.js > calc > adds".to_string(),
                "passed".to_string()
            )]
        );
        assert_eq!(result.selection.scopes, vec!["vitest:all"]);

        // The default `--reporter=json` writes to a file prog never sees.
        let result = normalize_coding_output(
            &argv(&["npx", "vitest", "run", "--reporter=json"]),
            "JSON report written to /repo/.vitest/json/output.json\n",
            "",
            true,
            Some(0),
        )
        .unwrap();
        assert!(!result.selection.exhaustive);
        assert!(
            result.warnings[0].contains("--outputFile=/dev/stdout"),
            "{:?}",
            result.warnings
        );
    }

    /// Jest 30 and Vitest 5 in an agent session print only failures.
    #[test]
    fn text_output_counts_and_names_failures_but_never_proves_a_fix() {
        let jest = "FAIL src/calc.test.js\n  ● calc › adds\n\n    expect(received).toBe(expected)\n\nFAIL src/broken.test.js\n  ● Test suite failed to run\n\nTest Suites: 2 failed, 1 passed, 3 total\nTests:       1 failed, 1 skipped, 1 todo, 2 passed, 5 total\nSnapshots:   0 total\nTime:        0.82 s\nRan all test suites.\n";
        let result =
            normalize_coding_output(&argv(&["npx", "jest"]), "", jest, true, Some(1)).unwrap();
        assert_eq!(result.input_format, "js_test_text");
        assert_eq!(
            statuses(&result),
            vec![
                ("src/broken.test.js".to_string(), "error".to_string()),
                (
                    "src/calc.test.js > calc > adds".to_string(),
                    "failed".to_string()
                )
            ]
        );
        assert_eq!(result.normalized["counts"]["passed"], 2);
        assert_eq!(result.normalized["counts"]["skipped"], 2);
        assert!(!result.selection.exhaustive);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("--json"))
        );

        let vitest = " \u{1b}[31m❯\u{1b}[39m calc.test.js (4 tests | 1 failed) 4ms\n FAIL  broken.test.js [ broken.test.js ]\n FAIL  calc.test.js > calc > adds\nAssertionError: expected +0 to be 4\n\n Test Files  2 failed | 1 passed (3)\n      Tests  1 failed | 2 passed | 1 skipped | 1 todo (5)\n";
        let result =
            normalize_coding_output(&argv(&["pnpm", "test"]), vitest, "", true, Some(1)).unwrap();
        assert_eq!(
            statuses(&result),
            vec![
                ("broken.test.js".to_string(), "error".to_string()),
                (
                    "calc.test.js > calc > adds".to_string(),
                    "failed".to_string()
                )
            ]
        );
        assert!(!result.selection.exhaustive);

        // A green summary with a failing exit is a contradiction.
        let result = normalize_coding_output(
            &argv(&["vitest", "run"]),
            "      Tests  2 passed (2)\n",
            "",
            true,
            Some(1),
        )
        .unwrap();
        assert!(!result.complete, "{:?}", result.warnings);
    }

    #[test]
    fn launches_and_scripts_are_recognized_only_when_safe() {
        let launch_of = |args: &[&str]| {
            let argv = argv(args);
            let program = argv[0].rsplit('/').next().unwrap().to_string();
            match launch(&program, &argv) {
                Some(Launch::Direct(Runner::Jest, rest)) => format!("jest {}", rest.join(" ")),
                Some(Launch::Direct(Runner::Vitest, rest)) => format!("vitest {}", rest.join(" ")),
                Some(Launch::Script(manager, rest)) => format!("{manager}:test {}", rest.join(" ")),
                None => "none".to_string(),
            }
        };
        assert_eq!(
            launch_of(&["npx", "--yes", "jest", "--json"]),
            "jest --json"
        );
        assert_eq!(
            launch_of(&["/repo/node_modules/.bin/vitest", "run"]),
            "vitest run"
        );
        assert_eq!(launch_of(&["pnpm", "exec", "vitest", "run"]), "vitest run");
        assert_eq!(launch_of(&["yarn", "jest", "--ci"]), "jest --ci");
        assert_eq!(
            launch_of(&["npm", "test", "--", "--json"]),
            "npm:test --json"
        );
        assert_eq!(launch_of(&["npm", "run", "test"]), "npm:test ");
        assert_eq!(launch_of(&["bun", "test"]), "none");
        assert_eq!(launch_of(&["npm", "run", "build"]), "none");
        assert_eq!(launch_of(&["npx", "tsc"]), "none");
        // An unrecognized script output stays unparsed.
        assert!(
            normalize_coding_output(&argv(&["npm", "test"]), "all good\n", "", true, Some(0))
                .is_none()
        );
    }
}

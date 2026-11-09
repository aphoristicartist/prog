//! Playwright Test (`playwright test`) results (#312).
//!
//! The text reporters (`list`, `line`, `dot`; `list` is the default) end with
//! one summary that counts every category and names each failed, flaky, and
//! interrupted test. They never name every passing test (`dot` and `line`
//! print none), so text output gives failures and counts but cannot prove a
//! fix. The JSON reporter (`--reporter=json`) names every test with its
//! outcome and is the only Playwright output that can.
//!
//! Formats checked against `@playwright/test` 1.65.0.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::js_test::strip_ansi;
use super::verification::Completion;
use super::{
    CodingProviderResult, InputLine, MAX_PROVIDER_ITEMS, NormalizedTest, bounded_text,
    provider_result,
};
use crate::SelectionCoverage;

const TEXT_ONLY: &str = "Playwright's text reporters do not name every passing test; rerun with `--reporter=json` to prove a fix";
const SKIPPED: &str = "Playwright skipped some tests, so this run alone does not prove the suite passed; to prove a fix, rerun skipping the same tests, named by the JSON report";

/// How the run was started: directly (`playwright test`, `npx playwright
/// test`, `pnpm exec playwright test`), or through a package script whose
/// output turned out to be Playwright's.
pub(super) enum Launch<'a> {
    /// The arguments after `test`.
    Direct(&'a [String]),
    /// `npm test`, `npm run e2e`: the manager and its trailing arguments.
    Script(&'a str, &'a [String]),
}

/// The arguments after `test` when argv starts Playwright Test directly.
pub(super) fn launch<'a>(program: &str, argv: &'a [String]) -> Option<&'a [String]> {
    let after_test = |rest: &'a [String]| -> Option<&'a [String]> {
        let index = rest.iter().position(|arg| !arg.starts_with('-'))?;
        (rest[index] == "playwright" && rest.get(index + 1).map(String::as_str) == Some("test"))
            .then(|| &rest[index + 2..])
    };
    match program {
        "playwright" => (argv.get(1).map(String::as_str) == Some("test")).then(|| &argv[2..]),
        "npx" | "bunx" | "pnpx" => after_test(&argv[1..]),
        "npm" | "pnpm" | "yarn" | "bun" => match argv.get(1).map(String::as_str) {
            Some("exec" | "x") => after_test(&argv[2..]),
            Some("playwright") if matches!(program, "pnpm" | "yarn") => {
                (argv.get(2).map(String::as_str) == Some("test")).then(|| &argv[3..])
            }
            _ => None,
        },
        _ => None,
    }
}

const VALUE_OPTIONS: &[&str] = &[
    "-c",
    "--config",
    "--reporter",
    "--output",
    "--timeout",
    "--retries",
    "--repeat-each",
    "-j",
    "--workers",
    "--global-timeout",
    "--trace",
    "--tsconfig",
    "--browser",
    "--update-source-method",
];

/// Options that run a subset of the selected tests, or none at all.
const FILTER_OPTIONS: &[(&str, bool)] = &[
    ("-g", true),
    ("--grep", true),
    ("--grep-invert", true),
    ("--project", true),
    ("--shard", true),
    ("--test-list", true),
    ("--test-list-invert", true),
    ("--last-failed", false),
    ("--only-changed", false),
    ("--list", false),
    ("--ui", false),
    ("--debug", false),
];

struct Invocation {
    filters: Vec<String>,
    /// `-x` / `--max-failures`: stops after a failure, so later tests may not run.
    max_failures: bool,
    targets: Vec<String>,
    scopes: Vec<String>,
}

fn invocation(launch: &Launch<'_>) -> Invocation {
    let (args, base) = match launch {
        Launch::Direct(args) => (*args, "playwright".to_string()),
        Launch::Script(program, args) => (*args, format!("{program}:script")),
    };
    let mut filters = Vec::new();
    let mut max_failures = false;
    let mut targets = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].as_str();
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg, false), |(name, _)| (name, true));
        if let Some((_, takes_value)) = FILTER_OPTIONS.iter().find(|(option, _)| *option == name) {
            filters.push(name.to_string());
            if *takes_value && !inline {
                index += 1;
            }
        } else if matches!(name, "-x" | "--max-failures") {
            max_failures = true;
            if name == "--max-failures" && !inline {
                index += 1;
            }
        } else if VALUE_OPTIONS.contains(&name) && !inline {
            index += 1;
        } else if !arg.starts_with('-') {
            // `file.spec.ts:12` selects one test, not a file.
            if arg.rsplit_once(':').is_some_and(|(_, line)| {
                !line.is_empty() && line.chars().all(|ch| ch.is_ascii_digit())
            }) {
                filters.push("file:line".to_string());
            }
            targets.push(bounded_text(arg));
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
        filters,
        max_failures,
        targets,
        scopes,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Passed,
    Skipped,
    Failed,
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
    /// Identities seen more than once (two projects with one name, or a
    /// repeated test), which make "the same test" ambiguous.
    repeated: bool,
}

impl Parsed {
    fn record(&mut self, id: String, record: Record) {
        if self.records.contains_key(&id) {
            self.repeated = true;
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

/// `[chromium] › e2e/a.spec.ts:4:3 › suite › title ─────` (or with
/// ` (retry #1)`) as `[chromium] e2e/a.spec.ts > suite > title`: the line and
/// column move with every edit above the test, so they are not part of it.
fn text_test_id(text: &str) -> Option<String> {
    let text = text.trim().trim_end_matches(['─', ' ']);
    let text = text.strip_suffix(')').map_or(text, |rest| {
        rest.rsplit_once(" (retry #").map_or(text, |(head, _)| head)
    });
    let mut parts = text.split(" › ").collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    let project = parts[0]
        .starts_with('[')
        .then(|| parts.remove(0).to_string());
    let location = parts.first_mut()?;
    let mut pieces = location.rsplitn(3, ':');
    let (column, line, file) = (pieces.next()?, pieces.next()?, pieces.next()?);
    if !column.chars().all(|ch| ch.is_ascii_digit()) || !line.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    *location = file;
    let id = parts.join(" > ");
    Some(bounded_text(&match project {
        Some(project) => format!("{project} {id}"),
        None => id,
    }))
}

#[derive(Default)]
struct TextFacts {
    planned: Option<u64>,
    summary: BTreeMap<&'static str, u64>,
    /// Ids named under each summary category.
    named: BTreeMap<&'static str, Vec<String>>,
    /// "N error(s) was/were not a part of any test".
    loose_errors: u64,
    /// Playwright's own `Running N tests using M workers` line, the only
    /// text that claims a package script's output for this provider.
    recognized: bool,
}

const CATEGORIES: &[&str] = &[
    "failed",
    "flaky",
    "skipped",
    "passed",
    "interrupted",
    "did not run",
];

fn summary_line(text: &str) -> Option<(&'static str, u64)> {
    let (number, rest) = text.trim().split_once(' ')?;
    let number = number.parse::<u64>().ok()?;
    let rest = rest.split_once(" (").map_or(rest, |(head, _)| head);
    CATEGORIES
        .iter()
        .find(|category| rest == **category)
        .map(|category| (*category, number))
}

fn parse_text(lines: &[InputLine<'_>]) -> (Parsed, TextFacts) {
    let mut parsed = Parsed::default();
    let mut facts = TextFacts::default();
    // Failure details (`  1) <test> ───`, then the error's first line), and
    // where the summary names each test.
    let mut details = BTreeMap::<String, (Option<String>, usize, &'static str)>::new();
    let mut named_at = BTreeMap::<String, (usize, &'static str)>::new();
    let mut header: Option<String> = None;
    let mut category: Option<&'static str> = None;
    for line in lines {
        let text = strip_ansi(line.text);
        let trimmed = text.trim();
        if let Some(rest) = trimmed.strip_prefix("Running ")
            && let Some((count, tail)) = rest.split_once(' ')
            && (tail.starts_with("test using ") || tail.starts_with("tests using "))
            && let Ok(count) = count.parse::<u64>()
        {
            facts.planned = Some(count);
            facts.recognized = true;
            continue;
        }
        // The summary follows the failure details: category lines are
        // indented two spaces, and the tests they name four.
        if text.starts_with("  ")
            && !text.starts_with("   ")
            && let Some((name, count)) = summary_line(trimmed)
        {
            facts.summary.insert(name, count);
            category = matches!(name, "failed" | "flaky" | "interrupted").then_some(name);
            header = None;
            continue;
        }
        if let Some((errors, _)) = trimmed
            .split_once(" error was not a part of any test")
            .or_else(|| trimmed.split_once(" errors were not a part of any test"))
            && let Ok(errors) = errors.parse::<u64>()
        {
            facts.loose_errors = errors;
            category = None;
            continue;
        }
        if let Some(name) = category {
            if text.starts_with("    ")
                && let Some(id) = text_test_id(trimmed)
            {
                facts.named.entry(name).or_default().push(id.clone());
                named_at.entry(id).or_insert((line.number, line.stream));
                continue;
            }
            category = None;
        }
        if let Some((number, rest)) = trimmed.split_once(") ")
            && !number.is_empty()
            && number.chars().all(|ch| ch.is_ascii_digit())
            && let Some(id) = text_test_id(rest)
        {
            details
                .entry(id.clone())
                .or_insert((None, line.number, line.stream));
            header = Some(id);
            continue;
        }
        if let Some(id) = &header
            && !trimmed.is_empty()
            && !trimmed.starts_with("Retry #")
        {
            if let Some(entry) = details.get_mut(id)
                && entry.0.is_none()
            {
                entry.0 = Some(bounded_text(trimmed));
            }
            header = None;
        }
    }
    for (name, status) in [
        ("failed", Status::Failed),
        ("interrupted", Status::Error),
        ("flaky", Status::Passed),
    ] {
        for id in facts.named.get(name).into_iter().flatten() {
            let (detail, number, stream) = details
                .get(id)
                .map(|(message, number, stream)| (message.clone(), *number, *stream))
                .or_else(|| {
                    named_at
                        .get(id)
                        .map(|(number, stream)| (None, *number, *stream))
                })
                .unwrap_or((None, 0, "stdout"));
            let message = match name {
                "flaky" => Some("flaky: passed only on retry".to_string()),
                "interrupted" => Some("interrupted before it finished".to_string()),
                _ => detail,
            };
            parsed.record(
                id.clone(),
                Record {
                    status,
                    message,
                    stream,
                    line: (number > 0).then_some(number),
                    pointer: None,
                },
            );
        }
    }
    (parsed, facts)
}

fn text_completion(parsed: &Parsed, facts: &TextFacts, exit_code: Option<i32>) -> Completion {
    if facts.summary.is_empty() {
        return Completion::Unproven("Playwright completion summary was not observed");
    }
    let Some(exit_code) = exit_code else {
        return Completion::Unproven(
            "Playwright completion requires an observed process exit code",
        );
    };
    let count = |name: &str| facts.summary.get(name).copied().unwrap_or(0);
    let failing = count("failed") + count("interrupted") + facts.loose_errors > 0;
    if failing != (exit_code != 0) {
        return Completion::Conflicting(
            "Playwright summary disagrees with the observed process exit",
        );
    }
    for name in ["failed", "flaky", "interrupted"] {
        let named = facts.named.get(name).map_or(0, Vec::len) as u64;
        if named != count(name) {
            return Completion::Conflicting(
                "Playwright summary names a different number of tests than it counts",
            );
        }
    }
    if parsed.repeated {
        return Completion::Conflicting("Playwright summary names one test twice");
    }
    if let Some(planned) = facts.planned
        && planned != CATEGORIES.iter().map(|name| count(name)).sum::<u64>()
    {
        return Completion::Conflicting(
            "Playwright summary counts a different number of tests than it planned to run",
        );
    }
    Completion::Unproven(TEXT_ONLY)
}

enum Report {
    None,
    One(Value),
    Embedded,
}

fn is_report(value: &Value) -> bool {
    value.get("suites").is_some_and(Value::is_array)
        && value.get("stats").is_some_and(|stats| {
            ["expected", "unexpected", "flaky", "skipped"]
                .iter()
                .all(|key| stats.get(key).is_some_and(Value::is_u64))
        })
        && value.get("config").is_some()
}

/// Package managers frame a script's output (`> pkg@1.0.0 e2e`, `$ playwright
/// test`); nothing else may share stdout with the report.
fn is_frame_line(text: &str) -> bool {
    text.is_empty()
        || text.starts_with("> ")
        || text.starts_with("$ ")
        || text.starts_with("yarn run v")
        || text.starts_with("Done in ")
}

fn json_report(stdout: &str) -> Report {
    let trimmed = stdout.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && is_report(&value)
    {
        return Report::One(value);
    }
    // A pretty-printed report after a package-manager banner.
    if let Some(start) = trimmed.find("\n{")
        && trimmed[..start]
            .lines()
            .all(|line| is_frame_line(line.trim()))
    {
        let body = &trimmed[start + 1..];
        let end = body.rfind('}').map_or(body.len(), |end| end + 1);
        if body[end..].lines().all(|line| is_frame_line(line.trim()))
            && let Ok(value) = serde_json::from_str::<Value>(&body[..end])
            && is_report(&value)
        {
            return Report::One(value);
        }
    }
    if trimmed.contains("\"suites\"") && trimmed.contains("\"stats\"") {
        return Report::Embedded;
    }
    Report::None
}

struct ReportFacts {
    stats: BTreeMap<&'static str, u64>,
    observed: BTreeMap<&'static str, u64>,
    flaky: u64,
    interrupted: u64,
    did_not_run: u64,
    load_errors: u64,
    stopped_early: bool,
}

fn first_line(text: &str) -> Option<String> {
    strip_ansi(text)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(bounded_text)
}

fn parse_report(report: &Value) -> (Parsed, ReportFacts) {
    let mut parsed = Parsed::default();
    let mut facts = ReportFacts {
        stats: ["expected", "unexpected", "flaky", "skipped"]
            .iter()
            .map(|key| (*key, report["stats"][key].as_u64().unwrap_or(0)))
            .collect(),
        observed: BTreeMap::new(),
        flaky: 0,
        interrupted: 0,
        did_not_run: 0,
        load_errors: 0,
        stopped_early: false,
    };
    fn walk(
        suite: &Value,
        pointer: String,
        titles: &[String],
        parsed: &mut Parsed,
        facts: &mut ReportFacts,
    ) {
        for (spec_index, spec) in suite["specs"].as_array().into_iter().flatten().enumerate() {
            let (Some(file), Some(title)) = (spec["file"].as_str(), spec["title"].as_str()) else {
                parsed.malformed = true;
                continue;
            };
            for (test_index, test) in spec["tests"].as_array().into_iter().flatten().enumerate() {
                let Some(raw) = test["status"].as_str() else {
                    parsed.malformed = true;
                    continue;
                };
                *facts
                    .observed
                    .entry(match raw {
                        "expected" => "expected",
                        "unexpected" => "unexpected",
                        "flaky" => "flaky",
                        "skipped" => "skipped",
                        _ => {
                            parsed.malformed = true;
                            continue;
                        }
                    })
                    .or_insert(0) += 1;
                let results = test["results"].as_array().map_or(&[][..], Vec::as_slice);
                let last = results.last().and_then(|result| result["status"].as_str());
                let (status, message) = match raw {
                    // `test.fail()` that failed as expected never ran the
                    // assertion it guards: like a skip, it proves nothing.
                    "expected" if test["expectedStatus"] != "passed" => {
                        (Status::Skipped, Some("expected to fail".to_string()))
                    }
                    "expected" => (Status::Passed, None),
                    "flaky" => {
                        facts.flaky += 1;
                        (
                            Status::Passed,
                            Some("flaky: passed only on retry".to_string()),
                        )
                    }
                    "skipped" if last == Some("interrupted") => {
                        facts.interrupted += 1;
                        (
                            Status::Error,
                            Some("interrupted before it finished".to_string()),
                        )
                    }
                    "skipped" if results.is_empty() => {
                        facts.did_not_run += 1;
                        (Status::Skipped, Some("did not run".to_string()))
                    }
                    "skipped" => (Status::Skipped, None),
                    _ => {
                        let message = results
                            .iter()
                            .rev()
                            .find_map(|result| {
                                result["errors"][0]["message"]
                                    .as_str()
                                    .or_else(|| result["error"]["message"].as_str())
                            })
                            .and_then(first_line)
                            .or_else(|| last.map(|status| format!("test {status}")));
                        (Status::Failed, message)
                    }
                };
                let path = titles
                    .iter()
                    .map(String::as_str)
                    .chain([title])
                    .collect::<Vec<_>>()
                    .join(" > ");
                let id = format!("{file} > {path}");
                let id = match test["projectName"].as_str().filter(|name| !name.is_empty()) {
                    Some(project) => format!("[{project}] {id}"),
                    None => id,
                };
                parsed.record(
                    bounded_text(&id),
                    Record {
                        status,
                        message,
                        stream: "stdout",
                        line: None,
                        pointer: Some(format!("{pointer}/specs/{spec_index}/tests/{test_index}")),
                    },
                );
            }
        }
        for (index, child) in suite["suites"].as_array().into_iter().flatten().enumerate() {
            let mut titles = titles.to_vec();
            titles.push(child["title"].as_str().unwrap_or("").to_string());
            walk(
                child,
                format!("{pointer}/suites/{index}"),
                &titles,
                parsed,
                facts,
            );
        }
    }
    // A file-level suite's title is the file itself, already in the id.
    for (index, file) in report["suites"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        walk(
            file,
            format!("/suites/{index}"),
            &[],
            &mut parsed,
            &mut facts,
        );
    }
    for (index, error) in report["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        // `-x`/`--max-failures` reports its stop as an error outside any
        // test; the tests it cut off are already marked as not run.
        if error["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("Testing stopped early after"))
        {
            facts.stopped_early = true;
            continue;
        }
        facts.load_errors += 1;
        let file = error["location"]["file"]
            .as_str()
            .and_then(|file| std::path::Path::new(file).file_name())
            .and_then(|name| name.to_str());
        let id = match file {
            Some(file) => format!("{file} (error outside any test)"),
            None => format!("error outside any test #{}", index + 1),
        };
        parsed.record(
            bounded_text(&id),
            Record {
                status: Status::Error,
                message: error["message"]
                    .as_str()
                    .and_then(first_line)
                    .or_else(|| Some("error outside any test".to_string())),
                stream: "stdout",
                line: None,
                pointer: Some(format!("/errors/{index}")),
            },
        );
    }
    (parsed, facts)
}

fn report_completion(parsed: &Parsed, facts: &ReportFacts, exit_code: Option<i32>) -> Completion {
    let Some(exit_code) = exit_code else {
        return Completion::Unproven(
            "Playwright completion requires an observed process exit code",
        );
    };
    if facts.stats != facts.observed_with_zeroes() {
        return Completion::Conflicting("Playwright JSON stats disagree with its test records");
    }
    if parsed.repeated {
        return Completion::Conflicting("Playwright JSON report repeats a test identity");
    }
    let failing = facts.stats["unexpected"] > 0 || facts.load_errors > 0;
    if failing != (exit_code != 0) {
        return Completion::Conflicting(
            "Playwright JSON report disagrees with the observed process exit",
        );
    }
    if facts.load_errors > 0 {
        return Completion::Unproven(
            "a test file or global setup failed outside any test, so its tests never ran",
        );
    }
    if facts.interrupted > 0 || facts.did_not_run > 0 || facts.stopped_early {
        return Completion::Unproven(
            "Playwright stopped early (interrupted or max failures), so some tests did not run",
        );
    }
    if facts.flaky > 0 {
        return Completion::Unproven(
            "a flaky test passed only on retry, which does not prove it fixed",
        );
    }
    if parsed.count(Status::Passed) + parsed.count(Status::Failed) == 0 {
        return Completion::Unproven("Playwright ran no tests");
    }
    if parsed.count(Status::Skipped) > 0 {
        return Completion::Skipped(SKIPPED);
    }
    Completion::Consistent
}

impl ReportFacts {
    fn observed_with_zeroes(&self) -> BTreeMap<&'static str, u64> {
        ["expected", "unexpected", "flaky", "skipped"]
            .iter()
            .map(|key| (*key, self.observed.get(key).copied().unwrap_or(0)))
            .collect()
    }
}

/// Normalize a Playwright run. For a package script, `None` unless the
/// output is recognizably Playwright's.
#[allow(clippy::too_many_arguments)]
pub(super) fn normalize(
    launch: &Launch<'_>,
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
    let (parsed, completion, counts, input_format, summary_seen) = match report {
        Report::One(report) => {
            let (parsed, facts) = parse_report(&report);
            let completion = report_completion(&parsed, &facts, exit_code);
            let passed = parsed.count(Status::Passed);
            let failed = parsed.count(Status::Failed);
            let counts = json!({
                "passed": passed,
                "failed": failed,
                "error": parsed.count(Status::Error),
                "skipped": parsed.count(Status::Skipped),
                "flaky": facts.flaky,
                "xfailed": 0,
                "xpassed": 0,
                "executed": passed + failed
            });
            (
                parsed,
                completion,
                Some(counts),
                "playwright_json_report",
                true,
            )
        }
        Report::Embedded => (
            Parsed::default(),
            Completion::Conflicting(
                "Playwright JSON report shares stdout with other output; completion is unproven",
            ),
            None,
            "playwright_json_report",
            false,
        ),
        Report::None => {
            let (parsed, facts) = parse_text(lines);
            if matches!(launch, Launch::Script(..)) && !facts.recognized {
                return None;
            }
            let completion = text_completion(&parsed, &facts, exit_code);
            let count = |name: &str| facts.summary.get(name).copied().unwrap_or(0);
            let counts = (!facts.summary.is_empty()).then(|| {
                let passed = count("passed") + count("flaky");
                json!({
                    "passed": passed,
                    "failed": count("failed"),
                    "error": count("interrupted") + facts.loose_errors,
                    "skipped": count("skipped") + count("did not run"),
                    "flaky": count("flaky"),
                    "xfailed": 0,
                    "xpassed": 0,
                    "executed": passed + count("failed")
                })
            });
            let summary_seen = !facts.summary.is_empty();
            (parsed, completion, counts, "playwright_text", summary_seen)
        }
    };
    let bound_hit = bound_hit || parsed.bound_hit;
    let complete = capture_complete && !bound_hit && !parsed.malformed;
    let failing = parsed
        .records
        .values()
        .any(|record| matches!(record.status, Status::Failed | Status::Error));
    let selection = SelectionCoverage {
        scopes: invocation.scopes.clone(),
        exhaustive: complete
            && summary_seen
            && invocation.filters.is_empty()
            && !(invocation.max_failures && failing),
        ..SelectionCoverage::default()
    };
    let mut warnings = Vec::new();
    if parsed.malformed {
        warnings.push("Playwright JSON report has records prog cannot read".to_string());
    }
    if !summary_seen {
        warnings.push(
            "Playwright completion summary was not observed; selection exhaustiveness is unproven"
                .to_string(),
        );
    }
    if !invocation.filters.is_empty() {
        warnings.push(format!(
            "{} runs a subset of the tests; broader absence is unprovable",
            invocation.filters.join(" ")
        ));
    }
    if invocation.max_failures && failing {
        warnings.push(
            "-x/--max-failures stops after a failure, so later tests may not have run; rerun without it to prove selection exhaustion"
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
        "playwright.v1",
        input_format,
        if input_format == "playwright_json_report" {
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
            "runner": "playwright",
            "filters": invocation.filters,
            "max_failures": invocation.max_failures,
            "targets": invocation.targets
        }),
        warnings,
    );
    Some((result, completion))
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

    /// The default `list` reporter, `@playwright/test` 1.65.0, one failing,
    /// one passing, and one skipped test (source excerpt trimmed).
    const LIST_FAILING: &str = "
Running 3 tests using 1 worker

  \u{2718}  1 e2e/calc.spec.js:4:3 \u{203a} calc \u{203a} adds (3ms)
  \u{2713}  2 e2e/calc.spec.js:5:3 \u{203a} calc \u{203a} muls (2ms)
  -  3 e2e/calc.spec.js:6:8 \u{203a} calc \u{203a} later


  1) e2e/calc.spec.js:4:3 \u{203a} calc \u{203a} adds \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}

    Error: expect(received).toBe(expected) // Object.is equality

    Expected: 5
    Received: -1

        at /repo/e2e/calc.spec.js:4:42

    Error Context: test-results/calc-calc-adds/error-context.md

  1 failed
    e2e/calc.spec.js:4:3 \u{203a} calc \u{203a} adds \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}
  1 skipped
  1 passed (488ms)
";

    #[test]
    fn launches_are_recognized_only_for_playwright_test() {
        for (command, expected) in [
            (&["playwright", "test", "-x"][..], Some(1)),
            (&["npx", "playwright", "test"], Some(0)),
            (
                &["npx", "--yes", "playwright", "test", "--grep", "a"],
                Some(2),
            ),
            (&["pnpm", "exec", "playwright", "test"], Some(0)),
            (&["npm", "x", "playwright", "test"], Some(0)),
            (&["bun", "x", "playwright", "test"], Some(0)),
            (&["pnpm", "playwright", "test"], Some(0)),
            (&["yarn", "playwright", "test", "e2e"], Some(1)),
            (&["npx", "playwright", "install"], None),
            (&["playwright", "codegen"], None),
            (&["npm", "playwright", "test"], None),
            (&["npx", "jest"], None),
        ] {
            let argv = argv(command);
            let program = argv[0].clone();
            assert_eq!(
                launch(&program, &argv).map(<[String]>::len),
                expected,
                "{command:?}"
            );
        }
    }

    #[test]
    fn text_reporters_name_failures_and_counts_but_never_prove_a_fix() {
        let result = normalize_coding_output(
            &argv(&["npx", "playwright", "test"]),
            LIST_FAILING,
            "",
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(result.provider, "playwright.v1");
        assert_eq!(result.input_format, "playwright_text");
        assert!(result.complete);
        assert!(!result.selection.exhaustive);
        assert_eq!(
            statuses(&result),
            [(
                "e2e/calc.spec.js > calc > adds".to_string(),
                "failed".to_string()
            )]
        );
        assert_eq!(
            result.normalized["tests"][0]["message"],
            "Error: expect(received).toBe(expected) // Object.is equality"
        );
        let counts = &result.normalized["counts"];
        assert_eq!(
            (
                counts["passed"].as_u64(),
                counts["failed"].as_u64(),
                counts["skipped"].as_u64()
            ),
            (Some(1), Some(1), Some(1))
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("--reporter=json"))
        );
        // An exit that contradicts the summary makes the run incomplete.
        let lying = normalize_coding_output(
            &argv(&["playwright", "test"]),
            LIST_FAILING,
            "",
            true,
            Some(0),
        )
        .unwrap();
        assert!(!lying.complete);
    }

    #[test]
    fn text_summary_accounts_for_flaky_interrupted_and_unrun_tests() {
        let text = "
Running 7 tests using 2 workers

  1 failed
    [chromium] \u{203a} e2e/a.spec.ts:2:1 \u{203a} slow \u{2500}\u{2500}
  1 flaky
    [chromium] \u{203a} e2e/a.spec.ts:9:1 \u{203a} flaky (retry #1) \u{2500}\u{2500}
  1 interrupted
    [chromium] \u{203a} e2e/b.spec.ts:4:3 \u{203a} suite \u{203a} cut \u{2500}\u{2500}
  2 did not run
  1 error was not a part of any test, see above for details
  2 passed (1.2s)
";
        let result = normalize_coding_output(
            &argv(&["playwright", "test", "-x"]),
            text,
            "",
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(
            statuses(&result),
            [
                (
                    "[chromium] e2e/a.spec.ts > flaky".to_string(),
                    "passed".to_string()
                ),
                (
                    "[chromium] e2e/a.spec.ts > slow".to_string(),
                    "failed".to_string()
                ),
                (
                    "[chromium] e2e/b.spec.ts > suite > cut".to_string(),
                    "error".to_string()
                ),
            ]
        );
        let counts = &result.normalized["counts"];
        assert_eq!(counts["passed"], 3);
        assert_eq!(counts["error"], 2);
        assert_eq!(counts["skipped"], 2);
        assert!(result.complete);
        assert!(!result.selection.exhaustive);
        // A summary that names fewer failures than it counts is not trusted.
        let short = text.replace("  1 failed\n", "  2 failed\n");
        let result =
            normalize_coding_output(&argv(&["playwright", "test"]), &short, "", true, Some(1))
                .unwrap();
        assert!(!result.complete);
    }

    fn json_test(
        title: &str,
        project: &str,
        expected: &str,
        status: &str,
        results: Value,
    ) -> Value {
        json!({
            "title": title, "file": "a.spec.ts", "line": 3, "column": 5,
            "tests": [{"projectName": project, "expectedStatus": expected, "status": status, "results": results}]
        })
    }

    fn json_report(specs: Vec<Value>, stats: Value, errors: Value) -> String {
        json!({
            "config": {},
            "suites": [{"title": "a.spec.ts", "file": "a.spec.ts", "specs": [],
                        "suites": [{"title": "suite", "file": "a.spec.ts", "specs": specs, "suites": []}]}],
            "errors": errors,
            "stats": stats
        })
        .to_string()
    }

    #[test]
    fn json_report_names_every_test_and_maps_outcomes_conservatively() {
        let specs = vec![
            json_test(
                "adds",
                "chromium",
                "passed",
                "expected",
                json!([{"status": "passed"}]),
            ),
            json_test(
                "adds",
                "firefox",
                "passed",
                "unexpected",
                json!([{"status": "failed", "errors": [{"message": "Error: boom\n  at x"}]}]),
            ),
            json_test(
                "known bug",
                "chromium",
                "failed",
                "expected",
                json!([{"status": "failed"}]),
            ),
            json_test(
                "later",
                "chromium",
                "skipped",
                "skipped",
                json!([{"status": "skipped"}]),
            ),
        ];
        let stats = json!({"expected": 2, "unexpected": 1, "flaky": 0, "skipped": 1});
        let result = normalize_coding_output(
            &argv(&["npx", "playwright", "test", "--reporter=json"]),
            &json_report(specs, stats, json!([])),
            "",
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(result.input_format, "playwright_json_report");
        assert!(result.complete);
        assert_eq!(
            statuses(&result),
            [
                (
                    "[chromium] a.spec.ts > suite > adds".to_string(),
                    "passed".to_string()
                ),
                (
                    "[chromium] a.spec.ts > suite > known bug".to_string(),
                    "skipped".to_string()
                ),
                (
                    "[chromium] a.spec.ts > suite > later".to_string(),
                    "skipped".to_string()
                ),
                (
                    "[firefox] a.spec.ts > suite > adds".to_string(),
                    "failed".to_string()
                ),
            ]
        );
        assert_eq!(result.normalized["tests"][3]["message"], "Error: boom");
        assert_eq!(
            result.normalized["tests"][3]["report_pointer"],
            "/suites/0/suites/0/specs/1/tests/0"
        );
    }

    #[test]
    fn json_report_that_disagrees_with_itself_or_the_exit_is_incomplete() {
        let specs = vec![json_test(
            "adds",
            "",
            "passed",
            "expected",
            json!([{"status": "passed"}]),
        )];
        for (stats, exit) in [
            // Stats count a test the records do not contain.
            (
                json!({"expected": 2, "unexpected": 0, "flaky": 0, "skipped": 0}),
                0,
            ),
            // A passing report with a failing exit.
            (
                json!({"expected": 1, "unexpected": 0, "flaky": 0, "skipped": 0}),
                1,
            ),
        ] {
            let result = normalize_coding_output(
                &argv(&["playwright", "test", "--reporter=json"]),
                &json_report(specs.clone(), stats, json!([])),
                "",
                true,
                Some(exit),
            )
            .unwrap();
            assert!(!result.complete, "{:?}", result.warnings);
        }
        // A report sharing stdout with other output proves nothing.
        let embedded = format!(
            "console noise\n{}",
            json_report(
                specs,
                json!({"expected": 1, "unexpected": 0, "flaky": 0, "skipped": 0}),
                json!([])
            )
        );
        let result = normalize_coding_output(
            &argv(&["playwright", "test", "--reporter=json"]),
            &embedded,
            "",
            true,
            Some(0),
        )
        .unwrap();
        assert!(!result.complete);
    }

    #[test]
    fn package_scripts_are_claimed_only_for_playwright_output() {
        let script = argv(&["npm", "run", "e2e"]);
        let framed = format!("\n> app@1.0.0 e2e\n> playwright test\n{LIST_FAILING}");
        let result = normalize_coding_output(&script, &framed, "", true, Some(1)).unwrap();
        assert_eq!(result.provider, "playwright.v1");
        assert_eq!(result.selection.scopes, ["npm:script:all"]);
        assert!(normalize_coding_output(&script, "built in 2s\n", "", true, Some(0)).is_none());
        // A summary-shaped line alone is not Playwright's output.
        assert!(normalize_coding_output(&script, "  2 passed (1s)\n", "", true, Some(0)).is_none());
        assert!(
            normalize_coding_output(
                &argv(&["npm", "run", "build"]),
                "Running 3 tests\n",
                "",
                true,
                Some(0)
            )
            .is_none()
        );
    }
}

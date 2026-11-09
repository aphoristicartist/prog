//! `go test`: `-json` events (`go test -json`, test2json), or text output with
//! `=== RUN`, `--- PASS/FAIL/SKIP:` and per-package `ok`/`FAIL`/`?` lines.
//!
//! Go prints no test counts, so a run names its tests only with `-v` or
//! `-json`. Without them, passing and skipped tests are invisible, and the run
//! cannot show that its selection was covered.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::verification::Completion;
use super::{
    CodingProviderResult, InputLine, MAX_PROVIDER_ITEMS, NormalizedTest, bounded_text,
    provider_result,
};
use crate::SelectionCoverage;

pub(super) const SKIPPED: &str = "go test skipped some tests, so this run alone does not prove the suite passed; to prove a fix, rerun skipping the same tests, named by `go test -v`";

/// `go test` flags whose value is the next argument (`-run X`). Flags may
/// start with one or two dashes, and the `test.` prefix is accepted.
const VALUE_FLAGS: &[&str] = &[
    "bench",
    "benchtime",
    "blockprofile",
    "blockprofilerate",
    "count",
    "coverpkg",
    "covermode",
    "coverprofile",
    "cpu",
    "cpuprofile",
    "exec",
    "fuzz",
    "fuzzcachedir",
    "fuzzminimizetime",
    "fuzztime",
    "list",
    "memprofile",
    "memprofilerate",
    "mutexprofile",
    "mutexprofilefraction",
    "o",
    "outputdir",
    "p",
    "parallel",
    "run",
    "shuffle",
    "skip",
    "timeout",
    "trace",
    "vet",
    "asmflags",
    "buildmode",
    "buildvcs",
    "C",
    "compiler",
    "gccgoflags",
    "gcflags",
    "installsuffix",
    "ldflags",
    "mod",
    "modfile",
    "overlay",
    "pgo",
    "pkgdir",
    "tags",
    "toolexec",
];

struct Invocation {
    json: bool,
    verbose: bool,
    failfast: bool,
    /// `-run`, `-skip`, `-list`, `-fuzz`, `-c`, `-n`: not every selected test runs.
    filters: Vec<&'static str>,
    packages: Vec<String>,
}

fn flag_name(arg: &str) -> Option<(&str, Option<&str>)> {
    let flag = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-'))?;
    if flag.is_empty() {
        return None;
    }
    let (name, value) = flag
        .split_once('=')
        .map_or((flag, None), |(name, value)| (name, Some(value)));
    Some((name.strip_prefix("test.").unwrap_or(name), value))
}

fn enabled(value: Option<&str>) -> bool {
    value.is_none_or(|value| value != "false" && value != "0")
}

fn invocation(args: &[String]) -> Invocation {
    let mut invocation = Invocation {
        json: false,
        verbose: false,
        failfast: false,
        filters: Vec::new(),
        packages: Vec::new(),
    };
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        // Everything after `-args` goes to the test binary.
        if arg == "-args" || arg == "--args" {
            break;
        }
        let Some((name, value)) = flag_name(arg) else {
            invocation.packages.push(bounded_text(arg));
            index += 1;
            continue;
        };
        match name {
            "json" => invocation.json = enabled(value),
            "v" => invocation.verbose = enabled(value),
            "failfast" => invocation.failfast = enabled(value),
            "run" => invocation.filters.push("-run"),
            "skip" => invocation.filters.push("-skip"),
            "list" => invocation.filters.push("-list"),
            "fuzz" => invocation.filters.push("-fuzz"),
            "c" if enabled(value) => invocation.filters.push("-c"),
            "n" if enabled(value) => invocation.filters.push("-n"),
            _ => {}
        }
        if value.is_none() && VALUE_FLAGS.contains(&name) {
            index += 1;
        }
        index += 1;
    }
    invocation.packages.sort();
    invocation.packages.dedup();
    invocation
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Passed,
    Skipped,
    Failed,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    /// `-count=N` repeats a test; its worst outcome wins.
    fn rank(self) -> u8 {
        match self {
            Self::Passed => 0,
            Self::Skipped => 1,
            Self::Failed => 2,
        }
    }
}

struct Record {
    status: Status,
    package: String,
    message: Option<String>,
    stream: &'static str,
    line: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PackageResult {
    Ok,
    Failed,
    NoTestFiles,
}

#[derive(Default)]
struct Parsed {
    records: BTreeMap<String, Record>,
    packages: BTreeMap<String, PackageResult>,
    /// Packages that produced events but never reported a package result.
    unfinished_packages: BTreeSet<String>,
    /// Tests that started (`=== RUN`, `run`) without a result.
    unfinished_tests: BTreeSet<String>,
    build_failed: bool,
    panicked: bool,
    no_tests_to_run: bool,
    /// Test results seen after the last package result (text output).
    trailing_results: bool,
    malformed: bool,
    verbose: bool,
    bound_hit: bool,
}

impl Parsed {
    fn record(&mut self, id: String, status: Status, record: Record) {
        if let Some(existing) = self.records.get_mut(&id) {
            if status.rank() > existing.status.rank() {
                existing.status = status;
                existing.message = record.message.or(existing.message.take());
            }
            return;
        }
        if self.records.len() >= MAX_PROVIDER_ITEMS {
            self.bound_hit = true;
            return;
        }
        self.records.insert(id, Record { status, ..record });
    }

    fn count(&self, status: Status) -> u64 {
        self.records
            .values()
            .filter(|record| record.status == status)
            .count() as u64
    }

    fn failed_packages_without_failed_tests(&self) -> bool {
        self.packages.iter().any(|(package, result)| {
            *result == PackageResult::Failed
                && !self
                    .records
                    .values()
                    .any(|record| record.status == Status::Failed && record.package == *package)
        })
    }
}

fn test_id(package: &str, test: &str) -> String {
    bounded_text(&format!("{package}.{test}"))
}

fn result_line(text: &str) -> Option<(Status, &str)> {
    let rest = text.trim_start().strip_prefix("--- ")?;
    let (status, rest) = rest.split_once(": ")?;
    let status = match status {
        "PASS" => Status::Passed,
        "FAIL" => Status::Failed,
        "SKIP" => Status::Skipped,
        _ => return None,
    };
    let (name, elapsed) = rest.rsplit_once(" (")?;
    (elapsed.ends_with("s)") && !name.is_empty() && !name.contains(char::is_whitespace))
        .then_some((status, name))
}

/// `ok  \tpkg\t0.1s`, `FAIL\tpkg [build failed]`, `?   \tpkg\t[no test files]`.
fn package_line(text: &str) -> Option<(PackageResult, &str, &str)> {
    // Go prints package results at column 0; test logs are indented.
    if text.starts_with(char::is_whitespace) {
        return None;
    }
    let mut parts = text.split_whitespace();
    let result = match parts.next()? {
        "ok" => PackageResult::Ok,
        "FAIL" => PackageResult::Failed,
        "?" => PackageResult::NoTestFiles,
        _ => return None,
    };
    let package = parts.next()?;
    let rest = text.split_once(package).map_or("", |(_, rest)| rest).trim();
    if package.starts_with('[') || rest.is_empty() && result != PackageResult::Failed {
        return None;
    }
    Some((result, package, rest))
}

fn log_message(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let (location, message) = trimmed.split_once(": ")?;
    (location.contains(".go:") && !message.is_empty()).then(|| bounded_text(trimmed))
}

fn parse_text(lines: &[InputLine<'_>]) -> Parsed {
    let mut parsed = Parsed::default();
    // Results and run starts since the last package line.
    let mut chunk: Vec<(String, Status, Option<String>, &'static str, usize)> = Vec::new();
    let mut started: BTreeSet<String> = BTreeSet::new();
    let mut last_log: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    for (index, line) in lines.iter().enumerate() {
        let text = line.text;
        if text.starts_with("panic: ") {
            parsed.panicked = true;
        }
        if let Some(name) = text.strip_prefix("=== RUN ") {
            let name = name.trim();
            parsed.verbose = true;
            started.insert(name.to_string());
            current = Some(name.to_string());
            continue;
        }
        if let Some(name) = ["=== CONT ", "=== NAME ", "=== PAUSE "]
            .iter()
            .find_map(|prefix| text.strip_prefix(prefix))
        {
            current = Some(name.trim().to_string());
            continue;
        }
        if let Some((status, name)) = result_line(text) {
            started.remove(name);
            // Verbose output logs before the result; quiet output after it.
            let message = (status == Status::Failed)
                .then(|| {
                    last_log.remove(name).or_else(|| {
                        lines
                            .get(index + 1)
                            .filter(|next| next.stream == line.stream)
                            .and_then(|next| log_message(next.text))
                    })
                })
                .flatten();
            chunk.push((name.to_string(), status, message, line.stream, line.number));
            current = None;
            continue;
        }
        if let Some((result, package, rest)) = package_line(text) {
            parsed.packages.insert(package.to_string(), result);
            if rest.contains("[build failed]") || rest.contains("[setup failed]") {
                parsed.build_failed = true;
            }
            if rest.contains("[no tests to run]") {
                parsed.no_tests_to_run = true;
            }
            for (name, status, message, stream, number) in chunk.drain(..) {
                parsed.record(
                    test_id(package, &name),
                    status,
                    Record {
                        status,
                        package: package.to_string(),
                        message,
                        stream,
                        line: number,
                    },
                );
            }
            parsed.unfinished_tests.extend(
                started
                    .iter()
                    .map(|name| test_id(package, name))
                    .collect::<Vec<_>>(),
            );
            started.clear();
            last_log.clear();
            current = None;
            continue;
        }
        if let (Some(name), Some(message)) = (&current, log_message(text)) {
            last_log.entry(name.clone()).or_insert(message);
        }
    }
    parsed.trailing_results = !chunk.is_empty() || !started.is_empty();
    parsed
}

fn parse_json(stdout_lines: &[&InputLine<'_>]) -> Parsed {
    let mut parsed = Parsed {
        verbose: true,
        ..Parsed::default()
    };
    let mut started_packages = BTreeSet::new();
    let mut started_tests = BTreeSet::new();
    let mut messages: BTreeMap<String, String> = BTreeMap::new();
    for line in stdout_lines {
        let trimmed = line.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(trimmed) else {
            parsed.malformed = true;
            continue;
        };
        let action = event.get("Action").and_then(Value::as_str);
        if event.get("ImportPath").is_some() {
            if action == Some("build-fail") {
                parsed.build_failed = true;
            }
            continue;
        }
        let (Some(action), Some(package)) = (action, event.get("Package").and_then(Value::as_str))
        else {
            parsed.malformed = true;
            continue;
        };
        started_packages.insert(package.to_string());
        let output = event.get("Output").and_then(Value::as_str);
        if output.is_some_and(|output| output.starts_with("panic: ")) {
            parsed.panicked = true;
        }
        match event.get("Test").and_then(Value::as_str) {
            Some(test) => {
                let id = test_id(package, test);
                match action {
                    "run" => {
                        started_tests.insert(id);
                    }
                    "output" => {
                        if let Some(message) = output.and_then(log_message) {
                            messages.entry(id).or_insert(message);
                        }
                    }
                    "pass" | "fail" | "skip" => {
                        started_tests.remove(&id);
                        let status = match action {
                            "pass" => Status::Passed,
                            "fail" => Status::Failed,
                            _ => Status::Skipped,
                        };
                        let message = (status == Status::Failed)
                            .then(|| messages.remove(&id))
                            .flatten();
                        parsed.record(
                            id,
                            status,
                            Record {
                                status,
                                package: package.to_string(),
                                message,
                                stream: line.stream,
                                line: line.number,
                            },
                        );
                    }
                    _ => {}
                }
            }
            None => match action {
                "pass" | "fail" | "skip" => {
                    let result = match action {
                        "pass" => PackageResult::Ok,
                        "fail" => PackageResult::Failed,
                        _ => PackageResult::NoTestFiles,
                    };
                    if event.get("FailedBuild").is_some() {
                        parsed.build_failed = true;
                    }
                    parsed.packages.insert(package.to_string(), result);
                }
                "output" => {
                    if output.is_some_and(|output| {
                        output.contains("[build failed]") || output.contains("[setup failed]")
                    }) {
                        parsed.build_failed = true;
                    }
                    if output.is_some_and(|output| output.contains("[no tests to run]")) {
                        parsed.no_tests_to_run = true;
                    }
                }
                _ => {}
            },
        }
    }
    parsed.unfinished_tests = started_tests;
    // Every package that produced any event must report a result.
    parsed.unfinished_packages = started_packages
        .into_iter()
        .filter(|package| !parsed.packages.contains_key(package))
        .collect();
    parsed
}

fn is_json_stream(stdout_lines: &[&InputLine<'_>]) -> bool {
    stdout_lines
        .iter()
        .find(|line| !line.text.trim().is_empty())
        .is_some_and(|line| {
            serde_json::from_str::<Value>(line.text.trim())
                .is_ok_and(|value| value.get("Action").is_some())
        })
}

pub(super) fn normalize(
    args: &[String],
    lines: &[InputLine<'_>],
    input_bytes: usize,
    bound_hit: bool,
    capture_complete: bool,
    exit_code: Option<i32>,
) -> (CodingProviderResult, Completion) {
    let invocation = invocation(args);
    let stdout_lines = lines
        .iter()
        .filter(|line| line.stream == "stdout")
        .collect::<Vec<_>>();
    let json_input = invocation.json || is_json_stream(&stdout_lines);
    let parsed = if json_input {
        parse_json(&stdout_lines)
    } else {
        parse_text(lines)
    };
    let verbose = parsed.verbose || invocation.verbose;
    let bound_hit = bound_hit || parsed.bound_hit;
    let complete = capture_complete && !bound_hit && !parsed.malformed;
    let any_failed = parsed
        .packages
        .values()
        .any(|result| *result == PackageResult::Failed)
        || parsed.count(Status::Failed) > 0;
    let counts = verbose.then(|| {
        let passed = parsed.count(Status::Passed);
        let failed = parsed.count(Status::Failed);
        json!({
            "passed": passed,
            "failed": failed,
            "error": 0,
            "skipped": parsed.count(Status::Skipped),
            "xfailed": 0,
            "xpassed": 0,
            "executed": passed + failed
        })
    });
    let scopes = if invocation.packages.is_empty() {
        vec!["go:test:.".to_string()]
    } else {
        invocation
            .packages
            .iter()
            .map(|package| format!("go:test:{package}"))
            .collect()
    };
    let selection = SelectionCoverage {
        scopes,
        exhaustive: complete
            && !parsed.packages.is_empty()
            && invocation.filters.is_empty()
            && !(invocation.failfast && any_failed),
        ..SelectionCoverage::default()
    };
    let mut warnings = Vec::new();
    if parsed.malformed {
        warnings.push("go test -json output contains lines that are not test events".to_string());
    }
    if parsed.packages.is_empty() {
        warnings.push(
            "go test reported no package results; selection exhaustiveness is unproven".to_string(),
        );
    }
    if !invocation.filters.is_empty() {
        warnings.push(format!(
            "go test {} does not run every selected test; broader absence is unprovable",
            invocation.filters.join(" ")
        ));
    }
    if invocation.failfast && any_failed {
        warnings.push(
            "go test -failfast stops at the first failure, so later tests may not have run; rerun without -failfast to prove selection exhaustion"
                .to_string(),
        );
    }
    let completion = completion(&parsed, verbose, any_failed, exit_code);
    let tests = parsed
        .records
        .iter()
        .map(|(id, record)| NormalizedTest {
            node_id: id.clone(),
            status: record.status.as_str().to_string(),
            message: record.message.clone(),
            evidence_stream: record.stream,
            evidence_line: Some(record.line.try_into().unwrap_or(u64::MAX)),
            report_pointer: None,
        })
        .collect::<Vec<_>>();
    let item_count = tests.len();
    let result = provider_result(
        "go_test.v1",
        if json_input {
            "go_test_json"
        } else {
            "go_test_text"
        },
        if json_input {
            0.99
        } else if tests.is_empty() {
            0.72
        } else {
            0.96
        },
        complete,
        selection,
        input_bytes,
        lines.len(),
        item_count,
        bound_hit,
        json!({
            "tests": tests,
            "summary_seen": !parsed.packages.is_empty(),
            "counts": counts,
            "packages": parsed.packages.iter().map(|(package, result)| {
                (package.clone(), Value::from(match result {
                    PackageResult::Ok => "ok",
                    PackageResult::Failed => "failed",
                    PackageResult::NoTestFiles => "no_test_files",
                }))
            }).collect::<serde_json::Map<_, _>>(),
            "verbose": verbose,
            "build_failed": parsed.build_failed,
            "panicked": parsed.panicked,
            "failfast": invocation.failfast,
            "filters": invocation.filters,
            "targets": invocation.packages
        }),
        warnings,
    );
    (result, completion)
}

fn completion(
    parsed: &Parsed,
    verbose: bool,
    any_failed: bool,
    exit_code: Option<i32>,
) -> Completion {
    let Some(exit_code) = exit_code else {
        return Completion::Unproven("go test completion requires an observed process exit code");
    };
    // RTK #4693: a stream cut off before its package result is never "passed".
    if parsed.trailing_results || !parsed.unfinished_packages.is_empty() {
        return Completion::Unproven("go test output ended before every package reported a result");
    }
    if parsed.packages.is_empty() {
        return Completion::Unproven("go test reported no package results");
    }
    match (exit_code, any_failed) {
        (0, true) => {
            return Completion::Conflicting(
                "go test output reports failure despite a successful process exit",
            );
        }
        (1, false) => {
            return Completion::Conflicting("go test exited 1 but no package or test failed");
        }
        (0 | 1, _) => {}
        _ => return Completion::Unproven("go test exited with an unexplained status"),
    }
    if parsed.build_failed {
        return Completion::Unproven(
            "go test could not build or set up some packages, so their tests never ran",
        );
    }
    if parsed.panicked {
        return Completion::Unproven(
            "a test panicked or timed out, so later tests in its package never ran",
        );
    }
    if !parsed.unfinished_tests.is_empty() {
        return Completion::Unproven("some tests started but never reported a result");
    }
    if parsed.failed_packages_without_failed_tests() {
        return Completion::Unproven(
            "a package failed without a failing test (TestMain, init, or os.Exit)",
        );
    }
    if !verbose {
        return Completion::Unproven(
            "go test without -v or -json names neither passing nor skipped tests; rerun with -v",
        );
    }
    if parsed.count(Status::Passed) + parsed.count(Status::Failed) == 0 {
        return Completion::Unproven("go test ran no tests");
    }
    if parsed.count(Status::Skipped) > 0 {
        return Completion::Skipped(SKIPPED);
    }
    Completion::Consistent
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

    /// Real `go test -v ./...` output (Go 1.27) for three packages: one with a
    /// failure, a skip, and a failing table subtest; one without test files;
    /// one passing.
    const VERBOSE: &str = "=== RUN   TestAdd
    calc_test.go:7: Add(2, 2) = 0, want 4
--- FAIL: TestAdd (0.00s)
=== RUN   TestZero
--- PASS: TestZero (0.00s)
=== RUN   TestSkip
    calc_test.go:17: needs network
--- SKIP: TestSkip (0.00s)
=== RUN   TestTable
=== RUN   TestTable/one
=== RUN   TestTable/two
    calc_test.go:23: got 0 want 2
--- FAIL: TestTable (0.00s)
    --- PASS: TestTable/one (0.00s)
    --- FAIL: TestTable/two (0.00s)
FAIL
FAIL\texample.com/gt/calc\t0.190s
?   \texample.com/gt/notests\t[no test files]
=== RUN   TestOK
--- PASS: TestOK (0.00s)
PASS
ok  \texample.com/gt/other\t0.326s
FAIL
";

    #[test]
    fn verbose_text_names_tests_per_package_with_their_messages() {
        let result = normalize_coding_output(
            &argv(&["go", "test", "-v", "./..."]),
            VERBOSE,
            "",
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(result.provider, "go_test.v1");
        assert_eq!(result.input_format, "go_test_text");
        assert!(result.complete);
        assert_eq!(
            statuses(&result),
            [
                ("example.com/gt/calc.TestAdd", "failed"),
                ("example.com/gt/calc.TestSkip", "skipped"),
                ("example.com/gt/calc.TestTable", "failed"),
                ("example.com/gt/calc.TestTable/one", "passed"),
                ("example.com/gt/calc.TestTable/two", "failed"),
                ("example.com/gt/calc.TestZero", "passed"),
                ("example.com/gt/other.TestOK", "passed"),
            ]
            .iter()
            .map(|(id, status)| ((*id).to_string(), (*status).to_string()))
            .collect::<Vec<_>>()
        );
        assert_eq!(
            result.normalized["tests"][0]["message"],
            "calc_test.go:7: Add(2, 2) = 0, want 4"
        );
        assert_eq!(
            result.normalized["counts"],
            json!({"passed": 3, "failed": 3, "error": 0, "skipped": 1, "xfailed": 0, "xpassed": 0, "executed": 6})
        );
        // The skip is the only gap.
        assert!(!result.selection.exhaustive);
        assert_eq!(
            result.selection.extra[crate::SELECTION_EXCLUDES_SKIPPED_TESTS],
            true,
            "{:?}",
            result.warnings
        );
        assert_eq!(result.selection.scopes, vec!["go:test:./..."]);
    }

    #[test]
    fn json_events_match_the_text_reading() {
        let events = [
            r#"{"Action":"start","Package":"example.com/gt/calc"}"#,
            r#"{"Action":"run","Package":"example.com/gt/calc","Test":"TestAdd"}"#,
            r#"{"Action":"output","Package":"example.com/gt/calc","Test":"TestAdd","Output":"    calc_test.go:7: Add(2, 2) = 0, want 4\n"}"#,
            r#"{"Action":"fail","Package":"example.com/gt/calc","Test":"TestAdd","Elapsed":0}"#,
            r#"{"Action":"run","Package":"example.com/gt/calc","Test":"TestZero"}"#,
            r#"{"Action":"pass","Package":"example.com/gt/calc","Test":"TestZero","Elapsed":0}"#,
            r#"{"Action":"output","Package":"example.com/gt/calc","Output":"FAIL\n"}"#,
            r#"{"Action":"fail","Package":"example.com/gt/calc","Elapsed":0.2}"#,
        ];
        let stdout = events.join("\n") + "\n";
        let result =
            normalize_coding_output(&argv(&["go", "test", "-json"]), &stdout, "", true, Some(1))
                .unwrap();
        assert_eq!(result.input_format, "go_test_json");
        assert!(result.selection.exhaustive, "{:?}", result.warnings);
        assert_eq!(
            statuses(&result),
            vec![
                (
                    "example.com/gt/calc.TestAdd".to_string(),
                    "failed".to_string()
                ),
                (
                    "example.com/gt/calc.TestZero".to_string(),
                    "passed".to_string()
                )
            ]
        );
        assert_eq!(
            result.normalized["tests"][0]["message"],
            "calc_test.go:7: Add(2, 2) = 0, want 4"
        );

        // RTK #4693: a cut-off stream (no package result) is never "passed".
        let cut = events[..6].join("\n") + "\n";
        let result =
            normalize_coding_output(&argv(&["go", "test", "-json"]), &cut, "", true, Some(0))
                .unwrap();
        assert!(!result.selection.exhaustive);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("ended before every package")),
            "{:?}",
            result.warnings
        );
    }

    #[test]
    fn runs_that_cannot_prove_a_fix_say_why() {
        let green = "=== RUN   TestA\n--- PASS: TestA (0.00s)\nPASS\nok  \texample.com/m\t0.1s\n";
        let failing = "=== RUN   TestA\n    a_test.go:3: no\n--- FAIL: TestA (0.00s)\nFAIL\nFAIL\texample.com/m\t0.1s\nFAIL\n";
        let cases: &[(&[&str], &str, Option<i32>, &str)] = &[
            (
                &["-v", "-run", "TestA"],
                green,
                Some(0),
                "-run does not run every",
            ),
            (
                &["-v", "-skip=TestB"],
                green,
                Some(0),
                "-skip does not run every",
            ),
            (&["-v", "-failfast"], failing, Some(1), "-failfast stops"),
            (
                &[],
                "ok  \texample.com/m\t0.1s\n",
                Some(0),
                "without -v or -json",
            ),
            (&["-v"], green, Some(1), "no package or test failed"),
            (
                &["-v"],
                failing,
                Some(0),
                "despite a successful process exit",
            ),
            (&["-v"], green, None, "requires an observed process exit"),
            (
                &["-v"],
                "=== RUN   TestA\n--- PASS: TestA (0.00s)\n",
                Some(0),
                "no package results",
            ),
            (
                &["-v"],
                "=== RUN   TestA\n--- PASS: TestA (0.00s)\nok  \texample.com/m\t0.1s\n=== RUN   TestB\n--- PASS: TestB (0.00s)\n",
                Some(0),
                "ended before every package",
            ),
            (
                &["-v"],
                "# example.com/m\n./a.go:3:1: syntax error\nFAIL\texample.com/m [build failed]\nFAIL\n",
                Some(1),
                "could not build",
            ),
            (
                &["-v"],
                "=== RUN   TestA\n--- PASS: TestA (0.00s)\n=== RUN   TestBoom\n--- FAIL: TestBoom (0.00s)\npanic: assignment to entry in nil map [recovered]\n\nFAIL\texample.com/m\t0.2s\nFAIL\n",
                Some(1),
                "panicked or timed out",
            ),
            (
                &["-v"],
                "=== RUN   TestA\n--- PASS: TestA (0.00s)\nFAIL\nFAIL\texample.com/m\t0.2s\nFAIL\n",
                Some(1),
                "without a failing test",
            ),
            (
                &["-v"],
                "=== RUN   TestA\nFAIL\texample.com/m\t0.2s\nFAIL\n",
                Some(1),
                "never reported a result",
            ),
            (
                &["-v"],
                "?   \texample.com/m\t[no test files]\n",
                Some(0),
                "ran no tests",
            ),
        ];
        for (args, stdout, exit_code, reason) in cases {
            let mut full = argv(&["go", "test"]);
            full.extend(argv(args));
            let result = normalize_coding_output(&full, stdout, "", true, *exit_code).unwrap();
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
        // A green fail-fast run stopped nowhere.
        let result = normalize_coding_output(
            &argv(&["go", "test", "-v", "-failfast"]),
            green,
            "",
            true,
            Some(0),
        )
        .unwrap();
        assert!(result.selection.exhaustive, "{:?}", result.warnings);
    }

    #[test]
    fn repeated_runs_keep_the_worst_outcome() {
        let stdout = "=== RUN   TestFlaky\n--- PASS: TestFlaky (0.00s)\n=== RUN   TestFlaky\n    f_test.go:3: flaked\n--- FAIL: TestFlaky (0.00s)\nFAIL\nFAIL\texample.com/m\t0.1s\nFAIL\n";
        let result = normalize_coding_output(
            &argv(&["go", "test", "-v", "-count=2"]),
            stdout,
            "",
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(
            statuses(&result),
            vec![("example.com/m.TestFlaky".to_string(), "failed".to_string())]
        );
    }

    #[test]
    fn flags_with_values_are_not_packages() {
        let parsed = invocation(&argv(&[
            "-v",
            "-count",
            "1",
            "-timeout=30s",
            "-tags",
            "integration",
            "./pkg/...",
            "-args",
            "-run",
            "X",
        ]));
        assert_eq!(parsed.packages, vec!["./pkg/..."]);
        assert!(parsed.filters.is_empty());
        assert!(parsed.verbose);
        assert!(!invocation(&argv(&["-v=false"])).verbose);
        assert_eq!(invocation(&argv(&["-test.run", "X"])).filters, vec!["-run"]);
    }
}

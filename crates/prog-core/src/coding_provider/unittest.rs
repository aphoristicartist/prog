//! `python -m unittest`: verbose (`-v`) per-test lines, failure headers, and
//! the closing `Ran N tests` summary, which the runner writes to stderr.
//!
//! Python 3.11+ prints the full test id in parentheses
//! (`test_add (pkg.mod.Class.test_add) ... ok`); older versions print only
//! `pkg.mod.Class`. Both normalize to `pkg.mod.Class.test_add`.

use std::collections::BTreeMap;

use serde_json::json;

use super::verification::Completion;
use super::{
    CodingProviderResult, InputLine, MAX_PROVIDER_ITEMS, NormalizedTest, bounded_text,
    provider_result,
};
use crate::SelectionCoverage;

const FIXTURES: [&str; 4] = [
    "setUpClass",
    "tearDownClass",
    "setUpModule",
    "tearDownModule",
];
const MESSAGE_SCAN_LINES: usize = 400;

pub(super) const SKIPPED: &str = "unittest skipped some tests, so this run alone does not prove the suite passed; to prove a fix, rerun skipping the same tests, named by `-v`";

struct Invocation {
    verbose: bool,
    failfast: bool,
    catch: bool,
    filtered: bool,
    scopes: Vec<String>,
    targets: Vec<String>,
}

fn invocation(args: &[String]) -> Invocation {
    let mut verbose = false;
    let mut failfast = false;
    let mut catch = false;
    let mut filtered = false;
    let mut discover = false;
    let mut discover_options = BTreeMap::new();
    let mut discover_positionals = Vec::new();
    let mut targets = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].as_str();
        let value_at = |offset: usize| args.get(index + offset).cloned();
        match arg {
            "-v" | "--verbose" => verbose = true,
            "-q" | "--quiet" => verbose = false,
            "-f" | "--failfast" => failfast = true,
            "-c" | "--catch" => catch = true,
            "-b" | "--buffer" | "--locals" => {}
            "-k" => {
                filtered = true;
                index += 1;
            }
            "--durations" => index += 1,
            "-s" | "--start-directory" | "-p" | "--pattern" | "-t" | "--top-level-directory"
                if discover =>
            {
                let key = match arg {
                    "-s" | "--start-directory" => "start",
                    "-p" | "--pattern" => "pattern",
                    _ => "top",
                };
                if let Some(value) = value_at(1) {
                    discover_options.insert(key, value);
                }
                index += 1;
            }
            "discover" if !discover && targets.is_empty() => discover = true,
            _ if arg.starts_with("--") => {
                if let Some((name, value)) = arg.split_once('=') {
                    match name {
                        "--start-directory" if discover => {
                            discover_options.insert("start", value.to_string());
                        }
                        "--pattern" if discover => {
                            discover_options.insert("pattern", value.to_string());
                        }
                        "--top-level-directory" if discover => {
                            discover_options.insert("top", value.to_string());
                        }
                        _ => {}
                    }
                }
            }
            _ if arg.starts_with("-k") => filtered = true,
            // Combined short flags such as `-vf`.
            _ if arg.len() > 1
                && arg.starts_with('-')
                && arg[1..].chars().all(|flag| "vqfcb".contains(flag)) =>
            {
                for flag in arg[1..].chars() {
                    match flag {
                        'v' => verbose = true,
                        'q' => verbose = false,
                        'f' => failfast = true,
                        'c' => catch = true,
                        _ => {}
                    }
                }
            }
            _ if arg.starts_with('-') => {}
            _ if discover => discover_positionals.push(arg.to_string()),
            _ => targets.push(bounded_text(arg)),
        }
        index += 1;
    }
    for (key, value) in ["start", "pattern", "top"].iter().zip(discover_positionals) {
        discover_options.entry(key).or_insert(value);
    }
    targets.sort();
    targets.dedup();
    let scopes = if targets.is_empty() {
        let mut scope = "unittest:discover".to_string();
        for key in ["start", "pattern", "top"] {
            if let Some(value) = discover_options.get(key) {
                scope.push_str(&format!(":{key}={}", bounded_text(value)));
            }
        }
        vec![scope]
    } else {
        targets
            .iter()
            .map(|target| format!("unittest:{target}"))
            .collect()
    };
    Invocation {
        verbose,
        failfast,
        catch,
        filtered,
        scopes,
        targets,
    }
}

/// `name (dotted.path)` and the rest of the line.
fn test_head(text: &str) -> Option<(String, &str, &str)> {
    let (name, rest) = text.split_once(" (")?;
    let identifier = !name.is_empty()
        && !name.starts_with(|ch: char| ch.is_ascii_digit())
        && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_');
    if !identifier {
        return None;
    }
    let (path, rest) = rest.split_once(')')?;
    let dotted = !path.is_empty()
        && path
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.'));
    if !dotted {
        return None;
    }
    let id = if path.ends_with(&format!(".{name}")) {
        path.to_string()
    } else {
        format!("{path}.{name}")
    };
    Some((bounded_text(&id), name, rest))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Passed,
    Skipped,
    XFailed,
    Failed,
    Unexpected,
    Error,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Skipped => "skipped",
            Self::XFailed => "xfailed",
            // An unexpected success fails the run (exit 1, `FAILED`).
            Self::Failed | Self::Unexpected => "failed",
            Self::Error => "error",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Passed => 0,
            Self::Skipped => 1,
            Self::XFailed => 2,
            Self::Failed | Self::Unexpected => 3,
            Self::Error => 4,
        }
    }
}

fn verbose_status(text: &str) -> Option<(Status, Option<String>)> {
    match text {
        "ok" => Some((Status::Passed, None)),
        "FAIL" => Some((Status::Failed, None)),
        "ERROR" => Some((Status::Error, None)),
        "expected failure" => Some((Status::XFailed, None)),
        "unexpected success" => Some((Status::Unexpected, Some("unexpected success".to_string()))),
        _ => {
            let reason = text.strip_prefix("skipped")?;
            let reason = reason.trim().trim_matches(|ch| ch == '\'' || ch == '"');
            Some((
                Status::Skipped,
                (!reason.is_empty()).then(|| bounded_text(reason)),
            ))
        }
    }
}

struct Record {
    status: Status,
    message: Option<String>,
    stream: &'static str,
    line: usize,
    fixture: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ok,
    Failed,
    NoTestsRan,
}

#[derive(Default, Clone, Copy)]
struct Summary {
    failures: u64,
    errors: u64,
    skipped: u64,
    expected_failures: u64,
    unexpected_successes: u64,
}

fn outcome_line(text: &str) -> Option<(Outcome, Summary)> {
    let (outcome, rest) = if let Some(rest) = text.strip_prefix("FAILED") {
        (Outcome::Failed, rest)
    } else if let Some(rest) = text.strip_prefix("OK") {
        (Outcome::Ok, rest)
    } else {
        (Outcome::NoTestsRan, text.strip_prefix("NO TESTS RAN")?)
    };
    let rest = rest.trim();
    let mut summary = Summary::default();
    if rest.is_empty() {
        return Some((outcome, summary));
    }
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
    for part in inner.split(", ") {
        let (key, value) = part.split_once('=')?;
        let value = value.parse::<u64>().ok()?;
        let slot = match key {
            "failures" => &mut summary.failures,
            "errors" => &mut summary.errors,
            "skipped" => &mut summary.skipped,
            "expected failures" => &mut summary.expected_failures,
            "unexpected successes" => &mut summary.unexpected_successes,
            _ => return None,
        };
        *slot = value;
    }
    Some((outcome, summary))
}

fn ran_line(text: &str) -> Option<u64> {
    let rest = text.strip_prefix("Ran ")?;
    let (count, rest) = rest.split_once(' ')?;
    let rest = rest
        .strip_prefix("tests in ")
        .or_else(|| rest.strip_prefix("test in "))?;
    let seconds = rest.strip_suffix('s')?.parse::<f64>().ok()?;
    (seconds.is_finite() && seconds >= 0.0)
        .then_some(())
        .and_then(|()| count.parse().ok())
}

fn is_separator(text: &str, ch: char) -> bool {
    text.len() >= 20 && text.chars().all(|candidate| candidate == ch)
}

/// The last line of a failure block's traceback (`AssertionError: 1 != 2`).
fn block_message(lines: &[InputLine<'_>], header: usize) -> Option<String> {
    let stream = lines[header].stream;
    let mut message = None;
    for line in lines
        .iter()
        .skip(header + 1)
        .take(MESSAGE_SCAN_LINES)
        .filter(|line| line.stream == stream)
        .skip_while(|line| is_separator(line.text.trim(), '-'))
    {
        let text = line.text.trim();
        if is_separator(text, '=') || is_separator(text, '-') || ran_line(text).is_some() {
            break;
        }
        if !text.is_empty() {
            message = Some(text);
        }
    }
    message.map(bounded_text)
}

/// Failure headers (`FAIL: name (id)`) follow a line of `=`, which keeps
/// log output such as `ERROR: name (detail)` from becoming a test.
fn follows_header_separator(lines: &[InputLine<'_>], index: usize) -> bool {
    let stream = lines[index].stream;
    lines[..index]
        .iter()
        .rev()
        .find(|line| line.stream == stream)
        .is_some_and(|line| is_separator(line.text.trim(), '='))
}

enum Pending {
    /// A test whose description (docstring) or status is on a later line.
    Status(String),
    /// A test whose failing subtests follow on indented lines (3.11+).
    Subtests(String),
}

#[derive(Default)]
struct Parsed {
    records: BTreeMap<String, Record>,
    ran: Vec<u64>,
    outcomes: Vec<(Outcome, Summary)>,
    verbose_lines: bool,
    bound_hit: bool,
}

impl Parsed {
    fn record(
        &mut self,
        id: String,
        status: Status,
        message: Option<String>,
        line: &InputLine<'_>,
    ) {
        let name = id.rsplit('.').next().unwrap_or_default();
        let fixture = FIXTURES.contains(&name);
        if let Some(existing) = self.records.get_mut(&id) {
            if status.rank() > existing.status.rank() {
                existing.status = status;
                if message.is_some() {
                    existing.message = message;
                }
            } else if existing.message.is_none() && status.rank() == existing.status.rank() {
                existing.message = message;
            }
            return;
        }
        if self.records.len() >= MAX_PROVIDER_ITEMS {
            self.bound_hit = true;
            return;
        }
        self.records.insert(
            id,
            Record {
                status,
                message,
                stream: line.stream,
                line: line.number,
                fixture,
            },
        );
    }

    fn count(&self, status: Status) -> u64 {
        self.records
            .values()
            .filter(|record| record.status == status)
            .count() as u64
    }

    fn tests(&self) -> u64 {
        self.records
            .values()
            .filter(|record| !record.fixture)
            .count() as u64
    }
}

fn parse(lines: &[InputLine<'_>]) -> Parsed {
    let mut parsed = Parsed::default();
    let mut pending: Option<Pending> = None;
    let mut expecting_outcome = false;
    'lines: for (index, line) in lines.iter().enumerate() {
        let trimmed = line.text.trim();
        if follows_header_separator(lines, index) {
            for (prefix, status) in [
                ("FAIL: ", Status::Failed),
                ("ERROR: ", Status::Error),
                ("UNEXPECTED SUCCESS: ", Status::Unexpected),
            ] {
                if let Some((id, _, _)) = trimmed.strip_prefix(prefix).and_then(test_head) {
                    let message = if status == Status::Unexpected {
                        Some("unexpected success".to_string())
                    } else {
                        block_message(lines, index)
                    };
                    parsed.record(id, status, message, line);
                    pending = None;
                    continue 'lines;
                }
            }
        }
        if let Some(count) = ran_line(trimmed) {
            parsed.ran.push(count);
            expecting_outcome = true;
            pending = None;
            continue;
        }
        if expecting_outcome && !trimmed.is_empty() {
            expecting_outcome = false;
            if let Some(outcome) = outcome_line(trimmed) {
                parsed.outcomes.push(outcome);
            }
            continue;
        }
        if let Some(head) = trimmed.strip_suffix(" ...") {
            if let Some((id, _, rest)) = test_head(head)
                && rest.is_empty()
            {
                parsed.verbose_lines = true;
                pending = Some(Pending::Subtests(id));
            }
            continue;
        }
        if let Some((description, status_text)) = trimmed.rsplit_once(" ... ") {
            let status = verbose_status(status_text.trim());
            let head = test_head(description);
            let new_test = !description.contains(" ... ")
                && matches!(&head, Some((_, _, rest)) if rest.trim().is_empty());
            // A docstring line, or output the test wrote before its status,
            // belongs to the test named on the previous line.
            if !new_test
                && let Some((status, message)) = status.clone()
                && let Some(Pending::Status(id)) = pending.take()
            {
                parsed.record(id, status, message, line);
                continue;
            }
            match head {
                // Python 3.10 and older print no newline after a test whose
                // subtests failed, so two tests share one line. Neither can
                // be attributed; the accounting check refuses the run.
                Some(_) if description.contains(" ... ") => pending = None,
                Some((id, _, rest)) if rest.trim().is_empty() => {
                    parsed.verbose_lines = true;
                    match status {
                        Some((status, message)) => {
                            parsed.record(id, status, message, line);
                            pending = None;
                        }
                        None => pending = Some(Pending::Status(id)),
                    }
                }
                Some((id, _, _subtest)) => {
                    parsed.verbose_lines = true;
                    if let Some((status @ (Status::Failed | Status::Error), message)) = status {
                        let parent = match &pending {
                            Some(Pending::Subtests(parent)) => parent.clone(),
                            _ => id,
                        };
                        parsed.record(parent, status, message, line);
                    }
                }
                None => {}
            }
            continue;
        }
        if let Some((id, _, rest)) = test_head(trimmed)
            && rest.is_empty()
        {
            parsed.verbose_lines = true;
            pending = Some(Pending::Status(id));
            continue;
        }
        if matches!(pending, Some(Pending::Status(_)))
            && let Some((status, message)) = verbose_status(trimmed)
            && let Some(Pending::Status(id)) = pending.take()
        {
            parsed.record(id, status, message, line);
        }
    }
    parsed
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
    let parsed = parse(lines);
    let bound_hit = bound_hit || parsed.bound_hit;
    let complete = capture_complete && !bound_hit;
    let verbose = invocation.verbose || parsed.verbose_lines;
    let summary = (parsed.ran.len() == 1 && parsed.outcomes.len() == 1)
        .then(|| (parsed.ran[0], parsed.outcomes[0].0, parsed.outcomes[0].1));
    let accounted = verbose && summary.is_some_and(|(ran, _, _)| parsed.tests() == ran);
    let failed_run = summary.is_some_and(|(_, outcome, _)| outcome == Outcome::Failed)
        || parsed
            .records
            .values()
            .any(|record| record.status.rank() >= Status::Failed.rank());

    let counts = summary.map(|(ran, _, totals)| {
        let executed = ran.saturating_sub(totals.skipped);
        if accounted {
            json!({
                "passed": parsed.count(Status::Passed),
                "failed": parsed.count(Status::Failed) + parsed.count(Status::Unexpected),
                "error": parsed.count(Status::Error),
                "skipped": parsed.count(Status::Skipped),
                "xfailed": parsed.count(Status::XFailed),
                "xpassed": 0,
                "executed": executed
            })
        } else {
            // Without per-test lines the summary is all there is. It counts
            // failure events, so subtests can count one test twice.
            let failing = totals.failures.saturating_add(totals.errors);
            json!({
                "passed": executed
                    .saturating_sub(totals.expected_failures)
                    .saturating_sub(totals.unexpected_successes)
                    .saturating_sub(failing),
                "failed": totals.failures.saturating_add(totals.unexpected_successes),
                "error": totals.errors,
                "skipped": totals.skipped,
                "xfailed": totals.expected_failures,
                "xpassed": 0,
                "executed": executed
            })
        }
    });

    let selection = SelectionCoverage {
        scopes: invocation.scopes.clone(),
        exhaustive: complete
            && summary.is_some()
            && !invocation.filtered
            && !invocation.catch
            && !(invocation.failfast && failed_run),
        ..SelectionCoverage::default()
    };
    let mut warnings = Vec::new();
    if summary.is_none() {
        warnings.push(
            "unittest completion summary was not observed; selection exhaustiveness is unproven"
                .to_string(),
        );
    }
    if invocation.filtered {
        warnings.push(
            "unittest -k runs a subset and does not count the rest; broader absence is unprovable"
                .to_string(),
        );
    }
    if invocation.catch {
        warnings.push(
            "unittest -c can stop early on Ctrl-C and still report OK; selection exhaustiveness is unproven"
                .to_string(),
        );
    }
    if invocation.failfast && failed_run {
        warnings.push(
            "unittest --failfast stops at the first failure, so later tests may not have run; rerun without -f to prove selection exhaustion"
                .to_string(),
        );
    }

    let completion = completion(&parsed, summary, verbose, accounted, exit_code);
    let mut tests = parsed
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
    tests.sort();
    let fixture_errors = parsed
        .records
        .iter()
        .filter(|(id, record)| record.fixture || is_loader_failure(id))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let item_count = tests.len();
    let result = provider_result(
        "unittest.v1",
        "unittest_text",
        if parsed.records.is_empty() {
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
            "summary_seen": summary.is_some(),
            "counts": counts,
            "ran": summary.map(|(ran, _, _)| ran),
            "verbose": verbose,
            "accounted": accounted,
            // #308: `-v`, every distinct test id accounted for by `Ran N`,
            // and no loader or fixture error that could hide a module's tests.
            "names_every_test_that_ran": complete
                && accounted
                && fixture_errors.is_empty()
                && matches!(completion, Completion::Consistent | Completion::Skipped(_)),
            "failfast": invocation.failfast,
            "filtered": invocation.filtered,
            "fixture_errors": fixture_errors,
            "targets": invocation.targets
        }),
        warnings,
    );
    (result, completion)
}

fn is_loader_failure(id: &str) -> bool {
    id.starts_with("unittest.loader.") || id.contains("._FailedTest.")
}

fn completion(
    parsed: &Parsed,
    summary: Option<(u64, Outcome, Summary)>,
    verbose: bool,
    accounted: bool,
    exit_code: Option<i32>,
) -> Completion {
    let Some((ran, outcome, totals)) = summary else {
        return Completion::Unproven("unittest requires one unambiguous completion summary");
    };
    let Some(exit_code) = exit_code else {
        return Completion::Unproven("unittest completion requires an observed process exit code");
    };
    let expected_exit = match outcome {
        Outcome::Ok => 0,
        Outcome::Failed => 1,
        Outcome::NoTestsRan => 5,
    };
    if exit_code != expected_exit {
        return Completion::Conflicting(
            "unittest summary disagrees with the observed process exit",
        );
    }
    let failing_events = totals
        .failures
        .saturating_add(totals.errors)
        .saturating_add(totals.unexpected_successes);
    if (outcome == Outcome::Failed) != (failing_events > 0) {
        return Completion::Conflicting("unittest summary status contradicts its counts");
    }
    if parsed.count(Status::Failed) > totals.failures
        || parsed.count(Status::Error) > totals.errors
        || parsed.count(Status::Unexpected) > totals.unexpected_successes
        || parsed.count(Status::Skipped) > totals.skipped
        || parsed.count(Status::XFailed) > totals.expected_failures
        || parsed.tests() > ran
    {
        return Completion::Conflicting(
            "unittest completion summary contradicts observed test outcomes",
        );
    }
    if outcome == Outcome::NoTestsRan || ran.saturating_sub(totals.skipped) == 0 {
        return Completion::Unproven("unittest ran no tests: none were found or all were skipped");
    }
    if parsed
        .records
        .iter()
        .any(|(id, record)| record.fixture || is_loader_failure(id))
    {
        return Completion::Unproven(
            "unittest could not import or set up some tests, so they never ran",
        );
    }
    if verbose && !accounted {
        return Completion::Unproven(
            "unittest -v output does not name every test the summary counts",
        );
    }
    if totals.skipped > 0 {
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

    /// Real `python3 -m unittest -v` output (Python 3.15) for a suite with a
    /// docstring test, an error, a skip, a failure, a failing subtest, an
    /// unexpected success, and an expected failure. Tracebacks trimmed.
    const VERBOSE_315: &str = "test_add (test_calc.TestCalc.test_add) ... ok
test_doc (test_calc.TestCalc.test_doc)
Has a docstring. ... ok
test_err (test_calc.TestCalc.test_err) ... ERROR
test_skip (test_calc.TestCalc.test_skip) ... skipped 'slow'
test_sub (test_calc.TestCalc.test_sub) ... FAIL
test_subtests (test_calc.TestCalc.test_subtests) ...
  test_subtests (test_calc.TestCalc.test_subtests) (i=1) ... FAIL
test_us (test_calc.TestCalc.test_us) ... unexpected success
test_xf (test_calc.TestCalc.test_xf) ... expected failure

======================================================================
ERROR: test_err (test_calc.TestCalc.test_err)
----------------------------------------------------------------------
Traceback (most recent call last):
  File \"test_calc.py\", line 15, in test_err
    raise RuntimeError(\"boom\")
RuntimeError: boom

======================================================================
FAIL: test_sub (test_calc.TestCalc.test_sub)
----------------------------------------------------------------------
Traceback (most recent call last):
  File \"test_calc.py\", line 12, in test_sub
    self.assertEqual(2 - 1, 2)
AssertionError: 1 != 2

======================================================================
FAIL: test_subtests (test_calc.TestCalc.test_subtests) (i=1)
----------------------------------------------------------------------
Traceback (most recent call last):
  File \"test_calc.py\", line 32, in test_subtests
    self.assertNotEqual(i, 1)
AssertionError: 1 == 1

======================================================================
UNEXPECTED SUCCESS: test_us (test_calc.TestCalc.test_us)
----------------------------------------------------------------------
Ran 8 tests in 0.079s

FAILED (failures=2, errors=1, skipped=1, expected failures=1, unexpected successes=1)
";

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

    #[test]
    fn verbose_output_names_every_test_and_merges_subtests_into_their_test() {
        let result = normalize_coding_output(
            &argv(&["python3", "-m", "unittest", "-v"]),
            "",
            VERBOSE_315,
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(result.provider, "unittest.v1");
        assert!(result.complete);
        let expected = [
            ("test_calc.TestCalc.test_add", "passed"),
            ("test_calc.TestCalc.test_doc", "passed"),
            ("test_calc.TestCalc.test_err", "error"),
            ("test_calc.TestCalc.test_skip", "skipped"),
            ("test_calc.TestCalc.test_sub", "failed"),
            ("test_calc.TestCalc.test_subtests", "failed"),
            ("test_calc.TestCalc.test_us", "failed"),
            ("test_calc.TestCalc.test_xf", "xfailed"),
        ];
        let mut actual = statuses(&result);
        actual.sort();
        assert_eq!(
            actual,
            expected
                .iter()
                .map(|(id, status)| ((*id).to_string(), (*status).to_string()))
                .collect::<Vec<_>>()
        );
        assert_eq!(result.normalized["accounted"], true);
        assert_eq!(
            result.normalized["counts"],
            json!({"passed": 2, "failed": 3, "error": 1, "skipped": 1, "xfailed": 1, "xpassed": 0, "executed": 7})
        );
        let messages = result.normalized["tests"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|test| Some((test["node_id"].as_str()?, test["message"].as_str()?)))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            messages["test_calc.TestCalc.test_sub"],
            "AssertionError: 1 != 2"
        );
        assert_eq!(
            messages["test_calc.TestCalc.test_err"],
            "RuntimeError: boom"
        );
        assert_eq!(messages["test_calc.TestCalc.test_us"], "unexpected success");
        // The skip is the only gap; a failing run is still exhaustive.
        assert!(!result.selection.exhaustive);
        assert_eq!(
            result.selection.extra[crate::SELECTION_EXCLUDES_SKIPPED_TESTS],
            true
        );
        assert_eq!(result.selection.scopes, vec!["unittest:discover"]);
    }

    #[test]
    fn python_310_ids_and_quiet_output_normalize_to_the_same_test_ids() {
        let old = "test_add (test_calc.TestCalc) ... ok\ntest_sub (test_calc.TestCalc) ... FAIL\n\n======================================================================\nFAIL: test_sub (test_calc.TestCalc)\n----------------------------------------------------------------------\nAssertionError: 1 != 2\n\n----------------------------------------------------------------------\nRan 2 tests in 0.001s\n\nFAILED (failures=1)\n";
        let quiet = ".F\n======================================================================\nFAIL: test_sub (test_calc.TestCalc.test_sub)\n----------------------------------------------------------------------\nAssertionError: 1 != 2\n\n----------------------------------------------------------------------\nRan 2 tests in 0.001s\n\nFAILED (failures=1)\n";
        let old = normalize_coding_output(
            &argv(&["python", "-m", "unittest", "-v"]),
            "",
            old,
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(
            statuses(&old),
            vec![
                (
                    "test_calc.TestCalc.test_add".to_string(),
                    "passed".to_string()
                ),
                (
                    "test_calc.TestCalc.test_sub".to_string(),
                    "failed".to_string()
                )
            ]
        );
        assert!(old.selection.exhaustive);
        let quiet = normalize_coding_output(
            &argv(&["python3", "-m", "unittest"]),
            "",
            quiet,
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(
            statuses(&quiet),
            vec![(
                "test_calc.TestCalc.test_sub".to_string(),
                "failed".to_string()
            )]
        );
        assert_eq!(quiet.normalized["verbose"], false);
        assert_eq!(quiet.normalized["counts"]["executed"], 2);
        assert_eq!(quiet.normalized["counts"]["passed"], 1);
        assert!(quiet.selection.exhaustive);
    }

    #[test]
    fn runs_that_cannot_prove_a_fix_say_why() {
        let green = "test_a (m.T.test_a) ... ok\n\n----------------------------------------------------------------------\nRan 1 test in 0.000s\n\nOK\n";
        let failing = "test_a (m.T.test_a) ... FAIL\n\n======================================================================\nFAIL: test_a (m.T.test_a)\n----------------------------------------------------------------------\nAssertionError\n\n----------------------------------------------------------------------\nRan 1 test in 0.000s\n\nFAILED (failures=1)\n";
        let cases: &[(&[&str], &str, Option<i32>, &str)] = &[
            (&["-v", "-k", "test_a"], green, Some(0), "-k runs a subset"),
            (&["-v", "-c"], green, Some(0), "-c can stop early"),
            (&["-v", "-f"], failing, Some(1), "--failfast stops"),
            (
                &["-v"],
                green,
                Some(1),
                "disagrees with the observed process exit",
            ),
            (&["-v"], green, None, "requires an observed process exit"),
            (
                &["-v"],
                "test_a (m.T.test_a) ... ok\n",
                Some(0),
                "summary was not observed",
            ),
            (
                &["-v"],
                "\n----------------------------------------------------------------------\nRan 0 tests in 0.000s\n\nNO TESTS RAN\n",
                Some(5),
                "ran no tests",
            ),
            (
                &["-v"],
                "test_broken (unittest.loader._FailedTest.test_broken) ... ERROR\ntest_ok (test_ok.U.test_ok) ... ok\n\n----------------------------------------------------------------------\nRan 2 tests in 0.000s\n\nFAILED (errors=1)\n",
                Some(1),
                "could not import or set up",
            ),
            (
                &["-v"],
                "setUpClass (m.Broken) ... ERROR\ntest_d (m.Fine.test_d) ... ok\n\n----------------------------------------------------------------------\nRan 1 test in 0.000s\n\nFAILED (errors=1)\n",
                Some(1),
                "could not import or set up",
            ),
            // Python 3.10 joins a test with failing subtests to the next test.
            (
                &["-v"],
                "test_s (m.T) ... test_u (m.T) ... ok\n\n----------------------------------------------------------------------\nRan 2 tests in 0.000s\n\nOK\n",
                Some(0),
                "does not name every test",
            ),
            (
                &["-v"],
                "test_a (m.T.test_a) ... FAIL\n\n----------------------------------------------------------------------\nRan 1 test in 0.000s\n\nOK\n",
                Some(0),
                "contradicts observed test outcomes",
            ),
            (
                &["-v"],
                "test_a (m.T.test_a) ... ok\nRan 1 test in 0.000s\n\nOK\ntest_b (m.T.test_b) ... ok\nRan 1 test in 0.000s\n\nOK\n",
                Some(0),
                "one unambiguous completion summary",
            ),
        ];
        for (args, stderr, exit_code, reason) in cases {
            let mut full = argv(&["python3", "-m", "unittest"]);
            full.extend(argv(args));
            let result = normalize_coding_output(&full, "", stderr, true, *exit_code).unwrap();
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
        // A green fail-fast run stopped nowhere, so it covers the selection.
        let result = normalize_coding_output(
            &argv(&["python3", "-m", "unittest", "-vf"]),
            "",
            green,
            true,
            Some(0),
        )
        .unwrap();
        assert!(result.selection.exhaustive, "{:?}", result.warnings);
    }

    #[test]
    fn invocation_scopes_name_discovery_options_and_targets() {
        let scopes = |args: &[&str]| invocation(&argv(args)).scopes;
        assert_eq!(scopes(&[]), vec!["unittest:discover"]);
        assert_eq!(
            scopes(&["discover", "-s", "tests", "-p", "test_*.py"]),
            vec!["unittest:discover:start=tests:pattern=test_*.py"]
        );
        assert_eq!(
            scopes(&["discover", "tests", "*_test.py"]),
            vec!["unittest:discover:start=tests:pattern=*_test.py"]
        );
        assert_eq!(
            scopes(&["-v", "test_b", "test_a.T"]),
            vec!["unittest:test_a.T", "unittest:test_b"]
        );
        assert!(invocation(&argv(&["-kfoo"])).filtered);
        assert!(!invocation(&argv(&["-v", "-q"])).verbose);
    }

    #[test]
    fn unrelated_python_commands_are_not_unittest() {
        for args in [
            &["python3", "script.py"][..],
            &["python3", "-m", "unittest_extra"],
            &["python4", "-m", "unittest"],
        ] {
            assert!(
                normalize_coding_output(
                    &argv(args),
                    "",
                    "Ran 1 test in 0.0s\n\nOK\n",
                    true,
                    Some(0)
                )
                .is_none(),
                "{args:?}"
            );
        }
        assert!(
            normalize_coding_output(
                &argv(&["/usr/bin/python3.12", "-m", "unittest"]),
                "",
                "",
                true,
                Some(0)
            )
            .is_some()
        );
    }
}

//! `bun test` results (#312).
//!
//! Bun prints each file's results after a `path:` header, then one summary:
//! ` 1 pass`, ` 1 fail`, ` 1 skip`, ` 1 todo`, `Ran 4 tests across 1 file.`
//! When it detects an agent (`CLAUDECODE` and similar), Bun 1.3 prints only
//! failing tests, and it has no stdout report, so its output gives failures
//! and counts but never proves a fix.
//!
//! Formats checked against Bun 1.3.14.

use std::collections::BTreeMap;

use serde_json::json;

use super::js_test::strip_ansi;
use super::verification::Completion;
use super::{
    CodingProviderResult, InputLine, MAX_PROVIDER_ITEMS, NormalizedTest, bounded_text,
    provider_result,
};
use crate::SelectionCoverage;

const TEXT_ONLY: &str = "bun test does not name every passing test (it prints only failures in agent sessions), so it cannot prove a fix";

pub(super) enum Launch<'a> {
    /// The arguments after `bun test`.
    Direct(&'a [String]),
    /// A package script whose output turned out to be Bun's.
    Script(&'a str, &'a [String]),
}

const VALUE_OPTIONS: &[&str] = &[
    "--timeout",
    "--rerun-each",
    "--preload",
    "-r",
    "--reporter",
    "--reporter-outfile",
    "--coverage-reporter",
    "--coverage-dir",
    "--seed",
    "--max-concurrency",
    "--define",
    "-d",
    "--env-file",
    "--tsconfig-override",
    "--conditions",
    "--cwd",
];

struct Invocation {
    filters: Vec<String>,
    bail: bool,
    targets: Vec<String>,
    scopes: Vec<String>,
}

fn invocation(launch: &Launch<'_>) -> Invocation {
    let (args, base) = match launch {
        Launch::Direct(args) => (*args, "bun:test".to_string()),
        Launch::Script(program, args) => (*args, format!("{program}:script")),
    };
    let mut filters = Vec::new();
    let mut bail = false;
    let mut targets = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].as_str();
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg, false), |(name, _)| (name, true));
        match name {
            "-t" | "--test-name-pattern" => {
                filters.push(name.to_string());
                if !inline {
                    index += 1;
                }
            }
            "--only" | "--todo" | "--changed" => filters.push(name.to_string()),
            "--bail" => bail = true,
            _ if VALUE_OPTIONS.contains(&name) && !inline => index += 1,
            _ if arg.starts_with('-') => {}
            // Bun treats each path argument as a substring filter on files.
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
        filters,
        bail,
        targets,
        scopes,
    }
}

struct Record {
    status: &'static str,
    message: Option<String>,
    stream: &'static str,
    line: usize,
}

#[derive(Default)]
struct Facts {
    banner: bool,
    summary: BTreeMap<&'static str, u64>,
    ran: Option<u64>,
    bailed: bool,
    unhandled: u64,
}

const CATEGORIES: &[&str] = &["pass", "fail", "skip", "todo", "error", "errors"];

/// `tests/calc.test.js:` (a file header), but not `error: …` or `at …:4:2`.
fn file_header(text: &str) -> Option<&str> {
    let path = text.strip_suffix(':')?;
    (!path.is_empty()
        && !path.contains(' ')
        && [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"]
            .iter()
            .any(|extension| path.ends_with(extension)))
    .then_some(path)
}

/// `(fail) calc > adds [0.16ms]` as `calc > adds`.
fn failed_title(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("(fail) ")?;
    Some(match rest.rsplit_once(" [") {
        Some((title, timing)) if timing.ends_with(']') => title,
        _ => rest,
    })
}

fn parse(lines: &[InputLine<'_>]) -> (BTreeMap<String, Record>, Facts, bool, bool) {
    let mut records = BTreeMap::new();
    let mut facts = Facts::default();
    let mut bound_hit = false;
    let mut repeated = false;
    let mut file: Option<String> = None;
    let mut last_error: Option<String> = None;
    for line in lines {
        let text = strip_ansi(line.text);
        let trimmed = text.trim();
        if trimmed.starts_with("bun test v") {
            facts.banner = true;
            continue;
        }
        if let Some(path) = file_header(trimmed)
            && !text.starts_with(' ')
        {
            file = Some(bounded_text(path));
            last_error = None;
            continue;
        }
        if let Some(message) = trimmed.strip_prefix("error: ") {
            last_error = Some(bounded_text(message));
            continue;
        }
        if trimmed.starts_with("# Unhandled error") {
            facts.unhandled += 1;
            continue;
        }
        if trimmed.starts_with("Bailed out after ") {
            facts.bailed = true;
            continue;
        }
        if let Some(title) = failed_title(trimmed) {
            let id = match &file {
                Some(file) => format!("{file} > {title}"),
                None => title.to_string(),
            };
            let id = bounded_text(&id);
            if records.contains_key(&id) {
                repeated = true;
            } else if records.len() >= MAX_PROVIDER_ITEMS {
                bound_hit = true;
            } else {
                records.insert(
                    id,
                    Record {
                        status: "failed",
                        message: last_error.take(),
                        stream: line.stream,
                        line: line.number,
                    },
                );
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Ran ")
            && let Some((count, tail)) = rest.split_once(' ')
            && (tail.starts_with("test across ") || tail.starts_with("tests across "))
            && let Ok(count) = count.parse::<u64>()
        {
            facts.ran = Some(count);
            continue;
        }
        if text.starts_with(' ')
            && let Some((number, label)) = trimmed.split_once(' ')
            && let Ok(number) = number.parse::<u64>()
            && let Some(category) = CATEGORIES.iter().find(|category| **category == label)
        {
            let category = if *category == "errors" {
                "error"
            } else {
                *category
            };
            facts.summary.insert(category, number);
        }
    }
    (records, facts, bound_hit, repeated)
}

fn completion(
    records: &BTreeMap<String, Record>,
    facts: &Facts,
    repeated: bool,
    exit_code: Option<i32>,
) -> Completion {
    if facts.summary.is_empty() || facts.ran.is_none() {
        return Completion::Unproven("bun test completion summary was not observed");
    }
    let Some(exit_code) = exit_code else {
        return Completion::Unproven("bun test completion requires an observed process exit code");
    };
    let count = |name: &str| facts.summary.get(name).copied().unwrap_or(0);
    let failing = count("fail") + count("error") + facts.unhandled > 0;
    if failing != (exit_code != 0) {
        return Completion::Conflicting(
            "bun test summary disagrees with the observed process exit",
        );
    }
    if repeated || records.len() as u64 != count("fail") {
        return Completion::Conflicting(
            "bun test names a different number of failing tests than it counts",
        );
    }
    if facts.ran != Some(count("pass") + count("fail") + count("skip") + count("todo")) {
        return Completion::Conflicting(
            "bun test summary counts a different number of tests than it ran",
        );
    }
    Completion::Unproven(TEXT_ONLY)
}

/// Normalize a `bun test` run. For a package script, `None` unless the
/// output is recognizably Bun's.
#[allow(clippy::too_many_arguments)]
pub(super) fn normalize(
    launch: &Launch<'_>,
    lines: &[InputLine<'_>],
    input_bytes: usize,
    bound_hit: bool,
    capture_complete: bool,
    exit_code: Option<i32>,
) -> Option<(CodingProviderResult, Completion)> {
    let invocation = invocation(launch);
    let (records, facts, parse_bound, repeated) = parse(lines);
    let summary_seen = !facts.summary.is_empty() && facts.ran.is_some();
    if matches!(launch, Launch::Script(..)) && !(facts.banner && summary_seen) {
        return None;
    }
    let completion = completion(&records, &facts, repeated, exit_code);
    let bound_hit = bound_hit || parse_bound;
    let complete = capture_complete && !bound_hit;
    let count = |name: &str| facts.summary.get(name).copied().unwrap_or(0);
    let failing = !records.is_empty() || count("fail") > 0;
    let mut warnings = Vec::new();
    if !summary_seen {
        warnings.push(
            "bun test completion summary was not observed; selection exhaustiveness is unproven"
                .to_string(),
        );
    }
    if !invocation.filters.is_empty() {
        warnings.push(format!(
            "{} runs a subset of the tests; broader absence is unprovable",
            invocation.filters.join(" ")
        ));
    }
    if (invocation.bail || facts.bailed) && failing {
        warnings.push(
            "--bail stops after a failure, so later tests may not have run; rerun without --bail to prove selection exhaustion"
                .to_string(),
        );
    }
    let selection = SelectionCoverage {
        scopes: invocation.scopes.clone(),
        exhaustive: complete
            && summary_seen
            && invocation.filters.is_empty()
            && !((invocation.bail || facts.bailed) && failing),
        ..SelectionCoverage::default()
    };
    let mut tests = records
        .iter()
        .map(|(id, record)| NormalizedTest {
            node_id: id.clone(),
            status: record.status.to_string(),
            message: record.message.clone(),
            evidence_stream: record.stream,
            evidence_line: Some(record.line.try_into().unwrap_or(u64::MAX)),
            report_pointer: None,
        })
        .collect::<Vec<_>>();
    if facts.unhandled > 0 || count("error") > 0 {
        tests.push(NormalizedTest {
            node_id: "<unhandled error between tests>".to_string(),
            status: "error".to_string(),
            message: Some("an error was thrown outside any test".to_string()),
            evidence_stream: "stderr",
            evidence_line: None,
            report_pointer: None,
        });
    }
    let counts = summary_seen.then(|| {
        json!({
            "passed": count("pass"),
            "failed": count("fail"),
            "error": count("error").max(facts.unhandled),
            "skipped": count("skip") + count("todo"),
            "xfailed": 0,
            "xpassed": 0,
            "executed": count("pass") + count("fail")
        })
    });
    let item_count = tests.len();
    let result = provider_result(
        "bun_test.v1",
        "bun_test_text",
        0.9,
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
            "runner": "bun",
            "filters": invocation.filters,
            "bail": invocation.bail || facts.bailed,
            "targets": invocation.targets
        }),
        warnings,
    );
    Some((result, completion))
}

#[cfg(test)]
mod tests {
    use super::super::normalize_coding_output;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    /// Bun 1.3.14 in an agent session (`CLAUDECODE=1`): only the failing test
    /// is named; the source excerpt is trimmed.
    const AGENT_FAILING: &str = "
tests/calc.test.js:
4 |   test('adds', () => { expect(add(2, 3)).toBe(5); });
                                             ^
error: expect(received).toBe(expected)

Expected: 5
Received: -1

      at <anonymous> (/repo/tests/calc.test.js:4:42)
(fail) calc > adds [0.16ms]

 1 pass
 1 skip
 1 todo
 1 fail
 2 expect() calls
Ran 4 tests across 1 file. [6.00ms]
";

    #[test]
    fn agent_output_names_failures_and_counts_but_never_proves_a_fix() {
        let result = normalize_coding_output(
            &argv(&["bun", "test"]),
            "bun test v1.3.14 (0d9b296a)\n",
            AGENT_FAILING,
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(result.provider, "bun_test.v1");
        assert!(result.complete);
        assert!(!result.selection.exhaustive);
        assert_eq!(
            result.normalized["tests"][0]["node_id"],
            "tests/calc.test.js > calc > adds"
        );
        assert_eq!(
            result.normalized["tests"][0]["message"],
            "expect(received).toBe(expected)"
        );
        let counts = &result.normalized["counts"];
        assert_eq!(
            (
                counts["passed"].as_u64(),
                counts["failed"].as_u64(),
                counts["skipped"].as_u64()
            ),
            (Some(1), Some(1), Some(2))
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("cannot prove a fix"))
        );
        // A summary the exit contradicts, or a count it cannot account for.
        for (text, exit) in [
            (AGENT_FAILING.to_string(), 0),
            (AGENT_FAILING.replace("Ran 4 tests", "Ran 5 tests"), 1),
            (AGENT_FAILING.replace(" 1 fail\n", " 2 fail\n"), 1),
        ] {
            let result =
                normalize_coding_output(&argv(&["bun", "test"]), "", &text, true, Some(exit))
                    .unwrap();
            assert!(!result.complete, "{text}");
        }
    }

    #[test]
    fn bail_and_filters_are_never_exhaustive() {
        let bailed = "
tests/more.test.js:
error: expect(received).toBe(expected)
(fail) other fails [4.26ms]
Ran 1 test across 1 file. [91.00ms]
Bailed out after 1 failure
";
        let result =
            normalize_coding_output(&argv(&["bun", "test", "--bail"]), "", bailed, true, Some(1))
                .unwrap();
        assert!(!result.selection.exhaustive);
        assert!(result.normalized["counts"].is_null());
        assert!(result.warnings.iter().any(|w| w.contains("--bail")));
        let filtered = normalize_coding_output(
            &argv(&["bun", "test", "-t", "adds", "tests/calc"]),
            "",
            AGENT_FAILING,
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(filtered.selection.scopes, ["bun:test:tests/calc"]);
        assert!(
            filtered
                .warnings
                .iter()
                .any(|w| w.contains("-t runs a subset"))
        );
    }

    #[test]
    fn package_scripts_are_claimed_only_for_bun_output() {
        let result = normalize_coding_output(
            &argv(&["npm", "test"]),
            "\n> app@1.0.0 test\n> bun test\n\nbun test v1.3.14 (0d9b296a)\n",
            AGENT_FAILING,
            true,
            Some(1),
        )
        .unwrap();
        assert_eq!(result.provider, "bun_test.v1");
        // Without Bun's banner a script's output is not claimed.
        assert!(
            normalize_coding_output(&argv(&["npm", "test"]), "", AGENT_FAILING, true, Some(1))
                .is_none()
        );
    }
}

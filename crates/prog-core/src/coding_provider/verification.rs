//! Cross-check completion claims without replacing retained source evidence.

use std::collections::{BTreeMap, BTreeSet};

use super::*;

enum Completion {
    Consistent,
    Unproven(&'static str),
    Conflicting(&'static str),
}

pub(super) fn constrain_completion(
    result: &mut CodingProviderResult,
    argv: &[String],
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) {
    // The normalizer's input bounds also bound this second, pure pass.
    if !result.complete {
        return;
    }
    let completion = match provider_kind(argv) {
        Some(ProviderKind::Pytest { .. }) => {
            if result.input_format == "pytest_json_report" {
                pytest_json(stdout, stderr, exit_code)
            } else {
                pytest_text(result, stdout, stderr, exit_code)
            }
        }
        Some(ProviderKind::CargoRust { program, args }) => {
            cargo_rust(result, program, args, stdout, stderr, exit_code)
        }
        None => return,
    };
    match completion {
        Completion::Consistent => {}
        Completion::Unproven(reason) => {
            result.selection.exhaustive = false;
            result.warnings.push(reason.to_string());
        }
        Completion::Conflicting(reason) => {
            result.complete = false;
            result.selection.exhaustive = false;
            result.warnings.push(reason.to_string());
        }
    }
}

fn count(value: &Value, key: &str) -> Option<u64> {
    value.get(key).map_or(Some(0), Value::as_u64)
}

fn pytest_json(stdout: &str, stderr: &str, exit_code: Option<i32>) -> Completion {
    // A report embedded in a larger stream is still useful for positive
    // findings, but cannot hide later failures or a second, conflicting report.
    let text = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (false, true) => stdout,
        (true, false) => stderr,
        _ => {
            return Completion::Conflicting(
                "pytest JSON report has additional stream output; completion is unproven",
            );
        }
    };
    let Ok(report) = serde_json::from_str::<Value>(text) else {
        return Completion::Conflicting(
            "pytest JSON report is embedded in other output; completion is unproven",
        );
    };
    let Some(tests) = report["tests"].as_array() else {
        return Completion::Conflicting("pytest JSON report has no test records");
    };
    let summary = &report["summary"];
    let mut outcomes = BTreeMap::<&str, u64>::new();
    let mut identities = BTreeSet::new();
    for test in tests {
        let Some(outcome) = test["outcome"].as_str() else {
            return Completion::Conflicting("pytest JSON test has no outcome");
        };
        if !identities.insert(test["nodeid"].as_str()) {
            return Completion::Conflicting("pytest JSON report repeats a test identity");
        }
        *outcomes.entry(outcome).or_default() += 1;
        if outcome == "passed"
            && ["setup", "call", "teardown"].iter().any(|stage| {
                test.get(stage).is_some_and(|stage| {
                    stage.get("outcome").and_then(Value::as_str) != Some("passed")
                })
            })
        {
            return Completion::Conflicting(
                "pytest JSON passing outcome conflicts with a test stage",
            );
        }
    }
    if summary["total"].as_u64() != Some(tests.len() as u64)
        || ["passed", "failed", "error", "skipped", "xfailed", "xpassed"]
            .iter()
            .any(|status| count(summary, status) != Some(*outcomes.get(status).unwrap_or(&0)))
    {
        return Completion::Conflicting("pytest JSON summary disagrees with test records");
    }
    if let Some(collectors) = report.get("collectors")
        && collectors.as_array().is_none_or(|collectors| {
            collectors
                .iter()
                .any(|collector| collector["outcome"] != "passed")
        })
    {
        return Completion::Unproven("pytest JSON collection did not complete successfully");
    }
    let deselected = count(summary, "deselected");
    if deselected.is_none()
        || summary.get("collected").is_some_and(|collected| {
            collected.as_u64() != deselected.and_then(|n| n.checked_add(tests.len() as u64))
        })
    {
        return Completion::Conflicting(
            "pytest JSON collection counts disagree with selected tests",
        );
    }
    let report_exit = report["exitcode"].as_i64();
    if report_exit.is_none() || exit_code.is_none() {
        return Completion::Unproven(
            "pytest completion requires a reported and observed process exit code",
        );
    }
    if report_exit != exit_code.map(i64::from) {
        return Completion::Conflicting(
            "pytest JSON exitcode disagrees with the observed process exit",
        );
    }
    let failed =
        outcomes.get("failed").copied().unwrap_or(0) + outcomes.get("error").copied().unwrap_or(0);
    if matches!(exit_code, Some(0 | 1)) && (exit_code == Some(0)) != (failed == 0) {
        return Completion::Conflicting("pytest JSON outcomes disagree with the process exit");
    }
    if tests.is_empty()
        || deselected != Some(0)
        || outcomes.get("skipped").is_some_and(|n| *n > 0)
        || !matches!(exit_code, Some(0 | 1))
    {
        return Completion::Unproven(
            "pytest selected tests were empty, skipped, deselected, or interrupted",
        );
    }
    Completion::Consistent
}

// Counts are parsed only from anchored completion summaries, not arbitrary
// occurrences of words such as "passed" in diagnostics or test names.
fn counts(text: &str, delimiter: char) -> Option<BTreeMap<&str, u64>> {
    let mut counts = BTreeMap::new();
    for part in text.split(delimiter) {
        let (number, status) = part.trim().split_once(' ')?;
        if counts.insert(status.trim(), number.parse().ok()?).is_some() {
            return None;
        }
    }
    Some(counts)
}

fn pytest_text(
    result: &CodingProviderResult,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> Completion {
    if stdout.lines().chain(stderr.lines()).any(|line| {
        let text = line.trim();
        text.starts_with("INTERNALERROR>") || text.contains("ERROR collecting ")
    }) {
        return Completion::Unproven("pytest reported an internal or collection error");
    }
    let summaries: Vec<_> = stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(|line| {
            let text = line.trim().trim_matches('=').trim();
            let (summary, duration) = text.split_once(" in ")?;
            let seconds = duration
                .split_whitespace()
                .next()?
                .strip_suffix('s')?
                .parse::<f64>()
                .ok()?;
            if !seconds.is_finite() || seconds < 0.0 {
                return None;
            }
            if summary == "no tests ran" {
                return Some(BTreeMap::new());
            }
            let parsed = counts(summary, ',')?;
            parsed
                .keys()
                .any(|key| {
                    matches!(
                        *key,
                        "passed"
                            | "failed"
                            | "error"
                            | "errors"
                            | "skipped"
                            | "deselected"
                            | "xfailed"
                            | "xpassed"
                    )
                })
                .then_some(parsed)
        })
        .collect();
    if summaries.len() != 1 {
        return Completion::Unproven("pytest requires one unambiguous completion summary");
    }
    let summary = &summaries[0];
    let get = |key: &str| summary.get(key).copied().unwrap_or(0);
    let failed = get("failed")
        .saturating_add(get("error"))
        .saturating_add(get("errors"));
    let mut observed = BTreeMap::<&str, u64>::new();
    for test in result.normalized["tests"].as_array().into_iter().flatten() {
        if let Some(status) = test["status"].as_str() {
            *observed.entry(status).or_default() += 1;
        }
    }
    if observed.iter().any(|(status, n)| {
        *n > if *status == "error" {
            get("error").saturating_add(get("errors"))
        } else {
            get(status)
        }
    }) {
        return Completion::Conflicting(
            "pytest completion summary contradicts observed test outcomes",
        );
    }
    if matches!(exit_code, Some(0 | 1)) && (exit_code == Some(0)) != (failed == 0) {
        return Completion::Conflicting("pytest summary disagrees with the observed process exit");
    }
    if !matches!(exit_code, Some(0 | 1))
        || get("passed")
            .saturating_add(failed)
            .saturating_add(get("xfailed"))
            .saturating_add(get("xpassed"))
            == 0
        || get("skipped") > 0
        || get("deselected") > 0
    {
        return Completion::Unproven(
            "pytest selected tests were empty, skipped, deselected, or interrupted",
        );
    }
    Completion::Consistent
}

fn cargo_rust(
    result: &CodingProviderResult,
    program: &str,
    args: &[String],
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> Completion {
    let test_invocation = program == "cargo"
        && cargo_subcommand(args).is_some_and(|(_, subcommand)| subcommand == "test");
    let mut passed = 0u64;
    let mut failed = 0u64;
    let mut skipped = false;
    let mut structured_failure = false;
    for line in stdout.lines().chain(stderr.lines()) {
        let text = line.trim();
        if let Ok(value) = serde_json::from_str::<Value>(text)
            && value["reason"] == "build-finished"
            && value["success"] != true
        {
            structured_failure = true;
        }
        let Some(summary) = text.strip_prefix("test result:") else {
            continue;
        };
        let Some((status, summary)) = summary.trim().split_once(". ") else {
            return Completion::Conflicting("Cargo test summary format is unrecognized");
        };
        let summary = summary.split("; finished in ").next().unwrap_or(summary);
        let Some(parsed) = counts(summary, ';') else {
            return Completion::Conflicting("Cargo test summary counts are malformed");
        };
        let (Some(p), Some(f)) = (parsed.get("passed"), parsed.get("failed")) else {
            return Completion::Conflicting("Cargo test summary omits passed or failed counts");
        };
        if !matches!(status, "ok" | "FAILED") || (status == "ok") != (*f == 0) {
            return Completion::Conflicting("Cargo test summary status contradicts its counts");
        }
        passed = passed.saturating_add(*p);
        failed = failed.saturating_add(*f);
        skipped |= parsed.get("ignored").is_some_and(|n| *n > 0)
            || parsed.get("filtered out").is_some_and(|n| *n > 0);
    }
    let compile_failure = result.normalized["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|diagnostic| {
            diagnostic["severity"] == "error"
                && !(test_invocation
                    && failed > 0
                    && diagnostic["diagnostic_code"].is_null()
                    && diagnostic["message"].as_str().is_some_and(|message| {
                        message
                            .strip_prefix("test failed, to rerun pass ")
                            .is_some_and(|target| !target.is_empty())
                    }))
        });
    let observed_failure = result.normalized["tests"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|test| test["status"] == "failed");
    if exit_code == Some(0)
        && (compile_failure || structured_failure || failed > 0 || observed_failure)
    {
        return Completion::Conflicting(
            "Cargo/rustc output reports failure despite a successful process exit",
        );
    }
    if test_invocation && observed_failure && failed == 0 {
        return Completion::Conflicting(
            "Cargo passing summaries contradict an observed test failure",
        );
    }
    let normal_exit = exit_code == Some(0)
        || program == "cargo" && exit_code == Some(101)
        || program == "rustc" && exit_code == Some(1);
    if !normal_exit
        || compile_failure
        || structured_failure
        || exit_code != Some(0) && failed == 0
        || test_invocation && (passed.saturating_add(failed) == 0 || skipped)
    {
        return Completion::Unproven(
            "Cargo/rustc completion is unproven: empty or skipped tests, incomplete compilation, or an unexplained process exit",
        );
    }
    Completion::Consistent
}

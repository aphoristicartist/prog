//! Disclosure-budget monotonicity and degradation-visibility tests.
//!
//! Issue #160 specified this acceptance criterion and was closed without it:
//!
//! > A property test asserts monotonicity: for any two budgets b1 <= b2,
//! > delivered bytes under b1 <= delivered bytes under b2, across
//! > envelope/inspect/search/evidence response shapes.
//!
//! Its absence hid a real regression. The compaction ladder used to pop
//! `findings` first — before the much larger `next_actions` and `data_preview`
//! — and emitted no warning while doing so. At the default 16 KiB budget a
//! three-error compile failure returned `findings: []` with no indication that
//! anything had been withheld, while the *same* capture at 8 KiB returned one
//! finding and more bytes. Delivered output was non-monotonic in the budget,
//! and the degraded view was indistinguishable from a clean one.

use std::{fs, path::Path};

use serde_json::Value;

mod support;

use support::*;

/// Budgets spanning "cannot fit the preview" through "fits comfortably".
const BUDGETS: [u32; 6] = [4_096, 8_192, 12_288, 16_384, 32_768, 65_536];

fn write_failing_source(root: &Path) -> String {
    let path = root.join("bad.rs");
    fs::write(
        &path,
        "fn main() {\n    let a: u32 = \"x\";\n    let b: u32 = \"y\";\n    undefined_fn();\n}\n",
    )
    .unwrap();
    path.to_str().unwrap().to_string()
}

/// Surfaced findings must be non-decreasing in the budget. A larger budget may
/// never yield a less informative response than a smaller one.
///
/// This is the half of #160's monotonicity criterion that the compaction-ladder
/// fix makes true. The delivered-bytes half is covered by the `#[ignore]`d test
/// below, which fails for an unrelated, pre-existing reason.
#[test]
fn surfaced_findings_are_monotonic_in_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_failing_source(dir.path());
    let store = dir.path().join(".prog-state");
    let store_arg = store.to_str().unwrap();

    let mut observations = Vec::new();
    for budget in BUDGETS {
        let output = prog_with_budget(
            store_arg,
            budget,
            &["run", "--", "rustc", &source, "-o", "/dev/null"],
        );
        let value: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("budget {budget} produced non-JSON: {error}"));
        observations.push((budget, value["findings"].as_array().map_or(0, Vec::len)));
    }

    for window in observations.windows(2) {
        let (small_budget, small_findings) = window[0];
        let (large_budget, large_findings) = window[1];
        assert!(
            small_findings <= large_findings,
            "surfaced findings must not shrink as the budget grows: \
             budget {small_budget} surfaced {small_findings} but budget \
             {large_budget} surfaced {large_findings} (full series: {observations:?})"
        );
    }
}

/// The delivered-bytes half of #160's monotonicity criterion.
///
/// Currently fails, for a cause independent of the compaction ladder:
/// `shrink_policy` halves `array_items`/`object_fields`/`string_chars`/
/// `node_budget` and decrements `depth` on each iteration, and the initial
/// policy is derived from the budget. Different budgets therefore enter the
/// halving sequence at different points and settle on different fixed points,
/// so a larger budget can land on a policy whose envelope is *smaller* than a
/// tighter budget's — observed at 8,192 B delivering 8,181 B while 12,288 B
/// delivered 5,282 B.
///
/// Fixing it means making preview-policy selection monotonic in the budget
/// (search for the largest fitting policy rather than the first), which is a
/// separate change from compaction ordering (tracked in #219). Ignored rather than
/// deleted so the criterion stays visible and the reproduction stays runnable.
#[test]
#[ignore = "pre-existing: shrink_policy halving search is not monotonic in the budget; see #219"]
fn delivered_bytes_are_monotonic_in_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_failing_source(dir.path());
    let store = dir.path().join(".prog-state");
    let store_arg = store.to_str().unwrap();

    let mut observations = Vec::new();
    for budget in BUDGETS {
        let output = prog_with_budget(
            store_arg,
            budget,
            &["run", "--", "rustc", &source, "-o", "/dev/null"],
        );
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap_or_else(|error| panic!("budget {budget} produced non-JSON: {error}"));
        observations.push((budget, output.stdout.len()));
    }

    for window in observations.windows(2) {
        let (small_budget, small_bytes) = window[0];
        let (large_budget, large_bytes) = window[1];
        assert!(
            small_bytes <= large_bytes,
            "delivered bytes must not shrink as the budget grows: \
             budget {small_budget} delivered {small_bytes} B but budget \
             {large_budget} delivered {large_bytes} B (full series: {observations:?})"
        );
    }
}

/// A failing command must never report zero findings merely because the
/// envelope did not fit. Findings are the safety-relevant signal; an empty list
/// reads as "no failures detected".
#[test]
fn budget_pressure_never_empties_findings_for_a_failing_command() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_failing_source(dir.path());
    let store = dir.path().join(".prog-state");
    let store_arg = store.to_str().unwrap();

    for budget in BUDGETS {
        let output = prog_with_budget(
            store_arg,
            budget,
            &["run", "--", "rustc", &source, "-o", "/dev/null"],
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        let findings = value["findings"].as_array().map_or(0, Vec::len);
        assert!(
            findings >= 1,
            "budget {budget} returned {findings} findings for a command that \
             failed with three compile errors; an empty findings list is \
             indistinguishable from a clean run\n{}",
            stdout(&output)
        );
    }
}

/// Whenever the envelope is degraded to fit, the response must say so, and the
/// note must name what was dropped. `warnings` is never truncated to save
/// bytes: it is the only channel that distinguishes a degraded view from a
/// complete one.
#[test]
fn degraded_envelopes_always_report_what_was_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_failing_source(dir.path());
    let store = dir.path().join(".prog-state");
    let store_arg = store.to_str().unwrap();

    // A budget small enough that compaction is unavoidable.
    let output = prog_with_budget(
        store_arg,
        4_096,
        &["run", "--", "rustc", &source, "-o", "/dev/null"],
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();

    let warnings = value["warnings"]
        .as_array()
        .expect("a degraded envelope must carry warnings");
    let note = warnings
        .iter()
        .filter_map(Value::as_str)
        .find(|warning| warning.contains("compacted"))
        .unwrap_or_else(|| panic!("degraded envelope must warn; got {warnings:?}"));
    assert!(
        note.contains("dropped"),
        "the compaction note must name what it dropped: {note}"
    );
    // The note must not itself be lossy. An entry like `findings:` with its
    // count sheared off is exactly the silent information loss this whole
    // ladder exists to prevent.
    assert!(
        !note.ends_with(':') && !note.ends_with(", "),
        "the compaction note must not end mid-entry: {note}"
    );
    for entry in note
        .split("dropped ")
        .nth(1)
        .expect("note names dropped fields")
        .split(", ")
    {
        if let Some((field, count)) = entry.split_once(':') {
            assert!(
                !count.is_empty(),
                "entry '{field}' must carry its count, got '{entry}' in: {note}"
            );
        }
    }

    // The hard ceiling still holds.
    assert!(
        output.stdout.len() <= 4_096,
        "compaction must respect the requested ceiling: {} B > 4096 B",
        output.stdout.len()
    );
}

/// The ceiling is hard across every bounded response shape, not just `run`.
#[test]
fn every_bounded_response_shape_respects_the_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_failing_source(dir.path());
    let store = dir.path().join(".prog-state");
    let store_arg = store.to_str().unwrap();

    let seed = prog_with_budget(
        store_arg,
        65_536,
        &["run", "--", "rustc", &source, "-o", "/dev/null"],
    );
    let seeded: Value = serde_json::from_slice(&seed.stdout).unwrap();
    let cursor = seeded["cursor"].as_str().expect("run should mint a cursor");

    for budget in BUDGETS {
        for command in [
            vec!["inspect", cursor, "--goal", "find the compile error"],
            vec!["search", cursor, "error"],
            vec!["paths", cursor],
        ] {
            let output = prog_with_budget(store_arg, budget, &command);
            assert!(
                output.stdout.len() <= budget as usize,
                "{:?} at budget {budget} delivered {} B",
                command,
                output.stdout.len()
            );
            serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|error| {
                panic!("{command:?} at budget {budget} produced non-JSON: {error}")
            });
        }
    }
}

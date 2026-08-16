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

use serde_json::{Value, json};

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

fn assert_sizes_are_monotonic(label: &str, observations: &[(u32, usize)]) {
    for window in observations.windows(2) {
        let (small_budget, small_bytes) = window[0];
        let (large_budget, large_bytes) = window[1];
        assert!(
            small_bytes <= large_bytes,
            "{label} must not shrink as the budget grows: budget {small_budget} delivered \
             {small_bytes} B but budget {large_budget} delivered {large_bytes} B \
             (full series: {observations:?})"
        );
    }
}

fn normalized_run_envelope_bytes(mut envelope: Value) -> usize {
    // Every budget executes the command independently. `duration_ms` is real
    // provenance, but its decimal width varies with runner scheduling and is
    // unrelated to disclosure-policy selection. It appears both in the outer
    // envelope provenance and in the projected run payload. Canonicalize those
    // two exact values while retaining the fields and every budget-dependent
    // byte. Without this, a 99 ms versus 101 ms execution can fabricate a
    // monotonicity failure even when both envelopes selected the same policy.
    for pointer in [
        "/provenance/duration_ms",
        "/data_preview/command/duration_ms",
    ] {
        if let Some(duration_ms) = envelope.pointer_mut(pointer) {
            *duration_ms = json!(1_000);
        }
    }
    serde_json::to_vec(&envelope).unwrap().len()
}

#[test]
fn run_envelope_size_normalization_ignores_only_duplicate_duration_values() {
    let short_duration = json!({
        "provenance": { "duration_ms": 9 },
        "data_preview": { "command": { "duration_ms": 10 } },
        "findings": []
    });
    let mut long_duration = json!({
        "provenance": { "duration_ms": 999 },
        "data_preview": { "command": { "duration_ms": 1_000 } },
        "findings": []
    });
    assert_eq!(
        normalized_run_envelope_bytes(short_duration),
        normalized_run_envelope_bytes(long_duration.clone())
    );

    long_duration["findings"] = json!([{"kind": "compile_error"}]);
    assert_ne!(
        normalized_run_envelope_bytes(json!({
            "provenance": { "duration_ms": 999 },
            "data_preview": { "command": { "duration_ms": 1_000 } },
            "findings": []
        })),
        normalized_run_envelope_bytes(long_duration)
    );
}

/// Surfaced findings must be non-decreasing in the budget. A larger budget may
/// never yield a less informative response than a smaller one.
///
/// This is the half of #160's monotonicity criterion that the compaction-ladder
/// fix makes true. The delivered-bytes half is covered by the next test.
#[test]
fn surfaced_findings_are_monotonic_in_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_failing_source(dir.path());

    let mut observations = Vec::new();
    for budget in BUDGETS {
        // Each budget receives the same first observation in a fresh store.
        // Reusing one store would compare different subjects: automatic delta
        // metadata depends on the preceding observation and is not a property
        // of preview-policy selection.
        let store = dir.path().join(format!("state-{budget}"));
        let output = prog_with_budget(
            store.to_str().unwrap(),
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
/// This used to fail for a cause independent of the compaction ladder:
/// `shrink_policy` halves `array_items`/`object_fields`/`string_chars`/
/// `node_budget` and decrements `depth` on each iteration, and the initial
/// policy is derived from the budget. Different budgets therefore enter the
/// halving sequence at different points and settle on different fixed points,
/// so a larger budget can land on a policy whose envelope is *smaller* than a
/// tighter budget's — observed at 8,192 B delivering 8,181 B while 12,288 B
/// delivered 5,282 B.
///
/// Preview-policy selection now walks one budget-independent finite ladder and
/// returns the first (richest) policy that fits. Optional delta metadata is
/// evaluated within each level so it cannot force primary evidence to coarsen.
#[test]
fn delivered_bytes_are_monotonic_in_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_failing_source(dir.path());

    let mut observations = Vec::new();
    for budget in BUDGETS {
        // Keep the payload and observation position fixed. Reusing one store
        // would add state-dependent automatic delta metadata after the first
        // capture, so the runs would no longer vary only by disclosure budget.
        let store = dir.path().join(format!("state-{budget}"));
        let output = prog_with_budget(
            store.to_str().unwrap(),
            budget,
            &["run", "--", "rustc", &source, "-o", "/dev/null"],
        );
        let value = serde_json::from_slice::<Value>(&output.stdout)
            .unwrap_or_else(|error| panic!("budget {budget} produced non-JSON: {error}"));
        observations.push((budget, normalized_run_envelope_bytes(value)));
    }
    assert_sizes_are_monotonic("run envelope", &observations);
}

/// Recovery affordances stay compact enough that evidence, findings, and
/// observation metadata—not repeated cursor-bearing commands—own the budget.
#[test]
fn action_encoding_stays_below_a_fixed_envelope_share() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_failing_source(dir.path());
    let store = dir.path().join(".prog-state");
    let output = prog_with_budget(
        store.to_str().unwrap(),
        65_536,
        &["run", "--", "rustc", &source, "-o", "/dev/null"],
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let action_bytes = serde_json::to_vec(&value["next_actions"]).unwrap().len()
        + serde_json::to_vec(&value["action_templates"])
            .unwrap()
            .len();
    assert!(
        action_bytes * 100 <= output.stdout.len() * 18,
        "next_actions + action_templates must stay at or below 18% of the envelope: \
         {action_bytes} of {} bytes",
        output.stdout.len()
    );
    let cursor = value["cursor"].as_str().expect("run cursor");
    assert!(
        !serde_json::to_string(&value["next_actions"])
            .unwrap()
            .contains(cursor),
        "per-action records must not repeat the top-level cursor"
    );
}

#[test]
fn observe_and_call_envelopes_are_monotonic_in_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let source = write_failing_source(dir.path());

    let observe_store = dir.path().join("observe-state");
    let observe_store_arg = observe_store.to_str().unwrap();
    let mut observe_sizes = Vec::new();
    for budget in BUDGETS {
        let output = prog_with_budget(
            observe_store_arg,
            budget,
            &["observe", "--file", &source, "--name", "bad-rust-source"],
        );
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap_or_else(|error| panic!("observe budget {budget} produced non-JSON: {error}"));
        observe_sizes.push((budget, output.stdout.len()));
    }
    assert_sizes_are_monotonic("observe envelope", &observe_sizes);

    let call_store = dir.path().join("call-state");
    let call_store_arg = call_store.to_str().unwrap();
    let fixture = repo_root().join("fixtures/cli/list_items.py");
    let fixture_arg = fixture.to_str().unwrap();
    let added = prog_with_budget(
        call_store_arg,
        65_536,
        &[
            "source",
            "add-cli",
            "demo",
            "--operation",
            "list",
            "--read-only",
            "--",
            "python3",
            fixture_arg,
        ],
    );
    assert!(added.status.success(), "{}", stdout(&added));

    let mut call_sizes = Vec::new();
    for budget in BUDGETS {
        let output = prog_with_budget(
            call_store_arg,
            budget,
            &["call", "demo", "list", "--args", "{}"],
        );
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap_or_else(|error| panic!("call budget {budget} produced non-JSON: {error}"));
        call_sizes.push((budget, output.stdout.len()));
    }
    assert_sizes_are_monotonic("call envelope", &call_sizes);
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

    for command in [
        vec!["inspect", cursor, "--goal", "find the compile error"],
        vec!["search", cursor, "error"],
        vec!["paths", cursor],
        vec!["evidence", cursor, "--path", "/failure_sections/0"],
    ] {
        let mut observations = Vec::new();
        for budget in BUDGETS {
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
            observations.push((budget, output.stdout.len()));
        }
        assert_sizes_are_monotonic(&format!("{command:?}"), &observations);
    }
}

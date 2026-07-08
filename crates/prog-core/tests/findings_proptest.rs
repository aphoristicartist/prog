//! Property and golden tests for the generic findings ranking engine.
//!
//! These pin the engine's contract guarantees that unit tests alone cannot
//! cover cheaply:
//!
//! - **Purity / determinism** — `ranked_findings` called repeatedly on the same
//!   payload yields byte-identical output (the engine has no hidden state).
//! - **Order-independence** — rebuilding the same logical object with reordered
//!   JSON keys produces an identical ranking. `serde_json` is built with
//!   `preserve_order`, so key insertion order is observable; the
//!   `best_by_path_kind` dedup plus the `compare_candidates` total order must
//!   wash that out. This is the invariant recorded in `INVARIANTS.md`.
//! - **Rank contiguity** — ranks are exactly `1..=len`, strictly increasing,
//!   every confidence lies in `[0, 1]`, and no `(path, kind)` pair repeats.
//! - **Golden snapshots** — checked-in `*.expected.json` fixtures asserted via
//!   deterministic `serde_json::to_string_pretty`; regenerated env-gated by
//!   `PROG_FINDINGS_UPDATE` (mirrors the `PROG_TOKEN_EVAL_UPDATE` pattern).
//!
//! Regenerate the goldens with:
//!
//! ```text
//! PROG_FINDINGS_UPDATE=1 cargo test -p prog-core --test findings_proptest
//! ```

use std::collections::HashSet;

use proptest::prelude::*;
use serde_json::{Map, Value};

use prog_core::{CommandHintConfig, FindingOptions, ranked_findings};

/// Canonical options for the property tests: a root-cause goal and a cursor so
/// command hints are populated. Hints use NAV_ALL so the serialized output
/// exercises every hint field.
fn prop_options() -> FindingOptions {
    FindingOptions {
        goal: Some("find the root cause".to_string()),
        cursor: Some("pc1_prop".to_string()),
        hints: CommandHintConfig::NAV_ALL,
        ..FindingOptions::default()
    }
}

/// Bounded JSON strategy mixing plain leaves with signal-bearing strings so the
/// properties exercise real detector + ranking behavior, not just empty output.
fn signal_string() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("error: something failed".to_string()),
        Just("Traceback (most recent call last): boom".to_string()),
        Just("error[E0308]: mismatched types".to_string()),
        Just("error: could not compile `foo`".to_string()),
        Just("rustc: error: unresolved import".to_string()),
        Just("tests/test_x.py::test_case FAILED".to_string()),
        Just("test foo::bar ... FAILED".to_string()),
        Just("  2 passing (3s)\n  1 failing".to_string()),
        Just("diff --git a/a b/b\n--- a/a\n+++ b/b\n@@ -1,2 +1,2 @@\n".to_string()),
        Just("warning: deprecated".to_string()),
        Just("ordinary text without signals".to_string()),
        Just("".to_string()),
    ]
}

/// Object-key strategy biased toward the realistic signal-bearing field names
/// the structured detectors actually match (error/failure_sections/severity/
/// command/diff/...), with a short random fallback for unrelated keys. This
/// makes the order-independence property exercise key_signal / object-severity
/// / run-command / failure-sections detectors and their `(path, kind)` dedup,
/// not just trivial unique-path string leaves.
fn signal_key() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("error".to_string()),
        Just("errors".to_string()),
        Just("message".to_string()),
        Just("reason".to_string()),
        Just("summary".to_string()),
        Just("severity".to_string()),
        Just("level".to_string()),
        Just("status".to_string()),
        Just("warning".to_string()),
        Just("warnings".to_string()),
        Just("diagnostic".to_string()),
        Just("diagnostics".to_string()),
        Just("exception".to_string()),
        Just("failure".to_string()),
        Just("failures".to_string()),
        Just("failure_sections".to_string()),
        Just("command".to_string()),
        Just("diff".to_string()),
        Just("issues".to_string()),
        Just("detail".to_string()),
        "[a-z]{1,6}",
    ]
}

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (0..=100i64).prop_map(Value::from),
        signal_string().prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..3).prop_map(Value::Array),
            prop::collection::vec((signal_key(), inner.clone()), 0..4).prop_map(|pairs| {
                let mut map = Map::new();
                for (k, v) in pairs {
                    map.insert(k, v);
                }
                Value::Object(map)
            }),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn ranked_findings_is_pure_and_deterministic(payload in json_value()) {
        let options = prop_options();
        let mut last: Option<String> = None;
        for _ in 0..5 {
            let findings = ranked_findings(&payload, &options).unwrap();
            let serialized = serde_json::to_string(&findings).unwrap();
            if let Some(previous) = &last {
                prop_assert_eq!(&serialized, previous);
            }
            last = Some(serialized);
        }
    }

    #[test]
    fn ranking_is_order_independent_of_key_order(
        pairs in prop::collection::vec((signal_key(), json_value()), 1..6)
    ) {
        // Collapse any duplicate keys (keeping the first value) so that the
        // forward and reversed maps describe the *same* logical object with the
        // same key/value set, differing only in insertion order.
        let mut deduped: Vec<(String, Value)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (key, value) in pairs {
            if seen.insert(key.clone()) {
                deduped.push((key, value));
            }
        }

        // Same key/value pairs, two different insertion orders. Because
        // serde_json preserves order, collect_generic_signals iterates the
        // children in different orders; the final ranking must still match.
        let mut forward = Map::new();
        for (key, value) in &deduped {
            forward.insert(key.clone(), value.clone());
        }
        let mut reversed = Map::new();
        for (key, value) in deduped.iter().rev() {
            reversed.insert(key.clone(), value.clone());
        }

        let options = prop_options();
        let left = ranked_findings(&Value::Object(forward), &options).unwrap();
        let right = ranked_findings(&Value::Object(reversed), &options).unwrap();

        prop_assert_eq!(
            serde_json::to_value(&left).unwrap(),
            serde_json::to_value(&right).unwrap(),
            "ranking changed when JSON key order changed"
        );
    }

    #[test]
    fn ranks_are_contiguous_and_confidences_bounded(payload in json_value()) {
        let options = prop_options();
        let findings = ranked_findings(&payload, &options).unwrap();

        let mut seen = HashSet::new();
        for (index, finding) in findings.iter().enumerate() {
            prop_assert_eq!(
                finding.rank,
                index as u64 + 1,
                "ranks must be contiguous starting at 1"
            );
            prop_assert!(
                (0.0..=1.0).contains(&finding.confidence),
                "confidence {} out of [0, 1]",
                finding.confidence
            );
            let key = (finding.path.clone(), finding.kind.clone());
            prop_assert!(seen.insert(key.clone()), "duplicate (path, kind): {:?}", key);
        }
    }
}

// ---------------------------------------------------------------------------
// Golden snapshots
// ---------------------------------------------------------------------------

const GOLDEN_CASES: &[(&str, &str)] = &[
    ("run-failure", "why did the run fail"),
    ("compile-error", "find the root cause"),
    ("diff-review", "review the diff"),
    ("test-failure", "which test failed"),
    ("mixed", "find the root cause"),
];

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("findings")
}

fn assert_golden(name: &str, goal: &str) {
    let dir = fixtures_dir();
    let payload: Value =
        serde_json::from_slice(&std::fs::read(dir.join(format!("{name}.json"))).unwrap()).unwrap();

    // NAV_ALL so every hint field is captured in the snapshot, pinning the full
    // hint format (including prog evidence, which is off by default until #92).
    let options = FindingOptions {
        goal: Some(goal.to_string()),
        cursor: Some("pc1_golden".to_string()),
        hints: CommandHintConfig::NAV_ALL,
        ..FindingOptions::default()
    };
    let findings = ranked_findings(&payload, &options).unwrap();

    let mut actual = serde_json::to_string_pretty(&serde_json::to_value(&findings).unwrap())
        .expect("findings serialize");
    actual.push('\n');

    let expected_path = dir.join(format!("{name}.expected.json"));
    if std::env::var_os("PROG_FINDINGS_UPDATE").is_some() {
        std::fs::write(&expected_path, &actual).expect("write golden");
        eprintln!("updated {}", expected_path.display());
    } else {
        let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|error| {
            panic!(
                "missing golden {} ({error}). \
                 Regenerate with PROG_FINDINGS_UPDATE=1 cargo test \
                 -p prog-core --test findings_proptest",
                expected_path.display()
            )
        });
        assert_eq!(actual, expected, "golden mismatch for {name}");
    }

    // Contiguity invariant on the real fixture too.
    for (index, finding) in findings.iter().enumerate() {
        assert_eq!(
            finding.rank,
            index as u64 + 1,
            "{name}: non-contiguous rank"
        );
        assert!(
            (0.0..=1.0).contains(&finding.confidence),
            "{name}: confidence {} out of [0, 1]",
            finding.confidence
        );
    }
}

#[test]
fn golden_findings_snapshots_are_stable() {
    for (name, goal) in GOLDEN_CASES {
        assert_golden(name, goal);
    }
}

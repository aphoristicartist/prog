# Seeded false-completion suite

An agent can make test output look like a fix without fixing anything: delete
the failing test, skip it, run fewer tests, stop early, or edit the code after
the green run. Each of these produces output a naive "tests are green" check
accepts. This suite seeds those edits and requires that none of them is ever
reported as `resolved`, a `passed` obligation, `ready`, or a `fixed` receipt,
while a real fix still is.

It runs in `cargo test` with no network or model access:

```sh
cargo test -p prog-cli --test false_completion
```

The scenarios use deterministic stand-ins for `cargo` and `pytest` whose output
is selected by a state file, so each edit is exact and repeatable. The rules
they exercise are in [delta.md](delta.md) and invariant I14 in
[INVARIANTS.md](../INVARIANTS.md).

## Scenarios

| Edit after the failing run | Expected | Test |
|---|---|---|
| Fix the bug (same tests) | `resolved`, `ready`, receipt `fixed` | `cargo_real_fix_is_resolved`, `verbose_runs_prove_a_real_fix_and_catch_a_swapped_test`, `readiness_is_not_ready_when_the_failing_test_was_deleted` (its `fixed` case) |
| Delete the failing test | not resolved, obligation not `passed`, not ready | `cargo_deleted_failing_test_is_not_resolved`, `pytest_deleted_or_skipped_failing_test_is_not_resolved`, `readiness_is_not_ready_when_the_failing_test_was_deleted` |
| `#[ignore]` / `@pytest.mark.skip` the failing test | not resolved | `cargo_ignored_failing_test_is_not_resolved`, `pytest_deleted_or_skipped_failing_test_is_not_resolved` |
| Delete the failing test and add a passing one (same count) | not resolved | `verbose_runs_prove_a_real_fix_and_catch_a_swapped_test` |
| Rerun quietly (`-q`), so passing tests are not named | not proven; reasons say to rerun verbosely | `cargo_quiet_deleted_test_is_caught_by_executed_counts`, `quiet_runs_cannot_prove_a_fix_and_say_how_to_get_one` |
| Default fail-fast run | not proven; reasons name `--no-fail-fast` | `fail_fast_baseline_explains_how_to_get_a_provable_comparison` |
| Fix the bug in a suite that already skips a test (same named skips) | `resolved` | `a_fix_is_provable_when_both_runs_skip_the_same_named_tests` |
| Skip the failing test, swap which test is skipped, or skip without naming it | not resolved | `changing_or_unnamed_skips_never_prove_a_fix` |
| Any of the above, read as a receipt | receipt says `NOT PROVEN`, never `fixed` | `receipts_are_small_and_never_claim_an_unproven_fix` |

Related scenarios are covered where their machinery lives:

| Edit | Test |
|---|---|
| Rerun a narrower selection | `crates/prog-cli/tests/cli.rs::verification_treats_targeted_incomplete_reruns_as_not_observed` |
| Truncated or incomplete capture | `crates/prog-core/src/delta.rs::tests::incomplete_or_different_invocations_never_resolve`; timeouts and cancellation in `crates/prog-cli/tests/capture_lifecycle.rs` |
| Edit files after the green run, or change files without testing | `crates/prog-cli/tests/claude_code_hook.rs::{stop_gate_blocks_until_the_fix_is_proven_and_the_workspace_is_unchanged,stop_gate_blocks_changed_files_that_were_never_tested}` |

## Live trials

The suite proves the rules, not that agents fake fixes in practice or that
prog changes how often they do. Measuring that needs paid agent runs. It is
tracked with the preregistered Terminal-Bench pilot in
[#236](https://github.com/aphoristicartist/prog/issues/236) and runs only with
an approved budget.

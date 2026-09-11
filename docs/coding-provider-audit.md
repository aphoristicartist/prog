# Coding-provider completion audit

This audit addresses [#281](https://github.com/aphoristicartist/prog/issues/281).
The supported families remain pytest and Cargo/rustc. Normalization is a bounded,
deterministic interpretation of captured evidence; it cannot establish that an
arbitrary test runner, plugin, or output format obeys those conventions.

## Confirmed gaps and changes

A minimal CLI reproduction on main at `25dfe74` ran a fixture executable named
`pytest` with `-x`, printed `1 passed in 0.01s`, and exited zero. In a stable Git
workspace, attaching that observation to a user-required obligation made
`session show --readiness` return `passed`. The provider had explicitly marked
selection exhaustiveness false. Readiness now honors that limit. An explicit
scope/exhaustiveness declaration also cannot override contrary provider evidence.

Completion validation now checks the observed process exit against pytest text
summaries, JSON counters and detailed outcomes, and Cargo summaries. It refuses
exhaustiveness for empty, skipped, deselected, interrupted, malformed, mixed, or
conflicting evidence. Compiler errors cannot prove that later checks completed.
Normal complete failing test runs retain their existing exhaustive-selection
behavior when the selected harness scope is provable.

Pytest collection failures and failing stages hidden behind a passing test
outcome now contribute bounded diagnostic records. Their original report remains
available through the captured stream. JSON records carry `report_pointer`
within that parsed report: an array index is not a source text line. Text test
records retain their actual stream line numbers.

The prior `pytest-json-report-deselected` golden fixture reported total three
with only one test record. The documented report format defines total as tests
run. The fixture is retained as `pytest-json-report-deselected-inconsistent-total`
and now expects incomplete normalization. This strengthens its expected outcome;
it does not alter the contradictory input to make the parser pass.

These interpretations follow the upstream [pytest exit-code documentation](https://docs.pytest.org/en/stable/reference/exit-codes.html)
and [pytest-json-report format](https://github.com/numirias/pytest-json-report#format).
Additional JSON stream content remains available for positive findings, but one
embedded report cannot prove the rest of the capture successful.

## Executable coverage map

| Evidence condition | Existing coverage retained | Added regression |
|---|---|---|
| Ordinary text/JSON failures, Unicode, ordering and line shifts | Core `coding_provider` golden matrix and unit tests; CLI `pytest_provider_is_persisted_and_compatible_across_text_and_json` | JSON source pointers are checked against independently parsed report records. |
| Green output and bare identifiers | Core findings `green_test_output_never_claims_a_failure`; CLI `verification_accepts_a_complete_successful_command_as_evidence` | Readiness matrix includes valid pytest text, pytest JSON, and Cargo success controls. |
| Collection/global setup/teardown failure with passing assertions | Generic run failure sections and failed-test normalization | Readiness matrix checks collection and teardown conflicts, visible diagnostic findings, retained evidence and refusal to pass. |
| Exit code, counters, detail records disagree | Ordinary nonzero-exit capture | Core `observed_exit_and_late_failures_constrain_completion`; readiness matrix includes contradictory JSON/text and Cargo summaries. |
| Empty/skipped/deselected suite or early stop | Separate capture and selection axes; pytest early-stop and Cargo fail-fast tests | Readiness matrix includes empty and skipped runs, repeated summaries, and explicit exhaustive-flag override; delta matrix includes empty and skipped subject runs. |
| Unknown/malformed/mixed output | CLI `malformed_cargo_provider_keeps_raw_bytes_and_generic_findings`; core work/output bounds and property tests | Unknown Cargo record, incomplete compiler message, malformed summary, embedded JSON plus later failure, and JSON in one stream with failure in the other. |
| Complete-looking prefix followed by failure | CLI timeout and detached-pipe tests; POSIX lifecycle tests below | Core observed-exit matrix and CLI delta late-failure/process-mismatch controls. |
| Missing failure after narrowed/incomplete rerun | CLI `verification_treats_targeted_incomplete_reruns_as_not_observed`; replay negative controls; installed coding loop | `coding_delta_never_resolves_from_empty_conflicting_or_late_failure_evidence`, with a complete positive control that must resolve. |
| Stale/evicted/redacted evidence | CLI workspace/readback/redaction tests; installed coding loop's narrow, stale, incomplete and evicted controls | Existing tests remain required; no new cache-age or historical-receipt semantics. |

New CLI cases live in
[`coding_verification.rs`](../crates/prog-cli/tests/coding_verification.rs).
They run real subprocesses in isolated stable Git workspaces, reopen persisted
evidence with separate CLI invocations, export exact retained stream values,
and check readiness or delta. A source-side execution counter proves that
navigation and readiness do not rerun the producer. The expected success and
failure cases are authored directly, independently of provider output.

The core matrix lives in
[`coding_provider.rs`](../crates/prog-core/tests/coding_provider.rs) and the
checked-in provider fixtures. Existing parser bound/property tests remain part
of the workspace suite. Existing process lifecycle coverage includes
`run_timeout_and_missing_command_return_structured_envelopes`,
`run_timeout_does_not_wait_for_detached_pipe_holders`, and the immediate-parent
exit / inherited-pipe cases already mapped in [INVARIANTS.md](../INVARIANTS.md).

## Contract and verification implications

Capture completeness remains independent of selection exhaustiveness. An
early-stopped or empty run can be fully captured without proving absence.
Contradictory or unrecognized structured evidence also prevents a claim of
complete normalization. Positive findings and recoverable source evidence are
retained in both cases.

The pure normalizer now receives the observed process exit code explicitly;
absence of that code cannot establish completion. Normalized JSON test records
replace their incorrect synthetic line numbers with report pointers. The local
store identity changes to `prog.store.coding_completion_evidence`, resetting
earlier immutable observations rather than reusing their stronger proof claims.
The reset notice and prior-schema regression are covered by the store tests.

No model, external test framework dependency, transport, scheduling, or new
public command is introduced. Required obligations remain user-owned. The
required validation is the workspace format, strict Clippy, all-feature tests,
and the installed coding-loop/host checks described in the repository workflow.

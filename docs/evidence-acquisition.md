# Evidence acquisition evaluation

The suite keeps two experiments separate: component rank/serialization checks,
and deterministic CLI workflows for the checked-in Cargo compile, Cargo test,
pytest, noisy-log, and SARIF artifacts. The CLI experiment stops after its
public strategy selects a candidate and attempts retrieval. A wrong or missing
candidate is insufficient; the grader never supplies a retry path.

```bash
cargo test -p prog-cli --test evidence_acquisition
PROG_BLESS=1 cargo test -p prog-cli --test evidence_acquisition
scripts/regenerate-eval-docs.sh --check
```

Each CLI strategy starts with its own empty temporary store. Fixture JSON is
serialized compactly and passed outside model context to `observe --stdin
--mime application/json --name evidence-eval`. The entire capture stdout counts,
including every finding, metadata field, formatting byte, and trailing newline.
The five inputs are already structured JSON, so no lens is needed. Fixture
creation and upstream compilation/test/service execution are outside this
experiment; local artifact capture is included.

## Public strategy rules

All commands use the built `prog` binary, a real `--dir` store, the default
response budget, and the cursor emitted by that strategy's capture. No expected
path, expected value, fixture name, or grader result reaches strategy execution.
The public goal is supplied to `inspect`. These are fixed schema-aware navigation
rules, not a general agent or an optimal search policy.

- **Paths:** capture, then `paths "$CURSOR" --prefix "" --limit 8 --depth 2`.
  Select the first actually listed numeric child of a `failure_sections` or
  `results` collection. If needed, narrow a returned collection or run prefix
  with another bounded `paths` request, up to five listings in total. When an
  actual `lines` array is listed, use `search "$CURSOR" 'FATAL|ERROR|error'
  --regex --path "$LISTED_LINES" --limit 8`, preferring a returned fatal match,
  then the first match. Unseen siblings are not synthesized, and a limited
  search can miss the cause. Every listing and search counts.
- **Findings:** capture with its full findings list, then select the first
  emitted finding. If none is emitted, stop with insufficient evidence.
- **Inspect:** capture, then `inspect "$CURSOR" --goal "$PUBLIC_GOAL" --limit 5`;
  select its first returned finding, or stop if there is none. The paths and
  inspect comparisons deliberately ignore capture findings while paying for
  their complete transport cost.

Each selected path must appear literally in the permitted earlier response.
Retrieve it with `evidence "$CURSOR" --path "$SELECTED_PATH"`. If that response
reports omissions, make one further `expand "$CURSOR" --path "$SELECTED_PATH"
--depth 12 --limit 100` request. A still-partial response remains insufficient.
This expansion is needed by the SARIF fixture; it is not a free internal lookup.
Errors and unsuccessful attempts contribute their complete stdout and calls.
No strategy is required to be cheaper than another.

After execution, a private grader checks the selected path's response provenance,
the final cursor/scope, and equality with the required retained redacted source
slice. A correct reference alone does not credit an incomplete excerpt, including
a generic redaction placeholder that differs from the retained value. Separate
regressions cover relocated/unranked/missing causes, oracle mutation, redaction,
actual prefix narrowing, emitted metadata, formatting, extra calls, and errors.

## Measurement and ceiling policy

`evidence-cli-metrics.json` records each actual argv (with `$STORE` and `$CURSOR`
bindings), exit status, offered paths, stdout size, selection provenance, and
outcome. Workflow bytes equal the sum of complete stdout buffers; calls equal
the invocation ledger length. Token estimates use `bytes_div_4_approximate`,
rounding bytes/4 up per workflow before aggregation. The traces are audit data,
not extra strategy observations. Exact sizes can vary with runtime metadata;
these measurements are not provider token counts or live-agent outcomes.

The original component artifact and its reviewed ceilings keep their original
meaning. CLI ceilings live in a separate, explicitly initialized artifact, with
initial byte headroom of 25% rounded up to KiB and one extra invocation. Ordinary
blessing preserves every existing ceiling and rejects unreviewed inventory or
public-task changes. The one-time initializer refuses to replace an existing
artifact. A ceiling increase requires a separate explicit review; blessing
cannot silently authorize it. Documentation renders saved measurements, while
normal runtime checks allow variation within those ceilings.

<!-- eval:evidence-table:start -->
## Component regression measurements

5/5 scenarios retain the expected top-ranked path. The legacy output tokens are
approximate bytes/4 counts of core structures, with modeled workflow calls;
these are not complete CLI stdout or acquisition costs. Their original ceilings
remain unchanged. Source: [`evidence-acquisition-metrics.json`](../fixtures/evals/evidence-acquisition-metrics.json).

| Scenario | Top rank | Correct path | Modeled baseline calls | Modeled findings calls | Modeled baseline tokens | Modeled findings tokens | Modeled inspect tokens |
|---|---:|---|---:|---:|---:|---:|---:|
| cargo_compile_error | 1 | true | 3 | 2 | 537 | 527 | 976 |
| cargo_test_failure | 1 | true | 3 | 2 | 552 | 534 | 988 |
| noisy_log_root_error | 1 | true | 3 | 2 | 747 | 347 | 503 |
| pytest_assertion_failure | 1 | true | 3 | 2 | 581 | 558 | 1004 |
| sarif_security_error | 1 | true | 3 | 2 | 554 | 460 | 510 |

## Actual CLI workflow measurements

Each workflow executes the built binary against its own fresh temporary store.
Response bytes sum complete stdout, including capture metadata, all emitted findings,
bounded navigation, errors, and any expansion. Tokens use
`bytes_div_4_approximate`: bytes/4 rounded up per workflow before aggregation.
Correctness requires an observed path and the complete required redacted slice.
These are deterministic CLI regressions, not agent reasoning, provider billing,
live-service latency, or real-world success measurements.

Source and normalized command traces: [`evidence-cli-metrics.json`](../fixtures/evals/evidence-cli-metrics.json).

| Scenario | Strategy | Complete evidence | Calls | Stdout bytes | Approx. tokens |
|---|---|---|---:|---:|---:|
| cargo_compile_error | paths | true | 3 | 11548 | 2887 |
| cargo_compile_error | findings | true | 2 | 7566 | 1892 |
| cargo_compile_error | inspect | true | 3 | 13853 | 3464 |
| cargo_test_failure | paths | true | 3 | 11536 | 2884 |
| cargo_test_failure | findings | true | 2 | 7554 | 1889 |
| cargo_test_failure | inspect | true | 3 | 13893 | 3474 |
| noisy_log_root_error | paths | true | 4 | 13358 | 3340 |
| noisy_log_root_error | findings | true | 2 | 6665 | 1667 |
| noisy_log_root_error | inspect | true | 3 | 9507 | 2377 |
| pytest_assertion_failure | paths | true | 3 | 11599 | 2900 |
| pytest_assertion_failure | findings | true | 2 | 7617 | 1905 |
| pytest_assertion_failure | inspect | true | 3 | 13913 | 3479 |
| sarif_security_error | paths | true | 4 | 15567 | 3892 |
| sarif_security_error | findings | true | 3 | 10359 | 2590 |
| sarif_security_error | inspect | true | 4 | 11946 | 2987 |

| Strategy total | Complete evidence / attempts | Calls | Stdout bytes | Approx. tokens |
|---|---:|---:|---:|---:|
| paths | 5/5 | 17 | 63608 | 15903 |
| findings | 5/5 | 11 | 39761 | 9943 |
| inspect | 5/5 | 16 | 63112 | 15781 |
<!-- eval:evidence-table:end -->

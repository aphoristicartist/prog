# Evidence acquisition evaluation

The evidence-acquisition suite measures the cost until the first correct causal
path across checked-in Cargo compile, Cargo test, pytest, noisy-log, and SARIF
scenarios.

```bash
cargo test -p prog-cli --test evidence_acquisition
scripts/regenerate-eval-fixtures.sh
```

The checked baseline records tool calls, output-token estimates, top finding
rank, and path correctness for:

- `envelope -> paths -> expand`
- `envelope with findings -> evidence`
- `envelope -> inspect --goal -> evidence`

CI asserts each scenario still finds the required path at rank 1 and stays
within its explicit tool-call and output-token ceilings. The fixture retains
exact measurements for reports, but normal metric drift within those ceilings
does not fail CI. To refresh those recorded values, run:

```bash
PROG_BLESS=1 cargo test -p prog-cli --test evidence_acquisition
```

The command never raises a ceiling. An intentional cost increase requires a
separate, reviewable edit to the named ceiling before blessing.

<!-- eval:evidence-table:start -->
## Recorded measurements

5/5 scenarios retain the expected top-ranked path. Output tokens are
approximate bytes/4 counts of core structures, with modeled workflow calls;
these are not complete CLI stdout or acquisition costs. Source: [`evidence-acquisition-metrics.json`](../fixtures/evals/evidence-acquisition-metrics.json).

| Scenario | Top rank | Correct path | Baseline calls | Findings calls | Baseline output tokens | Findings output tokens | Inspect output tokens |
|---|---:|---|---:|---:|---:|---:|---:|
| cargo_compile_error | 1 | true | 3 | 2 | 537 | 527 | 976 |
| cargo_test_failure | 1 | true | 3 | 2 | 552 | 534 | 988 |
| noisy_log_root_error | 1 | true | 3 | 2 | 747 | 347 | 503 |
| pytest_assertion_failure | 1 | true | 3 | 2 | 581 | 558 | 1004 |
| sarif_security_error | 1 | true | 3 | 2 | 554 | 460 | 510 |
<!-- eval:evidence-table:end -->

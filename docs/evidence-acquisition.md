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

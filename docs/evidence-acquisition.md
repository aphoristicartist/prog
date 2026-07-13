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

Baseline updates are explicit. CI fails when ranking or metric output changes
without updating `fixtures/evals/evidence-acquisition-metrics.json`.

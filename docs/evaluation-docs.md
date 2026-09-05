# Evaluation documentation

README evaluation headlines and their detailed reports are rendered from the
same saved JSON rows by `crates/prog-cli/tests/support/eval_reports.rs`. These
are reviewed measurement artifacts, not fresh measurements performed by the
documentation check. Generation preserves each experiment's qualifications;
it cannot turn a deterministic evidence check into actual-agent task success
or a byte estimate into provider token usage.

Use the repository's script:

```sh
scripts/regenerate-eval-docs.sh --write
scripts/regenerate-eval-docs.sh --check
```

With no argument the script checks. `--check` clears the documentation update
flag, reads saved artifacts, and fails with the affected filenames if a claim
or report has drifted. It makes no repository edits, runs no measurement suite,
and needs no credentials or live services. Cargo may compile the local test
runner and populate its build cache. Normal workspace tests run the same
consistency check. `--write` updates only changed generated documentation.

## Sources and generated content

| Saved artifact under `fixtures/evals/` | Generated documentation |
|---|---|
| `token-economics-metrics.json` | README HTTP “Discover shape” hero and token ratio range; `docs/token-economics.md`. |
| `evidence-acquisition-metrics.json` | README component causal-path count; legacy component table in `docs/evidence-acquisition.md`. |
| `evidence-cli-metrics.json` | README CLI outcomes/call/token totals; actual CLI tables in `docs/evidence-acquisition.md`. |
| `real-world-demo-metrics.json` | README demo count and ratio range; table in `docs/real-world-demos.md`. |
| `competitive-baseline-metrics.json` | README unknown-target comparison; `docs/competitive-baselines.md`. |
| `task-success-metrics.json` | `docs/task-success-eval.md`, explicitly known-path recoverability. |

Token estimates are bytes divided by four, rounded up, using the task's
recorded total response bytes. Ranges use those unrounded ratios before
formatting: one decimal for token economics and two for demos. Competitive
and recoverability aggregate estimates sum per-task estimates. Legacy evidence
component rows retain their modeled token estimates. Actual
evidence CLI totals derive estimates from complete stdout bytes per workflow;
aggregate claims include insufficient attempts and do not use component costs.
A correct top-ranked
path requires the recorded correctness flag, rank, and expected path to agree.
README prose formats large totals with thousands separators; detailed tables
retain plain numeric cells. Counts include insufficient attempts and distinguish
unavailable strategies according to the source experiment.

The renderer owns the full token-economics, competitive-baselines, and
recoverability reports. Explicit HTML comment boundaries delimit the README
hero/ranges and evidence/demo tables; the competitive README block is delimited
by its section headings. Text outside those regions, including demo commands,
is preserved. Missing or duplicate comment boundaries fail before any document
is written. Edit generated wording in the shared renderer, then use `--write`.

## Refreshing measurements

Regenerating documents does not refresh a measurement or approve a new ceiling.
After an intentional implementation change, run the owning measurement command:

```sh
PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture
PROG_BLESS=1 cargo test -p prog-cli --test evidence_acquisition
PROG_REAL_WORLD_DEMO_UPDATE=1 cargo test -p prog-cli --test real_world_demos -- --nocapture
PROG_BASELINE_EVAL_UPDATE=1 cargo test -p prog-cli --test competitive_baselines -- --nocapture
PROG_TASK_EVAL_UPDATE=1 cargo test -p prog-cli --test task_success -- --nocapture
```

Each command validates its runtime invariants, saves that family's measurements,
and invokes the shared documentation renderer. Run updates sequentially because
they write common documents, then review both artifacts and generated claims.
The existing `scripts/regenerate-eval-fixtures.sh` also refreshes evidence,
replay, and agent-eval fixtures under their existing blessing rules.

Normal runtime tests retain their minimum ratios, correctness requirements, and
reviewed cost ceilings. Benign runtime-byte drift within that headroom does not
require updating saved measurements. Blessing does not raise a ceiling or waive
a correctness failure. The separate documentation check only requires the
published claims to agree with the already reviewed artifacts.

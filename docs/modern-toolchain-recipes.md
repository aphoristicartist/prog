# Modern toolchain recipes

These recipes make current test runners and static analyzers feed their
existing interchange formats into `prog`. They do not add native parsers.

## JUnit test reports

| Recipe | Default command | Added configuration |
|---|---|---|
| `vitest` | `vitest run` | `--reporter=junit --outputFile=<temp>` |
| `playwright` | `playwright test` | `PLAYWRIGHT_JUNIT_OUTPUT_FILE=<temp>` and `--reporter=junit` |
| `bun-test` | `bun test` | `--reporter=junit --reporter-outfile=<temp>` |
| `deno-test` | `deno test` | `--junit-path=<temp>` |

The Playwright environment assignment is executed through `env` as argv; it is
not a shell string. The flags come from the current first-party references for
[Vitest](https://vitest.dev/guide/reporters),
[Playwright](https://playwright.dev/docs/test-reporters),
[Bun](https://bun.sh/docs/test/reporters), and
[Deno](https://docs.deno.com/runtime/reference/cli/test/).

```bash
prog recipe vitest -- vitest run src/cart.test.ts
prog recipe playwright -- npx playwright test checkout.spec.ts
prog recipe bun-test -- bun test cart.test.ts
prog recipe deno-test -- deno test cart_test.ts
```

Jest does not ship a first-party JUnit reporter. Projects that already depend
on a third-party Jest JUnit reporter can save its report and use `prog observe
--mime application/junit+xml --lens junit`; `prog` does not install or hide
that dependency.

## SARIF static analysis

| Recipe | Default command | Added configuration |
|---|---|---|
| `ruff` | `ruff check` | `--output-format=sarif --output-file=<temp>` |
| `biome` | `biome check` | `--reporter=sarif --reporter-file=<temp>` |
| `semgrep` | `semgrep scan` | `--sarif-output=<temp>` |

[Biome's SARIF reporter and `--reporter-file`](https://biomejs.dev/blog/biome-v2-4/)
require Biome 2.4 or newer. Older Biome versions fail normally and their
captured process evidence is returned; the recipe does not pretend a report
exists. Ruff's flags follow its current
[integration reference](https://docs.astral.sh/ruff/integrations/), while
Semgrep uses its dedicated `--sarif-output` file option.

```bash
prog recipe ruff -- ruff check .
prog recipe biome -- biome check .
prog recipe semgrep -- semgrep scan --config p/default .
```

Clippy remains under `cargo-test` and the existing Cargo/rustc provider. There
is no first-party JSON-to-SARIF converter. `npm audit --json` and `cargo audit
--json` remain generic JSON observations until replay or agent traces justify
tool-specific triage beyond the existing Trivy lens.

## Execution and storage contract

Each recipe:

1. creates a private temporary directory;
2. runs the exact user argv with only the documented reporter destination;
3. captures the command's stdout, stderr, exit code, timeout, and signal as a
   normal `run` observation;
4. observes a non-empty generated report exactly once through `junit` or
   `sarif`;
5. ranks the report findings and removes the temporary directory.

If the command produces no report, the recipe returns its bounded process
evidence with an explicit warning. A non-zero command that did produce a valid
report returns the report findings and retains the non-zero status under
`recipe.command_result`; it is never relabeled as command success.

The checked-in `fixtures/cli/modern_reporter.py` control emits more than 50 KiB
of raw console noise for every failing tool. The executable recipe matrix caps
each final model-visible envelope at 16 KiB while retaining the JUnit failure or
SARIF error and a cursor to the stored report. The store contains one command
observation and one report observation per recipe; the report is not imported
twice.

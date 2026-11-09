# Coding-output providers

`prog run` applies a small, internal normalization layer to recognized pytest,
`python -m unittest`, Cargo/rustc, `go test`, and Jest/Vitest invocations. The provider is pure over captured argv, stdout,
and stderr: it never executes another command, and the original redacted
capture remains stored and cursor-addressable if matching or parsing fails.

The normalized value is stored at `/provider`. Its observation record exposes
the provider and parser identities plus the exact selection and completeness
assessment:

```bash
prog run -- pytest -q
prog expand pc1_... --path /provider
prog cache observations --limit 1
```

## Supported inputs

| Provider | Recognized invocations | Preferred input | Text fallback |
|---|---|---|---|
| `pytest.v1` | `pytest`, `py.test`, `python -m pytest` | A complete `pytest-json-report` object captured on stdout or stderr | Node IDs, statuses, terminal summary, targets, and early-stop markers |
| `cargo_rustc.v1` | `rustc`; Cargo `bench`, `build`, `check`, `clippy`, `rustc`, and `test` | Cargo/rustc JSON diagnostics | rustc diagnostics and stable libtest lines |
| `unittest.v1` | `python`, `python3`, or `python3.N` `-m unittest` (with or without `discover`) | — | `-v` per-test lines (Python 3.9 to 3.15 id formats), `FAIL:`/`ERROR:`/`UNEXPECTED SUCCESS:` blocks, and the `Ran N tests` summary |
| `go_test.v1` | `go test` | `-json` events (test2json) | `-v` lines (`=== RUN`, `--- PASS/FAIL/SKIP:`) and per-package `ok`/`FAIL`/`?` lines |
| `js_test.v1` | `jest`, `vitest`, `npx`/`bunx`/`pnpx jest\|vitest`, `pnpm\|yarn [exec] jest\|vitest`, and `npm\|pnpm\|yarn\|bun [run] test` when the output is recognized | The JSON report on stdout: `jest --json`, `vitest run --reporter=json --outputFile=/dev/stdout` | The `Tests:` summary and the failing tests it names |

## Which runs can prove a fix

A fix is proven only from output that names every test that ran, so a
missing test means it did not run:

| Runner | Provable | Never provable |
|---|---|---|
| Cargo | `cargo test` (verbose by default) | `cargo test -q` |
| pytest | `pytest -v`, a `pytest-json-report` | `pytest -q`, default `pytest`, `-k`, `--lf`, `--ff`, `--sw`, a failing `-x` run |
| unittest | `python -m unittest -v` | default verbosity, `-q`, `-k`, `-c` |
| Go | `go test -v`, `go test -json` | `go test` without `-v` or `-json`, `-run`, `-skip`, `-list` |
| Jest | `jest --json` (also through `npx` or `npm test -- --json`) | any text output, `-t`, `--onlyChanged`, `--shard` |
| Vitest | `vitest run --reporter=json --outputFile=/dev/stdout` | any text output, `--reporter=json` without `--outputFile=/dev/stdout`, `-t`, `--changed` |

Jest 30 and Vitest 5 switch to a failures-only reporter when they detect an AI
agent (`CLAUDECODE`, `CODEX_THREAD_ID`, `AI_AGENT`, ...). Their text output then
never names passing tests, even with `--verbose`, so only the JSON report can
prove a fix. Vitest 5 writes `--reporter=json` to `.vitest/json/output.json`
unless `--outputFile=/dev/stdout` is given; prog never reads report files.

Runs that stop at the first failure (`cargo test` without `--no-fail-fast`,
`pytest -x`, `python -m unittest -f`, `go test -failfast`, `jest --bail`) can be the failing
run of a comparison, never the passing one unless it ran to completion. Each
provider also refuses: a summary that disagrees with the process exit code, a
cut-off stream (no `Ran N tests`, no package result, a truncated report), a
module, package, or test file that failed to import, build, or load
(`unittest.loader._FailedTest`, `[build failed]`, Jest's "Test suite failed
to run", which RTK's filters have reported as passing), and a test that
panicked or timed out before later tests ran.

`prog recipe cargo-test` adds `--message-format=json` to a Cargo test argv when
the option is absent. The expanded argv is recorded in the recipe envelope.
User-supplied argv is never converted into a shell string.

Structured and text encodings retain distinct `input_format` values inside
`/provider`, but share one stable observation parser identity per provider.
Equivalent evidence therefore remains comparable across a format transition.

## Completeness and selection

Two independent axes are recorded per observation, and only the combination
permits absence claims:

- `provider.complete` — whether the captured input was fully normalized
  within provider bounds. It is `false` when the capture itself was
  truncated, a provider bound (items, lines, bytes, spans, string length)
  was hit, or Cargo/rustc structured output was malformed. Incomplete
  provider output changes the observation's capture stop reason to
  `derivation_windowed`; it cannot prove a finding resolved.
- `selection.exhaustive` — whether the executed command plausibly covered
  the whole selected test universe. A pytest run that stopped early, a JSON
  report without `exitcode` or with deselected tests, or a failing Cargo
  test invocation without one exact package+harness target or
  `--no-fail-fast` keep normalization complete but mark selection
  non-exhaustive. A non-exhaustive subject cannot authorize `resolved`; the
  warning names the unproven side. A non-exhaustive baseline can, against an
  exhaustive rerun of the same command, because it still reports every
  failure it saw (see [delta.md](delta.md)). Runner-reported skips mark the
  selection `excludes_skipped_tests` when they are its only gap.

Targets become explicit selection scopes. A targeted pytest run is exhaustive
only for that target, never for a broader suite. Cargo target flags likewise
become namespaced scopes. Delta comparison still requires the normal
invocation, scope, provider/parser, capture, workspace, and source-state proof.

## Bounds and fallback

Provider work is capped at 1 MiB of input, 10,000 lines, 512 normalized items,
1 MiB of normalized output, 32 spans per structured diagnostic, and 2,048
characters per retained string. Hitting any bound makes the result incomplete.

Malformed and unsupported output remains available through `/stdout/text`,
`/stderr/text`, and `/failure_sections`. Generic deterministic findings remain
the fallback; provider failure never removes captured evidence or causes a
second execution.

The unittest, Go, and Jest/Vitest providers keep their real-tool fixtures
(Python 3.9 and 3.15, Go 1.27, Jest 30, Vitest 5) and refusal cases next to
the code in `crates/prog-core/src/coding_provider/`. The checked-in golden
matrix for pytest and Cargo lives under
`crates/prog-core/tests/fixtures/providers/` and includes success, failure,
negative, malformed, truncated, reordered, Unicode, line-shift,
tool-output-variant, and no-match cases. Property tests pin deterministic
bounded behavior.

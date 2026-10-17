# prog

[![CI](https://github.com/aphoristicartist/prog/actions/workflows/ci.yml/badge.svg)](https://github.com/aphoristicartist/prog/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.89+](https://img.shields.io/badge/rust-1.89%2B-orange.svg)](https://www.rust-lang.org)
[![Platforms](https://img.shields.io/badge/platforms-linux%20%7C%20macOS-lightgrey.svg)](#install)

**Agents run tools that produce more output than they can read. prog keeps the
output, gives the agent a map with an address for every part, and never calls
a fix proven until it is.**

prog is a command-line tool that coding agents call: Claude Code, Codex,
Cursor, Gemini CLI, or anything else that can run a command. It sits between
the agent and what the agent runs or reads (test suites, compilers, CLIs,
HTTP APIs, MCP servers, files, logs) and does three things:

- **Lets the agent read 300 records without reading 300 records.** Output is
  stored once, with secrets removed. The agent gets a small view, exact counts
  of what is inside (`state: open=5, closed=2`), and an address for every part,
  then asks for exactly the rows, fields, or lines it needs. Nothing is rerun.
- **Shows what changed since last time.** Run the same command again and prog
  lists the records added, removed, and changed, matched by key, with before
  and after values.
- **Says "fixed" only when it is proven.** A test fix counts when the same tests
  ran again and the failing one passed by name. Deleting, skipping, or swapping
  out the failing test does not pass, and neither does quiet output that cannot
  show it.

prog does not summarize or compress with a model, and it is not an agent or an
MCP server. Every value it returns is copied from the captured output (with
secrets replaced and long strings visibly cut) or counted from it.

> **Status: pre-1.0.** The latest release, v0.1.1 (August 2026), predates
> `select`, `coverage`, compact output, receipts, PATH shims, and the Claude
> Code plugin. Until the next release, [install from `main`](#install) to use
> everything below.

## Quickstart

Uses `prog` built from `main` and `python3` (only to read the cursor out of
the JSON). CI runs this block exactly as written.

```sh
cd "$(mktemp -d)"
cat > issues.json <<'EOF'
[
  {"id": 101, "state": "open",   "title": "Login times out", "labels": ["bug"]},
  {"id": 102, "state": "closed", "title": "Typo in docs",    "labels": ["docs"]},
  {"id": 103, "state": "open",   "title": "Crash on save",   "labels": ["bug", "p1"]},
  {"id": 104, "state": "open",   "title": "Slow search",     "labels": ["perf"]},
  {"id": 105, "state": "closed", "title": "Bad redirect",    "labels": ["bug"]},
  {"id": 106, "state": "open",   "title": "Dark mode",       "labels": []},
  {"id": 107, "state": "open",   "title": "Export to CSV",   "labels": ["feature"]}
]
EOF

prog --format compact observe --file issues.json
CURSOR=$(prog observe --file issues.json | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
prog --format compact select "$CURSOR" --where state=open --where 'labels~bug' --fields id,title
prog --format compact evidence "$CURSOR" --path /2/title

sed -i.bak 's/"id": 103, "state": "open"/"id": 103, "state": "closed"/' issues.json
prog --format compact observe --file issues.json
```

What comes back, abridged:

```text
observe   overview: 7 items; /id 101..107; /state open=5, closed=2; keys /id, /title
select    matched 2 of 7: /0 {"id": 101, "title": "Login times out"}, /2 {"id": 103, "title": "Crash on save"}
evidence  "Crash on save"   ref prog://pc1_…#/2/title
observe   changes_since.records: 1 changed, 6 unchanged; key 103: /state "open" -> "closed"
```

The same moves work on a test run (`prog run -- cargo test`), an API
(`prog run -- gh api repos/OWNER/REPO/issues`), an MCP tool, or a 16 MiB log.

## "Fixed" means proven

With PATH shims active (the Claude Code plugin installs them, or run `prog shim
install`), the agent runs `cargo test`, `pytest`, `go test`, `npm test`, and
other test, build, and lint tools as usual, even piped into `tail`. Every byte
reaches the agent unchanged; prog keeps a full copy and adds a receipt of a
few hundred bytes after each run:

```text
prog: `cargo test --no-fail-fast` · exit 0 · 1 passed, 0 failed · full output kept (15 lines)
vs previous run: NOT PROVEN (the subject executed 1 tests but the baseline executed 2; the test population shrank)
evidence: prog evidence pc1_… --path /stdout
```

That is what deleting the failing test looks like: green output, unproven fix.
A real fix reads `vs previous run: fixed: tests::adds`. prog reports a fix
only when all of these hold:

- the same command ran the same tests again, and at least as many executed;
- every test that failed before ran again and passed **by name**. Quiet output
  (`pytest -q`, `cargo test -q`) names only failures, so prog asks for a
  verbose rerun instead of guessing;
- neither capture was cut short, and each run's own summary agrees with its
  exit code.

A seeded suite ([`false_completion.rs`](crates/prog-cli/tests/false_completion.rs))
checks that deleted, skipped, ignored, swapped, and quiet-mode tests never come
out as fixed, and that real fixes still do.

The optional **stop gate** (Claude Code plugin setting `stop_gate`) blocks the
end of a turn once when the latest test run is failing or unproven, when files
changed after the last passing run, or when files changed and no test ran. It
never blocks twice in a row, so the agent can always explain and stop.

For scripted loops, `prog delta <before> <after>` classifies every finding as
`new`, `persisting`, `resolved`, `not_observed`, or `unknown`, and reports
`resolved` only when absence is provable. Obligations declared before the
result is known turn that into a gate that a recipe or harness cannot satisfy
on its own:

```sh
prog session obligation-add checkout-fixed --check "checkout error gone" --scope checkout \
  --origin-observation "$BASELINE" --expected-absent-fingerprint "$FINGERPRINT" \
  --evidence-observation "$VERIFICATION_RUN"
prog session show --readiness
```

Details: [shims and receipts](docs/shims-and-receipts.md),
[delta](docs/delta.md), [verification](docs/verification.md).

## Moving through large output

`prog run` parses output that is one JSON document or JSON Lines (`gh api`,
`kubectl get -o json`, `aws`, `curl`) into records; test, build, and lint
output stays verification evidence. `prog observe` reads files and stdin
(JSON, NDJSON, SARIF, JUnit, diffs, CSV and other tables, logs, text).
`prog call` reads registered HTTP, CLI, and MCP sources.

| Move | Command | Returns |
| --- | --- | --- |
| Map | the first response | `overview`: each field's kinds, how many records carry it, value counts when values repeat, number and date ranges, and fields unique enough to address a record |
| Filter and page | `prog select <cursor> --where state=open --where 'labels~bug' --fields number,title --offset 20` | matching rows, each with its JSON Pointer, plus `matched` and `next_offset` |
| Open one part | `prog evidence <cursor> --path /stdout/json/31/body` | the exact slice and a citable `prog://<cursor>#<path>` ref |
| Find | `prog search <cursor> "connection reset"`, `prog find <cursor> --kind error`, `prog inspect <cursor> --goal "root cause"` | ranked paths, with previews centered on the match |
| Check coverage | `prog coverage <cursor>` | which records prog has returned this session, and the ranges it never returned |

Only the capture touches the source; every other move reads the local store.
`--format compact` (or `PROG_FORMAT=compact`) returns the data, the next moves,
and every caveat (truncation, redaction, staleness, incomplete capture) and
leaves out budgets, verdicts, provenance, and templates. The full JSON
contract, published by `prog meta`, stays the default.

Limits are explicit. `run` keeps up to 1 MiB per stream by default
(`--max-stdout-bytes`), `observe` up to 16 MiB, and cursors expire after 24
hours by default. Each response is capped at 16 KiB by default
(`--budget-bytes`, up to 64 KiB). Anything cut is reported as cut, and a cut
capture can never prove that something is absent.

## What changed since last time

Capturing the same command, file, or call again attaches `changes_since` to
the new response: the findings that appeared or disappeared, and the records
added, removed, and changed. Records are matched by a field that is unique in
every record (`id`, `uid`, `key`, `number`, `name`, ...) or by `--key`.
`prog delta <before> <after>` returns the full lists.

The comparison refuses to guess. If either capture was cut short, or the key
is missing, duplicated, or redacted, it reports `incomplete` or `unkeyed` and
lists nothing. "Removed" means absent from the captured collection: an API
that pages or filters its results can drop a record that still exists, and
the response says so. See [record deltas](docs/delta.md#record-deltas).

## Install

### From `main` (everything in this README)

```sh
cargo install --locked --git https://github.com/aphoristicartist/prog prog-cli
```

Requires Rust 1.89 or newer. Until the next release, a build from `main` still
reports version 0.1.1. Linux (x86_64) and macOS (Apple Silicon and Intel) are
supported and tested in CI; Windows is not
([#140](https://github.com/aphoristicartist/prog/issues/140)).

### From the latest release (v0.1.1)

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aphoristicartist/prog/releases/latest/download/install.sh | sh
```

The installer checks `SHA256SUMS` and the GitHub build-provenance attestation
(it needs [`gh`](https://cli.github.com/)) before it places `prog` in
`~/.local/bin`. Later, `prog update --yes` replaces a curl-installed binary
with the newest verified release. v0.1.1 has capture, navigation, findings,
delta, and the Agent Skill, but none of the newer features listed above. Exact
versions, manual verification, and the release container image are in
[`docs/install.md`](docs/install.md).

### Connect your agent

- **Claude Code** (needs `prog` from `main`):

  ```text
  /plugin marketplace add aphoristicartist/prog
  /plugin install prog@prog
  ```

  At session start the plugin installs the PATH shims, adds a receipt after
  each captured test run, switches prog to compact output, and offers the
  opt-in stop gate. It never changes what a command prints.
- **Codex:** from a checkout, `codex plugin marketplace add /path/to/prog`, then
  `codex plugin add prog@personal`.
- **Any agent that reads skills or project instructions:**
  `prog harness install --root <project>` adds the Agent Skill, plus adapters
  for Codex, Claude Code, Cursor, and Gemini CLI when it detects them. The
  integration is ready only when `prog harness doctor --root <project>` returns
  `"ready": true`.
- **DeepSeek Harness:** `dsh plugin --profile web add ./extensions/deepseek-harness`
  replaces a tool result only when the prog envelope is smaller or redaction
  requires it.

Adapter paths and manual setup: [`docs/integrations.md`](docs/integrations.md).

## Built for loop engineering

prog is the observation and evidence layer inside an agent loop: observe,
orient, focus, act, verify, remember, stop. It does not choose actions,
schedule iterations, or approve changes.

### Example: fail, inspect, fix, verify

```sh
rm -rf /tmp/prog-loop
prog --dir /tmp/prog-loop session start --goal "compile the sample program"
printf '%s\n' 'fn main() { let value: u32 = "not a number"; println!("{value}"); }' > /tmp/prog-loop-demo.rs

RESULT=$(prog --dir /tmp/prog-loop run -- rustc /tmp/prog-loop-demo.rs -o /tmp/prog-loop-demo)
CURSOR=$(printf '%s' "$RESULT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
TOP_PATH=$(printf '%s' "$RESULT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["findings"][0]["path"])')
prog --dir /tmp/prog-loop inspect "$CURSOR" --goal "find the compile error" --limit 5
prog --dir /tmp/prog-loop evidence "$CURSOR" --path "$TOP_PATH"

printf '%s\n' 'fn main() { let value: u32 = 42; println!("{value}"); }' > /tmp/prog-loop-demo.rs
prog --dir /tmp/prog-loop run -- rustc /tmp/prog-loop-demo.rs -o /tmp/prog-loop-demo
prog --dir /tmp/prog-loop session note "compiled the corrected program"
prog --dir /tmp/prog-loop session show
```

The fix is a new observation; the failed one is never modified. The session
trail keeps navigation metadata and notes, not payload copies.

### Recipes and lenses

Recipes run a command or read a file, apply a bundled lens, and rank findings,
while recording exactly what ran:

```sh
prog recipe --timeout-ms 180000 cargo-test -- cargo test
prog recipe pytest -- pytest -v
prog recipe logs-root-cause --file /tmp/service.log
```

Recipes: `cargo-test`, `pytest`, `npm-test`, `go-test`, `vitest`,
`playwright`, `bun-test`, `deno-test`, `ruff`, `biome`, `semgrep`, `gh-issues`,
`diff-review`, `logs-root-cause`.

Bundled lenses cover Cargo, pytest, npm, and Go test output, JUnit, SARIF,
unified diffs, logs, NDJSON, OpenTelemetry NDJSON, GitHub issues and Actions
logs, kubectl JSON, Terraform plans, Trivy reports, MCP JSON-RPC errors, and
LLM API errors. Lenses are data: they select fields, declare omissions, and
add bounded finding rules, and they cannot run code ([`docs/lenses.md`](docs/lenses.md)).

### Registered sources

Register an HTTP API, CLI, or MCP server once and call it across iterations:

```sh
prog source add-cli repository --operation status --read-only -- git status --short
prog hints repository status
prog call repository status --args '{}'
```

HTTP `GET` operations are read-only and cacheable. Operations that are not
read-only need `--yes`, and shell- or network-backed sources also need their
trust flag (`trust.allow_shell`, `trust.allow_network`) even with `--yes`. For
read-only paginated operations, `prog call --pages N` follows
continuation hints under page, byte, time, and response caps. MCP tools,
resources, and long-running tasks ([`prog mcp-task`](docs/mcp-tasks.md)) are
read through the same adapter. Setup, OpenAPI and JSON Schema import, and a
tour using the bundled fixture: [`docs/source-setup.md`](docs/source-setup.md).

## Results

Measured on deterministic fixtures in this repository. Every number below is
generated from saved runs (`scripts/regenerate-eval-docs.sh`), not written by
hand. Tokens are estimated as bytes ÷ 4. None of these are live agent trials.

The fair comparison is with what agents already do: save the output and grep
it, or select the field directly. On finding an unknown cause, prog with
`--format compact` finds as much as grep on a saved file at a similar cost, and
less than reading everything raw. When the field is already known, a native
selector or `jq` is cheaper (see the known-path table in
[`docs/competitive-baselines.md`](docs/competitive-baselines.md)). prog's
advantage is not token count; it is stable addresses, keyed diffs, and
verification that refuses unproven claims.

### Deterministic retrieval correctness

Known-path cases measure recovery at an explicitly supplied selector. The
unknown-target cases keep the grader's path and answer private: strategies
select evidence from their actual observations. The set includes a fatal
record, its relocated counterpart, an unranked cause, and a no-answer control.

| Unknown-target strategy | Evidence available / attempted | Unavailable | Approx. response tokens |
| --- | ---: | ---: | ---: |
| `raw_context` | 3/4 | 0 | 142,205 |
| `head_tail_truncation` | 0/4 | 0 | 4,096 |
| `native_field_selection` | 0/0 | 4 | 0 |
| `rtk_grep_filter` | 0/4 | 0 | 3,009 |
| `broad_log_search` | 2/4 | 0 | 3,085 |
| `file_capture_search` | 2/4 | 0 | 3,123 |
| `prog_retrieve` | 2/4 | 0 | 12,636 |
| `prog_compact_retrieve` | 2/4 | 0 | 3,900 |

These are deterministic evidence-availability results, not actual-agent task
success. Raw context counts evidence present in the delivered artifact; a
strategy with insufficient evidence receives no discovery credit. Costs use
the bytes/4 approximation and include every capture, exploration, and lookup
response. Fixture setup and live source-acquisition costs are outside this
experiment. Broader search and a capture-once file baseline are included.

Known-path results, assumptions, and command traces are recorded in
[`docs/competitive-baselines.md`](docs/competitive-baselines.md) and the
[measurement rows](fixtures/evals/competitive-baseline-metrics.json).

### Correctness, not just savings

[`docs/replay-eval.md`](docs/replay-eval.md) replays multi-iteration
trajectories behind an oracle that must never see a false `resolved`,
false-fresh, or false-`passed` result, and makes no savings claim: its
payloads are small enough that envelopes cost more than raw output. The
false-completion suite above covers the same rule for test runs.

### Compared with pasting raw output

<!-- eval:hero:start -->
```text
                       approximate tokens into the model
  raw fixture        137,883
  prog task            1,736
```

<sub>HTTP “Discover shape” from [`docs/token-economics.md`](docs/token-economics.md),
rendered from the [checked-in rows](fixtures/evals/token-economics-metrics.json).
Ratios across these deterministic fixtures range 23.9x-79.4x.
Counts use the bytes/4 approximation, not provider tokens or a promise about your workload.</sub>
<!-- eval:hero:end -->

<!-- eval:tokens:start -->
Across the checked-in HTTP, CLI, and MCP tasks, raw-fixture token estimates
divided by complete `prog` task estimates range from **23.9x-79.4x**. Every
task includes its initial envelope and any expansions. Estimates use bytes/4,
rounded up; these are fixture measurements, not provider token counts. See
[`docs/token-economics.md`](docs/token-economics.md) and the
[measurement rows](fixtures/evals/token-economics-metrics.json).
<!-- eval:tokens:end -->

<!-- eval:demos:start -->
The 5 checked-in workflow demos report raw-to-envelope-plus-expansion ratios
from **8.82x to 13.30x**, using the bytes/4 token approximation. These are
generated local payloads, not credentialed live service measurements. See
[`docs/real-world-demos.md`](docs/real-world-demos.md) and the
[recorded metrics](fixtures/evals/real-world-demo-metrics.json).
<!-- eval:demos:end -->

### Ranking the cause

<!-- eval:evidence:start -->
The 5 checked-in evidence-acquisition scenarios rank the expected causal
path first in **5/5** component checks. Separate actual CLI workflows
measure complete stdout from capture through navigation and evidence retrieval.

| CLI strategy | Complete evidence / attempts | Tool calls | Approx. output tokens |
|---|---:|---:|---:|
| paths | 5/5 | 17 | 16,696 |
| findings | 5/5 | 11 | 10,362 |
| inspect | 5/5 | 16 | 16,492 |

Costs include every initial finding and metadata field, actual bounded path
listings/searches, and expansions needed after truncated evidence. Tokens use
bytes/4 rounded up per workflow; these are deterministic CLI regressions, not
provider token usage or actual-agent success rates. See
[`docs/evidence-acquisition.md`](docs/evidence-acquisition.md),
[component checks](fixtures/evals/evidence-acquisition-metrics.json), and
[CLI measurements and command traces](fixtures/evals/evidence-cli-metrics.json).
<!-- eval:evidence:end -->

## When not to use prog

- The output is already small and clear: an envelope adds bytes.
- You know the exact field: a native selector, API query, or `jq` is cheaper.
- The product is an interactive terminal or live stream.
- The host can only call MCP servers and cannot run a command, install a skill,
  or use a hook.
- You need an orchestrator, scheduler, merge policy, or deployment approval.
- One expansion would return almost the whole artifact anyway.

## Safety

The rules below are enforced in code and mapped to executable tests in
[`INVARIANTS.md`](INVARIANTS.md).

- **Redaction before persistence.** Payloads must pass redaction before the
  store accepts them; the store's API does not take an unredacted value.
  Secret-like keys, vendor credentials (GitHub, AWS, Anthropic, OpenAI, Slack,
  Stripe, and others), and embedded Bearer tokens, PEM keys, JWTs, and
  `name=secret` pairs are removed ([`docs/safety.md`](docs/safety.md)).
- **Fail closed.** Mutating operations need `--yes`; shell- and network-backed
  sources also need their trust flag, even with `--yes`. Discovery only probes
  operations proven read-only.
- **Bounded everywhere.** Capture, traversal, search, findings, pagination,
  and responses all have explicit limits, and hitting one is reported.
- **Local.** The store is `./.prog` by default (`--dir` or `PROG_DIR`), with
  retention limits enforced on every write (`prog cache retention`) and
  `prog cache purge --all` to clear it. Parallel processes can share a store.

## Reference

| Workflow | Commands |
| --- | --- |
| Install and verify the agent integration | `harness install`, `harness doctor`, `shim install`, `shim list` |
| Capture a command or artifact | `run`, `observe`, `recipe` |
| Register and call a source | `source add-http`, `source add-cli`, `source add-mcp`, `discover`, `hints`, `call` |
| Navigate a capture | `select`, `evidence`, `expand`, `search`, `find`, `inspect`, `paths`, `coverage` |
| Compare and verify | `receipt`, `delta`, `status`, `session obligation-add`, `session show --readiness`, `verification begin`, `verification readback` |
| Goals and notes for an investigation | `session start`, `session note`, `session show` |
| Long-running MCP tasks | `mcp-task start`, `get`, `result`, `cancel` |
| Storage, cost, and contracts | `cache`, `cost`, `meta` |
| Update a curl-installed binary | `update --yes` |
| Classify argv without running it; legacy single-host install | `route`, `init` |

Every command documents itself with `--help`. Global options: `--dir`
(`PROG_DIR`, default `./.prog`), `--format full|compact` (`PROG_FORMAT`),
`--budget-bytes` (`PROG_BUDGET_BYTES`), `--budget-tokens`
(`PROG_BUDGET_TOKENS`), `--lens-dir` (`PROG_LENS_DIR`), and `--pretty`.

Documentation: [walkthroughs](docs/walkthroughs.md) ·
[evidence navigation](docs/evidence-navigation.md) · [run](docs/run.md) ·
[observe](docs/observe.md) · [sources](docs/source-setup.md) ·
[integrations](docs/integrations.md) · [agent reference](docs/agent-reference.md) ·
[contracts](docs/contracts.md) · [safety](docs/safety.md) · [cache](docs/cache.md) ·
[lenses](docs/lenses.md) · [evaluations](docs/evaluation-docs.md) · [all docs](docs/).

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

CI runs these on Ubuntu and macOS (Apple Silicon and Intel), plus a Rust 1.89
build, dependency-advisory and license checks (`cargo deny`), a crate-package
check, and the native harness extension tests. The documentation is
executable: `docs_examples.rs` runs this README's quickstart and checks that
key claims stay present, so editing the README can fail the suite.
Contributors and coding agents start with [`AGENTS.md`](AGENTS.md).

## Project boundaries

- Not an HTTP proxy, transparent cache, agent runtime, autonomous coding loop,
  or deployment system, and no interactive UI.
- No MCP server mode: prog is not an MCP server. It reads MCP servers as
  upstream sources, and agents use it through the CLI, a skill, and hooks.

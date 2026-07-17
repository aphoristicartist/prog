# prog

[![CI](https://github.com/aphoristicartist/prog/actions/workflows/ci.yml/badge.svg)](https://github.com/aphoristicartist/prog/actions/workflows/ci.yml)

**Bounded, inspectable tool output for AI agents and loop engineering.**

`prog` captures output from local commands, files, HTTP APIs, and MCP servers,
redacts it before persistence, and returns a compact JSON envelope. Large or
uncertain regions stay available behind cursor-scoped JSON Pointers, so an
agent can inspect the shape first and retrieve only the evidence needed for the
next decision without rerunning the source.

```text
capture once -> redact -> bounded envelope -> inspect -> exact evidence -> verify again
```

The default model-visible response budget is 16 KiB. Cached navigation commands
(`inspect`, `search`, `find`, `evidence`, `paths`, and `expand`) operate on the
persisted, redacted payload and do not contact the upstream source.

Use `--budget-bytes N` (or `PROG_BUDGET_BYTES=N`) to set a hard stdout ceiling
for one invocation. `--budget-tokens N` and `PROG_BUDGET_TOKENS=N` are a
convenience conversion using the explicitly labeled `bytes_div_4_approximate`
estimator; they are not tokenizer measurements. Command flags override the
environment, which overrides a source profile's optional
`disclosure_budget.max_bytes`; a 64 KiB safety ceiling still applies. Every JSON response
reports its applied `disclosure_budget`, `capture_budget`, and `storage_budget`,
including actual emitted stdout bytes.

## Why prog

Tool output is often the largest and least predictable input in an agent run:
test failures, compiler diagnostics, CI logs, diffs, issue lists, API responses,
and security reports can all exceed what the current step needs.

`prog` provides one consistent result-side contract:

- **Bounded first view.** Data-capturing operations return a
  `DisclosureEnvelope` with a preview, shape hints, omissions, and findings.
- **Recoverable detail.** When policy allows the payload to be persisted, the
  envelope includes a cursor and omitted data remains addressable by JSON
  Pointer instead of being discarded by truncation.
- **Offline evidence navigation.** Repeated inspection reads the local cache;
  commands and APIs are not rerun just to reveal another slice.
- **Deterministic findings.** Generic and lens-provided findings rank likely
  failures and evidence paths without model calls.
- **Redaction before persistence.** Secret-bearing fields and supported secret
  patterns are removed before payloads enter the store.
- **Fail-closed execution.** Unknown, mutating, shell-backed, and sensitive
  operations remain gated by explicit effect and trust policy.
- **Machine-readable operations.** Operational successes, errors, schemas,
  evidence blocks, cache receipts, and session trails are JSON; CLI help remains
  conventional text.

`prog` is most useful when the relevant path is not known before capture, the
source is expensive or undesirable to rerun, or several loop iterations need
to inspect the same observation. If the exact field is already known, a native
API projection or `jq` is usually simpler.

## Install

The repository is a Rust workspace at version `0.1.0`. Install the `prog`
binary from a checkout:

```sh
git clone https://github.com/aphoristicartist/prog.git
cd prog
cargo install --path crates/prog-cli
prog --help
```

Prebuilt binaries for Ubuntu and macOS are published on the
[GitHub Releases page](https://github.com/aphoristicartist/prog/releases).
Each release ships a tarball per platform, a combined `SHA256SUMS`, a CycloneDX
SBOM, and a build-provenance attestation. Verify a download before use:

```sh
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify prog-*.tar.gz --owner aphoristicartist
```

For development, replace `prog` with `cargo run --` in the examples below.

## Supported platforms

- **Ubuntu** (linux-x86_64) and **macOS** are supported and CI-verified on every
  push and pull request (formatting, Clippy, full test suite, and an MSRV gate).
- **Windows is not supported.** The process-group, permissions, and signal
  semantics `prog` relies on are not implemented for Windows; see
  [#140](https://github.com/aphoristicartist/prog/issues/140).
- **MSRV** is pinned at Rust **1.89** (`rust-version = "1.89"` in the workspace
  `Cargo.toml`) and verified by a dedicated CI job on `rust-toolchain@1.89.0`.

See [`docs/release-notes.md`](docs/release-notes.md) for the full per-release
reference.

## Quickstart

This repository includes a deterministic CLI fixture. The command sequence is
covered by the documentation integration tests.

```sh
rm -rf /tmp/prog-demo
prog --dir /tmp/prog-demo --pretty source add-cli demo_cli --operation list --read-only -- python3 fixtures/cli/list_items.py
prog --dir /tmp/prog-demo --pretty call demo_cli list --args '{}'
CURSOR=$(prog --dir /tmp/prog-demo call demo_cli list --args '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
prog --dir /tmp/prog-demo --pretty expand "$CURSOR" --path /items --limit 3 --depth 3
prog --dir /tmp/prog-demo --pretty inspect "$CURSOR" --goal "find important evidence"
prog --dir /tmp/prog-demo --pretty search "$CURSOR" "Item 2"
prog --dir /tmp/prog-demo --pretty hints demo_cli list
prog --dir /tmp/prog-demo --pretty meta SourceProfile
```

The source command runs once for the first cache entry. The returned cursor can
then drive bounded expansion, ranked inspection, and search from the local
store. `meta` exposes `prog`'s own public contracts through the same envelope
mechanism.

## Built for loop engineering

In this README, **loop engineering** means designing a repeatable agent cycle
that observes a system, chooses an action, verifies the result, retains useful
state, and either continues or stops at an explicit gate.

`prog` is not the agent runtime or loop scheduler. It is the observation and
evidence layer inside that loop:

| Loop move | `prog` surface | What it contributes |
| --- | --- | --- |
| Observe | `run`, `observe`, `call`, `recipe` | Captures a command, artifact, API response, or MCP result into one bounded envelope |
| Orient | envelope `findings`, `inspect`, `search`, `find` | Identifies likely failures and locates relevant cached structure |
| Focus | `evidence`, `paths`, `expand` | Retrieves a cited path or bounded slice without rerunning the source |
| Act | external agent or human | Edits code or changes the system; `prog` does not make that decision |
| Verify | rerun `recipe`, `run`, or `call` | Produces a fresh observation for the next iteration |
| Remember | `session start`, `note`, `show` | Stores redacted goals, notes, and evidence-navigation metadata locally |
| Stop or approve | external loop or human gate | `prog` reports evidence; it does not merge, deploy, or approve changes |

That separation is useful for loops because raw tool output does not have to be
placed into every model turn, while the exact evidence remains reachable when a
later iteration needs it.

### Example: fail, inspect, fix, verify

This complete example creates a real Rust compiler error, captures it, retrieves
the top evidence path, fixes the source, and verifies a fresh run:

```sh
rm -rf /tmp/prog-loop
prog --dir /tmp/prog-loop session start --goal "compile the sample program"
printf '%s\n' 'fn main() { let value: u32 = "not a number"; println!("{value}"); }' > /tmp/prog-loop-demo.rs

RESULT=$(prog --dir /tmp/prog-loop run -- rustc /tmp/prog-loop-demo.rs -o /tmp/prog-loop-demo)
CURSOR=$(printf '%s' "$RESULT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
TOP_PATH=$(printf '%s' "$RESULT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["findings"][0]["path"])')

prog --dir /tmp/prog-loop inspect "$CURSOR" --goal "find the compile error" --limit 5
prog --dir /tmp/prog-loop evidence "$CURSOR" --path "$TOP_PATH"
```

The source edit is the action step. Verification creates a new observation
rather than mutating the failed one:

```sh
printf '%s\n' 'fn main() { let value: u32 = 42; println!("{value}"); }' > /tmp/prog-loop-demo.rs
prog --dir /tmp/prog-loop run -- rustc /tmp/prog-loop-demo.rs -o /tmp/prog-loop-demo
prog --dir /tmp/prog-loop run -- /tmp/prog-loop-demo
prog --dir /tmp/prog-loop session note "compiled and ran the corrected program"
prog --dir /tmp/prog-loop session show
```

The session trail records navigation metadata and notes, not copies of payload
bodies. A loop should decide success from the fresh command status and its own
acceptance criteria; `prog` does not claim that a ranked finding is a fix.

For real test loops, the first-party recipes add domain lenses and default
goals while preserving the executed command in the envelope:

```sh
prog recipe --timeout-ms 180000 cargo-test -- cargo test
prog recipe pytest -- pytest -q
prog recipe npm-test -- npm test
prog recipe go-test -- go test ./...
```

### Example: root-cause a saved log

```sh
printf '%s\n' 'INFO checkout started' 'ERROR checkout failed: timeout after 30s' > /tmp/service.log
RESULT=$(prog --dir /tmp/prog-logs recipe logs-root-cause --file /tmp/service.log)
CURSOR=$(printf '%s' "$RESULT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
prog --dir /tmp/prog-logs inspect "$CURSOR" --goal "find the root cause" --limit 5
prog --dir /tmp/prog-logs search "$CURSOR" "timeout" --path /lines
prog --dir /tmp/prog-logs find "$CURSOR" --kind error
```

The log recipe uses the checked-in `logs` lens. Search is case-insensitive by
default; `--regex` enables a size-bounded Rust regex.

### Example: review a diff without losing the source hunk

```sh
git diff --no-ext-diff HEAD^1 HEAD > /tmp/change.diff
RESULT=$(prog --dir /tmp/prog-diff recipe diff-review --file /tmp/change.diff)
CURSOR=$(printf '%s' "$RESULT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
prog --dir /tmp/prog-diff inspect "$CURSOR" --goal "find risky changed hunks"
prog --dir /tmp/prog-diff paths "$CURSOR" --prefix /files --expandable-only
```

Unified diffs are indexed into structured file and hunk metadata while source
lines remain available through cursor-backed paths.

### Example: loop over a registered source

Use a source profile when the same HTTP API, CLI, or MCP server will be called
across multiple iterations:

```sh
prog source add-cli repository --operation status --read-only -- git status --short
prog hints repository status
prog call repository status --args '{}'
```

The example marks `git status --short` read-only explicitly. HTTP `GET` source
operations are read-only and cacheable; non-`GET` operations are
confirmation-gated and non-cacheable. For read-only paginated operations,
`prog call --pages N` follows supported continuation hints under page, byte,
wall-time, and envelope caps.

## The disclosure envelope

`call`, `run`, `observe`, `recipe`, `expand`, and `meta` return the same
top-level disclosure contract. Navigation commands such as `inspect`, `search`,
and `evidence` have smaller dedicated JSON contracts.

```json
{
  "schema": "prog.disclosure",
  "source_id": "...",
  "operation": "...",
  "summary": {
    "kind": "...",
    "payload_bytes": 0,
    "approx_tokens": 0,
    "envelope_bytes": 0
  },
  "data_preview": {},
  "schema_hints": {},
  "omitted": [],
  "findings": [],
  "cursor": "pc1_...",
  "cache": { "status": "stored", "ttl_seconds": 86400 },
  "capture_budget": { "source": "default", "limits": [] },
  "storage_budget": { "source": "default" },
  "next_actions": []
}
```

The values above illustrate field shape only. `schema_hints` describe the full
payload, `omitted` explains what the preview withheld, and `next_actions`
contains machine-readable follow-up commands. Use `prog meta DisclosureEnvelope`
for the generated contract schema.

## Inputs and adapters

### Direct capture

- `prog run -- <command...>` captures bounded stdout, stderr, combined stream
  chunks, exit status, timing, and recognized failure sections.
- `prog observe --file ...` and `prog observe --stdin` accept JSON, SARIF,
  NDJSON, JUnit XML, basic HTML, unified diffs, CSV/TSV, Markdown or aligned
  tables, and bounded text fallback observations.
- Binary-looking observations are rejected with a structured error.

### Reusable sources

- HTTP source profiles with explicit methods, URLs, parameters, auth references,
  pagination hints, and effect policy.
- Local CLI source profiles stored as argv rather than shell command strings.
- MCP tools and resources consumed as upstream sources through the MCP adapter.
- OpenAPI, JSON Schema, and CLI-help imports with bounded schema depth and
  graded effect evidence.

### First-party recipes

```text
cargo-test  pytest  npm-test  go-test  gh-issues  diff-review  logs-root-cause
```

Recipes are thin, deterministic compositions of `run` or `observe`, a
first-party lens, and `inspect`. They do not start an agent or hide the expanded
command; the envelope records the command and recommended next evidence action.

### First-party lens coverage

The repository includes data-only lenses for Cargo, pytest, npm, Go tests,
JUnit, SARIF, GitHub issues, kubectl JSON, unified diffs, logs, run streams,
NDJSON records, and generic JSON item triage. Lens manifests can select fields,
declare omissions and next actions, and contribute bounded finding rules. They
cannot execute code.

## Agent integration

Install a project-local skill and explicit hook wrapper for a supported agent:

```sh
prog init --agent codex --project --dry-run
prog init --agent codex --project
```

Supported values are `codex`, `claude-code`, `cursor`, and `gemini-cli`. The
installer reports every planned file, does not silently overwrite existing
files, and includes a generated uninstall script. See
[`docs/integrations.md`](docs/integrations.md) for the exact paths.

`prog` can consume MCP as an upstream source, but **prog itself does not expose
an MCP server mode**. The durable integration surface is CLI + agent skill +
explicit project hooks.

## Command map

| Workflow | Commands |
| --- | --- |
| Capture one command or artifact | `run`, `observe` |
| Register and understand sources | `source add-http`, `source add-cli`, `discover`, `hints` |
| Call a reusable source | `call` |
| Navigate cached evidence | `inspect`, `search`, `find`, `evidence`, `paths`, `expand` |
| Run a domain workflow | `recipe` |
| Retain investigation metadata | `session start`, `session note`, `session show` |
| Inspect storage and economics | `cache`, `cost` |
| Inspect public contracts | `meta` |
| Install agent integration | `init` |

Run `prog <command> --help` for the complete argument surface. Global options
are `--dir <DIR>` (`PROG_DIR`, default `./.prog`), `--lens-dir <DIR>`
(`PROG_LENS_DIR`, default `./lenses`), `--budget-bytes <N>`
(`PROG_BUDGET_BYTES`), `--budget-tokens <N>` (`PROG_BUDGET_TOKENS`), and
`--pretty`. The byte budget is authoritative; when pretty formatting would
exceed it, `prog` emits compact JSON instead.

## Safety and storage

The safety model is enforced in code and mapped to executable tests in
[`INVARIANTS.md`](INVARIANTS.md).

- Raw payloads must cross the redaction boundary before the store accepts them.
- Secret-like object keys and supported embedded Bearer, PEM, JWT, name/value,
  and URL-parameter patterns are redacted before persistence.
- Sensitive or non-cacheable operation results are not persisted.
- Cursor expansion is provenance-scoped and rejects stale, foreign, or expired
  cursors. A pre-release store-contract change resets the local store instead
  of interpreting stale cursor records.
- Discovery probes only operations allowed by the read-only effect policy.
- Mutating operations require `--yes`; shell-backed operations additionally
  require source-profile trust.
- Traversal, search, findings, pagination, command capture, and envelopes have
  explicit bounds.
- `cache retention` persists independent payload-byte and age limits which are
  enforced on every cache write; evicted evidence remains metadata-only.
- `cache purge --all` removes cache state and session trails while preserving
  the retention policy.

Source profiles can be committed when they contain stable configuration and
environment references rather than literal credentials. The `.prog/` runtime
store contains captured payloads and is ignored by this repository.

## Measured results

All numbers below come from checked-in deterministic fixtures and use the
project heuristic of bytes / 4, rounded up. They are regression measurements,
not universal promises about model quality, latency, or cost.

### Token-economics fixtures

Across the checked-in HTTP, CLI, and MCP tasks, raw-payload tokens divided by
the complete `prog` task tokens range from **34.5x-162.8x**. Each task includes
the initial envelope and any expansion used to answer it. See
[`docs/token-economics.md`](docs/token-economics.md) for every row and the
regeneration command.

### Evidence-acquisition fixtures

The five checked-in Cargo compile, Cargo test, pytest, noisy-log, and SARIF
scenarios rank the expected causal path first in **5/5** cases. The findings
workflow uses 10 tool calls versus 15 for `envelope -> paths -> evidence`, and
the estimated output is 3,166 versus 3,446 tokens. See
[`docs/evidence-acquisition.md`](docs/evidence-acquisition.md) and the checked
baseline in
[`fixtures/evals/evidence-acquisition-metrics.json`](fixtures/evals/evidence-acquisition-metrics.json).

### Deterministic workflow demos

The checked-in GitHub review, kubectl events, CloudWatch-style logs, Jira-style
triage, and MCP incident demos report raw-to-envelope-plus-expansion ratios from
**9.61x to 15.40x**. These are generated local payloads, not credentialed live
service measurements. See [`docs/real-world-demos.md`](docs/real-world-demos.md).

## When not to use prog

Use the simplest precise tool available. `prog` is usually the wrong layer when:

- the payload is already smaller and clearer than an envelope;
- a native field selector, exact API query, or known `jq` expression returns
  the required value directly;
- an interactive TTY or live streaming output is the product experience;
- the loop needs an orchestrator, scheduler, isolated worktrees, merge policy,
  or deployment approval rather than an evidence layer;
- one expansion would reveal almost the entire artifact anyway.

The comparison report includes cases where native field selection and direct
queries beat `prog`: [`docs/positioning.md`](docs/positioning.md) and
[`docs/competitive-baselines.md`](docs/competitive-baselines.md).

## Documentation

### Start here

- [End-to-end walkthroughs](docs/walkthroughs.md)
- [Evidence navigation](docs/evidence-navigation.md)
- [Running commands](docs/run.md)
- [Observing files and stdin](docs/observe.md)
- [Adding HTTP and CLI sources](docs/source-setup.md)
- [Agent integrations](docs/integrations.md)

### Contracts and safety

- [Disclosure contracts](docs/contracts.md)
- [Safety and trust model](docs/safety.md)
- [Cache lifecycle](docs/cache.md)
- [Lens manifests and packs](docs/lenses.md)
- [Executable invariants](INVARIANTS.md)

### Evaluation

- [Token economics](docs/token-economics.md)
- [Evidence acquisition](docs/evidence-acquisition.md)
- [Task-success evaluation](docs/task-success-eval.md)
- [Competitive baselines](docs/competitive-baselines.md)
- [Real-world-shaped local demos](docs/real-world-demos.md)

The complete reference set is under [`docs/`](docs/).

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo run -- --help
```

The CI workflow runs formatting, Clippy with warnings denied, the full default
test suite, and a CLI help smoke test. Property tests, golden findings,
documentation examples, the 360-scenario transport matrix, and checked-in evals
run through ordinary Cargo integration tests.

## Project boundaries

- `prog` is not a general-purpose HTTP proxy or transparent cache.
- `prog` is not an agent runtime, autonomous coding loop, or deployment system.
- `prog` has no interactive UI.
- No MCP server mode is planned; MCP is supported as an upstream adapter.

`prog` keeps the first observation small, makes omitted evidence recoverable,
and gives repeated engineering loops a stable way to inspect what already ran.

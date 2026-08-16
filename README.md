# prog

[![CI](https://github.com/aphoristicartist/prog/actions/workflows/ci.yml/badge.svg)](https://github.com/aphoristicartist/prog/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.89+](https://img.shields.io/badge/rust-1.89%2B-orange.svg)](https://www.rust-lang.org)
[![Platforms](https://img.shields.io/badge/platforms-linux%20%7C%20macOS-lightgrey.svg)](#supported-platforms)

**An agent-harness extension for bounded, inspectable tool results.**

`prog` is built for agents and harnesses to invoke inside a tool loop. The CLI
is its local JSON transport and recovery surface, not a terminal application a
human is expected to operate interactively.

Your agent runs `cargo test`. The output is 130,000 tokens. It needs one error
line — but it can't know which line until it has read them all.

So you truncate, and lose the answer. Or you don't, and pay for the whole log
on every turn.

```text
                       tokens into the model
  raw payload   ████████████████████████████████████████  137,883
  prog          ▏                                             847
```

<sub>One row from [`docs/token-economics.md`](docs/token-economics.md): the "discover shape"
task over the checked-in HTTP fixture. Ratios across all fixtures range 24.4x-85.2x.
Measured on deterministic fixtures with a bytes/4 heuristic — not a promise about your workload.</sub>

The difference isn't compression. `prog` captures the payload **once**, redacts
it, stores it, and hands back a small envelope describing its *shape* — plus a
cursor. Everything omitted stays addressable by JSON Pointer, so the next step
retrieves exactly the evidence it needs **without rerunning the source**.

```text
capture once -> redact -> bounded envelope -> inspect -> exact evidence -> verify again
```

```sh
prog run -- cargo test          # 1 bounded envelope, ranked findings
prog inspect "$CURSOR" --goal "find the compile error"
prog evidence "$CURSOR" --path /failure_sections/0    # exact, cited, offline
```

Nothing was truncated away. The full redacted payload is still on disk.

## Contents

- [Install](#install) · [Quickstart](#quickstart)
- [Why prog](#why-prog) · [Built for loop engineering](#built-for-loop-engineering)
- [Verifying what changed](#verifying-what-changed) — the part most tools skip
- [The disclosure envelope](#the-disclosure-envelope) · [Inputs and adapters](#inputs-and-adapters)
- [Harness extensions and plugins](#harness-extensions-and-plugins) · [Command map](#command-map)
- [Safety and storage](#safety-and-storage) · [Measured results](#measured-results)
- [When not to use prog](#when-not-to-use-prog) · [Documentation](#documentation)

## Install

Install the latest verified binary with `curl` (requires
[`gh`](https://cli.github.com/) for mandatory build-provenance verification):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aphoristicartist/prog/releases/latest/download/install.sh | sh
```

The installer selects the supported target, downloads into a temporary
directory, verifies both `SHA256SUMS` and the GitHub build-provenance
attestation, and only then atomically places `prog` in `~/.local/bin`. Override
the destination with `PROG_INSTALL_DIR`. It refuses unsupported platforms,
checksum mismatches, missing attestations, and missing verification tools. If
the install directory is not already on `PATH`, it adds one idempotent entry to
the detected zsh, bash, or POSIX-shell profile. Open a new terminal, then
install the harness extension into a project:

```sh
prog harness install --root /path/to/project
prog harness doctor --root /path/to/project
```

`harness install` places the portable Agent Skill and detected host adapters in
the project without overwriting existing files. `harness doctor` is the
machine-readable readiness gate; the integration is usable only when it returns
`"ready": true`. `prog --help` remains available for harness discovery and
debugging.

Set `PROG_MODIFY_PATH=0` to leave shell startup files unchanged. Unknown shells
are never guessed; the installer keeps the verified installation and prints a
manual PATH instruction instead.

Update a curl-managed installation to the latest verified release explicitly:

```sh
prog update --yes
```

`prog update` never runs in the background. Without `--yes` it fails closed
before network access or filesystem mutation; package-manager installations
are not overwritten unless an explicit `--install-dir` is supplied. See
[`docs/install.md`](docs/install.md) for exact-version installs, profile
selection, manual verification, and the updater trust model.

For development from a checkout:

```sh
cargo install --path crates/prog-cli
```

Replace `prog` with `cargo run --` in the examples below when developing.

### Supported platforms

- **Ubuntu 22.04+ on x86_64** and **macOS 15+ on Apple Silicon or Intel** are
  supported and CI-verified on every push and pull request (formatting, Clippy,
  full test suite, and an MSRV gate).
- **Windows is not supported.** The process-group, permissions, and signal
  semantics `prog` relies on are not implemented for Windows; see
  [#140](https://github.com/aphoristicartist/prog/issues/140).
- **MSRV** is pinned at Rust **1.89** (`rust-version = "1.89"` in the workspace
  `Cargo.toml`) and verified by a dedicated CI job on `rust-toolchain@1.89.0`.

See [`docs/release-notes.md`](docs/release-notes.md) for the full per-release
reference.

## Quickstart

This repository includes a deterministic CLI fixture. Every command below is
re-executed by the documentation integration tests, so it stays copy-pasteable.

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
| Verify | `delta`, `session show --readiness`, rerun `recipe`/`run`/`call` | Produces a fresh observation and compares it conservatively against the baseline |
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

## Verifying what changed

Most observation tooling stops at "here is the new output." The hard part of a
loop is the next question: **did the thing I was trying to fix actually go
away — or did I just not look where it was?**

Raw output cannot distinguish those. `prog delta` refuses to.

```sh
prog delta "$BASELINE_OBSERVATION" "$SUBJECT_OBSERVATION"
```

Each finding is classified `new`, `persisting`, `resolved`, `not_observed`, or
`unknown`. A finding is only `resolved` when absence is **provable** — same
canonical invocation, same comparison family, complete captures on both sides,
compatible normalization, and exhaustive selection scope. Otherwise a
disappeared finding is `not_observed` or `unknown`, and the assessment lists the
reasons why.

That means a narrower rerun cannot clear a broad baseline, and a truncated log
cannot be mistaken for a clean one.

Verification obligations turn that into an explicit gate. You commit to the
success criterion *before* you have the result:

```sh
prog session obligation-add checkout-fixed \
  --check "checkout error no longer present" --scope checkout \
  --origin-observation "$BASELINE" --expected-absent-fingerprint "$FINGERPRINT" \
  --evidence-observation "$VERIFICATION_RUN"

prog session show --readiness
```

`ready` is true only when every required obligation passed. Obligations are
immutable once declared, and only user declarations can be *required* — a
recipe or harness cannot authorize its own success. Of the nine possible
statuses, exactly one is `passed`; the rest are distinct ways of saying "not
proven", including `unverifiable` for truncated evidence and `stale` for a
changed workspace.

See [`docs/delta.md`](docs/delta.md) and
[`docs/verification.md`](docs/verification.md).

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
    "estimated_envelope_tokens": 0,
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
  "next_actions": [],
  "action_templates": {}
}
```

The values above illustrate field shape only. `schema_hints` describe the full
payload, `omitted` explains what the preview withheld, and `next_actions`
references machine-readable follow-ups. Cursor-backed actions select one
symbolic argv entry in `action_templates`; direct reruns carry exact argv. Use
`prog meta DisclosureEnvelope` for the generated contract schema.

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
- MCP tools and resources consumed as upstream sources through the MCP adapter,
  including long-running tasks via [`prog mcp-task`](docs/mcp-tasks.md).
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

## Harness extensions and plugins

The preferred installation unit is a harness plugin or extension. Every format
derives from the same disclosure, redaction, persistence, evidence, and
verification contract in
[`docs/harness-extension-protocol.md`](docs/harness-extension-protocol.md).
Wrappers are discovery mechanisms, not independent implementations.

Install every detected project adapter in one operation:

```sh
prog harness install --dry-run --root /path/to/project
prog harness install --root /path/to/project
prog harness doctor --root /path/to/project
```

The universal `.agents/skills/prog` target is always installed. Codex, Claude
Code, Gemini CLI, and Cursor adapters are added when their executable or project
directory is detected. Repeat `--host` for an explicit deployment instead of
detection.

### Codex plugin

This repository is a Codex marketplace root and ships `plugins/prog`. From a
checkout:

```sh
codex plugin marketplace add /path/to/prog
codex plugin add prog@personal
```

The plugin includes the canonical Agent Skill, dependency doctor, and exact-
argv wrapper. It deliberately does not parse Codex shell-command strings.

### DeepSeek Harness plugin

`extensions/deepseek-harness` is a native `tools/post-execute` plugin. It
captures the accepted immutable tool result, never reruns the tool, and replaces
the result only when the `prog` envelope is cheaper or redaction requires it.

```sh
dsh plugin --profile web add ./extensions/deepseek-harness
```

### Generated project adapters

The legacy single-host command remains available for compatibility:

```sh
prog init --agent codex --project --dry-run
prog init --agent codex --project
prog init --agent agents-md --project
prog init --print-skill --frontmatter yaml
```

Built-in values are `agent-skills`, `agents-md`, `codex`, `claude-code`,
`cursor`, and `gemini-cli`. Additional targets come from validated JSON passed
with `--manifest-dir`, so they do not require a `prog` release. Existing files
are never silently overwritten. See
[`docs/integrations.md`](docs/integrations.md) for the exact paths.

`prog` can consume MCP as an upstream source, but **prog itself does not expose
an MCP server mode**. The durable integration surface is a native result plugin
when the host provides a lossless replacement boundary, otherwise the portable
Agent Skill plus explicit wrapper, all backed by the same local CLI transport.

## Command map

| Workflow | Commands |
| --- | --- |
| Install and verify the harness extension | `harness install`, `harness doctor` |
| Capture one command or artifact | `run`, `observe` |
| Classify exact argv without executing it | `route` |
| Register and understand sources | `source add-http`, `source add-cli`, `source add-mcp`, `discover`, `hints` |
| Call a reusable source | `call` |
| Navigate cached evidence | `inspect`, `search`, `find`, `evidence`, `paths`, `expand` |
| Run a domain workflow | `recipe` |
| Compare two observations | `delta` |
| Read facade readiness and optional delta | `status` |
| Drive long-running MCP tasks | `mcp-task start`, `get`, `result`, `cancel` |
| Retain investigation metadata | `session start`, `session note`, `session show` |
| Gate on verification criteria | `verification begin`, `verification readback`, `session obligation-add`, `session obligation-list`, `session show --readiness` |
| Inspect storage and economics | `cache`, `cost` |
| Inspect public contracts | `meta` |
| Install one legacy host adapter | `init` |
| Update a curl-managed binary | `update --yes` |

Harnesses can run `prog <command> --help` for the complete argument surface;
every command and subcommand self-describes. Global options are `--dir <DIR>` (`PROG_DIR`, default
`./.prog`), `--lens-dir <DIR>` (`PROG_LENS_DIR`, default `./lenses`),
`--budget-bytes <N>` (`PROG_BUDGET_BYTES`), `--budget-tokens <N>`
(`PROG_BUDGET_TOKENS`), and `--pretty`. The byte budget is authoritative; when
pretty formatting would exceed it, `prog` emits compact JSON instead.

## Safety and storage

The safety model is enforced in code and mapped to executable tests in
[`INVARIANTS.md`](INVARIANTS.md).

- Raw payloads must cross the redaction boundary before the store accepts them.
  This is a typestate boundary, not a convention: `Store::put_payload` does not
  accept an unredacted value.
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
- Parallel processes may share one `--dir`: `prog` releases the database while
  waiting on commands or upstream sources, retries brief redb contention with
  bounded backoff, and reports exhausted contention as retryable
  `storage_busy` JSON with the attempt count. The default `./.prog` is relative
  to the current working directory, so agents launched from separate Git
  worktree roots already receive separate stores. Set `PROG_DIR` explicitly
  only when evidence and cursors are intended to be shared across worktrees.

Source profiles can be committed when they contain stable configuration and
environment references rather than literal credentials. The `.prog/` runtime
store contains captured payloads and is ignored by this repository.

## Measured results

All numbers below come from checked-in deterministic fixtures and use the
project heuristic of bytes / 4, rounded up. They are regression measurements,
not universal promises about model quality, latency, or cost.

### Token-economics fixtures

Across the checked-in HTTP, CLI, and MCP tasks, raw-payload tokens divided by
the complete `prog` task tokens range from **24.4x-85.2x**. Each task includes
the initial envelope and any expansion used to answer it. See
[`docs/token-economics.md`](docs/token-economics.md) for every row and the
regeneration command.

### Evidence-acquisition fixtures

The five checked-in Cargo compile, Cargo test, pytest, noisy-log, and SARIF
scenarios rank the expected causal path first in **5/5** cases. The findings
workflow uses 10 tool calls versus 15 for `envelope -> paths -> evidence`, and
the estimated output is 3,218 versus 3,369 tokens. See
[`docs/evidence-acquisition.md`](docs/evidence-acquisition.md) and the checked
baseline in
[`fixtures/evals/evidence-acquisition-metrics.json`](fixtures/evals/evidence-acquisition-metrics.json).

### Deterministic workflow demos

The checked-in GitHub review, kubectl events, CloudWatch-style logs, Jira-style
triage, and MCP incident demos report raw-to-envelope-plus-expansion ratios from
**9.61x to 15.40x**. These are generated local payloads, not credentialed live
service measurements. See [`docs/real-world-demos.md`](docs/real-world-demos.md).

### Correctness under an unknown target

Savings ratios assume you already know what you are looking for. The harder and
more common case is that you do not, and there the relevant number is not
compression but **whether the answer survives at all**.

Across the eleven checked-in competitive-baseline scenarios:

| Strategy | Correct |
| --- | --- |
| `head_tail_truncation` | **1/11** |
| `rtk_grep_filter` | 10/11 |
| `native_field_selection` | 8/11 |
| `prog_paths_expand` | **11/11** |

Truncation is the cheapest bounded strategy and the least correct one: it is
wrong in ten of eleven scenarios, and its omissions are unrecoverable. Field
selection and grep are excellent — *when the path or the term is already known*.
The `unknown-target-buried-fatal` scenario removes that assumption: a long log
whose one causal `FATAL` line is not guessable from the prompt. There, no field
selector is derivable, a plausible pre-read `grep ERROR` returns matches but
misses the causal line, and `prog` is the cheapest correct strategy at **7,917
versus 35,594 raw input tokens (4.5x)**.

See [`docs/competitive-baselines.md`](docs/competitive-baselines.md).

### Correctness, not just savings

[`docs/replay-eval.md`](docs/replay-eval.md) replays whole multi-iteration
trajectories behind an oracle that must never observe a false `resolved`,
false-fresh, or false-`passed` classification. That report deliberately makes
**no savings claim** — its payloads are tiny enough that envelope overhead costs
more than raw output, which is exactly the small-payload caveat documented
below.

## When not to use prog

Use the simplest precise tool available. `prog` is usually the wrong layer when:

- the payload is already smaller and clearer than an envelope;
- a native field selector, exact API query, or known `jq` expression returns
  the required value directly;
- an interactive TTY or live streaming output is the product experience;
- the host can only call MCP servers and cannot invoke a CLI, install a skill,
  or use an explicit hook;
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

### Verification

- [Conservative observation delta](docs/delta.md)
- [Verification obligations and readiness](docs/verification.md)
- [Actual-agent evaluation and claim gate](docs/agent-eval.md)
- [Long-running MCP tasks](docs/mcp-tasks.md)

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
- [Replay and correctness](docs/replay-eval.md)
- [Competitive baselines](docs/competitive-baselines.md)
- [Real-world-shaped local demos](docs/real-world-demos.md)

The complete reference set is under [`docs/`](docs/). Contributors and coding
agents should start with [`AGENTS.md`](AGENTS.md).

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

Note that the documentation itself is executable: `docs_examples.rs` re-runs the
quickstart above and asserts that specific claims remain present in this file.
Editing the README can fail the test suite. See [`AGENTS.md`](AGENTS.md).

## Project boundaries

- `prog` is not a general-purpose HTTP proxy or transparent cache.
- `prog` is not an agent runtime, autonomous coding loop, or deployment system.
- `prog` has no interactive UI.
- No MCP server mode exists for the first release; #216 defers reconsideration
  until the measured three-operation facade in #120 exists. MCP is supported as
  an upstream adapter.

`prog` keeps the first observation small, makes omitted evidence recoverable,
and gives repeated engineering loops a stable way to inspect what already ran.

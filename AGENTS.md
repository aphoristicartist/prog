# AGENTS.md

Orientation for AI agents and new contributors working **on the `prog` codebase**.

If you want to *use* `prog` as a tool inside your own agent loop, read
[`skills/prog/SKILL.md`](skills/prog/SKILL.md) instead. This file is about
changing `prog` itself.

## What this project is

`prog` captures output from local commands, files, HTTP APIs, and MCP servers,
redacts it before persistence, and returns a bounded JSON envelope. Omitted
regions stay addressable by JSON Pointer behind a cursor, so a later step can
retrieve exact evidence without rerunning the source.

The project's core value is **not** the feature list — it is that every claim it
makes is provable. Findings are deterministic and model-free. Deltas refuse to
report `resolved` unless absence can be proven. Numbers in docs are regenerated
from checked-in fixtures. **When in doubt, prefer the conservative answer over
the useful-sounding one.** That is the single most important convention here.

## Workspace layout

Three crates, strictly layered. Dependencies point downward only.

```
prog-cli  ──depends on──>  prog-adapters  ──depends on──>  prog-core
   │                                                           ▲
   └───────────────────────depends on──────────────────────────┘
```

| Crate | Role | Rule |
|---|---|---|
| `crates/prog-core` | Contracts, store, redaction, findings, disclosure, delta. Pure and I/O-light. | Must not know about clap, HTTP, or process spawning. |
| `crates/prog-adapters` | Talks to the outside world: `http.rs`, `cli.rs`, `mcp.rs`. | Returns `RawPayload`; never persists. |
| `crates/prog-cli` | Argument parsing, command dispatch, output rendering. | Holds no disclosure logic of its own; composes core. |

### Where things live in `prog-core`

| Module | Responsibility |
|---|---|
| `contracts.rs` | Every public serialized type. Changing it is a contract change — see below. |
| `store.rs` | redb-backed persistence, cache entries, session trails, observations. |
| `redaction.rs` | Key-based and value-pattern secret removal. Must be idempotent (I4). |
| `disclosure.rs` | Projection into a bounded preview; cursor expansion. |
| `findings.rs` | Deterministic ranking. The largest module; no model calls, ever. |
| `delta.rs` | Conservative comparison of two observations. |
| `navigation.rs` | `inspect`, `search`, `find`, `evidence` over cached payloads. |
| `lens.rs` | Data-only lens manifests. Lenses **cannot execute code**. |
| `pointer.rs` | RFC 6901 JSON Pointer parsing and containment checks. |
| `policy.rs` | Effect and trust gating; what may run without `--yes`. |
| `shape.rs` | Schema-hint inference; `join` obeys lattice laws (I5). |
| `pagination.rs` | Bounded auto-pagination for read-only operations. |
| `workspace.rs`, `source_state.rs` | State tokens that decide whether two observations are comparable. |
| `importers.rs`, `table.rs` | OpenAPI/JSON Schema/CLI-help import; non-JSON table inference. |

## Non-negotiable invariants

[`INVARIANTS.md`](INVARIANTS.md) maps thirteen invariants (I1–I13) to the exact
tests that execute them. **Read it before changing anything in `prog-core`.**
The ones that most often catch people:

- **I2 — redaction before persistence.** Enforced by typestate, not discipline.
  Payloads enter as `RawPayload`, must become `RedactedPayload` to be stored, and
  come back as `PersistedPayload`. `Store::put_payload` does not accept a plain
  `serde_json::Value`. Do not add an escape hatch.
- **I3 — expansion never escapes cursor provenance.** Containment is
  segment-wise and escaping-aware. `/items` does not contain `/items2`.
  Expansion requires a `ValidatedCursor` plus a `ScopedSlice`; raw strings
  cannot reach the expansion function.
- **I1 — projection never invents values.** A preview leaf must equal the source
  leaf, be an explicit marker, or be a labelled truncated prefix.
- **I10 — findings ranking is pure and deterministic**, and independent of input
  key order. Golden snapshots live in
  `crates/prog-core/tests/fixtures/findings/*.expected.json`.
- **I7 — fail closed.** Mutating operations need `--yes`; shell-backed
  operations additionally need source-profile trust. Only *proven* read-only
  evidence relaxes a gate; `assumed` and `unproven` stay gated.

If you change behavior an invariant covers, update both the code and the
invariant's test — never weaken the test to make a change pass.

## The conservative-answer rule, concretely

`prog delta` classifies each finding as `new`, `persisting`, `resolved`,
`not_observed`, or `unknown`. It emits `resolved` **only** when
`ComparabilityAssessment::can_prove_absence` is true. If the subject run was
scoped, non-exhaustive, incomplete, or not comparable, a disappeared finding
becomes `not_observed` or `unknown` — never `resolved`.

Verification obligations behave the same way: `VerificationStatus` includes
`unverifiable` and `stale` precisely so the system can decline to claim success.
Only `ObligationDeclarer::User` declarations may be `required`; recipe,
normalizer, and harness declarations are advisory by contract, because a
component must not be able to authorize itself.

Preserve this. A change that makes `prog` claim success more often is a
regression even if every test still passes.

## Build, test, lint

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo run -- --help
```

CI (`.github/workflows/ci.yml`) runs exactly these on Ubuntu and macOS, plus an
MSRV gate. **MSRV is Rust 1.89** — do not use newer language or std features.
Clippy warnings are denied; `dbg!` and `todo!` are warn-level lints workspace-wide.

Windows is explicitly unsupported ([#140](https://github.com/aphoristicartist/prog/issues/140));
the process-group and signal semantics are POSIX-only.

## Docs are executable — this will bite you

`crates/prog-cli/tests/docs_examples.rs` is a real test that:

1. **Re-runs the README quickstart** end to end against `fixtures/cli/list_items.py`.
2. **Asserts specific literal strings exist in `README.md`**, including
   `34.5x-162.8x`, `5/5`, `Built for loop engineering`, `No MCP server mode`,
   and several exact command lines.
3. **Asserts a list of `docs/*.md` files and fixtures still exist.**

So: editing the README can fail the test suite. If you change a quickstart
command, change the test with it. If you change a measured number, regenerate it
rather than editing it by hand:

```sh
PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture
```

Never hand-edit a number in `docs/token-economics.md`,
`docs/evidence-acquisition.md`, or any `fixtures/evals/*.json`.

## Adding things

**A lens** — add a JSON manifest under `lenses/`. Manifests select fields,
declare omissions and next actions, and contribute bounded finding rules. They
are data. If you find yourself wanting a lens to run code, the answer is no;
that belongs in `findings.rs` as a deterministic rule.

**A recipe** — recipes are thin, deterministic compositions of `run`/`observe` +
a first-party lens + `inspect`. They must not hide the expanded command; the
envelope records exactly what ran.

**A CLI command** — define args in `crates/prog-cli/src/cli_args.rs` and the
implementation in `crates/prog-cli/src/commands/`. **Every command and
subcommand needs a `///` doc comment** — it becomes the `--help` description
that agents rely on to discover the surface.

**A contract change** — types in `contracts.rs` are published through
`prog meta`. Add new types to `public_contract_schemas()`. The store is
pre-1.0: a store-contract change resets the local store rather than
interpreting stale cursor records.

## Conventions

- **Conventional commits**: `feat:`, `fix:`, `refactor:`, `style:`, `docs:`.
- **argv, never shell strings.** CLI sources store `Vec<String>`. Do not add a
  code path that builds a command by string concatenation.
- **JSON out, text help in.** Operational output — successes, errors, schemas,
  evidence, receipts — is JSON. `--help` stays conventional text.
- **No MCP server mode.** `prog` consumes MCP as an upstream adapter. Exposing
  `prog` as an MCP server is a documented non-goal, not a missing feature; the
  durable integration surface is CLI + agent skill + explicit hooks.
- **No model calls in the library.** Ranking and findings are deterministic.

## Non-goals

`prog` is not an agent runtime, loop scheduler, HTTP proxy, transparent cache,
deployment system, or interactive UI. Requests to add orchestration, merge
policy, or approval gates belong in the calling agent, not here.

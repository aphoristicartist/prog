# prog

**A progressive-disclosure gateway for noisy HTTP APIs, local CLIs, and MCP servers — bounded envelopes first, expand only what you need, from a local cache. Keeps model context small.**

prog wraps high-volume, hard-to-predict tooling and returns a single bounded `DisclosureEnvelope` per call. Each envelope carries a short summary, a schema hint, and a cursor; anything large or uncertain is omitted behind a JSON Pointer you can expand later **without re-contacting the upstream**. The result: agents and humans see the shape of a response first, and pay tokens only for the slice they actually need.

Measured against raw payloads, prog shrinks the context an agent must hold by **34.5x-162.8x** while keeping every envelope under a 16 KiB invariant.

---

## Install

```sh
cargo install --path crates/prog-cli
prog --help
```

For development, substitute `cargo run --` for `prog` in any command below.

---

## Quickstart

These commands are copy-pasteable and exact — they are exercised by the test suite.

```sh
rm -rf /tmp/prog-demo
prog --dir /tmp/prog-demo --pretty source add-cli demo_cli --operation list --read-only -- python3 fixtures/cli/list_items.py
prog --dir /tmp/prog-demo --pretty call demo_cli list --args '{}'
CURSOR=$(prog --dir /tmp/prog-demo call demo_cli list --args '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
prog --dir /tmp/prog-demo --pretty expand "$CURSOR" --path /items --limit 3 --depth 3
prog --dir /tmp/prog-demo --pretty hints demo_cli list
prog --dir /tmp/prog-demo --pretty meta SourceProfile
```

What just happened: you registered a read-only CLI source, called it once into a bounded envelope, pulled a cursor, expanded one JSON Pointer (`/items`) from the local cache, asked for human-readable operation hints, then introspected prog's own `SourceProfile` contract through the same envelope loop.

---

## Mental model

prog is three layers of intelligence stacked on one envelope/expand primitive.

### Layer 1 — source intelligence

Understand a tool before you call it. `prog source add-http` / `prog source add-cli` build a `SourceProfile` from a single command line — no hand-written seeds. `prog discover` turns seeds, or imported OpenAPI / JSON Schema / CLI help, into a `SourceProfile`. `prog hints` summarizes each operation's inputs, effects, and likely next calls.

### Layer 2 — response intelligence

Call once, get a bounded view, expand on demand. `prog call` runs one operation and returns a `DisclosureEnvelope`. `prog expand` reveals a JSON Pointer path from the cached payload — no upstream re-contact.

Every envelope carries:

| Field | Meaning |
| --- | --- |
| `schema_version` | `prog.disclosure.v1` |
| `source_id`, `operation` | what was called |
| `summary` | `{kind, payload_bytes, approx_tokens, envelope_bytes}` |
| `schema_hints` | shape of the full payload |
| `omitted` | `[{path, reason, detail}]` — what was held back and why |
| `cursor` | e.g. `pc1_…` — the handle for expansion |
| `cache` | `{status, ttl_seconds}` |

### Layer n+1 — reflexivity

prog exposes its own JSON contracts through the same envelope/expand loop. `prog meta` returns prog's schemas (for example `prog --dir /tmp/prog-demo --pretty meta SourceProfile`), so an agent can learn prog the same way it learns any other source.

---

## AI-native integration (how prog is meant to be driven)

prog is designed to be **driven by agents**, not configured around them. The intended integration is a CLI plus a first-party agent skill plus hooks — prog is a tool the agent picks up and runs, never a server it connects to.

```sh
prog init --agent codex --project
```

`--project` installs a project-local, agent-scoped skill (for codex that lands at `.codex/skills/prog/SKILL.md`; `--agent` also accepts `claude-code`, `cursor`, and `gemini-cli`, each writing under its own agent directory) plus the run hook, manifest, and uninstall script. Use `--dry-run` to preview the plan before anything is written. The canonical skill source lives at [`skills/prog/SKILL.md`](skills/prog/SKILL.md) in the repo.

MCP has a role, but a narrow one: an **upstream adapter only**. prog can consume an MCP server as a source like any other; it does not become one. For the full integration model, see [`docs/integrations.md`](docs/integrations.md).

> **No MCP server mode.** This is a deliberate, permanent non-goal. A CLI + skill + hooks is the sharper, more composable, more natively agent-fit surface; exposing prog itself as an MCP server would add ceremony without adding capability. (Issue #71 was closed as not planned for this reason.)

---

## Capabilities and commands

```
prog [GLOBAL OPTIONS] <command> [OPTIONS]
```

**Global options:** `--dir <DIR>` (env `PROG_DIR`, default `./.prog`) · `--lens-dir <LENS_DIR>` (env `PROG_LENS_DIR`, default `./lenses`) · `--pretty` · `-h, --help` · `-V, --version`.

| Command | Purpose | Notable flags |
| --- | --- | --- |
| `discover` | Turn seeds or imported descriptors into a `SourceProfile` | `--kind` (http\|cli\|mcp), `--seed`, `--import` (auto\|openapi\|json-schema\|cli-help), `--command-base`, `--max-schema-depth`, `--probe` |
| `source add-http` | Register an HTTP operation | `--operation`, `--url`, `--method` (default GET), `--probe` |
| `source add-cli` | Register a local CLI operation | `--operation`, `--read-only`, `--probe` |
| `hints` | Summarize an operation's inputs, effects, next calls | — |
| `call` | Run one operation into a bounded envelope | `--args`, `--view`, `--lens`, `--yes`, `--no-cache`, `--refresh` |
| `expand` | Reveal a JSON Pointer from the cached payload | `--path`, `--limit`, `--depth`, `--fields`, `--out` |
| `paths` | Enumerate expandable pointers in a cached payload | `--prefix`, `--reason`, `--field`, `--omitted-only`, `--expandable-only`, `--limit`, `--depth` |
| `observe` | Ingest a file/stdin as a bounded observation | `--file`, `--stdin`, `--mime`, `--name`, `--lens`, `--ttl-seconds` |
| `run` | Run a one-shot command as a bounded observation | `--timeout-ms`, `--max-stdout-bytes`, `--max-stderr-bytes`, `--ttl-seconds`, `--preserve-exit-code`, `--out`, `--lens` |
| `cost` | Estimate token economics of a workflow | `--model-profile`, `--raw-file`, `--mime`, `--expand-path`, `--estimated-output-tokens`, `--repeated-inspections` |
| `cache list` / `cache get` / `cache purge` | Inspect and manage the local cache | `purge`: `--source`, `--expired`, `--all` |
| `meta` | Introspect prog's own contracts | — |
| `init` | Install agent skill + hooks | `--agent`, `--project`, `--dry-run`, `--root` |

**Token heuristic:** bytes / 4, rounded up. **Raw cost** = the full payload entering context; **prog cost** = the sum of every bounded envelope and expansion stdout consumed for a task.

---

## Token economics

Across HTTP, CLI, and MCP sources, prog reduces the tokens an agent must hold versus consuming the raw payload:

| Source type | Discover | Count | Target |
| --- | --- | --- | --- |
| HTTP | **162.8x** | 36.1x | 119.3x |
| CLI  | 153.9x | 35.2x | 109.9x |
| MCP  | 148.9x | 34.5x | 104.4x |

Aggregate range: **34.5x-162.8x**, with every envelope held under the 16 KiB invariant. Methodology and reproduction steps live in [`docs/token-economics.md`](docs/token-economics.md) and [`docs/evidence.md`](docs/evidence.md); regenerate with:

```sh
PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture
```

---

## Documentation

| Topic | Document |
| --- | --- |
| First-time walkthroughs | [`docs/walkthroughs.md`](docs/walkthroughs.md) |
| Adding HTTP / CLI sources | [`docs/source-setup.md`](docs/source-setup.md) |
| Cache lifecycle | [`docs/cache.md`](docs/cache.md) |
| Safety and trust model | [`docs/safety.md`](docs/safety.md) |
| Envelope and schema contracts | [`docs/contracts.md`](docs/contracts.md) |
| Reflexivity via `meta` | [`docs/metadata.md`](docs/metadata.md) |
| Observation lenses | [`docs/lenses.md`](docs/lenses.md) · [`docs/lens-packs.md`](docs/lens-packs.md) |
| `observe` reference | [`docs/observe.md`](docs/observe.md) |
| `run` reference | [`docs/run.md`](docs/run.md) |
| Findings ranking | [`docs/findings.md`](docs/findings.md) |
| Expandable pointers | [`docs/paths.md`](docs/paths.md) |
| Cost modeling | [`docs/cost.md`](docs/cost.md) |
| Agent integration model | [`docs/integrations.md`](docs/integrations.md) |
| Real-world demos | [`docs/real-world-demos.md`](docs/real-world-demos.md) |
| Positioning & baselines | [`docs/positioning.md`](docs/positioning.md) · [`docs/competitive-baselines.md`](docs/competitive-baselines.md) |
| Task-success evaluation | [`docs/task-success-eval.md`](docs/task-success-eval.md) |
| RFCs | [`0001`](docs/rfcs/0001-progressive-disclosure-gateway.md) · [`0002`](docs/rfcs/0002-type-theory-formal-methods-and-reflexivity.md) · [`0003`](docs/rfcs/0003-observation-lenses.md) |
| Invariants & changelog | [`INVARIANTS.md`](INVARIANTS.md) · [`CHANGELOG.md`](CHANGELOG.md) |

---

## Recently shipped

Former V1 non-goals, now implemented:

- **[#69](https://github.com/aphoristicartist/prog/issues/69) — upstream auto-pagination.** `prog call --pages N` follows cursor/page pagination for read-only operations under hard page/byte/time caps.
- **[#70](https://github.com/aphoristicartist/prog/issues/70) — table inference.** `prog observe` recognizes CSV/TSV, markdown, and aligned tables and exposes them as bounded, `/rows`-expandable payloads.
- **[#72](https://github.com/aphoristicartist/prog/issues/72) — automatic trust upgrade from imported descriptors.** A graded evidence model lets proven read-only importer evidence relax confirmation under `trust.auto_upgrade`.
- **[#73](https://github.com/aphoristicartist/prog/issues/73) — secrets embedded in string values** are redacted before persistence (Bearer/PEM/JWT and sensitive URL params).

## Roadmap

Remaining follow-ups from the above:

- Pagination URL continuation (Link `rel="next"` via an adapter `execute_url`) and resume cursors.
- Table inference for common log formats (e.g. CLF).
- Importer evidence-grading wiring that activates `trust.auto_upgrade` for real imports.
- Redaction value-scan keyword tuning via `RedactionConfig`.

## Non-goals

- **No MCP server mode.** prog is driven via CLI + skill + hooks; becoming an MCP server would add ceremony without capability. Permanent — see [#71](https://github.com/aphoristicartist/prog/issues/71).
- **Not a general-purpose HTTP cache or proxy.** prog bounds and shapes tool output for agent context; it is not a transparent traffic cache.
- **No bespoke UI.** prog is a CLI surface intended for agents and scripting; richer views come from whatever consumes the envelopes.

---

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- --help
```

---

prog keeps the first view small, makes the rest reachable, and never asks you to re-fetch what you already have.

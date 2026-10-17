# prog agent reference

The complete agent guidance that the default Agent Skill (`skills/prog/SKILL.md`)
links to. The skill itself stays small: with PATH shims and receipts, the
default loop needs only `prog evidence`. Everything below remains supported.

`prog` is an agent-facing transport, not an interactive terminal workflow for
humans. Use it inside the harness whenever tool output, files, command logs,
API responses, or MCP results are too large, noisy, expensive to rerun, or need
exact evidence.

Prefer this loop:

```text
observe/call/run -> overview + findings -> select/search/inspect -> evidence exact path -> answer with EvidenceRefs
```

## Harness Bootstrap

- Run `prog harness install --root <project>` to install the portable Agent
  Skill plus detected host adapters. Use `--dry-run` before writing when project
  ownership is uncertain.
- Run `prog harness doctor --root <project>` after installation or a `prog`
  upgrade. Do not claim the integration is ready unless it returns `ready:
  true`.
- Prefer a native post-tool-result plugin when the harness can replace an
  immutable result without rerunning the tool. Otherwise author the generated
  exact-argv wrapper explicitly.
- Never reconstruct argv by parsing a shell command string. Unsupported TTY,
  streaming, nested, or shell-structural calls pass through unchanged.

## Commands

- Use `prog run -- <command...>` for noisy commands such as test suites, build
  logs, package managers, and `gh api` calls.
- Use `prog observe --file <path>` or `prog observe --stdin` for raw JSON,
  NDJSON, text, logs, and saved tool output.
- Use `prog call <source> <operation> --args <json>` only when a source profile
  exists and the operation passes safety gates.
- Follow the envelope's top findings first. Use `prog inspect <cursor> --goal
  <goal>` when the task needs goal-directed ranking.
- For `next_actions`, execute direct `argv` as an argv array. For a
  cursor-backed action, select `action_templates[action.kind]`, replace
  `{cursor}` from the response and `{path}` from the action, and execute the
  resolved array without shell parsing. `scope: cached_evidence` is offline.
- Use `prog search <cursor> <query>` for a known clue and `prog find <cursor>
  --kind error|warning|test_failure` for structural evidence.
- Use `prog evidence <cursor> --path <json-pointer>` for a compact citation
  block. Use `prog expand` only when the evidence excerpt is insufficient.
- Use `prog recipe <name> -- <argv...>` for `cargo-test`, `pytest`, `npm-test`,
  `go-test`, `vitest`, `playwright`, `bun-test`, `deno-test`, `ruff`, `biome`,
  `semgrep`, `gh-issues`, `diff-review`, and `logs-root-cause`. Modern ones
  observe a temporary JUnit/SARIF report and expose exact argv.
- Wrappers stay argv: `pytest -- uv run pytest -q` and `npm-test -- pnpm test`.
- Use `prog cost` and `prog cache` to inspect stored bytes and retention when a
  session has accumulated many observations.

## Moving Through Captured Data

A capture is immutable and every part of it has a JSON Pointer, so the cursor
works like a zipper: a small focus, plus moves that return exactly the part
asked for. The same cursor always returns the same bytes.

- `prog run -- <cmd>` parses stdout that is one JSON document or JSON Lines
  into `/stdout/json` (test, build, and lint runners excepted: their output
  stays verification evidence). `prog observe --file` does the same for files.
- When a collection is only partly shown, the envelope carries an `overview`:
  each field's kinds, how many records carry it, every value with its count
  when there are few (`state: open=197, closed=103`), number and timestamp
  ranges, and `key_candidates` — fields unique in every record. Counting
  questions are often answered here without another call.
- `prog select <cursor> --where state=open --where 'labels~bug' --fields
  number,title,user/login --limit 20` filters (`=`, `!=`, `~` substring),
  projects, and pages (`--offset`, `next_offset`). Each row carries its path,
  so `prog evidence <cursor> --path <row path>/body` returns to it. A record
  whose filter field is redacted is counted as `unknown`, never as matching.
- `prog expand <cursor> --path <pointer> --limit N` reveals one slice; for a
  long string, `--limit` is its character count. `prog search` previews show
  the window around the match.
- Capture the same command, file, or call again and the envelope's
  `changes_since.records` lists records added, removed, and changed (with
  before and after values). `prog delta <old> <new> [--key id]` returns all of
  them; see [delta](delta.md#record-deltas).
- `prog coverage <cursor>` reports which records of the collection prog has
  returned this session and the index ranges it has not. Use it before
  claiming every record was reviewed.
- `--format compact` (or `PROG_FORMAT=compact`; the Claude Code plugin sets
  it) keeps the data, the moves, and every caveat — truncation, redaction,
  staleness, incomplete capture, warnings — and drops budgets, verdicts,
  provenance, and templates. `--format full` is the published contract.

## Verifying a Fix

Do not conclude that a problem is fixed because the rerun output looks clean.
A narrower rerun and a real fix produce the same absence.

- Capture the baseline and the verification run with the **same** invocation and
  the same `--comparison-family`, `--selection-scope`, and
  `--selection-exhaustive` flags. These cannot be added retroactively.
- Run `prog delta <baseline-observation> <subject-observation>` to compare.
- Trust only `resolved`. Treat `not_observed` and `unknown` as "did not verify",
  and read `assessment.reasons` to learn what was missing.
- For an explicit gate, declare the criterion before you have the result with
  `prog session obligation-add <id> --check ... --scope ... --origin-observation
  ... --expected-absent-fingerprint ... --evidence-observation ...`, then read
  `prog session show --readiness`.
- For an external API or MCP mutation, capture the entity first, run `prog
  verification begin --pre-observation ... --read-args ... --identity-path ...
  --version-path ... --expected ...`, execute the mutation outside `prog`, then
  run `prog verification readback <intent-id>`. Only `verified` passes; do not
  reinterpret `mismatched`, `stale_precondition`, `readback_failed`, or
  `unverifiable` as success.
- `ready` is true only when every required obligation passed. `configured:
  false` means nothing was declared — that is not a pass. Obligations are
  immutable, so declare them with the evidence observation attached.
- Report `persisting`, `new`, `stale`, and `unverifiable` to the user honestly
  rather than restating them as success.

## Long-Running MCP Tasks

When an MCP tool returns a task reference instead of a result, use
`prog mcp-task start|get|result|cancel <source> ...`. Each step records its own
observation, and an unreachable task is preserved as unavailable evidence rather
than reported as an empty result.

## Source Profiles

- Run `prog hints <source>` before calling a known source.
- For a new source, run
  `prog discover <source> --kind http|cli|mcp --seed <path-or-json>`.
- Do not add `--probe` by default; probing is an explicit read-only decision.
- Use `--refresh` when staleness warnings appear and freshness matters.
- Use `--yes` only after telling the user a mutation is about to happen.
- Run `prog meta` or `prog meta <ContractName>` for contract details instead
  of guessing envelope fields.

Shell-backed operations require explicit profile trust. Respect warnings about
mutation, shell execution, secrets, stale cache, and non-cacheable results.

## EvidenceRefs

When a conclusion depends on a specific expansion, cite it with the cursor and
JSON pointer:

```text
EvidenceRef: prog://pc1_...#/failure_sections/0
EvidenceRef: prog://pc1_...#/stderr/text
```

Do not cite the bounded preview as if it were the whole artifact when omissions
are present. Expand the exact path first.

## Safety

- Do not paste raw large payloads into model context by default.
- Do not bypass `prog` safety gates for mutating or shell-backed profile calls.
- Treat redacted fields as unavailable evidence.
- Prefer `--out <file>` when bulk post-processing is needed outside model
  context.
- Rerun the original command or call only when freshness matters or the cursor
  expired.

## Hook Usage

Project-local hooks installed by `prog init --agent <agent> --project` are
explicit argv wrappers. They may wrap the identical argv for capture, but never
perform semantic substitution or narrow the requested command:

```bash
<agent-dir>/prog-hooks/prog-run.sh cargo test
```

The wrapper uses `prog route`: progressive commands return a normal
`DisclosureEnvelope`; raw, passthrough, unknown, TTY/streaming, and shell-
structural commands run directly. Check `disclosure_verdict`
first. When it is `raw_cheaper`, use direct raw output on the next iteration;
the current envelope remains available for cached `prog inspect` and `prog
evidence` retrieval. Otherwise, follow its findings and retrieve exact cached
evidence as needed.

## MCP Stance

MCP is optional compatibility. Prefer the CLI, this skill, and explicit hooks as
the durable contract. Use MCP only when the host agent already speaks MCP well
and it preserves the same safety gates, cache semantics, cursor expansion, and
redaction behavior as the CLI. `prog` has no MCP server mode in the first
release, so an MCP-only host with no CLI, skill, or hook capability cannot use
it. Do not invent a bridge: #216 defers any facade-only transport until #120's
three-operation facade exists and has measured schema cost.

## Counterexamples

Do not use `prog` when a known `jq` query is enough, the user needs live
interactive streaming, the command requires a TTY, or the upstream API already
returns exactly the needed fields. For payload size, follow the envelope's
machine-readable `disclosure_verdict`: route the next iteration to raw output
when it reports `raw_cheaper`.

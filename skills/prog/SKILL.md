---
name: prog
description: Use prog to turn large API, CLI, MCP, log, and artifact results into bounded, cursor-backed observations before reasoning over them.
---

# prog Observation Workflow

Use `prog` when tool output, files, command logs, API responses, or MCP results
are too large, noisy, expensive to rerun, or need exact evidence.

Prefer this loop:

```text
observe/call/run -> ranked findings -> inspect/search -> evidence exact path -> answer with EvidenceRefs
```

## Commands

- Use `prog run -- <command...>` for noisy commands such as test suites, build
  logs, package managers, and `gh api` calls.
- Use `prog observe --file <path>` or `prog observe --stdin` for raw JSON,
  NDJSON, text, logs, and saved tool output.
- Use `prog call <source> <operation> --args <json>` only when a source profile
  exists and the operation passes safety gates.
- Follow the envelope's top findings first. Use `prog inspect <cursor> --goal
  <goal>` when the task needs goal-directed ranking.
- Use `prog search <cursor> <query>` for a known clue and `prog find <cursor>
  --kind error|warning|test_failure` for structural evidence.
- Use `prog evidence <cursor> --path <json-pointer>` for a compact citation
  block. Use `prog expand` only when the evidence excerpt is insufficient.
- Use `prog recipe <name> -- <command...>` for known domains (`cargo-test`,
  `pytest`, `npm-test`, `go-test`, `gh-issues`, `diff-review`,
  `logs-root-cause`); it applies a lens and default goal in one step.
- Use `prog cost` and `prog cache` to inspect stored bytes and retention when a
  session has accumulated many observations.

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
explicit wrappers, not hidden command rewrites:

```bash
<agent-dir>/prog-hooks/prog-run.sh cargo test
```

The wrapper returns a normal `DisclosureEnvelope`. Follow its findings, then use
`prog inspect` and `prog evidence` for exact cached evidence.

## MCP Stance

MCP is optional compatibility. Prefer the CLI, this skill, and explicit hooks as
the durable contract. Use MCP only when the host agent already speaks MCP well
and it preserves the same safety gates, cache semantics, cursor expansion, and
redaction behavior as the CLI.

## Counterexamples

Do not use `prog` when the payload is tiny, a known `jq` query is enough, the
user needs live interactive streaming, the command requires a TTY, or the
upstream API already returns exactly the needed fields.

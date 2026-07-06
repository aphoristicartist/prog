---
name: prog
description: Use prog to turn large API, CLI, MCP, log, and artifact results into bounded, cursor-backed observations before reasoning over them.
---

# prog Observation Workflow

Use `prog` when tool output, files, command logs, API responses, or MCP results
are too large, noisy, expensive to rerun, or need exact evidence.

Prefer this loop:

```text
observe/call/run -> inspect envelope -> paths -> expand exact evidence -> answer with EvidenceRefs
```

## Commands

- Use `prog run -- <command...>` for noisy commands such as test suites, build
  logs, package managers, and `gh api` calls.
- Use `prog observe --file <path>` or `prog observe --stdin` for raw JSON,
  NDJSON, text, logs, and saved tool output.
- Use `prog call <source> <operation> --args <json>` only when a source profile
  exists and the operation passes safety gates.
- Use `prog paths <cursor>` before guessing what to expand.
- Use `prog expand <cursor> --path <json-pointer>` to fetch exact evidence.

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

Project-local hooks installed by `prog init --agent codex --project` are explicit
wrappers, not hidden command rewrites:

```bash
.codex/prog-hooks/prog-run.sh cargo test
```

The wrapper returns a normal `DisclosureEnvelope`. Inspect `cursor`, then use
`prog paths` and `prog expand`.

## MCP Stance

MCP is optional compatibility. Prefer the CLI, this skill, and explicit hooks as
the durable contract. Use MCP only when the host agent already speaks MCP well
and it preserves the same safety gates, cache semantics, cursor expansion, and
redaction behavior as the CLI.

## Counterexamples

Do not use `prog` when the payload is tiny, a known `jq` query is enough, the
user needs live interactive streaming, the command requires a TTY, or the
upstream API already returns exactly the needed fields.

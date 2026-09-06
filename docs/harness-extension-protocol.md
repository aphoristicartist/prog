# Harness extension protocol

`prog` is an agent-harness extension. The `prog` executable is its local,
machine-readable transport and recovery surface; it is not an interactive user
interface and does not own an agent loop.

This document is the normative contract from which host-specific plugins,
skills, and wrappers derive. Wrapper formats are discovery mechanisms. They do
not get separate disclosure, redaction, storage, ranking, or verification
implementations.

## Result lifecycle

```text
host executes tool exactly once
  -> immutable tool result
  -> prog capture adapter
  -> redaction before persistence
  -> bounded DisclosureEnvelope or original result
  -> cached inspect/search/evidence
```

An adapter must integrate at the latest lossless point before a result becomes
model context. A native post-result plugin is preferred because it observes the
actual outcome and cannot duplicate side effects. Pre-execution wrapping is a
fallback only when the host supplies an exact argv vector or the agent authors
the generated wrapper explicitly.

## Required adapter behavior

Every adapter must:

1. Execute an upstream tool no more than once.
2. Preserve the host's tool identity, cwd, environment policy, status, signal,
   timeout, cancellation, stdout, and stderr whenever the host exposes them.
3. Send captured bytes through the same `RawPayload -> RedactedPayload ->
   PersistedPayload` boundary as the CLI.
4. Return the original result when it is cheaper and no redaction requires a
   replacement.
5. Replace an oversized result only with a byte-bounded
   `prog.disclosure` envelope containing a reusable cursor.
6. Fail open to an already-successful original result if `prog` is unavailable
   or capture fails. It must never rerun the tool as fallback.
7. Pass through TTY, interactive, streaming, shell-structural, nested `prog`,
   non-text, and otherwise unsupported results unless a host-specific fixture
   proves lossless behavior.
8. Never parse a shell command string back into argv.

The redaction exception to rule 4 is intentional: when a supported secret is
removed, the redacted envelope replaces the original even if its JSON is
larger. Content replacement is not a complete confidentiality boundary unless
the host also prevents programmatic consumers from retaining the original
value; adapters must describe that distinction honestly.

## Capability levels

| Level | Host capability | Integration |
| --- | --- | --- |
| A | Immutable post-result content can be replaced | Native result plugin; capture without rerun |
| B | Exact argv can be wrapped before execution | `prog run -- <argv>` |
| C | Agent can author argv but hooks expose shell strings | Skill plus explicit `prog-run.sh` |
| D | Instructions only | Portable Agent Skill; explicit `prog` calls |
| E | MCP-only, no local command or result hook | Unsupported until the measured facade gate is satisfied |

Adapters must declare the highest level their current, tested host contract
supports. They may not infer a stronger level from similarly named hook events.

## Canonical agent operations

The stable agent-facing workflow has three conceptual operations:

- **observe** — capture a command, artifact, or registered source into a bounded
  observation;
- **evidence** — retrieve exact cached paths or perform bounded cached search;
- **status** — report comparison, validity, availability, and verification
  readiness.

These compose the existing CLI contracts. They are not a second engine. The
advanced commands remain available to agents for debugging and recovery.

The [experimental registered host facade](registered-host-facade.md) exposes
these operations as opt-in DeepSeek Harness tools. Unlike an immutable-result
hook, a directly requested capture has no earlier successful host result to
restore: acquisition failures return errors without retrying the source.
Unknown/TTY/streaming inputs are rejected before launch. Canonical source
failures remain recoverable observations, with their original failure facts.
The owner must explicitly enable the layer; the experiment is not yet the
canonical agent surface.

## Integration context budget

CI gates the available integration surface: the top-level help, one help
response for every immediate command, and the portable Agent Skill. The
current reviewed ceiling is 34,000 bytes. Nested recovery-command help and
model-visible tool responses are not hidden from accounting; they occur only
when invoked and are measured by the actual-agent evaluation instead.

Host trials must record the instructions, schemas, help responses, and tool
results actually delivered in each arm. Available help text is counted in the
trial only when delivered. A proposed facade must report its fixed
schema/instruction cost and total task context against that measured baseline.

The [installed coding-loop smoke](installed-coding-loop.md) validates a real
failure-to-verification sequence through the shipped skill/CLI workflow and
records command and exported-file bytes. It provides a reproducible workflow
for the registered-host comparison; actual-agent outcomes require separate
measurement.

## Installation contract

`prog harness install` always installs the portable `agent-skills` target and
adds host adapters detected from project directories or executables.
`--host` selects an explicit repeatable set. Generated paths are deduplicated,
existing files are not overwritten, and conflicting manifests fail closed.

`prog harness doctor` compares every selected artifact with the running `prog`
version, verifies executable permissions, and returns nonzero unless `ready` is
true. A modified file is not silently accepted as compatible.

The legacy `prog init --agent ... --project` command remains a single-host
compatibility alias.

## Shipped formats

- `plugins/prog`: Codex marketplace plugin with the canonical Agent Skill and
  exact-argv wrapper.
- `extensions/deepseek-harness`: DeepSeek Harness `tools/post-execute` plugin
  and `dsh.bundle.patch` package.
- `.agents/skills/prog`: portable Agent Skills discovery for Codex, Copilot,
  VS Code, OpenCode, and compatible harnesses.
- `.claude`, `.gemini`, `.cursor`, and external manifest targets generated by
  `prog harness install`.

## Verification fixtures

A native result adapter is releasable only when fixtures cover small pass-
through, bounded replacement, redaction-forced replacement, invalid output,
missing binary, timeout, non-text content, nested calls, and preservation of an
already-successful original result. Exact-argv wrappers additionally cover cwd,
environment, quoting, exit status, signal, timeout, cancellation, streaming,
and pre-execution fallback.

The DeepSeek native adapter additionally exercises inherited stdout/stderr
after a capture parent exits, late valid output, incomplete stdin delivery,
overflow on either output stream,
pre-aborted signals, short-lived descendants, and descendants outside its
capture process group. Timeout and cancellation are terminal capture failures;
closing local pipes does not wait for an escaped descendant. Separate guarded
host processes verify natural exit after success or fallback, and Linux/macOS
CI runs these fixtures with the real-binary capture/evidence smoke.

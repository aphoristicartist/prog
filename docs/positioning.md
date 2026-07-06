# Positioning

`prog` is not a generic compressor and should not be sold as smarter
truncation. It is useful when an agent has a large or messy tool result before
it knows which parts matter.

The core bet is:

```text
capture once -> redact -> bounded view -> paths -> exact expansion
```

This is strongest for expensive, flaky, slow, private, or hard-to-repeat
observations where conclusions need cursor/path-backed evidence.

## Use prog When

- the result is too large to inspect safely in one model turn
- the query is not known before the first observation
- the source is slow, flaky, rate-limited, or expensive to rerun
- the agent needs exact evidence refs instead of summaries
- logs or tool outputs contain secrets that must be redacted before persistence
- repeated inspections should hit a local cursor-backed cache
- expensive models should reason over selected evidence, not raw dumps

## Do Not Use prog When

- the payload is tiny and the envelope would be larger than the raw data
- the API already returns exactly the needed fields
- pagination is the correct product behavior and fetching all pages is not
  needed
- a known `jq` query or domain-specific command directly extracts the answer
- the user needs an interactive TTY or live streaming terminal output
- a low-cost local model makes latency more important than input-token savings
- one expansion would reveal nearly the whole artifact anyway

## Comparison Matrix

| Alternative | Better When | Weakness | prog Position |
|---|---|---|---|
| Native API field selection | The needed fields are known upfront | Does not help exploratory inspection before the query is known | Prefer native filters first; use `prog` after capture when structure is unknown or repeated evidence is needed |
| Native pagination | The workflow naturally follows pages one at a time | Auto-fetching all pages can become an unbounded cost trap | `prog` records pagination hints but does not auto-follow upstream pagination in V1 |
| `jq` and shell filters | The extraction path is known and deterministic | Easy to drop needed context or leak raw secrets into history/context | Use `jq` for known extraction; use `prog paths` when the path is not known yet |
| Domain-specific CLI commands | The tool has a precise subcommand for the task | Often still emits huge logs/errors around the useful part | Prefer precise commands; wrap noisy results with `prog run` when output is still large |
| Simple truncation | A rough first glance is enough | Drops data without a recoverable path to exact evidence | `prog` bounds the first view but keeps cursor-backed expansion |
| RTK-style command interception | Low-friction terminal adoption is the main goal | Filters can be command-specific and lossy unless backed by recoverable storage | `prog` copies the hook ergonomics but keeps redacted payloads expandable |
| MCP gateways/proxies | The host agent already speaks MCP and needs tool-catalog integration | MCP does not guarantee result-side progressive disclosure | MCP can be an adapter later; CLI + skill + hooks remain the durable contract |
| Terse-output prompting | Assistant responses are too verbose | Does not reduce oversized tool-result input | Use terse responses on top of `prog`, not instead of result-side disclosure |
| Large context windows | Raw completeness matters more than cost/privacy/noise | Cost and attention still scale with raw input | Use raw context when it is worth it; use `prog cost` to quantify the tradeoff |

## Examples Where prog Should Lose

Native field selection is better when the answer is known upfront:

```bash
gh api repos/OWNER/REPO/issues --jq '.[].title'
```

`jq` is better when the path is known and the payload is local:

```bash
jq -r '.items[42].body' payload.json
```

A domain command is better when it emits exactly the needed fact:

```bash
git rev-parse HEAD
```

Raw output is better when the user explicitly needs a live stream:

```bash
npm run dev
```

## Scoped Claims

The README token-savings range is measured only on checked-in fixture evals.
It is not a universal promise. For a new workflow, use:

```bash
prog cost --model-profile models/fable-class-2026-07.json --raw-file payload.json
```

Then inspect whether the report shows meaningful savings for the actual model
profile, expected output size, expansions, and repeated-inspection count.

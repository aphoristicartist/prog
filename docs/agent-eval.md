# Actual-agent evaluation

Current status: **not claim-eligible**. This checked-in replay validates the graders; it is not an actual-agent A/B result.

Regenerate with PROG_AGENT_EVAL_BLESS=1 cargo test -p prog-cli --test agent_eval -- --nocapture.

| Measure | Value |
|---|---:|
| Actual agent trials | 0 |
| Synthetic replay traces | 4 |
| Adversarial false claims rejected | 2 |
| False completions accepted | 0 |
| prog skill bytes / estimated tokens | 7654 / 1914 |

The replay executes a narrowed coding rerun and an expired-validator entity read-back through the real CLI. Evidence citations are resolved from retained cursors and their redacted-slice SHA-256 values are checked. Deliberately false verified decisions are hard failures.

Raw, equal-budget truncation, native-selector, and prog performance remain unavailable because no credentialed model trials have been run. Tool-schema token counts also remain unavailable rather than being fabricated from bytes. A first-release performance claim requires multiple live trials for at least these coding and entity workflows, with provider/model/version, settings, date, region, total provider-reported token fields, dropouts, and uncertainty recorded.

## Live-trial claim gate

`fixtures/agent-eval/metrics.json` reserves the live-trial contract without
inventing results:

- every `live_trials` record identifies workflow, arm, provider, model/version,
  harness version, timestamp, region when relevant, trial seed when supported,
  settings, calls, upstream reruns, response bytes, and latency;
- token accounting keeps provider-reported input/output tokens separate from
  fixed system-prompt, tool-schema, skill, and model-visible tool-response token
  fields. Missing provider fields remain `null`;
- cached-input and reasoning-token fields are optional because providers do not
  expose them uniformly. Their absence does not cause an estimate to be
  fabricated;
- dropouts and all five replay graders, including `no_false_completion`, remain
  explicit per trial;
- `uncertainty` records its method, confidence level, trial count, and ordered
  intervals per workflow/arm/metric.

A report can set `claim_eligible: true` only when all raw,
equal-budget-truncation, native-selector, and prog cells for both reference
workflows have more than one completed trial; provider/model/harness/time
metadata and required token fields are present; no trial claims a false
completion; and uncertainty covers both the north-star efficiency metric and
false-completion count for every cell. Negative or mixed outcomes can still be
claim-eligible when their accounting and uncertainty are complete.

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

## Public-benchmark reporting dry run

Every result in this section is synthetic and exists only to exercise the reporting and claim-gate path before credentialed spend. The per-trial source is [`fixtures/agent-eval/public-benchmark-dry-run.json`](../fixtures/agent-eval/public-benchmark-dry-run.json). These values are not performance evidence. Wilson denominators include only trials with an official result; attempted N and dropouts are shown separately.

### terminal-bench-2.0 (synthetic)

Harness: harbor 0.22.0. Model: synthetic-provider/synthetic-model fixture-v1. Date: 2026-08-24. Subset: 3 synthetic task pairs. Seed: `reporting-dry-run-terminal-v1`. N per arm: raw 3, prog 3.

| Arm | Resolved (Wilson 95% CI) | False completion (Wilson 95% CI) | Dropouts |
|---|---:|---:|---:|
| raw | 2/2 (0.342–1.000) | 0/2 (0.000–0.658) | 1 |
| prog | 2/3 (0.208–0.939) | 1/3 (0.061–0.792) | 0 |

McNemar exact two-sided result: 2 complete pairs, raw-only 1, prog-only 0, p = 1.000. Interpretation: **negative paired result for prog**. Claim eligible: **false**.

### swe-bench-verified (synthetic)

Harness: official-swebench-harness synthetic-fixture-v1. Model: synthetic-provider/synthetic-model fixture-v1. Date: 2026-08-24. Subset: 2 synthetic instance pairs. Seed: `reporting-dry-run-swebench-v1`. N per arm: raw 2, prog 2.

| Arm | Resolved (Wilson 95% CI) | False completion (Wilson 95% CI) | Dropouts |
|---|---:|---:|---:|
| raw | 1/2 (0.095–0.905) | 0/2 (0.000–0.658) | 0 |
| prog | 1/2 (0.095–0.905) | 0/2 (0.000–0.658) | 0 |

McNemar exact two-sided result: 2 complete pairs, raw-only 1, prog-only 1, p = 1.000. Interpretation: **null paired result**. Claim eligible: **false**. Only a relative arm effect may be interpreted; this report does not support an absolute or SOTA claim. Contamination caveat: SWE-bench Verified is likely represented in model training data; only same-instance relative arm effects may be interpreted.


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
- public-benchmark trials use the same `LiveTrial` contract and gate, adding
  only benchmark, task, resolved, claimed-success, and timeout fields;
- `uncertainty` records its method, confidence level, trial count, and ordered
  intervals per workflow/arm/metric.

A report can set `claim_eligible: true` only when all raw,
equal-budget-truncation, native-selector, and prog cells for both reference
workflows have more than one completed trial; provider/model/harness/time
metadata and required token fields are present; no trial claims a false
completion; and uncertainty covers both the north-star efficiency metric and
false-completion count for every cell. Negative or mixed outcomes can still be
claim-eligible when their accounting and uncertainty are complete. Public
benchmark cells apply the same checks to raw/prog arms with Wilson resolved and
false-completion intervals; incomplete usage in either arm keeps the report
claim-ineligible.

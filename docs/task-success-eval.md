# Task-success eval

This deterministic eval asks whether each strategy exposes the evidence needed to answer fixed tasks. It is not a model-quality benchmark; optional model-backed scoring should be gated separately.

Regenerate this report and the raw metrics with `PROG_TASK_EVAL_UPDATE=1 cargo test -p prog-cli --test task_success -- --nocapture`.

## Aggregate

| Strategy | Correct | Scenarios | Input tokens | Tool calls | Expansions | Cache hits |
|---|---:|---:|---:|---:|---:|---:|
| raw | 13 | 13 | 790078 | 0 | 0 | 0 |
| simple_truncation | 1 | 13 | 12302 | 0 | 0 | 0 |
| jq_field_selection | 11 | 13 | 1068 | 13 | 0 | 0 |
| rtk_grep_filter | 13 | 13 | 732819 | 13 | 0 | 0 |
| prog_call_only | 1 | 13 | 21420 | 13 | 0 | 0 |
| prog_expand | 13 | 13 | 29532 | 26 | 13 | 13 |

## Scenarios

| Scenario | Artifact | Evidence path | Counterexample |
|---|---|---|---:|
| cli-body-42 | CLI | `/items/42/body` | false |
| cli-lookup_code-128 | CLI | `/items/128/lookup_code` | false |
| cli-lookup_code-211 | CLI | `/items/211/lookup_code` | false |
| http-body-42 | HTTP | `/items/42/body` | false |
| http-lookup_code-128 | HTTP | `/items/128/lookup_code` | false |
| http-lookup_code-211 | HTTP | `/items/211/lookup_code` | false |
| mcp-body-42 | MCP | `/results/42/body` | false |
| mcp-lookup_code-128 | MCP | `/results/128/lookup_code` | false |
| mcp-lookup_code-211 | MCP | `/results/211/lookup_code` | false |
| observe-json-body-150 | Observed JSON | `/items/150/body` | false |
| observe-ndjson-message-170 | Observed NDJSON | `/records/170/message` | false |
| observe-text-line-180 | Observed Text | `/lines/180/text` | false |
| tiny-payload-counterexample | Tiny JSON | `/answer` | true |

## Counterexamples

The tiny payload scenario is intentionally included: raw context is correct and cheaper than a `prog` envelope plus expansion. This report should keep that loss visible.

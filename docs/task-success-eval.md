# Known-path recoverability eval

This deterministic suite supplies the exact lookup selector in every task and grades evidence availability after execution. It does not measure discovery or actual-agent task success. Real-agent outcomes require separate live trials. The historical filenames remain for compatibility.

The expected answer is private to grading. Line-search terms are derived from the public selector, never from the answer. Native JSON selection is unavailable for non-JSON artifacts. The raw fixture is supplied outside model context to every strategy; source setup is excluded. All actual strategy stdout, including initial capture and expansion, is counted. No model answer tokens are generated, and timings are local measurements rather than assumed jq/RTK latency.

Token counts approximate total response bytes/4, rounded up per task before aggregation.

Source: [`task-success-metrics.json`](../fixtures/evals/task-success-metrics.json). Refresh measurements with `PROG_TASK_EVAL_UPDATE=1 cargo test -p prog-cli --test task_success -- --nocapture`; render reviewed artifacts with `scripts/regenerate-eval-docs.sh --write` or check them with `--check`.

## Aggregate

| Strategy | Evidence available | Attempted | Unavailable | Response bytes | Approx. input tokens | Tool calls | Expansions | Cache hits |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| raw | 13 | 13 | 0 | 3160292 | 790078 | 0 | 0 | 0 |
| simple_truncation | 1 | 13 | 0 | 49205 | 12302 | 0 | 0 | 0 |
| native_json_selection | 11 | 11 | 2 | 4265 | 1069 | 11 | 0 | 0 |
| rtk_grep_filter | 13 | 13 | 0 | 3084708 | 771180 | 13 | 0 | 0 |
| prog_call_only | 1 | 13 | 0 | 87438 | 21866 | 13 | 0 | 6 |
| prog_expand | 13 | 13 | 0 | 138144 | 34539 | 26 | 13 | 22 |

## Scenarios

| Scenario | Artifact | Public lookup path | Counterexample |
|---|---|---|---:|
| http-body-42 | HTTP | `/items/42/body` | false |
| http-lookup_code-128 | HTTP | `/items/128/lookup_code` | false |
| http-lookup_code-211 | HTTP | `/items/211/lookup_code` | false |
| cli-body-42 | CLI | `/items/42/body` | false |
| cli-lookup_code-128 | CLI | `/items/128/lookup_code` | false |
| cli-lookup_code-211 | CLI | `/items/211/lookup_code` | false |
| mcp-body-42 | MCP | `/results/42/body` | false |
| mcp-lookup_code-128 | MCP | `/results/128/lookup_code` | false |
| mcp-lookup_code-211 | MCP | `/results/211/lookup_code` | false |
| observe-json-body-150 | Observed JSON | `/items/150/body` | false |
| observe-ndjson-message-170 | Observed NDJSON | `/records/170/message` | false |
| observe-text-line-180 | Observed Text | `/lines/180/text` | false |
| tiny-payload-counterexample | Tiny JSON | `/answer` | true |

## Counterexamples

The tiny payload scenario remains visible without a required cost ordering. Successful `prog_expand` rows prove that supplied paths are recoverable; they do not prove an agent discovered the path or solved the task.

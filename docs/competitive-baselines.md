# Competitive baselines

This is a deterministic evidence-availability experiment, not actual-agent task success. Known-path tasks publish their selector; unknown-target strategies receive only the task and artifact. The grader's path and answer are consulted only after execution. No strategy receives answer-derived grep terms.

All arms start from the same fixture artifact outside model context. Source-profile discovery/setup is excluded. Raw context discloses that artifact; filters transform it; file capture writes it once and returns a receipt before searching; prog returns a capture envelope before retrieval. Every strategy response (including exploration, receipt, retrieval, and repeated retrieval) contributes to response bytes and tool-call counts. These are disclosure costs, not end-to-end live acquisition or latency comparisons. Python implements the declared JSON/line filters with actual argv and stdout; it is not the RTK or jq executable.

Broad search uses the declared case-insensitive severity pattern `fatal|panic|error|failed|exception` with word boundaries. The narrow unknown-target grep assumes `ERROR`. File search uses the public known-task term or that broad pattern. Minified JSON can make line search return the entire artifact.

Unknown-target prog retrieval expands the first finding returned in the initial envelope, or calls `inspect --goal root_cause` if none is returned. With no candidate it stops; an incorrect candidate has no oracle fallback. Repeated retrieval uses the same observed path. Known-path retrieval is explicitly assisted recoverability.

Token counts approximate response bytes/4 rounded up. No model answers are generated: the separate assumed-answer tokens and illustrative Fable prices in JSON are hypothetical, never provider usage. Unavailable arms are excluded from attempted counts; no-answer and missed/unranked evidence remain insufficient rather than being counted as discovery successes.

Source: [`competitive-baseline-metrics.json`](../fixtures/evals/competitive-baseline-metrics.json). Refresh measurements with `PROG_BASELINE_EVAL_UPDATE=1 cargo test -p prog-cli --test competitive_baselines -- --nocapture`. Render reviewed artifacts with `scripts/regenerate-eval-docs.sh --write`; `--check` detects documentation drift without running measurements.

## known_path_recoverability

| Strategy | Evidence available | Attempted | Unavailable | Response bytes | Approx. input tokens | Tool calls |
|---|---:|---:|---:|---:|---:|---:|
| broad_log_search | 2 | 10 | 0 | 49863 | 12466 | 10 |
| caveman_terse_output | 10 | 10 | 0 | 1299777 | 324949 | 0 |
| file_capture_search | 10 | 10 | 0 | 1200169 | 300046 | 20 |
| head_tail_truncation | 1 | 10 | 0 | 36921 | 9231 | 0 |
| native_field_selection | 8 | 8 | 2 | 1688 | 423 | 8 |
| prog_envelope_only | 1 | 10 | 0 | 66639 | 16664 | 10 |
| prog_repeated_cache | 10 | 10 | 0 | 150512 | 37629 | 30 |
| prog_retrieve | 10 | 10 | 0 | 110560 | 27643 | 20 |
| raw_context | 10 | 10 | 0 | 1299777 | 324949 | 0 |
| rtk_grep_filter | 10 | 10 | 0 | 1199796 | 299952 | 10 |

## deterministic_discovery

| Strategy | Evidence available | Attempted | Unavailable | Response bytes | Approx. input tokens | Tool calls |
|---|---:|---:|---:|---:|---:|---:|
| broad_log_search | 2 | 4 | 0 | 12336 | 3085 | 4 |
| caveman_terse_output | 3 | 4 | 0 | 568816 | 142205 | 0 |
| file_capture_search | 2 | 4 | 0 | 12488 | 3123 | 8 |
| head_tail_truncation | 0 | 4 | 0 | 16384 | 4096 | 0 |
| native_field_selection | 0 | 0 | 4 | 0 | 0 | 0 |
| prog_envelope_only | 0 | 4 | 0 | 36917 | 9231 | 4 |
| prog_repeated_cache | 2 | 4 | 0 | 109981 | 27496 | 11 |
| prog_retrieve | 2 | 4 | 0 | 68964 | 17242 | 8 |
| raw_context | 3 | 4 | 0 | 568816 | 142205 | 0 |
| rtk_grep_filter | 0 | 4 | 0 | 12030 | 3009 | 4 |

## Unknown-target outcomes

| Scenario | Strategy | Outcome | Selected path | Response bytes |
|---|---|---|---|---:|
| unknown-target-buried-fatal | raw_context | evidence_available | `none` | 142376 |
| unknown-target-buried-fatal | head_tail_truncation | insufficient_evidence | `none` | 4096 |
| unknown-target-buried-fatal | native_field_selection | not_attempted | `none` | 0 |
| unknown-target-buried-fatal | rtk_grep_filter | insufficient_evidence | `none` | 4010 |
| unknown-target-buried-fatal | broad_log_search | evidence_available | `none` | 4164 |
| unknown-target-buried-fatal | file_capture_search | evidence_available | `none` | 4202 |
| unknown-target-buried-fatal | caveman_terse_output | evidence_available | `none` | 142376 |
| unknown-target-buried-fatal | prog_envelope_only | insufficient_evidence | `none` | 7010 |
| unknown-target-buried-fatal | prog_retrieve | evidence_available | `/lines/1200/text` | 20707 |
| unknown-target-buried-fatal | prog_repeated_cache | evidence_available | `/lines/1200/text` | 34404 |
| unknown-target-relocated-fatal | raw_context | evidence_available | `none` | 142376 |
| unknown-target-relocated-fatal | head_tail_truncation | insufficient_evidence | `none` | 4096 |
| unknown-target-relocated-fatal | native_field_selection | not_attempted | `none` | 0 |
| unknown-target-relocated-fatal | rtk_grep_filter | insufficient_evidence | `none` | 4010 |
| unknown-target-relocated-fatal | broad_log_search | evidence_available | `none` | 4162 |
| unknown-target-relocated-fatal | file_capture_search | evidence_available | `none` | 4200 |
| unknown-target-relocated-fatal | caveman_terse_output | evidence_available | `none` | 142376 |
| unknown-target-relocated-fatal | prog_envelope_only | insufficient_evidence | `none` | 7010 |
| unknown-target-relocated-fatal | prog_retrieve | evidence_available | `/lines/347/text` | 20702 |
| unknown-target-relocated-fatal | prog_repeated_cache | evidence_available | `/lines/347/text` | 34394 |
| unknown-target-unranked | raw_context | evidence_available | `none` | 142375 |
| unknown-target-unranked | head_tail_truncation | insufficient_evidence | `none` | 4096 |
| unknown-target-unranked | native_field_selection | not_attempted | `none` | 0 |
| unknown-target-unranked | rtk_grep_filter | insufficient_evidence | `none` | 4010 |
| unknown-target-unranked | broad_log_search | insufficient_evidence | `none` | 4010 |
| unknown-target-unranked | file_capture_search | insufficient_evidence | `none` | 4048 |
| unknown-target-unranked | caveman_terse_output | evidence_available | `none` | 142375 |
| unknown-target-unranked | prog_envelope_only | insufficient_evidence | `none` | 6999 |
| unknown-target-unranked | prog_retrieve | insufficient_evidence | `/head/0` | 20627 |
| unknown-target-unranked | prog_repeated_cache | insufficient_evidence | `/head/0` | 34255 |
| unknown-target-no-answer | raw_context | insufficient_evidence | `none` | 141689 |
| unknown-target-no-answer | head_tail_truncation | insufficient_evidence | `none` | 4096 |
| unknown-target-no-answer | native_field_selection | not_attempted | `none` | 0 |
| unknown-target-no-answer | rtk_grep_filter | insufficient_evidence | `none` | 0 |
| unknown-target-no-answer | broad_log_search | insufficient_evidence | `none` | 0 |
| unknown-target-no-answer | file_capture_search | insufficient_evidence | `none` | 38 |
| unknown-target-no-answer | caveman_terse_output | insufficient_evidence | `none` | 141689 |
| unknown-target-no-answer | prog_envelope_only | insufficient_evidence | `none` | 15898 |
| unknown-target-no-answer | prog_retrieve | insufficient_evidence | `none` | 6928 |
| unknown-target-no-answer | prog_repeated_cache | insufficient_evidence | `none` | 6928 |

## Known-path fixtures

| Scenario | Public selector |
|---|---|
| http-body-42 | `/items/42/body` |
| http-lookup_code-128 | `/items/128/lookup_code` |
| http-lookup_code-190 | `/items/190/lookup_code` |
| cli-body-42 | `/items/42/body` |
| cli-lookup_code-128 | `/items/128/lookup_code` |
| cli-lookup_code-190 | `/items/190/lookup_code` |
| log-line-180 | `/lines/180/text` |
| diff-added-sentinel | `/lines/100/text` |
| sarif-report-message | `/runs/0/results/90/message/text` |
| tiny-baseline-counterexample | `/answer` |

The tiny payload counterexample is retained. These tables impose no requirement that prog win a cost comparison. Broad search, file search, and raw input may cost less. Exact command traces and every response size are recorded in the generated JSON; traces are audit data, not additional strategy observations.

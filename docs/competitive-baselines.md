# Competitive baselines

This deterministic eval compares `prog` with raw context, truncation, native field selection, RTK-style filtering, Caveman-style terse output, and repeated cursor-backed cache use. Costs use the checked-in `models/fable-class-2026-07.json` illustrative price profile.

Regenerate this report and the raw metrics with `PROG_BASELINE_EVAL_UPDATE=1 cargo test -p prog-cli --test competitive_baselines -- --nocapture`.

## Aggregate

| Strategy | Correct | Scenarios | Input tokens | Output tokens | Tool calls | Expansions | Cache hits | Est. Fable cost |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| raw_context | 11 | 11 | 360543 | 704 | 0 | 0 | 0 | 3.640630 |
| head_tail_truncation | 1 | 11 | 10255 | 64 | 0 | 0 | 0 | 0.105750 |
| native_field_selection | 8 | 11 | 423 | 512 | 10 | 0 | 0 | 0.029830 |
| rtk_grep_filter | 10 | 11 | 300915 | 640 | 11 | 0 | 0 | 3.041150 |
| caveman_terse_output | 11 | 11 | 360543 | 88 | 0 | 0 | 0 | 3.609830 |
| prog_envelope_only | 1 | 11 | 23540 | 64 | 11 | 0 | 4 | 0.238600 |
| prog_paths_expand | 11 | 11 | 46643 | 704 | 33 | 11 | 22 | 0.501630 |
| prog_repeated_cache | 11 | 11 | 53035 | 704 | 33 | 22 | 22 | 0.565550 |

## Scenarios

| Scenario | Artifact | Evidence | Counterexample |
|---|---|---|---:|
| cli-body-42 | CLI | `/items/42/body` (JSON pointer /items/42/body) | false |
| cli-lookup_code-128 | CLI | `/items/128/lookup_code` (JSON pointer /items/128/lookup_code) | false |
| cli-lookup_code-190 | CLI | `/items/190/lookup_code` (JSON pointer /items/190/lookup_code) | false |
| diff-added-sentinel | Unified diff | `/lines/100/text` (diff line 100) | false |
| http-body-42 | HTTP API | `/items/42/body` (JSON pointer /items/42/body) | false |
| http-lookup_code-128 | HTTP API | `/items/128/lookup_code` (JSON pointer /items/128/lookup_code) | false |
| http-lookup_code-190 | HTTP API | `/items/190/lookup_code` (JSON pointer /items/190/lookup_code) | false |
| log-line-180 | Text log | `/lines/180/text` (line 180) | false |
| sarif-report-message | Structured report | `/runs/0/results/90/message/text` (JSON pointer /runs/0/results/90/message/text) | false |
| tiny-baseline-counterexample | Tiny JSON | `/answer` (JSON pointer /answer) | true |
| unknown-target-buried-fatal | Text log | `/lines/1200/text` (line 1201) | false |

## Wins, Losses, And Counterexamples

- Native field selection is the cheapest correct strategy when a JSON path is already known.
- RTK-style grep filtering wins on logs and diffs when the exact search term is known, but can return an entire minified JSON payload.
- Caveman-style terse output reduces answer tokens but leaves raw tool input cost unchanged.
- `prog_envelope_only` intentionally loses when the bounded first view hides required evidence.
- `prog_paths_expand` and `prog_repeated_cache` solve every scenario here, but the tiny payload counterexample is cheaper as raw context.

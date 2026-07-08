# Competitive baselines

This deterministic eval compares `prog` with raw context, truncation, native field selection, RTK-style filtering, Caveman-style terse output, and repeated cursor-backed cache use. Costs use the checked-in `models/fable-class-2026-07.json` illustrative price profile.

Regenerate this report and the raw metrics with `PROG_BASELINE_EVAL_UPDATE=1 cargo test -p prog-cli --test competitive_baselines -- --nocapture`.

## Aggregate

| Strategy | Correct | Scenarios | Input tokens | Output tokens | Tool calls | Expansions | Cache hits | Est. Fable cost |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| raw_context | 10 | 10 | 324949 | 640 | 0 | 0 | 0 | 3.281490 |
| head_tail_truncation | 1 | 10 | 9231 | 64 | 0 | 0 | 0 | 0.095510 |
| native_field_selection | 8 | 10 | 423 | 512 | 10 | 0 | 0 | 0.029830 |
| rtk_grep_filter | 10 | 10 | 299946 | 640 | 10 | 0 | 0 | 3.031460 |
| caveman_terse_output | 10 | 10 | 324949 | 80 | 0 | 0 | 0 | 3.253490 |
| prog_envelope_only | 1 | 10 | 16536 | 64 | 10 | 0 | 4 | 0.168560 |
| prog_paths_expand | 10 | 10 | 26933 | 640 | 30 | 10 | 20 | 0.301330 |
| prog_repeated_cache | 10 | 10 | 28235 | 640 | 30 | 20 | 20 | 0.314350 |

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

## Wins, Losses, And Counterexamples

- Native field selection is the cheapest correct strategy when a JSON path is already known.
- RTK-style grep filtering wins on logs and diffs when the exact search term is known, but can return an entire minified JSON payload.
- Caveman-style terse output reduces answer tokens but leaves raw tool input cost unchanged.
- `prog_envelope_only` intentionally loses when the bounded first view hides required evidence.
- `prog_paths_expand` and `prog_repeated_cache` solve every scenario here, but the tiny payload counterexample is cheaper as raw context.

## Pagination (auto-fetch under the envelope budget)

A separate, self-contained baseline (`crates/prog-cli/tests/competitive_baselines.rs::pagination_competitive_baseline_vs_raw_page_by_page`) compares `prog call --pages N` against raw page-by-page fetching over a 5-page cursor-paginated endpoint (each page ~1 KiB).

- **Correctness parity**: every page's evidence is recoverable through its own `pc1_` per-page cursor (surfaced in `envelope.pagination.pages[].cursor`), so the envelope budget never hides data — correctness matches raw.
- **Cost win**: `prog --pages` emits a single bounded envelope (page-1 preview + pagination metadata; pages N>=2 contribute only omitted-region counts), whose approx-token cost is strictly less than the raw concatenation of all page bodies. Raw page-by-page pays the full input cost of every page up front.

This rows is maintained as an executable assertion rather than a regenerated metric because the comparison is structural (cheaper-and-equally-correct), not a dollar figure.

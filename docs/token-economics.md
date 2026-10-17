# Token economics eval

Token counts use the project heuristic `bytes / 4`, rounded up. Raw cost is the full fixture payload entering context. prog cost is the sum of every bounded envelope or expansion stdout consumed for the task, including the initial call envelope before any expansion. This is not a latency benchmark or a model-success benchmark.

Every `DisclosureEnvelope` reports a `disclosure_verdict`. When original input cost is known, its ratio is `baseline.bytes / envelope_bytes`: below `1.0` is `raw_cheaper`, from `1.0` through less than `1.25` is `neutral`, and `1.25` or above is `bounded_win` (the envelope is at least 20 percent smaller). The displayed ratio is rounded down to hundredths (or a conservative lower value when its encoded width cycles), but classification uses exact integer byte counts. `baseline.basis` identifies original command streams counted once, file/stdin bytes, or decoded HTTP body bytes. Unknown original costs (including MCP SDK-normalized content and incomplete HTTP bodies) report `unavailable` with null baseline and ratio. `summary.payload_bytes` is normalized, redacted storage size and is never the source comparison baseline. `summary.envelope_bytes`, verdict bytes, and disclosure-budget actual bytes all count final stdout including budget metadata, formatting, and its trailing newline. The verdict reports cost; it does not automatically replace the envelope with raw output.

Source: [`token-economics-metrics.json`](../fixtures/evals/token-economics-metrics.json). Refresh measurements with `PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture`; render reviewed artifacts with `scripts/regenerate-eval-docs.sh --write` or check them with `--check`. Runtime cost gates remain separate from documentation consistency.

| Fixture | Task | Raw tokens | prog tokens | Ratio |
|---|---:|---:|---:|---:|
| HTTP | Discover shape | 137883 | 1736 | 79.4x |
| HTTP | Count states | 137883 | 5384 | 25.6x |
| HTTP | Target body | 137883 | 2648 | 52.1x |
| CLI | Discover shape | 137753 | 1833 | 75.2x |
| CLI | Count states | 137753 | 5562 | 24.8x |
| CLI | Target body | 137753 | 2826 | 48.7x |
| MCP | Discover shape | 137753 | 1999 | 68.9x |
| MCP | Count states | 137753 | 5766 | 23.9x |
| MCP | Target body | 137753 | 2995 | 46.0x |

# Token economics eval

Token counts use the project heuristic `bytes / 4`, rounded up. Raw cost is the full fixture payload entering context. prog cost is the sum of every bounded envelope or expansion stdout consumed for the task, including the initial call envelope before any expansion. This is not a latency benchmark or a model-success benchmark.

Every `DisclosureEnvelope` reports a `disclosure_verdict`. When original input cost is known, its ratio is `baseline.bytes / envelope_bytes`: below `1.0` is `raw_cheaper`, from `1.0` through less than `1.25` is `neutral`, and `1.25` or above is `bounded_win` (the envelope is at least 20 percent smaller). The displayed ratio is rounded down to hundredths (or a conservative lower value when its encoded width cycles), but classification uses exact integer byte counts. `baseline.basis` identifies original command streams counted once, file/stdin bytes, or decoded HTTP body bytes. Unknown original costs (including MCP SDK-normalized content and incomplete HTTP bodies) report `unavailable` with null baseline and ratio. `summary.payload_bytes` is normalized, redacted storage size and is never the source comparison baseline. `summary.envelope_bytes`, verdict bytes, and disclosure-budget actual bytes all count final stdout including budget metadata, formatting, and its trailing newline. The verdict reports cost; it does not automatically replace the envelope with raw output.

Regenerate this table with `PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture`.

| Fixture | Task | Raw tokens | prog tokens | Ratio |
|---|---:|---:|---:|---:|
| HTTP | Discover shape | 137883 | 1629 | 84.6x |
| HTTP | Count states | 137883 | 5277 | 26.1x |
| HTTP | Target body | 137883 | 2540 | 54.3x |
| CLI | Discover shape | 137753 | 1727 | 79.8x |
| CLI | Count states | 137753 | 5456 | 25.2x |
| CLI | Target body | 137753 | 2719 | 50.7x |
| MCP | Discover shape | 137753 | 1865 | 73.9x |
| MCP | Count states | 137753 | 5605 | 24.6x |
| MCP | Target body | 137753 | 2834 | 48.6x |

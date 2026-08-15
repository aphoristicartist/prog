# Token economics eval

Token counts use the project heuristic `bytes / 4`, rounded up. Raw cost is the full fixture payload entering context. prog cost is the sum of every bounded envelope or expansion stdout consumed for the task, including the initial call envelope before any expansion. This is not a latency benchmark or a model-success benchmark.

Every `DisclosureEnvelope` reports a `disclosure_verdict` using the same fixed thresholds for every capture kind. Its ratio is `payload_bytes / envelope_bytes`: below `1.0` is `raw_cheaper`, from `1.0` through less than `1.25` is `neutral`, and `1.25` or above is `bounded_win` (the envelope is at least 20 percent smaller). The displayed ratio is rounded down to two decimal places, but classification uses the exact byte counts. The verdict reports cost; it does not automatically replace the envelope with raw output.

Regenerate this table with `PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture`.

| Fixture | Task | Raw tokens | prog tokens | Ratio |
|---|---:|---:|---:|---:|
| HTTP | Discover shape | 137883 | 1618 | 85.2x |
| HTTP | Count states | 137883 | 5262 | 26.2x |
| HTTP | Target body | 137883 | 2526 | 54.6x |
| CLI | Discover shape | 137753 | 1714 | 80.4x |
| CLI | Count states | 137753 | 5440 | 25.3x |
| CLI | Target body | 137753 | 2704 | 50.9x |
| MCP | Discover shape | 137753 | 1876 | 73.4x |
| MCP | Count states | 137753 | 5650 | 24.4x |
| MCP | Target body | 137753 | 2879 | 47.8x |

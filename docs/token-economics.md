# Token economics eval

Token counts use the project heuristic `bytes / 4`, rounded up. Raw cost is the full fixture payload entering context. prog cost is the sum of every bounded envelope or expansion stdout consumed for the task, including the initial call envelope before any expansion. This is not a latency benchmark or a model-success benchmark.

Regenerate this table with `PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture`.

| Fixture | Task | Raw tokens | prog tokens | Ratio |
|---|---:|---:|---:|---:|
| HTTP | Discover shape | 137883 | 847 | 162.8x |
| HTTP | Count states | 137883 | 3820 | 36.1x |
| HTTP | Target body | 137883 | 1156 | 119.3x |
| CLI | Discover shape | 137753 | 895 | 153.9x |
| CLI | Count states | 137753 | 3917 | 35.2x |
| CLI | Target body | 137753 | 1254 | 109.9x |
| MCP | Discover shape | 137753 | 925 | 148.9x |
| MCP | Count states | 137753 | 3994 | 34.5x |
| MCP | Target body | 137753 | 1319 | 104.4x |

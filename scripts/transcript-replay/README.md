# Transcript replay

The scripts that produced the numbers in #311, #305, and #312. They read your
own local transcripts (`~/.claude/projects/**/*.jsonl`, files modified in the
last `DAYS` days, default 120; `~/.codex/sessions/**/*.jsonl`) and print
aggregate counts only. Nothing is sent anywhere.

```sh
python3 -I scripts/transcript-replay/gate_replay.py            # stop-gate replay
NOTPROVEN=strict python3 -I scripts/transcript-replay/gate_replay.py
python3 -I scripts/transcript-replay/coverage.py              # capture/judge share
```

`gate_replay.py` models the stop gate as it was on `18c8c15` from transcripts:
the default PATH shims and `classify_route`, `provider_kind`, invocation chains,
the newest 100 observations, and its three block rules, plus the variants
discussed in #311 and #305 (`newest_gate`, `proposed`, `305newest`). Two things
cannot be read from transcripts, so the counts are ranges: when a fail-to-pass
is NOT PROVEN (`NOTPROVEN=optimistic|strict`) and the pass/fail state of piped
output. These are heuristics, not prog's own verdicts; since #314, the decision
log (`prog harness log --summary`) measures the real gate in real use instead.

`ROWS=/path/rows.json` writes one row per turn end for hand-checking. Rows
contain message text from your transcripts: keep that file local.

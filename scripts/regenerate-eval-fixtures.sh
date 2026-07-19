#!/usr/bin/env sh
set -eu

# Refresh exact human-readable measurements after an intentional behavior
# change. Neither command raises ceilings: edit the named ceiling in the
# relevant fixture first when a reviewed cost increase is warranted.
# Correctness `checks` in replay-metrics.json are never ceiling-gated and
# cannot be blessed away — a failing one means fix the regression first.
PROG_BLESS=1 cargo test -p prog-cli --test evidence_acquisition
PROG_REPLAY_EVAL_BLESS=1 cargo test -p prog-cli --test replay_eval

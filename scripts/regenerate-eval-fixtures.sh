#!/usr/bin/env sh
set -eu

# Eval baselines are intentionally updated only through this explicit command.
PROG_EVIDENCE_EVAL_UPDATE=1 cargo test -p prog-cli --test evidence_acquisition

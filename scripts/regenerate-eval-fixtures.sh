#!/usr/bin/env sh
set -eu

# Refresh exact human-readable measurements after an intentional behavior
# change. This command never raises ceilings: edit the named ceiling in the
# fixture first when a reviewed cost increase is warranted.
PROG_BLESS=1 cargo test -p prog-cli --test evidence_acquisition

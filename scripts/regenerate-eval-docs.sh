#!/usr/bin/env sh
set -eu

# Render reviewed JSON artifacts; never run measurements or change ceilings.
cd "$(dirname "$0")/.."
case "${1:---check}" in
    --check) unset PROG_EVAL_DOCS_UPDATE ;;
    --write) export PROG_EVAL_DOCS_UPDATE=1 ;;
    *) echo 'usage: scripts/regenerate-eval-docs.sh [--check|--write]' >&2; exit 2 ;;
esac
if [ "$#" -gt 1 ]; then
    echo 'usage: scripts/regenerate-eval-docs.sh [--check|--write]' >&2
    exit 2
fi
cargo test -p prog-cli --test eval_docs sync_evaluation_documents -- --exact --nocapture

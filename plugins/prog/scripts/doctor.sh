#!/usr/bin/env sh
set -eu

if ! command -v prog >/dev/null 2>&1; then
  printf '%s\n' '{"schema":"prog.harness.doctor","ready":false,"blockers":["prog is not on PATH"]}'
  exit 1
fi

version=$(prog --version)
printf '{"schema":"prog.harness.doctor","ready":true,"prog":"%s"}\n' "$version"

#!/usr/bin/env sh
set -eu

if [ "$#" -eq 0 ]; then
  printf '%s\n' '{"schema":"prog.integration_error","reason":"missing_command"}' >&2
  exit 64
fi

if ! command -v prog >/dev/null 2>&1; then
  printf '%s\n' '{"schema":"prog.integration_fallback","reason":"prog_unavailable","action":"execute_authored_argv"}' >&2
  exec "$@"
fi

if ! route_output=$(prog route -- "$@" 2>/dev/null); then
  printf '%s\n' '{"schema":"prog.integration_fallback","reason":"route_failed","action":"execute_authored_argv"}' >&2
  exec "$@"
fi

case "$route_output" in
  *'"guidance":"progressive"'*)
    timeout_ms=${PROG_HOOK_TIMEOUT_MS:-30000}
    case "$timeout_ms" in
      ''|*[!0-9]*)
        printf '%s\n' '{"schema":"prog.integration_error","reason":"PROG_HOOK_TIMEOUT_MS_must_be_an_unsigned_integer"}' >&2
        exit 64
        ;;
    esac
    exec prog run --preserve-exit-code --timeout-ms "$timeout_ms" -- "$@"
    ;;
  *)
    exec "$@"
    ;;
esac

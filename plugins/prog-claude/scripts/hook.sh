#!/bin/sh
# Forward one Claude Code hook event (JSON on stdin) to prog. Fails open: a
# missing, older, or failing prog prints nothing and exits 0, so the plugin
# never reports hook errors or blocks a session.
exec 2>/dev/null
for candidate in "$(command -v prog 2>/dev/null)" "$HOME/.local/bin/prog"; do
  if [ -n "$candidate" ] && [ -x "$candidate" ]; then
    if output=$("$candidate" harness hook claude-code) && [ -n "$output" ]; then
      printf '%s\n' "$output"
    fi
    exit 0
  fi
done
exit 0

#!/bin/sh
# Forward one Claude Code hook event (JSON on stdin) to prog. Without an
# installed prog binary this does nothing, so the plugin never breaks a session.
for candidate in "$(command -v prog 2>/dev/null)" "$HOME/.local/bin/prog"; do
  if [ -n "$candidate" ] && [ -x "$candidate" ]; then
    exec "$candidate" harness hook claude-code
  fi
done
exit 0

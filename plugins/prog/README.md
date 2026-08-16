# prog Codex plugin

This bundle makes the canonical `prog` skill installable through the Codex
plugin marketplace. The skill routes noisy commands through the included
exact-argv wrapper and retrieves exact cached evidence by cursor.

Codex currently exposes shell tool input as a shell string and does not expose
a lossless post-result replacement boundary. The plugin therefore does not
parse or transparently rewrite shell commands. It teaches the agent to author
the wrapper explicitly, preserving the command exactly.

The `prog` binary must be available on `PATH`. Run `scripts/doctor.sh` to verify
the dependency and `scripts/prog-run.sh <command...>` for direct use.

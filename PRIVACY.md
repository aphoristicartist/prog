# Privacy

prog, its Claude Code plugin and its Codex plugin run on your machine. They
collect no telemetry, have no account and send nothing to the maintainers.

## What stays on your machine

- **Captured output.** Test, build and lint runs captured by the PATH shims,
  `prog run` or `prog observe` are written to a local store. That store is
  under `$XDG_STATE_HOME/prog` (default `~/.local/state/prog`), or wherever
  `PROG_DIR` points. Recognized secrets are redacted before anything is
  stored. See [docs/evidence.md](docs/evidence.md).
- **Decision log.** The plugin hooks keep a decision log beside the store.
  It records each receipt and stop-gate decision, with hashed session and
  invocation ids and no message text. `prog harness log` reads it, and
  `PROG_HARNESS_LOG=0` turns it off.
- **Shims.** The plugins install shims under `$XDG_DATA_HOME/prog/shims`.
  These are short scripts that hand the command to the real tool.

## When prog uses the network

- **`prog update`** downloads the latest release from GitHub
  (`github.com/aphoristicartist/prog`) and verifies it, but only when you run
  it.
- **Sources you configure.** `prog call` contacts an HTTP or MCP source only
  when you have registered that source and called it.
- **Commands you run.** Commands run through `prog run` or a shim use the
  network exactly as they would without prog.

Nothing else is uploaded, and the plugin hooks only call the local `prog`
binary. Remove everything by deleting the store, the shims directory
(`prog shim uninstall`), and the plugin.

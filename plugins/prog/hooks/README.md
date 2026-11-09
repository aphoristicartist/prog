# Hooks

`hooks.json` registers three Codex hooks, all of which run
`prog harness hook codex` through `scripts/hook.sh` and fail open:

| Event | What happens |
|---|---|
| `SessionStart` | Installs the PATH shims and records when the session started, so the stop gate considers only this session's runs. |
| `PostToolUse` (`Bash`) | Adds the compact receipt of each run a shim captured as `additionalContext`, or says that a test command ran without a capture. Codex passes no exit code here; receipts take it from the shim's own capture. The tool output is never changed. |
| `Stop` | With `PROG_STOP_GATE=1` (`prog codex --stop-gate`), blocks once while the newest test run is failing, unproven, or older than the files it tested; see `docs/shims-and-receipts.md`. |

Commands are never rewritten: shims see argv after the shell has split it, so
prog never rebuilds a command from Codex's command string.

Two things are needed for the hooks to do anything:

1. **Trust.** Codex runs a non-managed hook only after you trust it in
   `/hooks` (TUI) or Settings > Hooks (desktop app). `codex exec` skips
   untrusted hooks silently. `prog harness doctor` reports whether trust
   entries exist.
2. **Launch through `prog codex`.** It puts the shims first on `PATH`, passes
   `-c allow_login_shell=false` (a login shell re-runs your profile, which can
   put Homebrew and `~/.local/bin` ahead of the shims), and adds prog's store
   to `sandbox_workspace_write.writable_roots` (the `workspace-write` sandbox
   otherwise denies the shims' writes). `prog codex --keep-codex-config`
   skips both overrides when you set them yourself.

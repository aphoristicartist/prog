# Command wrapper

`prog run -- <command...>` captures a normal command as a bounded
`DisclosureEnvelope` without requiring a source profile.

```bash
prog run -- cargo test
prog run -- pytest -q
prog run -- gh api repos/OWNER/REPO/issues
```

Use the first-party lens pack when the command output is noisy and failure
triage is the goal:

```bash
prog --lens-dir ./lenses run --lens run.failures -- cargo test
```

The stored payload includes:

- redacted argv and current working directory
- start/end time, duration, exit code, signal, timeout, and spawn error status
- stdout and stderr separately
- practical combined stream chunks
- failure sections for common Rust, Python, Node, timeout, spawn, and generic
  diagnostics
- bounded pytest and Cargo/rustc normalized evidence at `/provider`, when the
  argv matches a supported coding provider
- cursor-backed expansion paths such as `/stdout/text`, `/stderr/text`, and
  `/failure_sections/0`

Provider parsing never replaces the raw capture. See
[coding-output providers](coding-providers.md) for supported formats,
selection scopes, hard bounds, and conservative completeness rules.

`prog run` returns a successful `prog` process exit when it successfully writes
an envelope, even if the child command exits non-zero. Use
`--preserve-exit-code` for shell hooks that need the wrapper process to mirror
the child failure:

```bash
prog run --preserve-exit-code -- cargo test
```

On POSIX systems, `SIGINT` and `SIGTERM` cancel the captured process group,
persist a conservative `cancelled` observation, and return `128 + signal` when
`--preserve-exit-code` is active. Cancelled evidence cannot prove absence.

`--timeout-ms` sets one deadline for child execution and stdout/stderr drainage.
It remains active after the immediate child exits if descendants still hold the
pipes open. Deadline expiry keeps the captured partial evidence, records a
`timeout` observation that cannot prove absence, and returns `124` when
`--preserve-exit-code` is active. Signal cancellation also remains active during
post-exit drainage. Cleanup targets the original process group and bounds reader
shutdown even when a pipe holder has detached from that group. Store writes and
envelope rendering follow capture, so total wall time includes that work and
scheduling/cleanup overhead.

Registered CLI sources use the same execution-and-drainage deadline and retain
their structured `cli_timeout` error contract.

Use output caps to keep local capture bounded:

```bash
prog run --max-stdout-bytes 262144 --max-stderr-bytes 262144 -- npm test
```

Use `--out <file>` to write the full redacted structured capture to disk without
putting it in model context:

```bash
prog run --out ./run-capture.json -- cargo test
```

The `--out` file is redacted JSON, not raw terminal output. Raw secrets should
not be persisted by `prog`.

## Agent Loop

```bash
prog run -- cargo test
prog paths pc1_... --prefix /failure_sections
prog expand pc1_... --path /failure_sections/0
prog expand pc1_... --path /stderr/text
```

## Counterexamples

Do not use `prog run` when:

- raw streaming output is the user experience, such as an interactive progress
  display
- the command requires an interactive TTY
- a domain-specific tool already returns exactly the JSON needed
- rerunning the command is cheaper and clearer than inspecting cached output

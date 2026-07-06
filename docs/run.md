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
- cursor-backed expansion paths such as `/stdout/text`, `/stderr/text`, and
  `/failure_sections/0`

`prog run` returns a successful `prog` process exit when it successfully writes
an envelope, even if the child command exits non-zero. Use
`--preserve-exit-code` for shell hooks that need the wrapper process to mirror
the child failure:

```bash
prog run --preserve-exit-code -- cargo test
```

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

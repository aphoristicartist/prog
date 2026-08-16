# prog for DeepSeek Harness

This package installs `prog` at DeepSeek Harness's immutable
`tools/post-execute` boundary. Accepted oversized plain-text tool results are
captured locally, redacted before persistence, and replaced with the same
bounded `prog.disclosure` envelope used by every other integration. Small,
non-text, nested, already-`prog`, and unsupported results pass through.

The adapter never reruns a tool and never parses a shell command. If `prog` is
missing, times out, returns an invalid envelope, or cannot persist the result,
the successful original tool result is preserved.

Install the `prog` binary, then add this checkout or a published package to the
profile:

```sh
dsh plugin --profile web add ./extensions/deepseek-harness
```

The package declares `dsh.bundle.patch`, so DeepSeek Harness adds the
`prog-disclosure` layer automatically. Configuration keys are `minBytes`,
`budgetBytes`, `timeoutMs`, `storeDir`, `cwd`, `progCommand`, and `progArgs`.

Run the adapter contract tests with:

```sh
npm test --prefix extensions/deepseek-harness
```

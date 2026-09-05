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

`timeoutMs` covers the capture helper and drainage of both output pipes under
one deadline. On supported POSIX hosts, the helper starts in its own process
group. Timeout, cancellation, failed stdin delivery, and stdout/stderr overflow
terminate that group, close the adapter's pipes, and preserve the original host decision. A valid
JSON prefix cannot become a replacement after capture stops. A signal already
aborted before capture starts prevents the helper from being launched.

A descendant that deliberately leaves the capture group may survive its
termination. The adapter still closes its own descriptors and returns without
waiting for that descendant. These controls apply to the capture helper; the
upstream tool has already executed and is never rerun during fallback.

Run the adapter contract tests with:

```sh
npm test --prefix extensions/deepseek-harness
```

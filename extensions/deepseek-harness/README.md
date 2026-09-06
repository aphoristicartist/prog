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

Capture helpers use `scrubbedParentEnv()` from the installed host's
`@deepseek-ai/dsh-subprocess` package. The host's shared filter removes ambient
credential-shaped names and managed `DSH_*` variables, case-insensitively, while
retaining ordinary child settings such as `PATH`, `HOME`, locale, proxy
variables, and `PROG_DIR`. It leaves the harness's own environment intact and
also applies to configured `progCommand` helpers. The adapter has no credential
forwarding option; result capture does not need provider credentials.

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
npm ci --ignore-scripts --no-audit --no-fund --prefix extensions/deepseek-harness
npm test --prefix extensions/deepseek-harness
```

The lockfile pins the tested host packages. The environment regressions use
synthetic credentials in an isolated host process and inspect a real capture
child on both replacement and fallback paths.

## Experimental registered tools

An owner can enable `facade: true` in the existing `prog-disclosure` layer's
configuration. The default bundle still installs only the post-result hook.
For example, the layer configuration can contain:

```yaml
progCommand: /absolute/path/to/prog
budgetBytes: 16384
timeoutMs: 30000
facade: true
```

This registers `prog_observe`, `prog_evidence`, and `prog_status` through the
real host tool registry. It requires the host's `tools`, `systemPrompt`,
`subprocess`, `sandboxPolicy`, and `shellEnv` services, plus a working `sandbox`
provider for confined modes. The owner must provide a compatible `prog`
executable containing the registered-call cancellation support. The tested
host package set is pinned at `0.1.0-rc.6` in the development lockfile.

`facade` also accepts a configuration object overriding `progCommand`,
`storeDir`, `budgetBytes`, `timeoutMs`, `graceMs`, and `maxInputBytes`. Defaults
are `prog`, `.prog` under the session workspace, 16,384 bytes, 30,000 ms,
5,000 ms, and 16,777,216 bytes respectively. `maxInputBytes` bounds artifact
acquisition; it does not change the existing command/source capture limits.
`graceMs` allows capture completion and process cleanup after the source
deadline. `progArgs` and the post-result hook's `cwd` do not configure these
tools. Relative stores stay anchored to the host session workspace even when
a run uses another `workdir`.

| Tool | Supported operations |
| --- | --- |
| `prog_observe` | Exact `argv` run, configured source call, file, or text capture |
| `prog_evidence` | Findings inspection, reference/path evidence, expansion/export, bounded literal search |
| `prog_status` | Shared readiness, evidence availability, and optional baseline/subject delta |

These tools invoke the canonical CLI inside the host subprocess and sandbox
boundary. They honor host guards and the current session policy on every call.
Command/source children receive the host's scrubbed ambient environment and a
fresh managed `DSH_*` overlay. Source confirmation and profile trust remain
the CLI's existing gates. `yes` forwards explicit prior user confirmation;
the facade never supplies it implicitly or declares required obligations.

A successful capture can contain a failed command or MCP response. Inspect
the canonical exit/signal facts and failure evidence. A failed invocation
does not establish whether upstream effects occurred and never triggers an
automatic retry. Cancellation asks `prog` to release its separately owned
source groups before the host terminates its own group. Deliberately detached
descendants remain outside that process-group guarantee.

TTY, streaming, unknown keys, and mode-inappropriate fields are rejected.
Shell syntax is never parsed or substituted; an explicitly supplied shell
program in `argv` remains subject to the host policy. Cached evidence can be
omitted even in an `exact` excerpt: follow the canonical omissions, or use an
explicit `expand` export and the host's file reader. The export receipt and
slice hash support exact verification. File-reader fallback bytes count toward
the task's delivered context.

`budgetBytes` bounds the canonical value and final text presentation. Host
notices and protocol framing are additional context. Oversized host-modified
presentations produce a bounded refusal without changing a host denial into
success. Sandbox notices retain the backend's reported full/partial
enforcement; missing confinement fails closed. Disposing the owner layer
removes its tools and instructions.

The installed comparison and its accounting limits are documented in
[`docs/registered-host-facade.md`](../../docs/registered-host-facade.md).
The three-tool surface is experimental: an actual-agent comparison in #139
must precede any claim that it is canonical, cheaper, or more effective.

# Registered host facade experiment

DeepSeek Harness can opt into three registered tools from the native package:
`prog_observe`, `prog_evidence`, and `prog_status`. They compose the existing
CLI inside the host's subprocess, session policy, environment, and sandbox
services. There is no new cache, disclosure engine, readiness engine, model
call, or MCP server. See the [owner configuration](../extensions/deepseek-harness/README.md#experimental-registered-tools).

## Installed coding-loop comparison

The native test packs the npm artifact, unpacks its published file set outside
checkout, and loads its owner-enabled plugin through the real Cordis registry.
It uses the exact locked host providers via a dependency link. This validates
the installed artifact and plugin lifecycle, not a network package installer
or the full `dsh` CLI profile manager.

```sh
cargo build -p prog-cli
npm ci --ignore-scripts --no-audit --no-fund --prefix extensions/deepseek-harness
npm rebuild node-pty --prefix extensions/deepseek-harness
PROG_TEST_BINARY="$PWD/target/debug/prog" npm test --prefix extensions/deepseek-harness
```

The Python driver reuses the [installed skill/CLI fixture](installed-coding-loop.md)
for both arms. Each uses a separate copied executable, project, store, and
Cargo target outside checkout. The registered arm routes failure capture,
findings inspection, exact cached evidence, and status through the actual
tools. It applies the known correction externally and captures a fresh run of
the same complete suite. Exact argv, cwd, ordinary environment, and source
exit facts are checked from execution records and cached evidence.

For parity, advanced CLI evidence reads address the same facade-created
cursors and immutable slice references. Direct readiness and delta commands
use the same observation IDs and user-declared criteria as the facade status.
The fixture checks historical evidence stability and proves that navigation
does not rerun tests. Narrow success, workspace mutation, incomplete capture,
and eviction of previously passing evidence still block readiness. Explicit criteria and the truncated-capture
control use the advanced CLI; those fallback calls remain in the ledger.

To save a report while iterating on a checkout:

```sh
python3 fixtures/harness/registered_coding_loop.py \
  --prog "$PWD/target/debug/prog" --node "$(command -v node)" \
  --driver extensions/deepseek-harness/fixtures/registered-loop-driver.mjs \
  --artifact extensions/deepseek-harness/index.js \
  --output /tmp/prog-registered-loop.json
```

That command loads the checkout's plugin; the native test above separately
proves the unpacked artifact. Both are deterministic protocol callers using
real host services, without a model driver or provider. The test-only line
transport lives in a fixture and is not a product server or agent runtime.

## Accounting and claim limits

The report retains the assembled tool schemas and instruction sections
actually sent to the scripted caller, every tool request and result, every
CLI invocation and response, and exported-file reads. The CLI arm reads its
installed skill before the first capture. Setup, criteria, navigation,
verification, and parity reference calls have explicit roles. Full ledgers
allow a reader to assess those costs without treating available help as
delivered context.

The native test gates the assembled schema and instruction surface under an
8,192-byte ceiling. This is a reviewed size limit, not a task-cost or savings
measurement.

`context_bytes` reports UTF-8 bytes of instructions, JSON tool schemas and
requests, model-facing presentation objects including notices, CLI output,
and exported file reads. `fixture_host_result_bytes` separately records the
full internal host results, whose `value` duplicates rendered text. It is not
added again to presentation cost. No provider-specific tokenization or
assumed per-turn schema resend is included.

The source correction and obligation are known to the fixture caller. This
proves transport, evidence, and verification composition; it does not measure
agent problem-solving, ambiguity, or independent task success. There is no
claim that three tools are automatically cheaper. Actual-agent outcomes,
total task context, small-output overhead, ambiguity, and fallback behavior
still require the paired experiment in #139 before #120 can declare this
surface canonical. The server-transport deferral in #216 remains in effect.

## Failure and policy coverage

Real host tests cover guards, explicit source confirmation/profile trust,
session cwd and sandbox changes, environment filtering, argv preservation,
stdout/stderr, source exit/signal facts, cancellation, artifact packaging, and
owner disposal/reinstallation. A denied external write is tested against the
actual local sandbox. Linux CI installs Bubblewrap; macOS uses Seatbelt.

Lifecycle fixtures exercise parent exit with inherited stdout/stderr,
finite drainage, escaped descendants, deadlines, overflow, failed input
delivery, malformed JSON/UTF-8, and incomplete cleanup reporting. A valid
prefix cannot authorize success after any transport failure. Registered CLI
and MCP cancellation also has Rust integration coverage: it returns a
non-retryable `call_cancelled` error with uncertain upstream effects and
releases the adapters' owned process groups. These guarantees preserve the
existing I7 and I14 boundaries; they do not authorize automatic retries.

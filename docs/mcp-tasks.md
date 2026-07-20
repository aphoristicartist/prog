# Long-running MCP tasks

Some MCP tools do not return a result within a single call. They return a task
reference, run in the background, and expose the result later. `prog mcp-task`
follows that lifecycle while keeping every transition in the observation store.

```sh
prog mcp-task start  <source> <operation> --args '<json>' [--ttl-ms N] [--yes]
prog mcp-task get    <source> <task-id>
prog mcp-task result <source> <task-id>
prog mcp-task cancel <source> <task-id>
```

This is `prog` acting as an MCP **client**. `prog` does not expose an MCP server
mode; see [`integrations.md`](integrations.md).

## Why it is a separate command

`prog call` captures one request and one response. A task has a lifecycle —
started, polled, completed, possibly cancelled — and each step is independent
evidence about a system that is changing while you watch it. Collapsing that
into one call would discard the history and make it impossible to tell a task
that never started from one that started and later became unreachable.

Every subcommand records an observation, so the whole lifecycle stays inspectable
after the fact.

## Output

Each subcommand returns:

```json
{
  "schema": "prog.mcp_task",
  "observation_id": "obs_...",
  "availability": "recoverable",
  "payload": {}
}
```

Use `observation_id` to feed later navigation, [`delta`](delta.md), or a
[verification obligation](verification.md). `availability` reports whether the
recorded payload is still retrievable.

## Lifecycle

### start

```sh
prog mcp-task start incidents search --args '{"query":"checkout"}' --ttl-ms 60000
```

Only **MCP tool** operations can be started as tasks; a resource operation is
rejected with a structured `bad_args` error. `--ttl-ms` requests an upstream
time-to-live for the task.

`start` passes the same safety gates as `prog call`: mutating operations require
`--yes`, and shell-backed profiles additionally require profile trust. Starting a
task is not treated as a read just because the result arrives later.

### get, result, cancel

`get` reports current status, `result` retrieves the completed value, and
`cancel` requests cancellation. Each takes the task id returned by `start`.

Pass `--parent-observation <id>` to link a lifecycle step to the observation it
follows, preserving lineage across the sequence.

## Unavailable transitions are recorded, not swallowed

A task reference can stop resolving independently of the original call — the
server restarts, the TTL expires, the transport drops. When that happens, `prog`
does **not** raise a bare error and lose the attempt. It records an observation
marking the transition as unavailable evidence.

This is the [conservative-answer rule](delta.md) applied to time: "the task
result is unavailable" and "the task produced nothing" are different facts, and
a loop that cannot distinguish them will draw wrong conclusions. Protocol,
transport, and timeout failures are preserved as attempted transitions rather
than reinterpreted as results.

Errors that are *not* result-unavailability — argument validation, policy
refusals — still surface as ordinary structured errors.

## Related

- [Agent integrations](integrations.md) — the MCP stance and why there is no
  server mode.
- [Adding sources](source-setup.md) — registering an MCP source profile.
- [Safety and trust model](safety.md) — the gates `start` enforces.
- [Evidence and observations](evidence.md) — observation records and lineage.

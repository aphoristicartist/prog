# Source setup

`prog source` creates simple source profiles without hand-writing seed JSON.
Generated seeds are returned in the command output so agents and humans can
review exactly what was persisted.

## HTTP From A URL

```bash
prog source add-http api --operation list --url https://api.example.test/items
prog call api list --args '{}'
```

`add-http` splits the URL into `base_url`, `path`, and static query parameters.
Only `http://` and `https://` URLs are accepted. URL fragments and embedded
credentials are rejected.

`GET` operations are generated as read-only, cacheable, and non-mutating.
Non-`GET` methods are generated as confirmation-gated and non-cacheable:

```bash
prog source add-http api --operation create --method POST --url https://api.example.test/items
prog call api create --args '{}' --yes
```

Pass `--probe` when the generated operation is safe to execute immediately and
you want the profile to learn an output shape during setup.

## CLI From A Command

```bash
prog source add-cli local --operation list --read-only -- python3 fixtures/cli/list_items.py
prog call local list --args '{}'
```

`--read-only` marks the command as safe to invoke automatically, non-mutating,
cacheable, and non-sensitive. Omit `--read-only` for commands whose effects are
unknown; the generated operation stays fail-closed and requires `--yes`:

```bash
prog source add-cli local --operation inspect -- python3 tool.py
prog call local inspect --args '{}' --yes
```

The command is stored as argv, not as a shell string. Shell-backed sources still
require explicit seed/profile editing because shell trust should be reviewed.

`source add-cli` detects existing `--json`, `--format json`, `--output json`,
`-o json`, and Cargo JSON message flags. For known invocations of `gh`,
`kubectl`, Cargo, and structured npm commands it returns a conservative
`structured_output` suggestion. Nothing is changed by default. Pass
`--prefer-json` to apply only a high-confidence suggestion:

```bash
prog source add-cli pods --operation list --read-only --prefer-json -- kubectl get pods
```

Unknown or ambiguous CLIs fail `--prefer-json` with an actionable error instead
of guessing a flag. Detection never executes help commands.

## MCP From A Command

Register an MCP stdio server directly as argv. This uses the same validated
discovery path as `discover --kind mcp --seed` and never invokes a shell:

```bash
prog source add-mcp docs -- python3 fixtures/mcp/fixture_mcp.py
prog call docs search_docs --args '{"query":"release"}'
```

The server is started during discovery so `prog` can record the operations and
their advertised effect annotations. Operations without proven read-only
annotations remain confirmation-gated.

Each MCP operation owns a stdio connection. The seed's `timeout_ms` applies
separately to initialization, each request, graceful shutdown, and the final
stderr drain; it is not one deadline for the entire operation. A shutdown that
times out stops diagnostic collection immediately. Failed or interrupted
connections abort their stderr reader and request termination of the owned
process group, including descendants that retained its pipes. Descendants that
created a separate process group can survive; local diagnostic collection still
ends within its bound. Successful shutdown with stderr EOF releases ownership
without killing background workers that have released the transport pipes.

A received response remains evidence even if stderr collection is interrupted.
`provenance.adapter.diagnostics.stderr` retains a redacted prefix of at most
`max_stderr_bytes` and reports:

- `complete` and `stop_reason`: only `eof` proves collection reached the end.
  Other reasons are `timeout`, `shutdown_timeout`, `shutdown_failed`,
  `read_error`, `reader_failed`, and `unavailable`.
- `byte_count`: the exact total after EOF, otherwise `null`.
  `observed_byte_count` counts bytes actually read and is only a lower bound
  without EOF; `captured_byte_count` counts retained prefix bytes before
  redaction.
- `line_count`: exact only after EOF with the whole stream retained, otherwise
  `null`. `captured_line_count` counts lines in the retained prefix, including a
  possible partial final line. `head` and `tail` show at most ten lines each
  from that prefix; `tail` is not necessarily the end of the source stream.
- `truncated`: true when collection is incomplete, bytes were discarded at the
  prefix limit, or lines were omitted between the displayed head and tail.

Incomplete collection also adds a warning. Known-empty stderr has
`complete: true`, `stop_reason: "eof"`, and `byte_count: 0`; interrupted or
unavailable collection never claims an empty total.

## Import Existing Descriptors

`prog discover --import` seeds profiles from descriptors that tools already
publish. Imports never execute upstream calls during discovery, even if
`--probe` is passed.

```bash
prog discover api --kind http --seed openapi.json --import openapi
prog discover schema_api --kind http --seed schema.json --import json-schema
prog discover taskctl --kind cli --seed taskctl.help --import cli-help --command-base taskctl
```

Use `--import auto` to detect OpenAPI 3.x or JSON Schema JSON. CLI help imports
need `--command-base` so the generated profile records the exact executable.

Imported schemas are bounded by `--max-schema-depth` and preserve `$ref` values
without dereferencing external documents. Declared schemas are stored as
`declared_output_schema`; later probes or calls learn `output_shape`
separately, so observations refine priors without overwriting them.

CLI help imports are conservative: parsed commands are not marked read-only,
are not cacheable, and require `--yes`. MCP tools without `readOnlyHint: true`
follow the same fail-closed rule in the importer API.

### Graded evidence and confirmation

Importers stamp a graded `evidence_grade` on every derived operation:
*proven* (HTTP `GET`/`HEAD`/`OPTIONS`; an MCP tool with `readOnlyHint: true`
and no contradictory `destructiveHint`; an MCP resource), *assumed* (a JSON
Schema synthesized op), or *unproven* (non-GET HTTP, MCP tools without a read
hint, CLI help). Imported read-only operations are **stored** with
`requires_confirmation: true` and **relaxed** at call/discovery time when the
descriptor is *proven* read-only and `trust.auto_upgrade` is enabled (the
default). So a committed OpenAPI or trusted-MCP source can be explored without
passing `--yes` on every read-only call, while `assumed`/`unproven` ops stay
gated. Set `trust.auto_upgrade: false` on a profile to re-gate even *proven*
read-only ops and restore the strict behavior. See `docs/safety.md` for the
full grade table and the audit location.

## Generated Output

Both source-add commands return:

- `generated_seed`: the seed JSON used for discovery
- `discovery`: the same report returned by `prog discover`
- `next_steps`: copy-pasteable `prog hints` and `prog call` commands
- `structured_output`: detected or suggested JSON-mode flags with confidence
- `warnings`: confirmation-gating, probe, or discovery warnings

Use `prog discover --seed` when you need advanced seed features such as auth
refs, headers, templated parameters, shell-backed commands, or MCP servers.

An advanced operation may also declare an exact `source_state.path` JSON
Pointer and optional `expires_at_path`. Selected values are hashed immediately,
never persisted raw, and malformed or missing selectors cannot establish
freshness. See [source-state evidence](source-state.md).

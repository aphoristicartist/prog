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
- `warnings`: confirmation-gating, probe, or discovery warnings

Use `prog discover --seed` when you need advanced seed features such as auth
refs, headers, templated parameters, shell-backed commands, or MCP servers.

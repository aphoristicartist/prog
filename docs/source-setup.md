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

## Generated Output

Both source-add commands return:

- `generated_seed`: the seed JSON used for discovery
- `discovery`: the same report returned by `prog discover`
- `next_steps`: copy-pasteable `prog hints` and `prog call` commands
- `warnings`: confirmation-gating, probe, or discovery warnings

Use `prog discover --seed` when you need advanced seed features such as auth
refs, headers, templated parameters, shell-backed commands, or MCP servers.

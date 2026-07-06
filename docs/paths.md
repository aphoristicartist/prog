# Path discovery and planner actions

`prog paths <cursor>` lists expandable JSON Pointer paths from a cached,
redacted payload. It never contacts the upstream API, CLI, or MCP server.

Use it when a preview is useful but incomplete:

```bash
prog call demo list --args '{}'
prog paths pc1_... --prefix /items
prog expand pc1_... --path /items/0/body
```

Every `DisclosureEnvelope` also includes `next_actions`. Expansion actions are
ranked and include:

- `path`: the exact JSON Pointer to inspect
- `omitted_reason`: why the preview omitted or redacted it
- `detail`: source-specific size or omission detail when available
- `argv`: exact `prog expand` arguments
- `offline`: confirmation that expansion reads the cached redacted payload

Agents should prefer `next_actions` first, then use `prog paths` when they need
to search or filter the address space.

## Filters

List only omitted regions:

```bash
prog paths pc1_... --omitted-only
```

Filter by omission reason:

```bash
prog paths pc1_... --reason large_string
prog paths pc1_... --reason long_array
prog paths pc1_... --reason many_fields
prog paths pc1_... --reason deep_object
prog paths pc1_... --reason redacted
```

Filter by field name:

```bash
prog paths pc1_... --field body
prog paths pc1_... --field token --expandable-only
```

Narrow traversal with `--prefix` before raising `--limit` or `--depth`:

```bash
prog paths pc1_... --prefix /items --limit 50 --depth 4
```

## Invariants

- Paths are deterministic JSON Pointers.
- Path discovery is bounded by `--limit` and `--depth`.
- Cursor root boundaries are enforced before path resolution.
- Redacted paths remain redacted in `paths`, `next_actions`, and `expand`.
- Expansion reads cache only; refresh requires rerunning the original call or
  observation.

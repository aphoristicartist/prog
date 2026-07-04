# End-to-end walkthroughs

These walkthroughs use fixtures committed under `fixtures/`. Commands assume you installed the CLI with `cargo install --path crates/prog-cli` and are running from the repository root. During development you can replace `prog` with `cargo run --`.

## CLI fixture

The CLI fixture runs `python3 fixtures/cli/list_items.py` and returns 30 JSON items with large `body` fields.

```bash
rm -rf /tmp/prog-cli-demo
prog --dir /tmp/prog-cli-demo discover demo_cli --kind cli --seed fixtures/cli/seed.json
prog --dir /tmp/prog-cli-demo hints demo_cli list
prog --dir /tmp/prog-cli-demo call demo_cli list --args '{}'
```

The call envelope includes `summary.envelope_bytes`, `schema_hints`, `omitted`, `cursor`, `next_actions`, and `cache.status: "stored"`.

Expand a bounded slice from the cached payload:

```bash
CURSOR=$(prog --dir /tmp/prog-cli-demo call demo_cli list --args '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
prog --dir /tmp/prog-cli-demo expand "$CURSOR" --path /items --limit 3 --depth 3
prog --dir /tmp/prog-cli-demo expand "$CURSOR" --path /items --out /tmp/prog-cli-items.json
```

The `--out` form writes the selected JSON to disk and returns a receipt containing the output path, JSON Pointer, byte count, and SHA-256.

## HTTP fixture

Start a local file server in one terminal:

```bash
python3 -m http.server 8765 --directory fixtures/http
```

Run `prog` in another terminal:

```bash
rm -rf /tmp/prog-http-demo
prog --dir /tmp/prog-http-demo discover demo_http --kind http --seed fixtures/http/seed.json
prog --dir /tmp/prog-http-demo call demo_http list --args '{}'
CURSOR=$(prog --dir /tmp/prog-http-demo call demo_http list --args '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
prog --dir /tmp/prog-http-demo expand "$CURSOR" --path /items --limit 2
```

The second `call` normally returns `cache.status: "hit"` because the first call stored the read-only fixture response. Stop the file server with `Ctrl-C` when finished.

## MCP fixture

The MCP fixture is a tiny stdio server at `fixtures/mcp/fixture_mcp.py`. Discovery reads its tool catalog, then calls the `search_docs` tool.

```bash
rm -rf /tmp/prog-mcp-demo
prog --dir /tmp/prog-mcp-demo discover demo_mcp --kind mcp --seed fixtures/mcp/seed.json
prog --dir /tmp/prog-mcp-demo call demo_mcp search_docs --args '{"query":"fixture"}'
CURSOR=$(prog --dir /tmp/prog-mcp-demo call demo_mcp search_docs --args '{"query":"fixture"}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
prog --dir /tmp/prog-mcp-demo expand "$CURSOR" --path /results --limit 2
```

MCP `readOnlyHint` annotations are mapped into operation effects. The fixture advertises a read-only tool, so the call does not require `--yes`.

## Reflexive contracts

Every adapter output uses the same disclosure envelope. `prog meta` exposes the public contracts through that envelope too:

```bash
prog --dir /tmp/prog-cli-demo meta
prog --dir /tmp/prog-cli-demo meta SourceProfile
prog --dir /tmp/prog-cli-demo meta DisclosureEnvelope
```

Use the returned cursor with `prog expand` if a schema section is omitted.

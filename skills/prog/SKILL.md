---
name: prog
description: Use prog for progressive disclosure over noisy API, CLI, or MCP sources. Trigger when the user mentions large JSON responses, pagination-less APIs, context blowups from tool output, arbitrary HTTP/CLI/MCP data sources, or needing bounded inspection with expandable cached results instead of raw dumps.
---

# prog

Use `prog` as a progressive-disclosure gateway. Teach the agent loop; let `prog meta` disclose detailed contracts.

## Loop

1. Run `prog hints <source>` before calling a known source.
2. For a new source, run `prog discover <source> --kind http|cli|mcp --seed <path-or-json>`. Do not add `--probe` by default; probing is an explicit read-only decision.
3. Run `prog call <source> <operation> --args '<json>'` to get a bounded envelope.
4. Read `summary.approx_tokens`, `warnings`, `omitted`, `cursor`, and `next_actions` before asking for more.
5. Run `prog expand <cursor> --path <json-pointer> --limit N --depth N` for omitted regions. Use concrete paths and bounded limits.
6. Run `prog expand <cursor> --path <json-pointer> --out <file>` for bulk data you will grep or process with code. Bulk goes to files, never to context.
7. Use `--refresh` when staleness warnings appear and freshness matters.
8. Use `--yes` only after telling the user a mutation is about to happen.
9. Never ask for raw dumps into context; that is the failure mode `prog` exists to prevent.
10. Run `prog meta` or `prog meta <ContractName>` for prog's own contracts.

Respect warnings about mutation, shell trust, secrets/redaction, stale cache, and non-cacheable results.

## HTTP Example

Seed:

```json
{
  "kind": "http",
  "base_url": "http://127.0.0.1:8000",
  "operations": [
    {
      "name": "list",
      "method": "GET",
      "path": "/items"
    }
  ]
}
```

Commands:

```bash
prog discover api --kind http --seed http.json
prog hints api
prog call api list --args '{}'
prog expand pc1_example --path /items --limit 5 --depth 3
prog expand pc1_example --path /items --out /tmp/items.json
```

Replace `pc1_example` with the cursor returned by `call`.

## CLI Example

Seed:

```json
{
  "kind": "cli",
  "operations": [
    {
      "name": "hello",
      "command": "python3",
      "args": ["-c", "import json; print(json.dumps({'hello':'{name}'}))"],
      "input_schema": {
        "type": "object",
        "required": ["name"],
        "properties": {"name": {"type": "string"}}
      },
      "effect": {
        "read_only": true,
        "mutating": false,
        "network": false,
        "shell": false,
        "sensitive": false,
        "cacheable": true,
        "requires_confirmation": false
      }
    }
  ]
}
```

Commands:

```bash
prog discover local --kind cli --seed cli.json
prog hints local hello
prog call local hello --args '{"name":"Ada"}'
```

Shell-backed operations require `trust.allow_shell` in the source profile.

## MCP Example

Seed:

```json
{
  "kind": "mcp",
  "command": "python3",
  "args": ["fixture_mcp.py", "normal"]
}
```

Commands:

```bash
prog discover docs --kind mcp --seed mcp.json
prog hints docs
prog call docs search_docs --args '{"query":"rust"}'
prog expand pc1_example --path /results --limit 5 --depth 3
```

MCP catalog discovery is allowed, but tool calls still obey effect policy.

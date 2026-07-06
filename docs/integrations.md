# Agent integrations

`prog` is designed to stay useful without MCP server mode. The durable V1
surface is:

```text
CLI + agent skill + explicit project hooks
```

MCP can be added for hosts that already speak MCP well, but it should reuse the
same CLI/core semantics, safety gates, cache behavior, cursor expansion, and
redaction rules.

## Matrix

| Surface | Status | Command | Writes |
|---|---|---|---|
| Codex project skill and hooks | implemented | `prog init --agent codex --project` | `.codex/skills/prog/SKILL.md`, `.codex/prog-hooks/*` |
| Codex dry run | implemented | `prog init --agent codex --project --dry-run` | nothing |
| Claude Code project skill and hooks | planned | not enabled | nothing |
| Cursor project rules/hooks | planned | not enabled | nothing |
| Gemini CLI project hooks | planned | not enabled | nothing |
| Global shell aliases | planned | not enabled | nothing |
| MCP server mode | optional future adapter | not enabled | nothing |

## Generated Codex Files

`prog init --agent codex --project` creates reviewable, reversible files:

- `.codex/skills/prog/SKILL.md`
- `.codex/prog-hooks/prog-run.sh`
- `.codex/prog-hooks/README.md`
- `.codex/prog-hooks/manifest.json`
- `.codex/prog-hooks/uninstall.sh`

Existing files are never overwritten silently. If a generated path already
exists, the installer reports `action: "exists"` and leaves it unchanged. Remove
the file first if regeneration is intentional.

The hook helper is explicit:

```bash
.codex/prog-hooks/prog-run.sh cargo test
```

It returns a bounded `DisclosureEnvelope`; use `prog paths <cursor>` before
expanding exact evidence with `prog expand <cursor> --path <json-pointer>`.

## Reversal

Generated files can be removed with:

```bash
sh .codex/prog-hooks/uninstall.sh
```

The uninstall script only removes the files listed in the generated manifest and
then prunes empty generated directories.

## Without MCP

Use these workflows:

```bash
prog run -- cargo test
prog paths pc1_...
prog expand pc1_... --path /failure_sections/0
```

```bash
gh api repos/OWNER/REPO/issues | prog observe --stdin --mime application/json
prog paths pc1_... --field body
prog expand pc1_... --path /items/7/body
```

Use MCP later only when the host agent makes MCP the lowest-friction path and
the adapter keeps the same safety and evidence contracts.

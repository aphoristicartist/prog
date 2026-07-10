# Agent integrations

`prog` is designed to stay useful without MCP server mode. The durable V1
surface is:

```text
CLI + agent skill + explicit project hooks
```

MCP tools and resources can already be consumed as upstream sources through the
MCP adapter. `prog` itself remains a CLI and does not expose an MCP server mode.

## Matrix

| Surface | Status | Command | Writes |
|---|---|---|---|
| Codex project skill and hooks | implemented | `prog init --agent codex --project` | `.codex/skills/prog/SKILL.md`, `.codex/prog-hooks/*` |
| Codex dry run | implemented | `prog init --agent codex --project --dry-run` | nothing |
| Claude Code project skill and hooks | implemented | `prog init --agent claude-code --project` | `.claude/skills/prog/SKILL.md`, `.claude/prog-hooks/*` |
| Cursor project rule and hooks | implemented | `prog init --agent cursor --project` | `.cursor/rules/prog.mdc`, `.cursor/prog-hooks/*` |
| Gemini CLI project skill and hooks | implemented | `prog init --agent gemini-cli --project` | `.gemini/skills/prog/SKILL.md`, `.gemini/prog-hooks/*` |
| Global shell aliases | planned | not enabled | nothing |
| `prog` as an MCP server | permanent non-goal | not enabled | nothing |

## Generated Files

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

It returns a bounded `DisclosureEnvelope`; follow its ranked findings or use
`prog inspect <cursor> --goal <goal>`, then cite exact evidence with
`prog evidence <cursor> --path <json-pointer>`.

Claude Code and Gemini CLI receive the same canonical `SKILL.md` under their
documented workspace skill directories. Cursor receives an agent-requested MDC
rule under `.cursor/rules`. Every agent gets an explicit `prog-run.sh`, manifest,
README, and uninstall script under its own project directory.

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
prog inspect pc1_... --goal "find the root cause"
prog evidence pc1_... --path /failure_sections/0
```

```bash
gh api repos/OWNER/REPO/issues | prog observe --stdin --mime application/json
prog paths pc1_... --field body
prog expand pc1_... --path /items/7/body
```

Use the MCP adapter when an upstream already exposes MCP and keep the same
safety and evidence contracts for the resulting observation.

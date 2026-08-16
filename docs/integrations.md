# Harness extensions and plugins

`prog` is an agent-harness extension. The CLI is its local JSON transport and
debugging surface; agents and harnesses are the intended callers. Every host
format derives from the normative
[`harness-extension-protocol.md`](harness-extension-protocol.md).

The preferred surface is:

```text
native post-result plugin -> portable Agent Skill -> explicit argv wrapper
```

MCP tools and resources can already be consumed as upstream sources through the
MCP adapter. `prog` itself remains a CLI and does not expose an MCP server mode
in the first release. The #216 decision defers reconsideration until #120 has a
measured three-operation facade; an MCP-only host is unsupported today.

## Matrix

| Surface | Status | Command | Writes |
|---|---|---|---|
| Auto-detected harness extension | implemented | `prog harness install` | portable skill plus detected adapters |
| Harness readiness | implemented | `prog harness doctor` | nothing |
| Codex marketplace plugin | implemented | `codex plugin add prog@personal` after adding this checkout as a marketplace | Codex plugin cache |
| DeepSeek Harness result plugin | implemented | `dsh plugin --profile web add ./extensions/deepseek-harness` | profile dependency and bundle layer |
| Portable Agent Skills target | implemented | `prog harness install --host agent-skills` | `.agents/skills/prog/SKILL.md`, `.agents/prog-hooks/*` |
| Codex project skill and hooks | implemented | `prog init --agent codex --project` | `.agents/skills/prog/SKILL.md`, `.codex/prog-hooks/*` |
| Codex dry run | implemented | `prog init --agent codex --project --dry-run` | nothing |
| Claude Code project skill and hooks | implemented | `prog init --agent claude-code --project` | `.claude/skills/prog/SKILL.md`, `.claude/prog-hooks/*` |
| Cursor project rule and hooks | implemented | `prog init --agent cursor --project` | `.cursor/rules/prog.mdc`, `.cursor/prog-hooks/*` |
| Gemini CLI project skill and hooks | implemented | `prog init --agent gemini-cli --project` | `.gemini/skills/prog/SKILL.md`, `.gemini/prog-hooks/*` |
| AGENTS.md marked section | implemented | `prog init --agent agents-md --project` | `AGENTS.md` append only |
| Skill export | implemented | `prog init --print-skill --frontmatter yaml\|mdc\|none` | nothing |
| External target manifest | implemented | `prog init --agent NAME --manifest-dir DIR --project` | manifest-declared project-relative paths |
| Global shell aliases | planned | not enabled | nothing |
| `prog` as an MCP server | deferred by #216 pending #120 measurements | not enabled | nothing |

## Generated Files

The normal entry point is:

```sh
prog harness install --dry-run
prog harness install
prog harness doctor
```

The installer always selects the portable `agent-skills` target and detects
Codex, Claude Code, Cursor, and Gemini CLI from project directories or commands
on `PATH`. Repeat `--host` to select an explicit set. Shared output paths are
deduplicated and conflicting manifests fail closed.

`prog init --agent codex --project` creates reviewable, reversible files:

- `.agents/skills/prog/SKILL.md`
- `.codex/prog-hooks/prog-run.sh`
- `.codex/prog-hooks/README.md`
- `.codex/prog-hooks/manifest.json`
- `.codex/prog-hooks/uninstall.sh`

Existing files are never overwritten silently. If a generated path already
exists, the installer reports `action: "exists"` and leaves it unchanged. Remove
the file first if regeneration is intentional.

The wrapper helper is explicit:

```bash
.codex/prog-hooks/prog-run.sh cargo test
```

It calls `prog route` over the exact argv. `progressive` guidance prepends
`prog run --`; `raw`, `passthrough`, and `unknown` execute the authored argv
directly. Wrapping identical argv is capture, while substituting a different or
narrower command is prohibited. The wrapper never reparses a shell string.

The host-visible facade has three operations in the generated manifest:

- `observe`: command argv, file capture, or a registered source call
- `evidence`: exact path retrieval or explicitly bounded cached search
- `status`: readiness alone or readiness plus canonical delta/comparability

Advanced CLI commands remain available for debugging and recovery. The facade
composes the same observation, evidence, delta, and verification contracts.

For Codex, this is deliberately a skill plus an explicit argv wrapper, not an
installed `PreToolUse` rewrite. The current [official Codex Hooks
contract](https://learn.chatgpt.com/docs/hooks) exposes Bash tool input as one
shell command string. Converting that string back to argv would violate prog's
no-reparse rule. Codex project hooks also require explicit trust review, so
`prog init` does not silently install one. This limitation is recorded in the
generated manifest.

TTY, interactive, follow/streaming, nested `prog`, and shell-structural
invocations pass through. `PROG_HOOK_TIMEOUT_MS` controls the progressive
capture timeout. If `prog` is unavailable or routing fails before execution,
the wrapper emits a small JSON fallback receipt on stderr and runs the exact
authored argv directly. It never retries after `prog run` starts.

Claude Code and Gemini CLI receive the same canonical `SKILL.md` under their
documented workspace skill directories. Cursor receives an agent-requested MDC
rule under `.cursor/rules`. Every agent gets an explicit `prog-run.sh`, manifest,
README, and uninstall script under its own project directory. The Codex fixture
proves argv, cwd, inherited environment, stdout, stderr, exit status, signal,
timeout, TTY/streaming passthrough, and pre-execution fallback behavior.

The `agents-md` target is deliberately append-only. It preserves all existing
text and adds one section bounded by `<!-- prog:skill:start -->` and
`<!-- prog:skill:end -->`. A second run sees the marker and leaves the file
unchanged. It does not install hooks or generate an uninstall command that could
delete an owner-maintained `AGENTS.md`.

## Skill export

An unknown harness can consume the canonical instructions without any target
manifest or file write:

```bash
prog init --print-skill --frontmatter yaml
prog init --print-skill --frontmatter mdc
prog init --print-skill --frontmatter none
```

The skill is written directly to stdout. No store, project file, hook, or
manifest is created.

## Add a target without a prog release

Integration targets are data. The built-ins live in
`crates/prog-cli/integration-manifests/*.json`; an organization or repository can
add another target immediately by placing the same schema in its own directory:

```json
{
  "schema": "prog.integration_target",
  "agent": "zed",
  "skill_path": ".zed/skills/prog.md",
  "hook_dir": ".zed/prog-hooks",
  "frontmatter": "none",
  "write_mode": "create"
}
```

```bash
prog init --agent zed --manifest-dir ./integration-targets --project --dry-run
prog init --agent zed --manifest-dir ./integration-targets --project
```

Names must use lowercase letters, digits, and hyphens. Paths must be normalized,
project-relative paths and cannot contain `..`; duplicate built-in names are
rejected. `frontmatter` is `yaml`, `mdc`, or `none`. A normal `create` target
requires a hook directory and receives the same five reviewable files as the
first-party hook targets. `append_marker` is reserved for a single document and
cannot install hooks.

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

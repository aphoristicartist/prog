---
name: prog
description: Capture large command, API, MCP, and file output once and move through it with select, expand, and evidence; read receipts after test runs and never claim a fix prog has not proven.
---

# prog

prog keeps the full output and returns a small view plus a cursor. Move
through the cursor instead of rerunning or rereading.

Test, build, and lint runs (cargo, pytest, go, npm, ...) get a receipt.
Prefer this loop: run, read the receipt, use `prog evidence` for detail, and
claim only what the receipt proves. `fixed: X` is proven. `NOT PROVEN
(reason)`: a test was deleted, skipped, unnamed in quiet output, or fewer
ran. Rerun the full command verbosely, or tell the user it is not verified.

Large or messy data: `prog run -- gh api ...` or `prog observe --file f.json`.
The `overview` maps the records: fields, value counts, keys. Then:
- `prog select <cursor> --where state=open --fields id,title`; rows keep paths
- `prog expand <cursor> --path /3/body --limit 2000`; `prog search <cursor> text`;
  `prog inspect <cursor> --goal <goal>` ranks findings
- rerun the same command later: `changes_since` lists changed records, and
  `prog delta <old> <new>` lists all of them
- `prog coverage <cursor>` names the records not yet returned

Pass `--format compact` for smaller responses (the Claude Code plugin sets
it). Cite as `EvidenceRef: prog://<cursor>#<path>`. MCP is optional: prog is a
CLI plus this skill. More: `prog --help` and docs/agent-reference.md in the
prog repository.

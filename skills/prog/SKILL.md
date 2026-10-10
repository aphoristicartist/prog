---
name: prog
description: Read prog receipts after test, build, and lint runs; fetch exact evidence with prog evidence; never claim a fix prog has not proven.
---

# prog

prog records test, build, and lint runs (cargo, pytest, go, npm, ...) and adds
a short receipt after each. Run commands normally; `| tail` is fine, because
the full output is kept.

Prefer this loop: run, read the receipt, use `prog evidence` for detail, and
claim only what the receipt proves.

- `exit N · P passed, F failed: ids` and `first failure:` say what broke.
- `vs previous run: fixed: X`: the same command ran the same tests and X
  passed. That is proven.
- `NOT PROVEN (reason)`: a test was deleted, skipped, or ignored, fewer tests
  ran, or fail-fast stopped early. Rerun the full command (such as
  `cargo test --no-fail-fast`) or tell the user it is not verified.

The receipt's `prog evidence <cursor> --path <path>` returns the exact slice
offline. Cite it as `EvidenceRef: prog://<cursor>#<path>`. `prog receipt`
repeats the latest verdict. Without receipts, use `prog run -- <command>`; for
files, `prog observe --file <path>`, then `prog inspect <cursor> --goal <goal>`.

MCP is optional: prog is a CLI plus this skill. More: `prog --help` and
docs/agent-reference.md in the prog repository.

# Evidence navigation

Evidence navigation is offline and cursor-backed. `inspect`, `search`, `find`,
and `evidence` resolve the same validated cursor used by `expand`; they never
contact an upstream API or rerun a command.

```bash
prog run -- cargo test
prog inspect pc1_... --goal "find the root cause" --limit 5
prog search pc1_... "E0308" --path /failure_sections
prog find pc1_... --kind error
prog evidence pc1_... --path /failure_sections/0
```

`inspect` combines deterministic generic findings with the lens recorded on the
cursor. `--kind` filters semantic kinds, while `--path` scopes traversal inside
the cursor root. Missing, expired, foreign, redaction-version-mismatched, and
out-of-scope cursors fail through the same structured errors as `expand`.

`search` matches string values, keys, and JSON Pointer paths. Matching is
case-insensitive by default; `--regex` uses a size-bounded Rust regex. `find`
is the structural form for JSON types and semantic kinds such as `error`,
`warning`, and `test_failure`. Both commands cap traversal and report
`node_budget` omission metadata when results are partial.

`evidence` emits a compact citation-oriented block with a stable `EvidenceRef`,
bounded excerpt, parser line/byte ranges when known, safe source command,
redaction state, cache age, and expansion commands.

## Recipes and sessions

Recipes compose these primitives without becoming an agent runtime:

```bash
prog recipe cargo-test -- cargo test
prog recipe pytest -- pytest -q
prog recipe diff-review --file change.diff
prog recipe logs-root-cause --file service.log
```

Every navigation action is recorded as metadata in the current local session.
No payload body is copied into the trail.

```bash
prog session start --goal "debug checkout failure"
prog session show
prog session note "failure came from stale database credentials"
```

`cache purge --all` also purges session trails. `session show` marks expired or
missing cursor references instead of hiding them.

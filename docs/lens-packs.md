# First-party lens packs

`prog` ships a small in-tree lens pack in `lenses/`. Use it when the artifact
family is known and the next useful slice is not.

```bash
prog --lens-dir ./lenses run --lens run.failures -- cargo test
prog --lens-dir ./lenses observe --file service.log --mime text/plain --lens observe.text.logs
prog --lens-dir ./lenses call github list_issues --args '{}' --lens github.issues.triage
```

The first-party pack is deliberately boring: each lens is declarative, fixture
backed, and tested. A wrong lens fails through `match` checks instead of
silently producing a misleading preview.

## Included lenses

| Lens | Use |
|---|---|
| `run.failures` | Show command status, ranked failure sections, stdout/stderr heads, and expandable full streams. |
| `run.streams` | Show stdout/stderr heads and tails for successful or noisy command captures. |
| `observe.text.logs` | Show log head/tail, byte count, and line count while omitting expandable `/lines`. |
| `observe.ndjson.records` | Show bounded event records and omit expandable payloads/stacks. |
| `json.items.triage` | Triage observed JSON objects with an `/items` collection. |
| `github.issues.triage` | Triage issue-list responses while omitting expandable issue bodies and PR metadata. |

## Quality Rules

CI validates that:

- every top-level lens parses and passes `LensManifest` validation
- lens ids are unique
- every lens has positive and counterexample fixtures
- every lens declares invariants
- positive fixture projections are smaller than raw fixture payloads
- positive fixture projections beat a simple 2 KiB truncation baseline
- projected fixtures do not expose unredacted `plain-secret` counterexamples

CLI tests also exercise real `run`, `observe`, and `call` flows with first-party
lenses and expansion from the redacted cache.

## Counterexamples

Do not use the pack when a native query can return exactly the fields needed, a
known `jq` expression is clearer, the payload is tiny, or the workflow requires
live streaming output instead of cached expansion.

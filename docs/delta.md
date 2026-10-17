# Conservative observation delta

`prog delta <baseline> <subject>` compares two stored observations and reports
what changed between them, finding by finding.

Its defining property is that it is **conservative about absence**. When a
finding present in the baseline is missing from the subject, that is only
reported as `resolved` if `prog` can prove the subject run actually covered the
region where the finding would have appeared. Otherwise the finding is reported
as `not_observed` or `unknown`.

This matters in a verification loop. "The error is gone" and "I did not look
where the error was" produce identical raw output. `prog delta` refuses to
conflate them.

```sh
prog delta <baseline-observation-id> <subject-observation-id>
```

Observation ids come from any capturing command's `observation.observation_id`,
or from `prog cache observations`.

## Finding statuses

| Status | Meaning |
|---|---|
| `new` | Present in the subject, absent from the baseline. |
| `persisting` | Present in both. |
| `resolved` | Present in the baseline, absent from the subject, **and absence is provable**. |
| `not_observed` | Absent from the subject, but the subject's scope did not cover it. Not evidence of a fix. |
| `unknown` | Absent from the subject, and comparability could not be established at all. |

Findings are matched by fingerprint, not by path, so a finding that moves within
the payload is still recognized as the same finding.

### Truncation

`ObservationDelta.findings` is capped at 100 entries, most-severe-first:
`new` and `persisting` findings sort ahead of `resolved`, which sorts ahead of
`not_observed` and `unknown`. When a comparison produces more than 100
findings, the excess is dropped from `findings`, `ObservationDelta.truncated`
is set to `true`, and `counts` still reflects every finding in the full
comparison — not just the retained 100 — so the summary counts remain
trustworthy even when the finding list itself is cut short.

## What makes absence provable

`ComparabilityAssessment.can_prove_absence` is true only when **all** of the
following hold:

- **`invocation_match`** — both observations share a canonical invocation
  fingerprint. Two different commands are not a before/after pair.
- **`comparison_family`** matches on both sides.
- **`subject_identity` is `same`** — same `source_id` and `operation`.
- **`scope_relationship` is `equal` or `superset`** — the subject covered at
  least what the baseline covered.
- **Both captures are complete** — neither was truncated, byte-capped, or
  incompletely executed. A run that timed out or was cancelled records an
  incomplete capture even if bytes were captured.
- **Neither capture was redacted at a proof-bearing path** — redaction of
  stdout, stderr, argv, or call provenance forces
  `capture.can_prove_absence: false` with stop reason `redacted`. A secret
  removed from the output could have masked the finding, so a redacted
  capture is `unknown`/`not_observed`, never `resolved`.
- **`normalization_compatible`** — same provider, parser, and lens. A payload
  read through a different lens is not directly comparable.
- **`source_validity` is `confirmed_unchanged`.**
- **Both redacted payloads are still available** — retention eviction leaves the
  metadata but removes the ability to re-derive findings.
- **Selection coverage is exhaustive** — both observations declare at least one
  selection scope and assert `--selection-exhaustive`.
- **The same tests ran again** — when the Cargo/Rust or pytest coding provider
  recorded test results, the subject must have executed at least as many tests
  as the baseline (from the runner's own summary line), and every
  baseline-failing test must have run again and passed by name: a test that is
  missing, skipped, or ignored in the subject blocks the proof. Deleting or
  skipping the failing test produces exactly the clean output of a real fix, so
  it can never be reported as `resolved`.
- **Each baseline failure is named, and its pass is named too** — quiet output
  (`pytest -q`, `cargo test -q`) names only failures. "The failing test now
  passes" and "the failing test was deleted and a new passing test took its
  place in the count" print the same thing, so a quiet subject cannot prove a
  fix; the reason says to rerun verbosely (`pytest -v`, `cargo test` without
  `-q`). Likewise a baseline whose summary counts more failures than it names
  cannot match the unnamed ones to a later pass. libtest's closing `failures:`
  list names failing tests even in quiet mode, and is read for that.

The assessment always reports `reasons`, so a non-provable comparison explains
itself rather than failing silently. When a coding provider already knows how
to make its selection provable, its advice is repeated in `reasons` (for
example `baseline provider: Cargo's default fail-fast may skip later test
harnesses after a failure; use ... --no-fail-fast`). Scopes that completed
normally are not listed as stops, and findings that share a fingerprint within
one observation are disclosed instead of silently merged.

Pairwise source validity may come from equal scoped HTTP validators, equal
hashed profile change tokens, or equal hashed MCP modification annotations.
Cache age alone never supplies this proof; see
[source-state evidence](source-state.md).

`prog run`'s per-line finding derivation only examines the first and last 10
lines of stdout/stderr, even though the full output is captured and stored. A
finding whose evidence lies outside that head/tail window — for example, an
error line that moved from line 5 to line 15 across two runs — forces
`can_prove_absence: false` rather than a false `resolved`, even though the
full output is captured and retrievable via `prog evidence`. This does not
yet reclassify such a finding as `persisting`; that requires deriving
findings from the full text and is tracked as future work.

The same fail-closed rule applies to registered CLI/MCP text adapters that
retain only head/tail lines and to payloads that reach the generic finding
traversal's node or depth bound. Those observations record
`derivation_windowed`, so a missing finding becomes `unknown`, never
`resolved`. Within the traversal bound, delta derives every candidate before
applying its separate 100-entry disclosure cap; the cap can shorten the
returned `findings` list but cannot manufacture absence.

## Worked example

The same log goes from an error to a clean run. First, captured without any
comparability declarations:

```sh
printf 'INFO ok\nERROR checkout failed: timeout after 30s\n' > /tmp/svc.log
BASE=$(prog --dir /tmp/prog-delta run -- cat /tmp/svc.log \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["observation"]["observation_id"])')

printf 'INFO ok\nINFO checkout succeeded\n' > /tmp/svc.log
SUBJ=$(prog --dir /tmp/prog-delta run -- cat /tmp/svc.log \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["observation"]["observation_id"])')

prog --dir /tmp/prog-delta delta "$BASE" "$SUBJ"
```

The error is gone, but the delta declines to call it fixed:

```json
{
  "assessment": {
    "can_prove_absence": false,
    "reasons": ["selection coverage is unknown or not exhaustive"]
  },
  "findings": [{ "status": "unknown", "title": "log error" }]
}
```

Now the same two captures, with the comparability contract declared:

```sh
prog --dir /tmp/prog-delta2 run \
  --comparison-family checkout-log \
  --selection-scope checkout \
  --selection-exhaustive \
  -- cat /tmp/svc.log
```

With an identical invocation, a shared family, and an exhaustive scope on both
sides, absence becomes provable:

```json
{
  "assessment": { "can_prove_absence": true },
  "counts": { "resolved": 2 },
  "findings": [
    { "status": "resolved", "title": "log error", "baseline_path": "/stdout/head/1" },
    { "status": "resolved", "title": "log error", "baseline_path": "/stdout/text" }
  ]
}
```

Nothing about the payloads changed between these two runs. What changed is
whether `prog` was given enough information to make a claim.

## Using delta in a loop

Declare the comparison contract at capture time, on the *first* observation —
you cannot retrofit it later, because the fingerprint and selection metadata are
part of the stored record.

```sh
# Iteration 1: capture the failing baseline.
prog run --comparison-family test-suite --selection-scope unit --selection-exhaustive -- cargo test

# ... make an edit ...

# Iteration 2: capture the verification run identically.
prog run --comparison-family test-suite --selection-scope unit --selection-exhaustive -- cargo test

# Ask what actually changed.
prog delta "$BASELINE_ID" "$SUBJECT_ID"
```

A targeted rerun — for example `cargo test --test one_file` — deliberately
produces `not_observed` rather than `resolved` for findings outside its scope.
That is the intended behavior: a narrow rerun cannot clear a broad baseline.

To bind a specific disappearance to an explicit success criterion, declare a
verification obligation instead; see [`verification.md`](verification.md).

## Record deltas

When both payloads hold the same collection of records — a JSON array from
`prog observe`, parsed JSON or JSON Lines stdout from `prog run`, or an API
response from `prog call` — `prog delta` also compares the records by key and
returns a `records` block:

```sh
prog delta "$BEFORE" "$AFTER"                 # key inferred
prog delta "$BEFORE" "$AFTER" --key number    # key named (a pointer: metadata/uid)
prog delta "$BEFORE" "$AFTER" --records-path /items
```

```json
"records": {
  "status": "compared",
  "path": "/stdout/json",
  "key": "/id",
  "counts": {"added": 1, "removed": 0, "changed": 3, "unchanged": 297},
  "added": [{"key": 2000, "path": "/stdout/json/300"}],
  "changed": [{"key": 1017, "baseline_path": "/stdout/json/16", "subject_path": "/stdout/json/16",
               "fields": [{"field": "/state", "before": "open", "after": "closed"}]}],
  "reasons": ["added and removed describe the captured collections only; ..."]
}
```

The comparison reads the whole persisted payloads, not a preview, so its counts
are exact for what was captured. It is conservative in the same way as the
finding delta:

- The key must be present, unredacted, and unique in every record on both
  sides; otherwise the status is `unkeyed` and nothing is listed. An inferred
  key prefers `id`, `uid`, `uuid`, `key`, `number`, `node_id`, `name`, and
  `slug`, top-level fields first.
- Either capture stopping early (byte limit, timeout, cancellation) makes the
  status `incomplete`: a record missing from a cut capture proves nothing.
  Redaction and windowed finding derivation do not hide records, because the
  comparison reads every stored record.
- "Removed" means absent from the subject's *captured* collection. Unless both
  captures declared an exhaustive selection, the reasons say so: an API that
  pages or filters results can drop a record that still exists.
- A record holding redacted values is compared as stored; a change inside a
  redacted value is not visible, and the reasons count such records.

Capturing the same command, file, or call again in a session attaches the same
comparison to the new envelope as `changes_since` (only changed findings and
records are listed, at most five of each; counts are always complete), so rerunning `prog run -- gh api ...` answers "what changed
since I last looked" without a separate step.

## Related

- [Verification obligations](verification.md) — gate readiness on a delta result.
- [Evidence and observations](evidence.md) — observation records and lineage.
- [Replay evaluation](replay-eval.md) — the oracle that gates delta correctness
  across multi-iteration trajectories.
- `prog meta ObservationDelta`, `prog meta ComparabilityAssessment`,
  `prog meta DeltaFinding` — generated contract schemas.

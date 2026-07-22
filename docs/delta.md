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
- **Both captures are complete** — neither was truncated or byte-capped.
- **`normalization_compatible`** — same provider, parser, and lens. A payload
  read through a different lens is not directly comparable.
- **`source_validity` is `confirmed_unchanged`.**
- **Both redacted payloads are still available** — retention eviction leaves the
  metadata but removes the ability to re-derive findings.
- **Selection coverage is exhaustive** — both observations declare at least one
  selection scope and assert `--selection-exhaustive`.

The assessment always reports `reasons`, so a non-provable comparison explains
itself rather than failing silently.

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

## Related

- [Verification obligations](verification.md) — gate readiness on a delta result.
- [Evidence and observations](evidence.md) — observation records and lineage.
- [Replay evaluation](replay-eval.md) — the oracle that gates delta correctness
  across multi-iteration trajectories.
- `prog meta ObservationDelta`, `prog meta ComparabilityAssessment`,
  `prog meta DeltaFinding` — generated contract schemas.

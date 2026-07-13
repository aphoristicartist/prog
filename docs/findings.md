# Findings ranking

`prog-core` ships a generic evidence-ranking engine
([`crates/prog-core/src/findings.rs`](../crates/prog-core/src/findings.rs)) that
projects a ranked, navigable view over an **already-redacted and stored**
payload. It is the read-only optic behind `prog inspect`, initial envelope
findings, lens finding providers, and `prog evidence`.

The engine is **pure, store-less, and deterministic**. It never mutates cursor
state (effect policy I6/I7 are untouched: ranking is a read-only projection),
never fabricates cursors (fail-closed `pc1_` cursors, I9), and only ever reads
text that has already been through redaction. Reason and title text are static
literals — payload values are never echoed into reasons, and secrets are never
persisted or replayed.

## Entry points

```rust
// Low-level stable primitive: pure, deterministic, no store.
pub fn ranked_findings(payload: &Value, options: &FindingOptions)
    -> Result<Vec<Finding>>;

// Full response assembly used by the `prog inspect` CLI.
pub fn build_inspect_response(payload: &Value, request: &InspectRequest)
    -> Result<InspectResponse>;
```

`InspectRequest { goal, cursor (required), scope_path, limit, hints }` is an
**input** boundary type. It is *not* a contract type and is *not* serialized as
part of `InspectResponse`; it lives at the request edge so the non-optional
`InspectResponse::cursor` is enforced before assembly instead of being
panic-recovered later. Use `InspectRequest::builder(cursor)` to construct one.

`schema` on the response is stamped from the existing
`prog.inspect` const (`INSPECT_SCHEMA` in `contracts.rs`) — there is no
second version constant to drift.

## How a payload becomes findings

1. **Scope.** `options.scope_path` (a JSON Pointer) is resolved with
   `pointer::get`. A missing scope yields no findings; an invalid pointer errors
   `bad_pointer`. An empty `scope_path` (`""`) ranks the whole payload.
2. **Collect.** Two passes walk the scoped value under hard 10,000-node and
   64-level traversal caps:
   - `collect_run_signals` — recognizes the `run` shape (`command` metadata +
     `failure_sections`).
   - `collect_generic_signals` — walks every object/array/string recursively,
     classifying by field name (`key_signal`), object severity
     (`collect_object_level_signal`), and string contents (`string_signal`).
3. **Dedup.** Candidates are folded into a `BTreeMap<(path, kind), Candidate>`
   keeping only the best candidate per `(path, kind)` pair.
4. **Score.** Each candidate gets `score = confidence + kind_bonus(kind, intent)
   + adjustments`, clamped to `[0, 1.25]`.
5. **Order.** `sort_by(compare_candidates)` and truncate to `options.limit`
   (default `10`). Ranks are assigned `1..=len`.

## Signal kinds

The detector ladder is ordered most-specific first so that, for example, a rustc
diagnostic is never double-counted as a generic compile error.

| Kind | Source | Meaning |
|------|--------|---------|
| `command_timeout` | run.command / failure_sections | command metadata reports a timeout |
| `command_spawn_error` | run.command / failure_sections | command metadata reports a spawn error |
| `nonzero_exit` | run.command | command exited unsuccessfully |
| `rust_compile_error` | string / failure_sections | `error[Ennnn]` rustc diagnostic marker |
| `compile_error` | string / failure_sections | generic build framing: `could not compile`, `cargo: error`, `rustc: error`, `gmake`/`ninja` + error |
| `rust_panic` | string / failure_sections | `panicked at` |
| `python_traceback` | failure_sections | `Traceback (most recent call last)` inside a run failure section |
| `stack_trace` | string | a traceback marker in arbitrary text |
| `test_failure` | string / failure_sections / field | `AssertionError` / `assertion failed`, or a `failure` field |
| `test_name` | string / failure_sections | pytest nodeid (`x.py::test_case`), cargo result line (`test foo ... FAILED`), `--gtest_filter` / "Google Test filter", mocha/jest counts (`2 passing`, `1 failing`) |
| `exception` | string / failure_sections / field | generic exception marker or field |
| `stderr_error` | string / failure_sections | generic `error:` / ` failed` text |
| `diff_hunk` | string / field | unified-diff markers (`diff --git`, `@@ ... @@`, `+++ `/`--- ` headers); severity `None` — diffs are evidence to review, not errors |
| `generic_error_field` | field | an `error`/`errors` field |
| `diagnostic` | object severity / field | object `severity`/`level`/`status` is `error`/`failed`/`failure`/`critical`/`fatal`, or a `diagnostic` field |
| `warning` | string / field | `warning:` text or a `warning` field |

**Precedence note.** `rust_compile_error` (`error[`) is checked *before*
`compile_error`, so `error[E0308]: mismatched types` classifies as
`rust_compile_error`, never double-counted as the generic `compile_error`.

## Intent from the goal

The free-text `goal` is normalized (lower-cased; `_`/`-` -> space) and matched
against keyword buckets, first match wins:

| GoalIntent | Keyword bucket | `as_str()` |
|------------|----------------|------------|
| `RootCause` | `root cause`, `why`, `debug`, `fix` | `root_cause` |
| `TestFailure` | `test`, `pytest`, `cargo`, `fail` | `test_failure` |
| `SummarizeIssues` | `issue`, `summar`, `triage` | `summarize_issues` |
| `Security` | `security`, `secret`, `vulnerab`, `cve` | `security` |
| `Logs` | `log` | `logs` |
| `DiffReview` | `diff`, `review` | `diff_review` |
| `General` | (otherwise) | `general` |

`normalized_goal(goal)` projects the `as_str()` of the inferred intent (or
`None` for an empty goal).

## `kind_bonus(kind, intent)` table

The additive bonus the intent pays each kind. Rows not listed fall through to the
intent's default arm (shown via `_` in the column header).

| kind \ intent | RootCause | TestFailure | SummarizeIssues | Security | Logs | DiffReview | General |
|---------------|-----------|-------------|-----------------|----------|------|------------|---------|
| `rust_compile_error` | +0.12 | +0.14 | 0 | 0 | 0 | 0 | 0 |
| `compile_error` | +0.12 | +0.14 | 0 | 0 | 0 | 0 | 0 |
| `rust_panic` | +0.12 | +0.14 | 0 | 0 | 0 | 0 | 0 |
| `python_traceback` | +0.12 | +0.14 | 0 | 0 | 0 | 0 | 0 |
| `command_timeout` | +0.12 | _ +0.02 | 0 | 0 | 0 | 0 | 0 |
| `command_spawn_error` | +0.12 | _ +0.02 | 0 | 0 | 0 | 0 | 0 |
| `test_failure` | +0.12 | +0.14 | 0 | 0 | 0 | 0 | 0 |
| `test_name` | +0.04 | +0.10 | 0 | 0 | 0 | 0 | 0 |
| `nonzero_exit` | +0.02 | _ +0.02 | 0 | 0 | 0 | 0 | 0 |
| `diff_hunk` | -0.05 | _ +0.02 | 0 | 0 | 0 | **+0.14** | 0 |
| `warning` | -0.08 | -0.08 | +0.06 | 0 | +0.08 | +0.04 | 0 |
| `generic_error_field` | _ +0.04 | _ +0.02 | +0.06 | +0.04 | 0 | 0 | 0 |
| `diagnostic` | _ +0.04 | _ +0.02 | +0.06 | +0.04 | 0 | +0.04 | 0 |
| `stack_trace` | _ +0.04 | _ +0.02 | 0 | 0 | +0.08 | 0 | 0 |
| `stderr_error` | _ +0.04 | _ +0.02 | 0 | 0 | +0.08 | 0 | 0 |
| `exception` | _ +0.04 | _ +0.02 | 0 | 0 | +0.08 | 0 | 0 |

`_ +0.0X` means the kind falls through to the intent's wildcard arm (RootCause
default `+0.04`, TestFailure default `+0.02`, all others `0.0`).

### Score adjustments

In addition to `kind_bonus`:

- **`+0.08`** when `candidate.source == "generic.run.failure_sections"` — run
  failure sections are the most reliable signal source.
- **`-0.04`** when `candidate.path` ends with `/text` — a raw text blob is
  usually a less specific pointer than a structured failure section.
- The total is clamped to `[0.0, 1.25]`.

### Rounding and truncation

- `confidence` is rounded to 2 decimal places via `round_confidence`
  (`(clamp(0,1) * 100).round() / 100`) and is always within `[0, 1]`.
- `reason` is truncated to `MAX_REASON_CHARS = 180` chars (appending `...`).

## Tie-break chain

When scores tie, `compare_candidates` resolves deterministically:

1. **score** descending
2. **confidence** descending
3. **path** ascending (lexicographic)
4. **kind** ascending (lexicographic)

Because this is a total order and dedup uses a `BTreeMap` keyed by
`(path, kind)`, ranking is **order-independent of JSON key insertion order** —
pinned by the property tests in
[`crates/prog-core/tests/findings_proptest.rs`](../crates/prog-core/tests/findings_proptest.rs)
and recorded in [`INVARIANTS.md`](../INVARIANTS.md).

## Worked example: predicting rank #1

Fixture `tests/fixtures/findings/run-failure.json` with goal
`"why did the run fail"` (intent -> `RootCause` because of `why`).

The Python failure section becomes a candidate:

- `confidence = (0.78 + priority*0.002).clamp(signal_confidence, 0.99)`
  with `priority = 90` and `signal_confidence = 0.92` -> `0.96`.
- `kind_bonus(python_traceback, RootCause) = +0.12`.
- source `generic.run.failure_sections` -> `+0.08`.
- path `/failure_sections/0` does not end in `/text` -> no penalty.
- `score = 0.96 + 0.12 + 0.08 = 1.16` (clamped well within `[0, 1.25]`).

No other candidate reaches that score, so rank #1 is
`{ kind: "python_traceback", path: "/failure_sections/0", confidence: 0.96 }`,
which is exactly what the
[`run-failure.expected.json`](../crates/prog-core/tests/fixtures/findings/run-failure.expected.json)
golden snapshot asserts. Changing the goal to `"review the diff"` over
`diff-review.json` instead promotes `diff_hunk` to rank #1 (DiffReview bonus
`+0.14`), again matching the golden.

## Command hint gating

Each `Finding` carries `commands: FindingCommandHints` populated from
`options.hints: CommandHintConfig`:

- `CommandHintConfig::NAV_EXPAND_ONLY` (the **default**) emits only
  `prog expand {cursor} --path {path}` for library callers that want the
  narrowest compatibility surface.
- `CommandHintConfig::NAV_ALL` additionally emits `prog inspect`, `prog
  evidence`, and a runnable semantic `prog find --kind ...` hint.

CLI envelopes and `inspect` use `NAV_ALL`; low-level library callers retain the
minimal default. Every emitted command is runnable: the `search` hint field uses
`prog find <cursor> --kind <kind> --path <path>` because a text search command
cannot honestly invent a query.

The evidence-acquisition pipeline is documented
separately in [`evidence.md`](./evidence.md).

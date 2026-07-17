# RFC 0003: Observation Lenses and Agent Optics

- Status: Draft
- Date: 2026-07-05
- Owner: aphoristicartist
- Repository: https://github.com/aphoristicartist/prog
- Related issues: #35, #38, #40, #41, #42, #43, #44, #45, #46, #47, #48, #49, #50
- Amends: [RFC 0001](0001-progressive-disclosure-gateway.md), [RFC 0002](0002-type-theory-formal-methods-and-reflexivity.md)

## Summary

`prog` should be understood as an **observation optics runtime** for agents, not
as a generic compressor.

Agents work with many artifacts that were not designed for conversational
reasoning: HTTP responses, CLI output, MCP results, logs, traces, test reports,
Git diffs, PR review threads, browser DOMs, HTML, SARIF, JUnit XML, documents,
spreadsheets, database query results, and previous agent transcripts. The
common failure mode is not just size. The agent receives an unbounded artifact
before it knows which parts matter.

This RFC generalizes RFC 0001's source-response progressive disclosure into a
portable lens model:

```text
capture -> redact -> index -> view -> paths -> expand -> export
```

The first view is bounded and truthful. Later inspection is exact, targeted,
cursor-backed, and provenance-aware. Compression is one policy inside this
runtime; the durable product is recoverable, structured observation.

## Product Thesis

As of July 2026, the agent ecosystem has several partial answers:

- large-context models reduce hard context-window failures, but do not remove
  cost, attention pollution, privacy risk, or repeated-tool waste
- native API field selection, pagination, `jq`, and shell filters are best when
  the query is already known
- RTK-style command interception shows that transparent hooks are a strong
  adoption path for noisy terminal output
- Caveman-style terse response discipline reduces assistant output, but does
  not solve oversized tool-result input
- MCP gateways and deferred tool schemas help with tool catalogs, but not
  uniformly with huge or messy tool results

`prog`'s niche is the moment before the query is known:

1. capture the full redaction-safe artifact once
2. show a bounded, loss-aware view
3. list useful expandable paths
4. let the agent inspect exact evidence without rerunning the upstream source

This matters most with expensive long-context models. A Fable-class model
should reason over bounded evidence, not pay to ingest complete raw blobs by
default. Pricing and model names change, so `prog` should model this through
configurable cost profiles rather than hard-coded vendor facts.

## Definitions

### Observation

An `Observation` is a captured artifact after policy-sensitive processing.

It records:

- artifact identity: source id, operation, command, file path, URL, or generated
  observation id
- artifact kind: JSON, text, NDJSON, diff, XML, SARIF, HTML, command run, or
  unknown
- captured time, cwd, environment references, and invocation metadata where
  relevant
- parser and indexer metadata
- cache key and cursor roots
- safety, freshness, trust, completeness, lossiness, and cost metadata

An observation is not necessarily a public document. The agent-visible public
contract remains the bounded envelope and targeted expansions.

### Lens

A `Lens` is a deterministic get-only optic from an observation to a bounded
agent view.

It is not a full bidirectional lens. There is no `put` operation back into the
source. The useful laws are:

- no fabricated visible values
- expansion remains inside the cursor's provenance boundary
- redaction dominates every view and expansion
- lossy transforms are explicit in metadata
- the same observation plus the same lens policy yields the same visible view

### LensManifest

A `LensManifest` is a small declarative contract for applying a reusable lens.
It should be YAML or JSON, committable, reviewable, and safe to load.

V1 should not be a general programming language. It should describe matching,
viewing, omitting, redacting, expansion paths, next actions, and test fixtures.
If an arbitrary program is needed, it belongs in a trusted adapter or parser,
not inside the manifest.

### ExpandablePath

An `ExpandablePath` is a stable address into the redacted stored observation.
For JSON-like trees it should use JSON Pointer where possible. For text-backed
artifacts it should use explicit line, byte, or logical-section paths. Other
parsers may expose typed paths if they can remain stable and explainable.

### EvidenceRef

An `EvidenceRef` is the compact citation form agents can attach to conclusions:

- observation or source id
- operation or artifact name
- cursor
- path or range
- captured timestamp
- freshness and cache status
- redaction and lossiness flags
- optional hash of the redacted slice

It must never become a capability that bypasses cursor safety.

## Lifecycle

### 1. Capture

Capture receives an upstream result from one of three routes:

- profile-backed adapters: `prog call`
- profile-free artifacts: `prog observe`
- command wrapper: `prog run -- <command...>`

Capture records invocation metadata before parsing. For command runs, this
includes argv, cwd, start/end time, duration, stdout, stderr, and exit status.

### 2. Redact

Redaction remains before persistence and before shape inference:

```text
capture -> redact -> infer/index -> store -> project
```

No lens may weaken this ordering. If a parser needs raw bytes to recognize a
format, it may do bounded pre-redaction sniffing, but parsed values and stored
artifacts must use redacted content.

### 3. Index

Indexing turns the redacted artifact into an inspectable structure:

- JSON: tree paths and shape hints
- NDJSON: record paths plus per-line ranges
- text/logs: line ranges, head/tail, repeated sections, error clusters
- diffs: files, hunks, added/removed context
- XML reports: suites, cases, failures, messages, logs
- SARIF: runs, rules, results, locations, snippets
- HTML: title, headings, links, text sections, selected attributes

Indexing must report parser confidence and lossiness. Unsupported artifacts
fall back to bounded text observation where safe.

### 4. View

The first view is a `DisclosureEnvelope`:

- summary
- bounded preview
- omitted regions
- schema or structure hints
- cursor
- next actions
- warnings
- provenance
- cache and freshness metadata

The hard budget contract from RFC 0001 still applies.

### 5. Paths

`prog paths <cursor>` should list expandable paths without contacting the
upstream source.

Path listings should be:

- bounded
- deterministic
- ranked when a lens can estimate usefulness
- filterable by prefix, field name, omission reason, parser kind, confidence,
  shape hint, or safety metadata
- explicit about why each path was omitted or why expansion might help

### 6. Expand

Expansion returns a bounded slice from the redacted stored observation:

```bash
prog expand pc1_... --path /items/0/body --depth 4
```

Expansion does not rerun the source by default. It should include an
`EvidenceRef` so agents can cite the basis for a conclusion without pasting the
entire raw artifact into context.

### 7. Export

Bulk work should go to files, not the model context:

```bash
prog expand pc1_... --path /items --out /tmp/issues.json
```

Export still respects redaction and cursor boundaries.

## LensManifest V1

The manifest should be intentionally small:

```yaml
id: github.issues.triage

match:
  source_kind: http
  operation: list_issues
  mime: application/json

view:
  root: /items
  limit: 20
  fields:
    number: /number
    title: /title
    state: /state
    updated_at: /updated_at
    labels: /labels/*/name

omit:
  - path: /items/*/body
    reason: large_text
    expandable: true
  - path: /items/*/user
    reason: low_value_nested_object
    expandable: true

next_actions:
  - kind: expand
    path: /items/{index}/body
    reason: inspect issue body only if title or labels look relevant
  - kind: call
    operation: list_issue_comments
    reason: comments are separate evidence when discussion matters

invariants:
  - envelope_under_budget
  - no_fabricated_values
  - redaction_dominates_expansion
  - every_omission_has_reason

fixtures:
  positive:
    - fixtures/http/issues-large.json
  negative:
    - fixtures/http/users-small.json
```

The compiler should reject manifests that:

- include executable code
- use unsupported selector syntax
- reference paths outside the lens root
- attempt to show redacted classes
- make the envelope budget impossible to satisfy without explicit lossiness
- omit required counterexample fixtures for first-party lenses

## Selector Strategy

V1 should prefer existing path conventions:

| Artifact | Primary selector | Notes |
|---|---|---|
| JSON | JSON Pointer | Exact expansion path. |
| JSON collections | JSON Pointer plus pointer globs in manifests | Globs compile to concrete paths after indexing. |
| NDJSON | Record index plus JSON Pointer | Must preserve line provenance. |
| Text/logs | Line/range paths | Use stable logical sections when parser confidence is high. |
| Unified diff | File and hunk paths | Expansion can reveal full hunks or file-level diff. |
| XML reports | Parser tree paths | Expose original text range when possible. |
| HTML | DOM-like paths plus text-section paths | No browser rendering in V1. |

Do not invent a general query language until repeated first-party lenses prove
that JSON Pointer, pointer globs, and range paths are insufficient.

## Comparison Matrix

| Approach | Best At | Weakness | Relationship To prog |
|---|---|---|---|
| Native field selection and pagination | Exact known queries | Requires knowing the useful slice first | Prefer native when the query is already known; use `prog` while exploring. |
| `jq` and shell filters | Local deterministic transforms | Easy to drop needed evidence or leak secrets | Useful baseline and export consumer. |
| RTK-style hooks | Low-friction terminal adoption | Usually lossy and command-domain-specific | Copy the hook ergonomics; keep `prog` views recoverable by cursor. |
| Caveman-style terse replies | Reducing assistant output | Does not reduce tool-result input | Apply as skill/report discipline on top of `prog`. |
| MCP gateways | Tool catalog and protocol integration | Does not guarantee result-side progressive disclosure | Optional adapter surface, not the core strategy. |
| Large-context expensive models | Long reasoning over much evidence | Cost and attention still scale with raw input | Feed bounded observations first; expand only needed evidence. |
| Raw context | Simple and complete | Expensive, noisy, unsafe for secrets | Keep as a baseline and counterexample. |

## When To Use prog

Use `prog` when:

- the response is large or messy and the agent does not yet know the useful path
- the source is expensive, rate-limited, flaky, or slow to rerun
- a bounded first view is safer than raw output
- the agent may need to inspect multiple slices over time
- conclusions need cursor/path-backed evidence
- hooks can route repeated noisy commands through a consistent observation loop
- an expensive model should reason over selected evidence rather than raw blobs

## When Not To Use prog

Do not use `prog` when:

- the payload is tiny
- the upstream API already returns exactly the needed fields
- a one-line `jq` query is known and enough
- the user explicitly needs the complete raw artifact in the terminal
- the command is interactive or TTY-dependent
- the data must be streamed and cannot be safely cached
- the metadata overhead exceeds the observation size

These are not failures. They are required counterexamples for honest evals.

## Agent Workflow

The agent-facing loop should become:

```text
observe/call/run -> inspect summary -> paths -> expand selected evidence -> answer with EvidenceRefs
```

Examples:

```bash
prog run -- cargo test
prog paths pc1_... --reason failure
prog expand pc1_... --path /stderr/failures/0 --depth 3
```

```bash
gh api repos/OWNER/REPO/issues \
  | jq '{items: .}' \
  | prog observe --stdin --mime application/json --name list_issues --lens github.issues.triage
prog paths pc1_... --field body
prog expand pc1_... --path /items/7/body
```

The skill should teach agents to avoid dumping raw payloads, to inspect paths
before guessing, and to cite cursor/path evidence when the answer depends on a
specific expansion.

## Expensive Model Pattern

The intended architecture for Fable-class and other expensive long-context
models is:

```text
cheap/local collector -> prog observation -> expensive model reasoning -> targeted expansion
```

The expensive model should receive:

- bounded envelope
- ranked paths
- cost estimate for expanding
- exact evidence on demand

It should not receive:

- raw full API dumps by default
- entire logs when only one failure matters
- repeated identical payloads after cache hits
- secrets or redacted fields

Cost reports should be profile-driven:

```json
{
  "model": "fable-class-2026-07",
  "input_price_per_million_tokens": 10.0,
  "output_price_per_million_tokens": 50.0,
  "context_window_tokens": 1000000,
  "source": "user-maintained profile",
  "priced_at": "2026-07-05"
}
```

The numeric values are inputs to an eval profile, not stable product constants.

## Quality Metadata

Every envelope and expansion should eventually expose enough metadata for an
agent to know whether it can rely on the view:

- `complete`: whether the visible view is complete for the selected path
- `freshness`: captured time, TTL, staleness warnings, refresh route
- `trust`: declared schema, observed shape, parser confidence, source profile
  provenance
- `lossiness`: truncation, sampling, summarization, parser fallback, hidden
  sections
- `safety`: redaction classes applied, sensitive omission, mutating/source
  effects
- `cost`: approximate visible tokens, raw artifact tokens, expansion cost
  estimate where a model profile is configured

This extends issue #37. The metadata must be machine-readable, not only prose.

## Invariants

The following are normative:

| # | Invariant | Required Tests |
|---|---|---|
| O1 | Bounded first view | Unit and integration tests for every parser/lens. |
| O2 | No visible value is fabricated | Property tests over JSON and parser fixtures. |
| O3 | Redaction dominates view, paths, expansion, and export | Secret fixtures for every artifact kind. |
| O4 | Expansion remains inside cursor provenance | Boundary and path traversal tests. |
| O5 | Same observation plus same lens yields same envelope | Snapshot or structural determinism tests. |
| O6 | Every omission has a reason | Envelope tests and manifest compiler checks. |
| O7 | Expandable omissions have expansion routes | `paths` tests for omitted regions. |
| O8 | Lossiness is explicit | Parser fallback and truncation tests. |
| O9 | Unknown effects and unsafe hooks fail closed | Policy tests for `run`, `observe`, and adapters. |
| O10 | Cost claims are generated from measurements | Eval tests with raw metrics artifacts. |

## Counterexamples

The eval suite must include cases where `prog` loses:

- a 500-byte payload where the envelope is larger than raw
- an API call with exact native field selection
- a known `jq` query that extracts the answer directly
- a command where the user needs real-time streaming output
- a text artifact where parser confidence is low and only fallback text is safe
- a task where one expansion reveals almost the whole artifact
- a low-cost model run where latency matters more than token savings

The docs should use these counterexamples to build trust rather than hide them.

## Implementation Slices

1. **RFC and contracts**: this document plus `LensManifest`, `Observation`,
   `ExpandablePath`, and `EvidenceRef` contracts.
2. **Path discovery**: `prog paths <cursor>` over existing cached JSON payloads.
3. **Profile-free observe**: `prog observe` for stdin and file-backed JSON/text.
4. **Command wrapper**: `prog run -- <command...>` with recoverable output
   control.
5. **Parser/indexer pipeline**: JSON, NDJSON, text, diffs, XML reports, SARIF,
   and HTML metadata.
6. **First-party lens packs**: Rust, JavaScript/TypeScript, Python, GitHub,
   Git, logs, JUnit, SARIF, and generic large JSON.
7. **Hooks and skills**: installer plus agent skill updates for CLI + skill +
   hooks first, MCP optional.
8. **Cost planner**: configurable expensive-model profiles and cache-aware
   savings reports.
9. **Competitive evals**: raw, truncation, native filters, RTK-style filters,
   Caveman-style output discipline, and `prog` paths/expand.
10. **Profile importers**: OpenAPI, JSON Schema, MCP schemas, CLI help, and
    examples as priors.

## Compatibility

RFC 0001 remains valid. Its HTTP/CLI/MCP `call -> expand` loop is the first
implementation of this more general observation model.

New surfaces should reuse the existing contracts wherever possible:

- `DisclosureEnvelope`
- `Summary`
- `OmittedRegion`
- `NextAction`
- `CursorRecord`
- `CacheEntryMeta`
- `CallProvenance`
- `CacheInfo`

New fields must preserve forward compatibility. Older consumers should be able
to ignore `observation`, `lens`, `evidence_ref`, and quality metadata fields
without breaking core envelope handling.

## Decision Defaults

- Do not build a large DSL before manifest v1 proves insufficient.
- Keep CLI + skill + hooks first-class. MCP is optional compatibility.
- Prefer lossless recoverability over prettier lossy summaries.
- Prefer exact native filters when the desired query is already known.
- Prefer files for bulk export, not model context.
- Treat pricing data as user-maintained inputs, not stable constants.
- Make every marketing claim traceable to checked-in eval metrics.

## References

- RTK: https://github.com/rtk-ai/rtk
- Caveman: https://github.com/juliusbrussee/caveman
- MCP specification: https://modelcontextprotocol.io/specification
- Anthropic model and pricing documentation: https://platform.claude.com/docs/

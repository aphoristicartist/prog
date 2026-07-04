# RFC 0002: Type Theory, Formal Methods, and the Reflexive Meta-Tower

- Status: Accepted
- Date: 2026-07-03
- Owner: aphoristicartist
- Amends: RFC 0001

This addendum records the research verdicts requested before implementation:
do we benefit from type-theory inspirations or formal proofs, and how do we
get "double / triple / multi" meta layers without building N special layers?

## 1. Landscape check (July 2026)

Progressive disclosure is now the default context pattern for production
agents. Anthropic's code-execution-with-MCP pattern reports ~98.7% input-token
reduction; independent implementations report 85–100x. The ecosystem solved
this for *tool catalogs* (deferred tool schemas, tool search). Nothing popular
solves it uniformly for *tool results* across HTTP + CLI + MCP — that is
`prog`'s gap, and RFC 0001 aims at it correctly.

Relevant platform facts we build against:

- MCP spec `2025-11-25` (stable) has structured tool output: tools may declare
  `outputSchema`, and results carry `structuredContent` validating against it.
  The `2026-07-28` release candidate lifts schemas to full JSON Schema 2020-12
  and allows any JSON value in `structuredContent`. The MCP adapter must
  harvest `outputSchema` into `OperationProfile.output_shape` as a *trusted
  prior* that observation then refines.
- Official Rust SDK `rmcp` is at 2.x — use it for the MCP client.
- Implementations must not auto-dereference external `$ref` URIs and should
  bound schema depth/validation time. We adopt the same rule for imported
  schemas.

## 2. Verdict: type theory — yes, exactly three imports

Type theory pays for itself here only where a law becomes a test. Three
structures qualify. Everything else in the literature (dependent types,
session types, full refinement typing) is cost without benefit for V1.

### 2.1 Shapes form a bounded join-semilattice (CUE-style)

The internal `Shape` model from RFC 0001 is formalized as a join-semilattice
with `Unknown` as the identity (bottom of the information order) and `join`
as least-upper-bound-style widening:

```text
join(Unknown, x)            = x
join(x, x)                  = x
join(Integer, Number)       = Number
join(Null, x)               = Nullable(x)          (rendered "T | null")
join(Object a, Object b)    = fieldwise join; one-sided fields become optional
join(Array a, Array b)      = Array(join(elem a, elem b))
incompatible scalars        = Union, flattened and canonicalized
```

Why this matters operationally, not academically:

- **Order independence.** Because join is commutative, associative, and
  idempotent, learning from responses in any order, with duplicates, converges
  to the same profile. Re-running `prog discover` can never corrupt a profile,
  only refine it — which is exactly issue #9's acceptance criterion.
- **Monotone learning.** `join(a, b) ⊒ a` gives "schema refinement is
  monotonic" (issue #3) as an algebraic law instead of a code-review hope.
- **Honest uncertainty.** `Unknown` and `rest: Unknown` on objects are
  first-class (gradual typing + row polymorphism), so hints can say
  "there are more fields here" truthfully.

Prior art: CUE's value lattice proves this design is practical for
configuration-scale data; F#-style type providers prove example-driven
structural inference is useful to consumers.

### 2.2 The disclosure lens is a *get-only* optic

RFC 0001 called the disclosure layer a lens. Precision: it is a **projection
(get-only optic)** — there is no `put` back into the source. That kills the
hard lens laws (GetPut/PutGet) and leaves three cheap, testable laws:

1. **No fabrication.** Every value in a preview/expansion is drawn from the
   cached source payload; string truncation is explicit and marked.
2. **Boundary containment.** Expansion resolves strictly within the cursor's
   root path in the same cached payload (provenance boundary).
3. **Redaction dominance.** Redaction composes *before* projection; no
   composition of expand/slice operations can recover a redacted value.

These three laws *are* the property-test suite for issues #4, #5, #12.

### 2.3 Effects are a set with a fail-closed order

Effects (`read_only`, `mutating`, `network`, `shell`, `sensitive`,
`cacheable`, `requires_confirmation`) form a set where policy checks are
monotone: adding an effect can only remove permissions, never add them.
Unknown effect ⇒ treated as worst case. Plain Rust structs + policy
functions; no effect-system machinery.

## 3. Verdict: formal proofs — property tests now, proofs on a zero-rewrite path

Full formal verification (TLA+/Alloy/Coq) is rejected for V1: the state
machine (cache, cursor, redaction) is small and the invariants are data laws,
not concurrency laws. Instead:

- **proptest** harnesses for: join commutativity/associativity/idempotence/
  identity/monotonicity; projection-never-invents; redaction idempotence and
  non-reappearance; cursor boundary containment; JSON Pointer slice bounds.
- **Kani (bounded model checking)** is the upgrade path, not a rewrite:
  Kani's PropProof feature consumes proptest-style harnesses, so writing the
  laws as proptests today means the same properties can later be *proved*
  (exhaustively, up to bounds) for the small pure functions — cursor
  encode/decode, pointer slicing, redaction. Keep those functions pure and
  dependency-free to preserve this path.

## 4. The multi-meta question: reflexivity, not layer-stacking

The requested "double / triple / multi meta" capability is achieved by one
closure property instead of N bespoke layers:

> **Every artifact `prog` produces (envelope, profile, hints, cache listing)
> is itself a JSON document, and the disclosure lens is defined over JSON
> documents. Therefore `prog`'s own outputs are disclosable through the same
> `expand`/slice machinery. The disclosure algebra is closed under itself.**

Concretely:

- Layer 0: the raw source (HTTP/CLI/MCP response).
- Layer 1 (meta): `SourceProfile` — what the source can do.
- Layer 2 (meta²): `DisclosureEnvelope` — a controlled view of one response.
- Layer n+1 (meta^(n+1)): any layer-n artifact that exceeds preview budgets is
  itself passed through the lens, yielding an envelope over an envelope —
  with cursors, omitted paths, and hints. No new code per level.

Two visible consequences in V1:

1. If an envelope or hints document would itself be large (e.g., a source
   with 500 operations), it is budgeted by the same preview policy and gets
   its own omitted paths + cursors. Depth is unbounded by construction.
2. `prog meta` exposes prog's own contracts (JSON Schemas of
   `SourceProfile`, `DisclosureEnvelope`, `SliceRequest`, cursor semantics)
   through the same hints format — the agent learns *prog* the same way it
   learns any source.

## 5. Amendments to RFC 0001

1. **Workspace shape** (RFC 0001 permitted simplification): three crates —
   `prog-core` (contracts, shape lattice, lens, cursors, redaction, cache,
   safety), `prog-adapters` (http, cli, mcp behind one trait), `prog-cli`
   (binary `prog`). Boundaries stay conceptually identical.
2. **Cursors are stored, not encoded.** Cursor tokens are random IDs
   (`pc1_…`) referencing records in the local store (cache key, root path,
   redaction version, expiry). Unforgeable by construction (128-bit random),
   fail closed on expiry/redaction-version mismatch, survive across
   processes, and enable offline expansion. This replaces the
   encode-and-sign option; no crypto dependency needed for a local store.
3. **MCP adapter harvests `outputSchema`** (2025-11-25) as a schema prior;
   observed `structuredContent` refines it via lattice join. Declared and
   observed provenance are distinguished in hints.
4. **`prog meta` command added** as the reflexive self-description entry
   point (section 4).
5. **Skill format:** the repo-local skill targets the SKILL.md agent-skill
   convention used by Claude Code and compatible tools ("Codex skill" in
   RFC 0001 reads as "agent skill").

## 6. Gap analysis of RFC 0001 draft v1 (resolved in draft v2)

The deep review found the following gaps; each is now folded into RFC 0001
draft v2 and the corresponding issues. Recorded here so the reasoning
survives.

- **G1 — The value guarantee was never stated.** The point of `prog` is
  bounded context injection, yet no contract bounded the envelope. Draft v2
  adds "The Envelope Budget": hard configurable `max_envelope_bytes`
  (default 16 KiB), deterministic projection via per-node caps *plus a global
  node budget* (per-node caps alone still explode combinatorially at
  depth × fields), reflexive re-projection at a coarser policy when exceeded,
  and `approx_tokens` in summaries.
- **G2 — Concurrency was unaddressed.** redb's single-writer model is fine,
  but profiles-as-JSON-files had racy read-modify-write; a lost update
  silently violates monotone learning (I5). Draft v2: profile writes go
  through compare-and-swap on `version` with a lock file + atomic rename;
  CAS failure re-reads and re-joins (join makes retry convergent).
- **G3 — Cursor lifecycle.** Encode-and-sign cursors were replaced with
  stored random-id cursors (amendment 2), and `cache purge` now cascades to
  cursors so dangling references are impossible.
- **G4 — Redaction ordering was underspecified.** Normative order:
  **redact → infer → store → project**. Inference before redaction would
  leak secrets into observed string-value sets inside *profiles*, which are
  designed to be committable. Test target, not convention (I2).
- **G5 — Discovery probing was a trap.** "Safe probes" still hit rate limits
  and cost during what agents treat as cheap setup. Draft v2: zero upstream
  calls by default (exception: MCP catalog listing), probing behind
  `--probe` and gated to read-only operations (I6).
- **G6 — Pagination promise scoped honestly.** `prog` never auto-follows
  upstream pagination (unbounded-fetch trap); it records pagination signals
  as hints and lets the agent decide. Now an explicit non-goal.
- **G7 — Text/table parsing scoped small.** JSON detection + bounded line
  previews (head + tail + counts). No column inference in V1.
- **G8 — Missing escape hatch.** `prog expand --out <file>` writes a
  redaction-respecting slice to disk for bulk processing — aligned with the
  2026 code-execution pattern; bulk data goes to files, never to context.
- **G9 — MCP `outputSchema` harvesting** (amendment 3) plus the spec's own
  schema-safety rules: no auto-dereference of external `$ref`s, bounded
  schema depth.
- **G10 — Forward compatibility.** Contracts must preserve unknown fields on
  round-trip; otherwise the first schema evolution breaks every stored
  profile.
- **G11 — No value proof in the plan.** New issue #15: token-economics eval
  harness measuring raw-payload tokens vs. envelope + expansions on the
  fixture sources; the ratio is the README headline and a CI regression
  gate for G1.
- **G12 — Small corrections.** SKILL.md agent-skill convention (not
  Codex-specific); 3-crate workspace; staleness `age` + warning on old
  cache; `Tuple`/`Cursor<Shape>` dropped from the shape algebra; semilattice
  terminology (only `join` has a consumer).

Issue-level mapping: #2←G10; #3←semilattice+absorbing-cap+purity; #4←G1;
#5←G2,G3,G4; #6←G6; #7←G7; #8←G9; #9←G5; #10←G8; #12←invariant table;
#13/#14←G12; #15←G11.

## 7. Invariants (normative, tested)

| # | Invariant | Mechanism |
|---|-----------|-----------|
| I1 | Projection never invents values | proptest over arbitrary JSON |
| I2 | Persistence-redacted data never reaches disk | redact-before-hash-before-store; proptest |
| I3 | Expansion never escapes its cursor's provenance boundary | path containment check; proptest |
| I4 | Redaction is idempotent | proptest |
| I5 | Shape join is commutative, associative, idempotent, monotone | proptest |
| I6 | Discovery never invokes non-read-only operations | policy gate; unit test |
| I7 | Mutating/shell/sensitive ops fail closed without explicit flags | policy gate; unit tests |
| I8 | Non-cacheable results are never persisted | policy gate; unit test |
| I9 | Stale/foreign cursors fail with actionable errors, never wrong data | stored-cursor lookup; unit test |

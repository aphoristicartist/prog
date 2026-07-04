# RFC 0001: Prog Progressive Disclosure Gateway

- Status: Draft v2 (revised 2026-07-03 after research review; see RFC 0002)
- Date: 2026-07-04
- Owner: aphoristicartist
- Repository: https://github.com/aphoristicartist/prog

## Summary

`prog` is a Rust system for giving agents a universal, progressive-disclosure interface over noisy external systems: HTTP APIs, dependent CLIs, and MCP servers.

The core problem is that many tools are bad conversational partners for agents. They return huge JSON blobs, unstructured logs, table-formatted text, inconsistent schemas, or complete non-paginated datasets. An agent then has to spend context, attention, and tool calls just to discover what is available.

By 2026 the ecosystem solved this for *tool catalogs* (deferred tool schemas, tool search, code-execution-with-MCP, with reported 85–100x token reductions). Nothing popular solves it uniformly for *tool results* across HTTP + CLI + MCP. That is the gap `prog` fills.

`prog` sits between the agent and raw sources. It learns source capabilities, captures noisy responses, caches them when appropriate, and exposes compact, inspectable envelopes with hints, previews, omitted paths, cursors, and expansion operations.

The intended agent loop is:

```text
discover -> hints -> call -> expand -> refine
```

The result should feel like every API, CLI, and MCP server has become a carefully designed, agent-native schema with pagination, field discovery, provenance, and safety metadata, even when the upstream source does not provide those properties.

## Goals

- Provide one universal agent-facing protocol for HTTP APIs, CLIs, and MCP servers.
- Support progressive disclosure over large or noisy responses.
- **Guarantee a bounded envelope**: agent-visible output has a hard, configurable byte budget (see "The Envelope Budget"). This is the central contract, not a nice-to-have.
- Cache raw source payloads locally so agents can request slices later without repeated upstream calls.
- Infer source and response shapes from first-round learning.
- Preserve provenance for every visible value, cursor, and inferred schema hint.
- Classify operations by effects and safety: read-only, mutating, networked, shell-backed, sensitive, cacheable, and non-cacheable.
- Be reflexive: prog's own outputs (envelopes, profiles, hints) are JSON documents and are disclosable through the same lens. The meta-tower comes from this closure property, not from bespoke layers.
- Use modern Rust as the implementation baseline.
- Keep the agent skill thin: it should teach agents how to use `prog`; it should not contain the engine.

## Non-Goals

- `prog` is not a full API gateway for production traffic.
- `prog` is not an authorization server.
- `prog` is not a replacement for OpenAPI, JSON Schema, Smithy, TypeSpec, or MCP schemas.
- `prog` does not try to formally prove the entire system.
- **`prog` does not auto-follow upstream pagination.** For sources that paginate (e.g. GitHub Link headers), V1 records pagination signals as hints and lets the agent decide; auto-fetching "all pages" is an unbounded-cost trap.
- **`prog` does not infer tables from text.** The CLI adapter does JSON detection plus bounded line previews; column inference is a rabbit hole with mediocre payoff for agents that handle bounded text well.
- V1 does not expose an agent-facing MCP server. It consumes MCP upstreams through an adapter.
- V1 does not require perfect schema inference. It should prefer useful, explicit uncertainty over false precision.

## Core Idea

`prog` has two engineered meta layers plus a closure property that yields every further layer for free.

### Meta Layer 1: Source Intelligence

This layer models what a source can do.

Examples:

- A GitHub HTTP API source has operations like `list_issues`, `get_issue`, and `list_comments`.
- A local CLI source has operations like `kubectl_get_pods` or `internal_deploy_status`.
- An MCP source has tools, resources, prompts, and their declared schemas.

This layer produces and persists a `SourceProfile`.

The profile includes:

- source identity and kind
- available operations
- input schemas
- observed output shapes
- invocation templates
- auth references (environment-variable names only; profiles must always be committable)
- cache policy
- safety/effect metadata
- trust settings
- examples
- learning provenance

### Meta Layer 2: Response Intelligence

This layer models what is visible from a specific source response.

It does not dump the full upstream response by default. It returns a `DisclosureEnvelope`.

The envelope includes:

- compact summary (including approximate token count)
- data preview
- schema hints
- omitted paths
- one root cursor
- next suggested actions
- warnings (including staleness)
- provenance
- cache metadata

The agent can then request targeted expansion:

```bash
prog expand <cursor> --path /items/0/body --limit 1 --depth 4
```

This makes non-paginated and noisy sources behave like they have field selection, pagination, and inspection controls.

### Meta Layer n+1: Reflexivity

Every artifact `prog` produces — envelope, profile, hints document, cache listing — is itself a JSON document, and the disclosure lens is defined over JSON documents. Therefore `prog`'s own outputs are disclosable through the same `expand`/slice machinery. The disclosure algebra is closed under itself.

Consequences:

1. If an envelope or hints document would itself exceed the budget (e.g. a source with 500 operations), it is passed through the same lens and gets its own omitted paths and cursor. Depth of the meta-tower is unbounded by construction, with zero code per level.
2. `prog meta` exposes prog's own contracts (JSON Schemas of `SourceProfile`, `DisclosureEnvelope`, `SliceRequest`, cursor semantics) through the same hints format. An agent learns `prog` the same way it learns any source.

## The Envelope Budget

The value proposition of `prog` is bounded context injection. That bound is a contract:

- Every agent-visible document (envelope, hints, expansion result) respects `max_envelope_bytes` (default 16 KiB, configurable per store and per call).
- The projection algorithm is deterministic: per-node caps (array items, object fields, string length, depth) plus a global node budget, applied in traversal order. Same payload + same policy = same preview, always.
- If a projection still exceeds the budget, it is re-projected at a coarser policy (reflexive budgeting), never silently truncated mid-JSON.
- Summaries include `approx_tokens` (bytes/4 heuristic) so agents can reason about cost before expanding.

## Example Interaction

```bash
prog discover github --kind http --seed ./sources/github.json
prog hints github
prog call github list_issues --args '{"owner":"aphoristicartist","repo":"prog"}'
prog expand pc1_01H... --path /items/0/body --depth 2
```

A `call` should return something like:

```json
{
  "schema_version": "prog.disclosure.v1",
  "source_id": "github",
  "operation": "list_issues",
  "summary": {
    "kind": "array",
    "item_count": 87,
    "preview_count": 5,
    "payload_bytes": 412773,
    "approx_tokens": 103193,
    "envelope_bytes": 2841
  },
  "data_preview": [
    {
      "number": 12,
      "title": "Implement local cache and redaction model",
      "state": "open",
      "updated_at": "2026-07-04T14:20:00Z",
      "body": "«string: 2.1 KB»"
    }
  ],
  "schema_hints": {
    "/items/*/number": "integer",
    "/items/*/title": "string",
    "/items/*/state": "\"open\" | \"closed\"",
    "/items/*/body": "string, omitted",
    "/items/*/labels": "array<object>, omitted",
    "/items/*/user": "object, omitted"
  },
  "omitted": [
    { "path": "/items", "reason": "long_array", "detail": "87 items, showing 5" },
    { "path": "/items/*/body", "reason": "large_string" },
    { "path": "/items/*/user", "reason": "deep_object" }
  ],
  "cursor": "pc1_01HZX...",
  "next_actions": [
    { "kind": "expand", "path": "/items/0/body", "reason": "issue body omitted from preview" },
    { "kind": "call", "operation": "list_issue_comments", "reason": "comments are represented as a count" }
  ],
  "provenance": {
    "source_call_id": "call_01H...",
    "cache_key": "sha256:...",
    "captured_at": "2026-07-04T14:21:00Z"
  },
  "cache": {
    "status": "stored",
    "ttl_seconds": 86400,
    "expires_at": "2026-07-05T14:21:00Z"
  },
  "warnings": []
}
```

Note the single root `cursor`: expansion addresses regions with `--path`; per-omission cursors would bloat the envelope and the cursor store for no gain.

## Public CLI

The CLI binary is named `prog`. All machine-readable output is JSON on stdout by default; human formatting is opt-in (`--pretty`). Errors are structured JSON with actionable messages and a non-zero exit code.

### `prog discover`

Learns or updates a source profile.

```bash
prog discover <source-id> --kind http|cli|mcp --seed <path-or-json> [--probe]
```

Responsibilities:

- read seed config
- inspect source capabilities when possible **without upstream calls by default** (the one exception: MCP `tools/list`/`resources/list`, which is the cheap catalog call the protocol is designed around)
- with `--probe` (opt-in): execute read-only probes only, never mutating operations
- infer candidate operations and shapes
- persist a `SourceProfile` (merge = shape join + version bump; see Concurrency)
- return a compact discovery report

Rationale for zero-probe default: "safe probes" against real HTTP APIs still hit rate limits and cost during what agents treat as cheap setup. Probing is a decision, not a default.

### `prog hints`

Returns compact source guidance.

```bash
prog hints <source-id> [operation]
```

Responsibilities:

- show available operations
- show required and optional inputs
- show known output fields (from declared schema priors and observed shapes, with provenance distinguished)
- show omitted/expandable regions from examples
- show effect, cache, and safety notes
- suggest next calls

The hints document is itself subject to the envelope budget (reflexivity).

### `prog call`

Invokes a source operation and returns a `DisclosureEnvelope`.

```bash
prog call <source-id> <operation> --args '<json>' [--view '<json>'] [--yes] [--no-cache] [--refresh]
```

Responsibilities:

- validate arguments against the known operation profile
- enforce effect/safety policy (mutating requires `--yes`; shell-backed requires profile trust)
- execute the upstream call
- capture raw output
- **redact, then infer, then store, then project** — in that order (see Redaction Ordering)
- cache when policy permits
- return a bounded envelope

### `prog expand`

Expands a cached result through a cursor.

```bash
prog expand <cursor> [--path <json-pointer>] [--limit N] [--depth N] [--fields a,b] [--out <file>]
```

Responsibilities:

- validate cursor (existence, expiry, redaction version)
- check provenance boundary: the requested path must be within the cursor's root path
- load cached payload and return a bounded expansion, reporting further omitted paths
- work offline: expansion never contacts the upstream source
- warn when serving stale data (age included)
- `--out <file>`: write the (still redaction-respecting) slice to a file instead of stdout, for bulk data the agent wants to grep or process with code rather than read — the escape hatch that keeps context clean

Expansion must never reveal data removed by redaction.

### `prog cache`

Inspects and manages the local cache.

```bash
prog cache list
prog cache get <cache-key>
prog cache purge [--source <source-id>] [--expired] [--all]
```

Responsibilities:

- show cache entries without dumping sensitive payloads
- retrieve cached metadata and optionally bounded payload slices
- purge by source, key, age, or expiration
- **purge cascades to cursors** referencing purged entries; dangling cursors must be impossible

### `prog meta`

Reflexive self-description.

```bash
prog meta [contract]
```

Returns prog's own contracts (JSON Schemas generated via `schemars`) rendered through the same hints format as any source. `prog meta` with no argument lists available contracts; with an argument it discloses that contract's schema.

## Public Contracts

All machine-readable output uses these contracts rather than ad hoc JSON.

Forward-compatibility rule: **consumers must preserve unknown fields on round-trip** (serde `flatten` extra map). Without this, the first schema evolution breaks every stored profile.

### `SourceProfile`

```json
{
  "schema_version": "prog.source_profile.v1",
  "id": "github",
  "kind": "http",
  "display_name": "GitHub REST API",
  "version": 3,
  "auth_refs": [{ "name": "token", "env": "GITHUB_TOKEN" }],
  "operations": [],
  "cache_policy": { "enabled": true, "ttl_seconds": 86400, "max_payload_bytes": 33554432 },
  "safety": {},
  "trust": { "allow_shell": false, "allow_mutating": false },
  "learned_at": "2026-07-04T14:21:00Z",
  "provenance": { "created_by": "seed", "discovery_runs": 2 }
}
```

Required fields: `schema_version`, `id`, `kind`, `version`, `operations`, `cache_policy`, `safety`, `provenance`.

Profiles must never contain secrets. Auth is referenced by environment-variable name only; profiles are designed to be committed to a repo.

### `OperationProfile`

```json
{
  "name": "list_issues",
  "description": "List issues for a repository.",
  "input_schema": { "type": "object", "required": ["owner", "repo"] },
  "output_shape": { "kind": "unknown" },
  "declared_output_schema": null,
  "invocation": { "http": { "method": "GET", "path": "/repos/{owner}/{repo}/issues" } },
  "pagination": { "style": "link_header", "note": "agent decides; prog does not auto-follow" },
  "examples": [],
  "cost_hint": { "network": true, "estimated_bytes": null },
  "effect": {
    "read_only": true,
    "mutating": false,
    "network": true,
    "shell": false,
    "sensitive": false,
    "cacheable": true,
    "requires_confirmation": false
  }
}
```

`output_shape` is the observed shape (join of all observations). `declared_output_schema` is a trusted prior (e.g. MCP `outputSchema`); hints distinguish declared from observed provenance.

### `DisclosureEnvelope`

```json
{
  "schema_version": "prog.disclosure.v1",
  "source_id": "github",
  "operation": "list_issues",
  "view": null,
  "summary": {},
  "schema_hints": {},
  "data_preview": null,
  "omitted": [],
  "cursor": null,
  "next_actions": [],
  "warnings": [],
  "provenance": {},
  "cache": {}
}
```

### `SliceRequest`

```json
{
  "fields": [],
  "omit": [],
  "path": "/items",
  "limit": 25,
  "depth": 3,
  "sample": true,
  "refresh": false
}
```

## Internal Type Model

The internal schema model is richer than JSON Schema but exportable to JSON Schema where useful.

The model is a **bounded join-semilattice** (not a full lattice — only `join` has a consumer; `meet` is not implemented).

Core variants:

```text
Unknown
Null
Boolean
Integer
Number
String            (with optional small observed-value set, for enum-like hints)
Timestamp         (RFC 3339 string refinement)
Array<T>
Object {
  known_fields: Map<FieldName, { shape, optional, seen }>,
  rest: Shape
}
Union<[Shape...]> (canonical: flattened, per-kind merged, sorted, deduped)
Sensitive<Shape>  (sticky under join)
```

Deliberately dropped from the earlier draft: `Tuple` (no V1 consumer) and `Cursor<Shape>` (cursors live in envelopes, not in the shape algebra). `Bytes` and `Never` are omitted until a consumer exists.

`Unknown` is not a failure. It is an honest representation of insufficient evidence, and the identity element of `join`.

Join rules:

```text
join(Unknown, x)            = x
join(x, x)                  = x
join(Integer, Number)       = Number
join(Timestamp, String)     = String
join(Null, x)               = Union[Null, x]        (rendered "T | null")
join(Array a, Array b)      = Array(join(elem a, elem b))
join(Object a, Object b)    = fieldwise join; one-sided fields become optional; rest = join(rest)
join(Sensitive a, b)        = Sensitive(join(a, unwrap b))    (sensitivity is sticky)
incompatible kinds          = Union, canonicalized
```

For arrays:

```text
Array<Object { id: Integer }>
+ Array<Object { id: Integer, name: String }>
-> Array<Object { id: Integer, name?: String, rest: Unknown }>
```

**Enum-value caps must be absorbing.** Observed string-value sets are tracked only while every value is short (≤ 40 chars) and the set is small (≤ 8). Once either bound would be exceeded, the shape collapses to plain `String` and never re-acquires a value set. If instead a subset were kept, `join` would lose associativity and learning would become order-dependent. The same absorbing rule applies to any future bounded refinement.

## Type-Theory Inspirations

`prog` borrows exactly three structures — each one earns its place by turning a design promise into a runnable law. See RFC 0002 for the full research verdict.

### Gradual Typing / Row Polymorphism

External systems are partially known. Start with `Unknown`, refine from examples, preserve uncertainty. Objects are known fields plus an unknown `rest` — essential for large API objects where the agent needs a few fields but hidden fields must remain expandable.

### The Disclosure Layer Is a Get-Only Optic

The disclosure layer is a **projection**, not a full lens: there is no `put` back into the source, so the hard lens laws (GetPut/PutGet) do not apply. The complete law set is:

1. **No fabrication**: every value in a preview or expansion is drawn from the cached source payload; truncation is explicit and marked (`«…»` markers).
2. **Boundary containment**: expansion resolves strictly within the cursor's root path in the same cached payload.
3. **Redaction dominance**: redaction composes before projection; no composition of expand/slice operations can recover a redacted value.

These three laws are the property-test suite for the lens.

### Effect Sets With a Fail-Closed Order

Operations carry effect metadata. Policy checks are monotone: adding an effect can only remove permissions. Unknown effects are treated as worst case. Plain Rust structs and policy functions — no effect-system machinery.

## Formal Methods Scope

V1 does not attempt full formal verification. TLA+/Alloy are rejected for V1: the state machine is small, single-process, and its invariants are data laws, not concurrency laws.

Instead, protect the small core with property tests on a **zero-rewrite path to proofs**: Kani's PropProof feature consumes proptest-style harnesses, so laws written as proptests today can later be model-checked (proved up to bounds) without rewriting. The design consequence is architectural: **cursor decoding, pointer slicing, projection, and redaction must remain pure, dependency-free functions** to stay Kani-eligible.

Normative invariants (each maps to a test; see RFC 0002 §6):

- I1. Projection never invents values.
- I2. Persistence-redacted data never reaches disk.
- I3. Expansion never escapes its cursor's provenance boundary.
- I4. Redaction is idempotent.
- I5. Shape join is commutative, associative, idempotent, and monotone, with `Unknown` as identity.
- I6. Discovery never invokes non-read-only operations.
- I7. Mutating, shell-backed, and sensitive operations fail closed without explicit flags/trust.
- I8. Non-cacheable or sensitive results are never persisted.
- I9. Stale or foreign cursors fail with actionable errors, never wrong data.

## Adapter Model

Adapters normalize source-specific execution into a common call result.

```text
AdapterInput:
  SourceProfile
  OperationProfile
  Args
  ExecutionPolicy

AdapterOutput:
  StructuredPayload (JSON; text is wrapped, never dumped)
  RawByteCount
  Diagnostics
  Provenance
  EffectReport
```

The disclosure layer must not care whether a payload came from HTTP, CLI, or MCP.

### HTTP Adapter

Responsibilities:

- method, URL, query, headers, body via operation templates (`{param}` substitution)
- auth strictly by env-var reference; auth values never persisted, never in provenance
- timeout and max-response-bytes hard caps (truncate + warn, never unbounded)
- status, header, and timing capture into provenance
- JSON body detection; text fallback wrapped as bounded lines
- pagination signals (Link headers, `next_page`-like fields) recorded as hints; never auto-followed

Example seed:

```json
{
  "kind": "http",
  "base_url": "https://api.github.com",
  "auth_refs": [{ "name": "token", "env": "GITHUB_TOKEN", "header": "Authorization", "format": "Bearer {value}" }],
  "operations": [
    {
      "name": "list_issues",
      "method": "GET",
      "path": "/repos/{owner}/{repo}/issues",
      "args": { "owner": "string", "repo": "string" }
    }
  ]
}
```

The seed format is extensible; OpenAPI import (`--seed openapi:./spec.yaml`) is a natural post-V1 addition and the format should not preclude it.

### CLI Adapter

Responsibilities:

- execute a binary with argv arrays, never shell strings
- capture stdout, stderr, exit code, duration into structured provenance
- parse JSON when possible
- otherwise: bounded line preview (head + tail + line/byte counts) with cursors over line ranges — **no table/column inference in V1**
- enforce timeout and output byte limits
- shell-backed operations require `trust.allow_shell` in the profile

Example seed:

```json
{
  "kind": "cli",
  "operations": [
    {
      "name": "list_branches",
      "command": "git",
      "args": ["branch", "--format", "%(refname:short)"],
      "effect": { "read_only": true, "shell": false }
    }
  ]
}
```

### MCP Adapter

Responsibilities:

- connect to upstream MCP servers with the official Rust SDK (`rmcp` 2.x, pinned; it has broken once at 1.x→2.x, so wrap it thin behind the adapter boundary)
- V1 connection model: per-call child process session (spawn → initialize → operate → shutdown); persistent sessions are a later optimization
- list tools, resources, and prompts where supported (this is the one permitted zero-flag discovery call)
- map MCP tool `inputSchema` into `OperationProfile.input_schema`
- **harvest `outputSchema` (MCP spec 2025-11-25) into `declared_output_schema`** as a trusted prior; observed `structuredContent` refines it via lattice join; hints distinguish declared from observed
- map MCP tool annotations (e.g. `readOnlyHint`) into effect metadata, conservatively
- prefer `structuredContent`; fall back to `content` text with JSON detection
- adopt the spec's schema-safety rules: never auto-dereference external `$ref` URIs; bound schema depth and validation time

V1 consumes MCP. It does not expose `prog` as an MCP server.

## Cache And Redaction

Default behavior: cache by default when policy permits.

Cache entries are content-addressed and source-aware:

- `payloads` table: `sha256(payload)` → redacted payload bytes
- `entries` table: deterministic call-signature key (`sha256(source_id + operation + canonicalized args)`) → entry metadata
- `cursors` table: cursor id → cursor record

Cache metadata:

```json
{
  "cache_key": "sha256:...",
  "source_id": "github",
  "operation": "list_issues",
  "created_at": "2026-07-04T14:21:00Z",
  "expires_at": "2026-07-05T14:21:00Z",
  "redaction_version": 1,
  "payload_bytes": 123456,
  "sensitive": false
}
```

### Redaction Ordering (normative)

```text
redact -> infer -> store -> project
```

Redaction must happen **before shape inference**, not just before persistence: inference tracks observed string values for enum hints, so inferring from raw data would leak secrets into profiles — which are designed to be committable. This ordering is a test target (I2), not a convention.

Redaction classes:

- **persistence redaction**: value removed before hashing/storing; the sentinel `"[REDACTED:<rule>]"` takes its place
- **display redaction**: hidden in previews (`«redacted»` marker) but retained in cache
- **expansion redaction**: expansion refuses to return the field

Default persistence rules match sensitive field names case-insensitively: password, passwd, secret, token, api_key/apikey, authorization, credential, private_key, session, cookie, bearer.

Conservative V1 posture:

- secrets and auth headers are never persisted
- operation args marked sensitive are redacted from provenance
- non-cacheable operation results are not persisted (in-memory expansion only, if implemented)
- `.prog/` gets `0700` and its files `0600` permissions — the cache holds API response data

## Cursor Model

Cursors are opaque to agents and **stored, not encoded-and-signed**. A cursor token is `pc1_` plus a random 128-bit id referencing a record in the local store:

```json
{
  "id": "pc1_01HZX...",
  "cache_key": "sha256:...",
  "source_id": "github",
  "operation": "list_issues",
  "root_path": "",
  "redaction_version": 1,
  "created_at": "2026-07-04T14:21:00Z",
  "expires_at": "2026-07-05T14:21:00Z"
}
```

Rationale: encode-and-sign drags in crypto and key management for a local store; random ids are unforgeable by construction, survive across processes, enable offline expansion, and centralize invalidation.

Cursor requirements:

- cannot be forged to access another cache entry (128-bit random id)
- cannot escape its `root_path` provenance boundary (segment-wise containment check, I3)
- expires with the underlying cache entry
- fails closed if the redaction policy version changed incompatibly
- **garbage-collected when the referenced cache entry is purged** — dangling cursors must be impossible
- one root cursor per call; expansion addresses regions via `--path`

## Safety Model

Operations are classified explicitly:

```json
{
  "read_only": true,
  "mutating": false,
  "network": true,
  "shell": false,
  "sensitive": false,
  "cacheable": true,
  "requires_confirmation": false
}
```

Policy rules (all fail closed; unknown = worst case):

- discovery (`--probe`) may only call read-only operations (I6)
- mutating operations require explicit `--yes` (I7)
- shell-backed operations require `trust.allow_shell` in the source profile (I7)
- sensitive args are redacted from logs and provenance
- cache policy respects sensitivity and non-cacheable markers (I8)
- warnings are surfaced in envelopes and hints, not buried in logs

## Concurrency

Two agent processes may run `prog` against the same `.prog/` simultaneously.

- The redb store has a single-writer model; concurrent writers block briefly. Acceptable for V1.
- Profiles-as-JSON-files are the hazard: racy read-modify-write loses learned schema, which silently violates monotone learning (I5). Profile writes therefore go through **compare-and-swap on the profile `version` field** (write to temp file, verify version unchanged under a lock file, atomic rename). On CAS failure: re-read, re-join (join makes retry safe and convergent), retry.

## Storage Layout

Default project-local layout:

```text
.prog/            (0700)
  profiles/
    <source-id>.json
  cache/
    data.redb     (0600)
  logs/
    traces.jsonl
```

Future versions may support a user-level global store, but V1 stays project-local by default to keep behavior inspectable. `.prog/` is gitignored except `profiles/`, which is committable by design.

## Rust Workspace Shape

Three crates (the earlier 7-crate draft was ceremony; boundaries stay conceptually identical):

```text
crates/
  prog-core/       contracts, shape semilattice, lens, cursors, redaction, cache, safety policy
  prog-adapters/   http, cli, mcp behind one adapter boundary
  prog-cli/        binary `prog`
skills/
  prog/            SKILL.md agent skill
docs/
  rfcs/
```

Dependencies (pinned in workspace): `tokio`, `clap`, `serde`, `serde_json`, `schemars`, `reqwest` (rustls), `rmcp` 2.x, `redb` 4.x, `tracing`, `thiserror`, `chrono`, `sha2`, `uuid`, `proptest` (dev), `wiremock` (dev), `tempfile` (dev). `miette` optional for human-mode errors; agent-facing errors are structured JSON.

## Agent Skill

The skill follows the SKILL.md agent-skill convention (Claude Code and Codex compatible — the earlier draft said "Codex skill"; read that as "agent skill"). It is thin and procedural:

1. Use `prog hints` before making calls against known sources.
2. Use `prog discover` for new sources.
3. Use `prog call` to get bounded envelopes.
4. Use `prog expand` for omitted paths; `--out <file>` for bulk data you intend to grep or process with code.
5. Use `--refresh` when staleness warnings appear and freshness matters.
6. Do not ask for raw dumps into context; that is the failure mode `prog` exists to prevent.
7. Respect warnings about mutating operations, secrets, stale cache, and non-cacheable sources.
8. Use `prog meta` to learn prog's own contracts.

The skill does not duplicate this RFC.

## Testing Strategy

### Unit Tests

- schema join and refinement; object optionality; union widening; enum-cap absorption
- JSON Pointer slicing and escaping; boundary containment
- preview generation and node budgets; omitted path calculation
- cursor lifecycle (create, expire, purge cascade, redaction-version mismatch)
- redaction classes and ordering
- cache keys and canonicalized args
- effect policy checks (every fail-closed rule)

### Integration Tests

- mock HTTP server (wiremock) with large non-paginated JSON, errors, timeouts, truncation
- fixture CLI returning JSON; returning text; failing with stderr + non-zero exit; timing out; exceeding output caps
- fixture MCP server exposing one tool (with `outputSchema`) and one resource
- cache and expansion across process boundaries; expansion with upstream unavailable

### Property Tests (proptest, PropProof-compatible style)

- I1: projection returns values from the original source only (modulo explicit `«…»` markers)
- I2: serialized stored payloads never contain persistence-redacted values
- I3: expansion paths outside the cursor boundary always fail
- I4: redaction is idempotent
- I5: join laws — commutativity, associativity (including enum-cap absorption cases), idempotence, identity, monotonicity

### Snapshot Tests

- `prog --help`, `prog discover`, `prog hints`, `prog call`, `prog expand`, `prog meta`, common error outputs

### Token-Economics Eval (issue #15)

- for each fixture source: measure approx tokens of the raw payload vs. envelope + the expansions needed for three representative tasks ("find field X", "count items", "get item N's body")
- print a ratio table; fail CI if the envelope budget regresses
- this number is the README headline

## Implementation Roadmap

1. Scaffold Rust workspace and `prog` CLI shell.
2. Define universal profile and disclosure contracts.
3. Implement schema join-semilattice and inference core.
4. Implement disclosure lens: preview, omit, and expand.
5. Implement local cache, cursor store, and redaction model.
6. Add HTTP source adapter.
7. Add CLI source adapter.
8. Add MCP upstream adapter.
9. Implement discover and hints workflow.
10. Implement call and expand workflow.
11. Add safety and effect model.
12. Add property tests and formal invariant checks.
13. Create agent skill for `prog`.
14. Add documentation and acceptance examples.
15. Add token-economics eval harness.

Critical path: 2 → 3 → 4 → 5 → 10. Adapters (6, 7, 8) parallelize after 2. Issue 11 lands with 10 at the latest; its rules are load-bearing for 9 and 10.

## Acceptance Scenario

A user has a CLI that returns a huge JSON blob:

```bash
internal-tool list-customers --all --json
```

They create a minimal source seed:

```json
{
  "kind": "cli",
  "operations": [
    {
      "name": "list_customers",
      "command": "internal-tool",
      "args": ["list-customers", "--all", "--json"],
      "effect": { "read_only": true, "cacheable": true }
    }
  ]
}
```

Then:

```bash
prog discover customers --kind cli --seed customers.json
prog call customers list_customers
```

The agent receives: customer count, first few fields, schema hints, omitted nested paths, one cursor, cache metadata, and warnings if sensitive-looking fields were redacted. It then inspects only what it needs:

```bash
prog expand pc1_abc --path /items/42/billing --depth 2
```

The original huge output remains outside the context window, but still inspectable — and the eval harness (#15) reports exactly how many tokens that saved.

## Resolved Questions (previously open)

- **Project-local or user-global cache?** Project-local only in V1; the layout section stands.
- **Natural-language discovery hints?** No; structured seed JSON only in V1. The seed format stays extensible (OpenAPI import later).
- **How much text/table parsing?** JSON detection + bounded line previews. No table inference (non-goal).
- **Expose prog as an MCP server?** Deferred until the CLI + skill prove useful; consuming MCP comes first.
- **Hand-authored vs learned profile fields?** Seeds hand-author identity, invocation, auth refs, trust, and effect defaults; learning owns `output_shape`, examples, and pagination signals. Learned fields are merged by join and never clobber hand-authored ones.
- **Profiles human-editable or generated?** Both, safely: profile writes go through CAS (see Concurrency), and hand edits win over learned data on conflict.

## Decision Defaults

- Name: `prog`
- Language: Rust (edition 2024)
- Repo: `aphoristicartist/prog`
- Agent-facing V1 surface: CLI plus agent skill
- Upstream source kinds in V1: HTTP, CLI, MCP
- Cache default: cache when policy permits
- Safety default: fail closed for mutating, sensitive, or shell-backed ambiguity
- Schema default: preserve uncertainty rather than pretending precision
- Envelope default: 16 KiB budget, deterministic projection
- Discovery default: zero upstream calls (except MCP catalog listing); probing is opt-in

# Evidence references

`EvidenceRef` is the compact citation protocol for `prog` observations. It lets
an agent support a conclusion with cursor/path-backed evidence without pasting a
raw payload into context.

> **Where do the paths in an `EvidenceRef` come from?** The generic findings
> ranking engine (`prog_core::findings`) projects a deterministic ranked view
> over an already-redacted payload and is surfaced by `prog inspect`.
> See [`findings.md`](./findings.md) for the kind/intent scoring table, the
> tie-break chain, and the command-hint gating policy.

Example citation:

```text
EvidenceRef: prog://pc1_abc123#/failure_sections/0
```

The structured ref includes:

- source id and operation
- cursor and JSON Pointer path
- captured timestamp
- cache status, age, expiry, and stale flag
- observation-wide `availability` and `capture` lifecycle facts
- redaction and lossiness flags
- `redacted_slice_sha256`, a hash of the already-redacted visible slice

The hash is an integrity hint, not a secret recovery mechanism. It is computed
after redaction and should not be used as a capability. Expanding still requires
the original cursor, and stale, purged, expired, or redaction-version-mismatched
cursors fail closed.

## Capture lifecycle

Capture, storage, and disclosure have independent limits. A small first-view
envelope does not mean the captured payload was incomplete, and a cursor cannot
recover bytes that an adapter did not capture. Every observation envelope
reports `observation.availability`; nonstandard lifecycle states also include
the detailed `observation.capture` record. The immutable observation record
always retains full capture accounting. `capture_truncated`, `redacted`, `metadata_only`, `expired`, and
`unavailable` evidence never grants `can_prove_absence`; only recoverable,
complete evidence can participate in a resolved delta or verification claim.

Every `EvidenceRef` also carries the immutable observation's `availability`
and complete `capture` record. Its `redacted` and `lossy` flags remain local to
the cited path or preview; use the lifecycle fields to decide whether the
underlying observation can support an absence claim. A ref with no attached
immutable observation explicitly reports `availability: unavailable` and a
non-proving capture record.

For a canonical recoverable and complete observation, the initial envelope
omits its redundant root `EvidenceRef`; `observation` already carries the same
lifecycle facts and the cursor remains available for navigation. Path-specific
responses still emit their refs. Nonstandard lifecycle states retain the root
ref so the limitation is visible immediately.

For CLI runs, capture records report separate `stdout` and `stderr` byte facts.
For HTTP, the default response-body capture limit is 2 MiB for both direct
`HttpSource` configuration and generated or loaded source profiles. A profile
can set `adapter.http.max_response_bytes` explicitly. When that limit stops a
response body early, the capture record reports the bytes read and an unknown
total, because the adapter cannot truthfully infer the full body size.

MCP structured content is measured before its bounded projection. When it
exceeds `max_content_bytes`, its capture record retains that known original
size, reports `storage_limit`, and still disallows absence claims. Preview
omissions remain in `observation.completeness` and describe only model
disclosure, not upstream capture completeness.

To apply a durable storage limit over redacted payload blobs, run:

```bash
prog cache purge --payload-budget-bytes 33554432
```

No payload quota is enforced until this explicit command is run. This keeps
normal capture behavior independent from retention policy; the command output
is the durable accounting record for the quota pass.

The quota groups identical payload hashes, evicts oldest groups first, removes
their dependent cursors, and retains immutable observations as
`metadata_only`. It never leaves one surviving cache entry pointing at a
deleted shared blob. This is distinct from capture limits and the per-response
disclosure budget.

## Workflow

```bash
prog run -- cargo test
prog inspect pc1_... --goal "find the root cause"
prog evidence pc1_... --path /failure_sections/0
```

When the answer depends on the expanded failure section, cite the ref:

```text
EvidenceRef: prog://pc1_...#/failure_sections/0
```

`prog evidence` returns a bounded `prog.evidence` block with an excerpt,
line/byte ranges when the parser knows them, safe provenance, redaction state,
and exact follow-up commands. Do not paste the full stdout/stderr unless the user explicitly needs it. Use
`prog expand <cursor> --path <path> --out <file>` for bulk evidence that should
stay out of model context; the receipt includes its own `evidence_ref`.

## Counterexamples

Do not cite an `EvidenceRef` as proof of content you did not expand. If an
envelope or path has `lossy: true`, expand the narrower path needed for the
claim. If `redacted: true`, treat the redacted value as unavailable evidence.

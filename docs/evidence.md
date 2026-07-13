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

For CLI runs, capture records report separate `stdout` and `stderr` byte facts.
For transport adapters, byte limits report the captured body size and retain an
unknown total when the adapter stopped early. Preview omissions remain in
`observation.completeness` and describe only model disclosure, not upstream
capture completeness.

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

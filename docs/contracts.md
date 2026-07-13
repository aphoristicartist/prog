# JSON contracts

`prog meta` is the source of truth for public JSON contracts. It generates schemas from the Rust types and returns them in the same `DisclosureEnvelope` used for adapter responses.

Every JSON response also has a compact top-level `disclosure_budget` block. It
identifies the applied source and byte ceiling, labels the optional bytes/4
token estimate, and records the final emitted stdout byte count. See
`prog --help` for `--budget-bytes` and `--budget-tokens`.

List available contracts:

```bash
prog meta
```

Inspect one contract:

```bash
prog --pretty meta SourceProfile
prog --pretty meta DisclosureEnvelope
prog --pretty meta EvidenceRef
prog --pretty meta InspectResponse
prog --pretty meta EvidenceBlock
prog --pretty meta SearchResponse
prog --pretty meta CacheEntryMeta
```

The current public contracts include:

- `SourceProfile`
- `OperationProfile`
- `Shape`
- `EffectSet`
- `CachePolicy`
- `TrustSettings`
- `AuthRef`
- `DisclosureEnvelope`
- `ObservationMetadata`
- `EvidenceRef`
- `InspectResponse`
- `Finding`
- `FindingCommandHints`
- `EvidenceBlock`
- `EvidenceCitation`
- `SearchResponse`
- `SearchHit`
- `LineRange`
- `ByteRange`
- `SourceSpan`
- `SourceSpanExactness`
- `RedactionState`
- `Summary`
- `OmittedRegion`
- `NextAction`
- `LensManifest`
- `LensFindingRule`
- `LensMatch`
- `LensView`
- `LensOmission`
- `LensFixtures`
- `SliceRequest`
- `CursorRecord`
- `CacheEntryMeta`
- `CallProvenance`
- `CacheInfo`
- `CacheList`
- `PurgeSummary`
- `SessionEvent`
- `SessionTrail`

## Forward compatibility

Consumers must ignore unknown object fields. The contracts intentionally allow extra fields in profiles, envelopes, cache metadata, and provenance so adapters can add details without breaking older clients.

Consumers should branch on stable required fields first:

- `schema`
- `source_id`
- `operation`
- `summary`
- `data_preview`
- `omitted`
- `cursor`
- `cache`
- `warnings`

For expansions, use JSON Pointer paths from `omitted` or `next_actions` instead of guessing positions from a preview. Previews are bounded and may omit long arrays, large strings, deep objects, or high-cardinality fields.

`next_actions` may include forward-compatible planner metadata such as
`priority`, `omitted_reason`, `detail`, `argv`, and `offline`. Consumers should
use known fields and ignore unknown extras.

`observation` describes how to interpret the preview: completeness, freshness,
trust, safety, and cache-backed payload availability. This metadata is additive;
consumers should ignore unknown fields inside its subobjects.

`evidence_ref` is a compact citation for cursor/path-backed evidence. It may
appear on envelopes, expanded slices, path entries, and export receipts. It is
not a capability; consumers must still call `prog expand` with the cursor and
path when they need evidence.

Evidence-navigation contracts are the machine-readable surface for
ranked evidence workflows. `InspectResponse` contains ranked `Finding` entries,
`EvidenceBlock` contains compact citation-oriented evidence for one path, and
`SearchResponse` contains path-backed hits over redacted cached payloads. These
are returned by `inspect`, `evidence`, `search`, and `find`; `SessionTrail`
records metadata-only navigation events.

For lens-driven calls, inspect the flattened `lens` metadata on the returned
envelope before assuming the preview shape is the source's native shape. Lens
previews may select or rename fields, but expansion still addresses the
original cached payload by JSON Pointer.

## Drift checks

The CLI integration tests execute the README quickstart against `fixtures/cli/seed.json` and assert that documented subcommand flags appear in `--help`. The token economics report is regenerated through:

```bash
PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture
```

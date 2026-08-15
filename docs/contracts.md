# JSON contracts

`prog meta` is the source of truth for public JSON contracts. It generates schemas from the Rust types and returns them in the same `DisclosureEnvelope` used for adapter responses.

Every JSON response also has compact top-level budget blocks. `disclosure_budget`
identifies the applied response byte ceiling, labels the optional bytes/4 token
estimate, and records the final emitted stdout byte count. `capture_budget`
identifies the source and limits that governed source capture when one was
performed. `storage_budget` identifies the durable retention policy used by the
local store. See `prog --help` for `--budget-bytes` and `--budget-tokens`.

`DisclosureEnvelope.summary` keeps payload cost and immediate envelope cost
separate. `payload_bytes` is the size of the complete redacted payload retained
behind the cursor. `envelope_bytes` is the serialized disclosure envelope, and
`estimated_envelope_tokens` is exactly `ceil(envelope_bytes / 4)` using the
named approximate estimator; it never estimates the hidden payload. For the
final process-level response, including the common budget blocks and trailing
newline, use `disclosure_budget.actual_bytes`. Replay reports likewise label
actual model-visible response bytes separately from their `bytes/4-ceiling`
token estimate.

For `call` and `hints`, a source profile may declare
`"disclosure_budget": {"max_bytes": 4096}`. Precedence is command flag,
environment variable, source profile, then the default. The response's
`disclosure_budget.source` reports the winning tier.

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
prog --pretty meta ActionIntent
prog --pretty meta ReadbackVerificationReceipt
prog --pretty meta SearchResponse
prog --pretty meta CacheEntryMeta
```

The current public contracts include:

- `ErrorEnvelope`
- `ErrorBody`
- `SourceProfile`
- `DisclosureBudget`
- `SourceKind`
- `OperationProfile`
- `Shape`
- `EffectSet`
- `ObservationMetadata`
- `ObservationCompleteness`
- `ObservationFreshness`
- `ObservationTrust`
- `ObservationSafety`
- `ObservationPayloadStatus`
- `ObservationRecord`
- `WorkspaceState`
- `WorkspacePathState`
- `SubmoduleState`
- `WorkspaceValidity`
- `WorkspaceComparison`
- `ObservationLineage`
- `EvidenceAvailability`
- `BudgetSource`
- `CaptureLimit`
- `CaptureBudget`
- `StorageBudget`
- `StorageQuotaSummary`
- `StorageBudgetSummary`
- `CaptureStopReason`
- `CaptureScope`
- `CaptureCompleteness`
- `SourceStateToken`
- `SourceStateSelector`
- `SourceStateKind`
- `SourceValidity`
- `SubjectIdentity`
- `ScopeRelationship`
- `SelectionCoverage`
- `ComparabilityAssessment`
- `DeltaFindingStatus`
- `DeltaFinding`
- `ObservationDelta`
- `ObligationDeclarer`
- `VerificationOperation`
- `VerificationStateRelationship`
- `VerificationObligation`
- `ObligationEvaluation`
- `ReadinessReport`
- `VerificationStatus`
- `ExpectedStateChange`
- `ActionIntent`
- `ReadbackVerificationStatus`
- `ReadbackCheck`
- `ReadbackVerificationReceipt`
- `RouteGuidance`
- `RouteRule`
- `RoutePolicy`
- `RouteAssessment`
- `StatusReport`
- `CachePolicy`
- `TrustSettings`
- `AuthRef`
- `DisclosureEnvelope`
- `DisclosureVerdict`
- `DisclosureVerdictResult`
- `EvidenceRef`
- `InspectResponse`
- `Finding`
- `FindingCommandHints`
- `NavigationCommand`
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
- `OmissionReason`
- `ActionExactness`
- `ActionScope`
- `ActionTemplate`
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
- `CacheStatus`
- `CacheList`
- `ObservationList`
- `ObligationList`
- `PurgeSummary`
- `SessionEvent`
- `SessionTrail`

## Forward compatibility

The pre-release reset leaves one representation of each current contract. There
are no migration tables, deprecated aliases, dual-write records, or accepted
internal `schema_version`/`version` fields. Profiles and lenses with an obsolete
identity are rejected; persisted stores with a different `STORE_SCHEMA` are
audibly reset instead of interpreted.

The remaining version-like values are evidence, not compatibility scaffolding:

- `SourceProfile.revision` serializes concurrent profile updates and must
  increase locally; it does not select a wire format.
- MCP `protocol_version` is the negotiated upstream protocol revision.
- OpenAPI and API-info versions describe an imported upstream document.
- Cargo/package versions identify a released artifact.

The extension audit follows two rules. Closed user-authored manifests such as
`LensManifest`, and closed result subobjects such as `CacheInfo`, reject unknown
fields. Provider-owned profile invocation data, provenance, and agent-facing
result objects retain flattened extension maps because those fields carry
source-specific evidence and additive output metadata; they do not select a
legacy parser or alternate representation. Inert observation fields are not
kept as speculative extensions: `subject_keys` and `environment_state` were
removed with the canonical observation-store reset because no capture populated
them and no reader consumed them.

Consumers must ignore unknown object fields on additive result and provider
extension surfaces. Those contracts intentionally allow extra fields so
adapters can add evidence details without breaking older clients.

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

`next_actions` may include planner metadata such as `priority`,
`omitted_reason`, and `detail`. Cursor-backed actions do not repeat rendered
commands: `action_templates` declares symbolic argv once per action kind using
`{cursor}` and `{path}`, and `NextAction.kind` selects that template. The typed
`cached_evidence` scope is offline by contract and never contacts upstream.
Direct rerun recommendations keep an exact argv on the individual action.

Finding command hints are similarly compact. `commands.available` lists typed
navigation kinds; their cursor, path, and finding kind come from the containing
response/finding instead of being repeated as shell strings.

`observation` describes how to interpret the preview: completeness, freshness,
trust, safety, and cache-backed payload availability. This metadata is additive;
consumers should ignore unknown fields inside its subobjects.

`evidence_ref` is a compact citation for cursor/path-backed evidence. It may
appear on envelopes, expanded slices, path entries, and export receipts. It is
not a capability; consumers must still call `prog expand` with the cursor and
path when they need evidence.

Each ref includes the immutable observation's `availability` and `capture`
facts. Treat those as observation-wide lifecycle metadata: a locally complete
path cannot prove absence when its containing capture was truncated, redacted,
expired, metadata-only, or unavailable.

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

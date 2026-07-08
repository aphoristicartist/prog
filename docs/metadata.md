# Observation metadata

Every `DisclosureEnvelope` includes an `observation` object that tells agents how
to treat the preview.

Use these fields before answering from a preview:

- `completeness.status`: one of `complete`, `truncated`, `redacted`,
  `partial`, or `path_scoped`
- `completeness.preview_complete`: true only when the preview represents the
  selected payload without omitted paths
- `completeness.omitted_count` and `redacted_count`: how much was hidden from
  the preview
- `freshness.age_seconds`, `stale`, and `refresh_recommended`: whether cached
  evidence should be refreshed before acting
- `trust.profile_backed` and `source_kind`: whether the data came through a
  declared source profile, profile-free observation, or internal `prog` source
- `safety.effects`: operation effect flags when known
- `safety.redacted_before_persistence`: whether secret-like values were removed
  before shape inference, cache persistence, and expansion
- `payload.cache_status`, `cached`, and `expandable`: whether the full redacted
  payload can be expanded later
- `parser.id`, `confidence`, `lossy`, and `fallback`: which observation
  parser/indexer handled the artifact, how strong the match was, whether the
  preview is extracted or bounded, and whether the bounded text fallback was
  used
- `parser.path_semantics` and `range_semantics`: how to interpret expansion
  paths and text ranges for the selected parser
- `value_scan.lossy`, `high_confidence_count`, and `low_confidence_count`:
  value-pattern redaction outcome. `lossy` is true when a low-confidence
  secret-like shape (an ambiguous long base64/JWT-like blob) was observed and,
  by default, preserved verbatim rather than redacted; the counts break down
  high-confidence value redactions vs. low-confidence observations. When `lossy`,
  `parser.lossy` is also OR-folded to `true` and `parser.confidence` is capped at
  `0.6` so the redaction uncertainty propagates. Enable
  `redaction.redact_low_confidence_values` on the source profile to redact (not
  just flag) low-confidence shapes.

## Agent Rules

- Do not treat a preview as complete unless `preview_complete` is true and
  `omitted_count` is zero.
- If `redacted_count` is non-zero, expansion will only reveal the stored
  redacted value, not the original secret.
- If `payload.expandable` is false, there is no cursor-backed recovery path for
  omitted data.
- If `parser.lossy` is true, use `prog expand` for exact cited slices before
  relying on extracted summaries such as JUnit XML test cases, HTML headings, or
  unified diff file lists.
- If `value_scan.lossy` is true, a string value contained a secret-like shape
  the scanner was not confident about and preserved verbatim; do not cite that
  value as evidence unless `redaction.redact_low_confidence_values` has been
  enabled to redact it.
- If `parser.fallback` is true, treat the artifact as bounded text even when the
  MIME type looked structured.
- If `freshness.refresh_recommended` is true, rerun the original call or
  observation before making freshness-sensitive claims.
- If `trust.profile_backed` is false, prefer explicit citations to the observed
  artifact path or cursor instead of assuming a stable upstream API contract.

Metadata is additive. Consumers should ignore unknown fields inside
`observation` subobjects.

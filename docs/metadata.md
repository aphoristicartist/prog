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

## Agent Rules

- Do not treat a preview as complete unless `preview_complete` is true and
  `omitted_count` is zero.
- If `redacted_count` is non-zero, expansion will only reveal the stored
  redacted value, not the original secret.
- If `payload.expandable` is false, there is no cursor-backed recovery path for
  omitted data.
- If `freshness.refresh_recommended` is true, rerun the original call or
  observation before making freshness-sensitive claims.
- If `trust.profile_backed` is false, prefer explicit citations to the observed
  artifact path or cursor instead of assuming a stable upstream API contract.

Metadata is additive. Consumers should ignore unknown fields inside
`observation` subobjects.

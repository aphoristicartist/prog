# Source-state evidence

Source state is evidence supplied by the upstream system. It is separate from
cache age and from `prog`'s own profile/schema version. An observation may carry
one `SourceStateToken` with a source, operation, invocation subject scope,
capture time, provider, validity, and optional expiry.

Supported origins are:

- HTTP `ETag` (strong or weak), falling back to valid `Last-Modified`;
- an exact profile-declared JSON Pointer selecting a scalar change token;
- MCP content-block `annotations.lastModified` when the server supplies one.

Opaque profile and MCP values are persisted only as SHA-256 digests. Extraction
happens before payload redaction so a declared token may be used as state
evidence without leaking into the stored payload. Missing, non-scalar,
redacted, oversized, malformed, wildcard, ambiguous, and expired forms fail
closed as `validator_unavailable` or `validator_expired`.

## Declared selector

Advanced HTTP or CLI seed operations may declare one exact RFC 6901 pointer and
an optional expiry pointer:

```json
{
  "name": "get_entity",
  "method": "GET",
  "path": "/entities/{id}",
  "source_state": {
    "path": "/meta/change~1token",
    "expires_at_path": "/meta/expires_at"
  }
}
```

The pointer cannot contain wildcards. `/meta/change~1token` selects the literal
key `change/token`; `/items` never selects `/items2`. The selected value must be
one bounded string, number, or boolean. Expiry must be an RFC 3339 string.

## HTTP revalidation

`prog call SOURCE OP --args '{}' --refresh` conditionally revalidates a cached
HTTP observation only when all of these still match:

- source and operation;
- exact call arguments/subject scope;
- authentication policy and current credential identity;
- effective effect policy;
- an unexpired HTTP validator.

A `304 Not Modified` creates a new immutable observation that references the
prior payload and sets `source_validity: confirmed_unchanged`. A changed `200`
creates a new payload/token and reports `source_changed`. No validator reports
`validator_unavailable`.

A non-success refresh response is stored as separate, navigable error evidence
with `refresh_failed`; it does not replace or relabel the prior successful cache
entry. A transport failure likewise cannot make old evidence current.

## Comparison

Matching scoped tokens of the same kind prove `confirmed_unchanged`; differing
tokens prove `source_changed`. Missing, expired, malformed, differently scoped,
or differently typed tokens remain unknown. This pairwise assessment feeds
delta, readiness, verification, and the `status` facade. It does not infer
freshness from age.

# Safety model

`prog` treats source profiles as executable configuration. The safety model is intentionally fail-closed: missing or partial effect metadata becomes restrictive instead of permissive.

## Effect flags

Each operation has an `effects` set:

| Flag | Meaning |
|---|---|
| `read_only` | The operation should not mutate upstream state. |
| `mutating` | The operation may write, delete, create, or otherwise change state. |
| `network` | The operation uses network access. |
| `shell` | The operation runs through a shell-backed CLI path. |
| `sensitive` | The operation may handle secrets or sensitive payloads. |
| `cacheable` | The response may be persisted when cache policy allows it. |
| `requires_confirmation` | A human confirmation flag is required before call execution. |

HTTP `GET` defaults are hardened toward read-only network access. Non-GET HTTP operations become mutating and require confirmation. CLI operations without complete effect metadata are treated as unsafe. MCP tools use server annotations such as `readOnlyHint`, then harden conflicting or missing claims.

Effects are grounded in the configured adapter, not only in profile metadata.
An operation served by the HTTP adapter is always network-backed and an
operation served by the shell-backed CLI adapter is always shell-backed, even
if a hand-authored profile tries to understate those flags. Adapter facts are
tightened into the effective effect set before policy, cache identity, or
discovery see them.

## Fail-closed rules

Discovery probing only invokes operations that are read-only, non-mutating, and do not require confirmation. Unsafe operations stay in the profile but are skipped during `--probe`.
Calls enforce three gates:

```bash
prog call <source-id> <operation> --args '<json>' --yes
```

`--yes` is required for mutating operations or operations marked
`requires_confirmation`. It is not enough for shell-backed or network-backed
operations.

```json
{
  "trust": {
    "allow_shell": true,
    "allow_network": true
  }
}
```

`trust.allow_shell` must be present in the source profile before shell-backed
operations can run, and `trust.allow_network` must be present before
network-backed operations can run. Both gates fire before transport — even
`--yes` does not bypass them, and a missing flag is a `network_not_trusted` or
`shell_not_trusted` error rather than a silent attempt. Set them only for
profiles whose source you are willing to contact or execute.

### Network boundary

Once network access is trusted, it stays scoped to the declared source origin:

- HTTP redirects are followed only within the source origin (scheme, host,
  and port must match); a cross-origin redirect is refused before the foreign
  server is contacted, and more than ten redirects are refused as well.
- Pagination continuations (`Link rel="next"` or body cursor fields) must
  target the same origin and are issued as forced GETs that never replay the
  base operation's request body.
- Transport error messages are stripped of request and redirect URLs and
  scanned for secret-shaped text before they can reach structured output, so
  a failed connection cannot leak a credential-bearing URL.
- The final URL recorded in provenance is redacted of sensitive argument
  values, including query parameters introduced by a redirect.

## Graded evidence and auto-upgrade

Importers stamp a `requires_confirmation` gate plus a graded `evidence_grade` on every derived operation, recording how strongly the source descriptor declares the effect. Trust policy then evaluates that evidence at call and discovery time.

| Grade | Meaning | May skip confirmation? |
|---|---|---|
| `proven` | The descriptor explicitly declares the effect: HTTP `GET`/`HEAD`/`OPTIONS`, an MCP tool with `readOnlyHint: true` and no contradictory `destructiveHint`, an MCP resource. | Yes, under `trust.auto_upgrade: true` (the default). |
| `assumed` | The effect is inferred from method or shape, not declared: a JSON Schema synthesized op. | Never. Hard-fenced. |
| `unproven` | Ambiguous or absent: non-GET HTTP, an MCP tool without `readOnlyHint`, a contradictory `readOnlyHint`+`destructiveHint`, CLI help text. | Never. |

Imported read-only operations are **stored** with `requires_confirmation: true` (the conservative default) and **relaxed** to `false` at call/discovery time when the descriptor is *proven* read-only and `trust.auto_upgrade` is enabled. Mutating, shell-backed, and sensitive operations are never relaxed (I7 preserved); `assumed`/`unproven` evidence is never relaxed.

`trust.auto_upgrade` is a per-source escape hatch and a live post-import knob:

```json
{
  "trust": {
    "auto_upgrade": false
  }
}
```

Flipping it to `false` re-gates even *proven* read-only operations, restoring the strict V1 behavior (calls need `--yes`, discovery skips them with the I6 warning) without re-importing. Default is `true`.

Every auto-upgrade records its evidence chain so the decision is inspectable: the relaxed `EffectSet` carries an `extra.auto_upgrade` stamp, and the call envelope surfaces a structured record under `observation.trust.extra.auto_upgrade` (`{grade, relaxed_requires_confirmation, reason}`). A call that was not upgraded leaves that field absent.

## Redaction

Before inference and persistence, `prog` redacts object fields whose names look secret-bearing:

`password`, `passwd`, `secret`, `token`, `api_key`, `apikey`, `authorization`, `credential`, `private_key`, `session`, `cookie`, and `bearer`.

HTTP and CLI adapters also redact sensitive argument values from provenance URLs, command argv, and recorded args. Operation seeds can list explicit `sensitive_args` to extend this behavior.

The default value scan also recognizes quoted sensitive JSON keys embedded in
text, including escaped key names and quoted values. `run` redacts each complete
stream before deriving text/head/tail views, and uses that stream's full context
when redacting interleaved output fragments. Explicit markers replace secret
content; benign quoted fields remain visible. Redaction applies the supported
name/value policy and does not guarantee detection of arbitrary secret formats.

Verification obligation descriptions and advisory reasons pass through text and
persistence redaction at `Store::put_obligation`. Extension metadata uses the
default persistence policy. Recognized sensitive data in exact argv (including
advisory argv), source operations, scope/family constraints, or identity fields
is rejected with a structured error that does not echo the input. These fields
are never silently rewritten into a different check. The store returns the safe
declaration for display, preserving system identifiers such as `session_id`.

The pre-release store contract resets older local observation/session stores on
opening; records written without the obligation metadata boundary are not reused.

Sensitive operations are not cached. If a persisted payload would contain redacted fields, the envelope includes a warning with the count of redacted paths.

## Profiles and cache

Profiles are committable when they describe stable sources and do not embed secrets. Use environment references in `auth` instead of literal credentials. Cache data is not committable: it contains captured upstream payloads, cursor state, and provenance.

## Counterexamples

- A `POST` seed that claims `read_only: true` is hardened to mutating and requires `--yes`.
- A CLI seed with only `read_only: true` still defaults missing flags to unsafe values and is skipped by discovery probing.
- A shell-backed operation with `--yes` still fails unless the profile has `trust.allow_shell: true`.
- A network-backed operation with `--yes` still fails before transport unless the profile has `trust.allow_network: true`, even when profile metadata claims `effects.network: false`.
- An HTTP redirect to a different origin is refused before the foreign host is contacted, and the resulting error never includes the credential-bearing URL.
- A response containing `token` is persisted with that value replaced by a redaction marker.

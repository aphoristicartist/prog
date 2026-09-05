# Verification obligations and readiness

A verification obligation is a **claim you commit to before you have the
evidence for it.** You declare what must be true for work to count as done;
`prog` later evaluates whether the evidence you attached actually establishes it.

This exists because the failure mode in an agent loop is not usually a wrong
answer — it is an unearned "done". An agent that reruns a narrower test, reads a
truncated log, or checks a stale observation can produce output that looks like
success. Obligations make the success criterion explicit and machine-checkable.

```sh
prog session obligation-add <id> --check <description> --scope <scope> [options]
prog session obligation-list
prog session show --readiness
```

For a stateful system changed outside `prog`, use a non-coding read-back
obligation:

```sh
prog verification begin \
  --pre-observation "$PRE" \
  --read-args '{"id":"entity-1"}' \
  --identity-path /id \
  --version-path /version \
  --expected '{"/state":"enabled"}'

# Execute the mutation externally. prog does not run it.

prog verification readback "$INTENT_ID"
```

`begin` persists an immutable `ActionIntent` containing the source/read
operation, redaction-safe structured read arguments, hashes of the pre-change
identity and version, expected path/value mappings, and a required user
obligation. It rejects missing, wildcard, malformed, redacted, oversized, or
non-scalar identity/version mappings. `--eventual-consistency-ms` optionally
defines the only interval in which a mismatch may remain `pending`.

`readback` rechecks that the stored operation is protocol- or
descriptor-proven read-only, then invokes it through the ordinary `prog call`
policy and adapter path with a forced refresh. It never executes a mutation,
retry, rollback, or repair. `--mutation-response <observation-id>` may link a
separately captured response as evidence; it does not authorize replaying that
operation. Repeating `readback` performs another independent read and creates
a new immutable receipt.

### Read-back receipt statuses

| Status | Meaning |
|---|---|
| `verified` | Identity matches, every expected value matches, and the entity version advanced. |
| `mismatched` | A fresh read completed but at least one expected value differs after any consistency window. |
| `stale_precondition` | A 409/412 response or divergent mutation-response/read-back versions prove the mutation precondition was stale or another writer intervened. |
| `pending` | Expected state is not visible and the explicitly declared consistency window is still open. |
| `readback_failed` | The independent read failed at transport or upstream level. |
| `unverifiable` | Identity, version, mapping, payload, redaction, retention, or validator evidence is insufficient. |

Receipts link the intent, pre-observation, optional mutation-response
observation, read-back observation, conservative comparability assessment, and
the readiness obligation. Readiness maps `verified` to `passed`; every other
receipt remains blocking except `pending`, which remains pending. Raw entity
identity/version values are not copied into the intent or receipt.

The agent-facing facade exposes the identical report, optionally alongside the
canonical delta and comparability assessment:

```bash
prog status --baseline "$BEFORE" --subject "$AFTER"
```

Omit the observation pair to request readiness alone. `--session-id` selects a
specific session; otherwise the active session is used.

## Declaring an obligation

```sh
prog session start --goal "fix checkout timeout"

prog session obligation-add checkout-fixed \
  --check "checkout error no longer present" \
  --scope checkout \
  --origin-observation "$BASELINE_ID" \
  --expected-absent-fingerprint "$FINGERPRINT" \
  --evidence-observation "$VERIFICATION_ID"
```

**User-declared obligations are immutable.** Re-declaring an existing id in the same session
is a `bad_args` error, not an update. You cannot quietly move the goalposts
after seeing a result — which is the point. Attach the evidence observation at
declaration time, or declare a new obligation with a new id.

`verification begin` creates a special read-back obligation whose declared
criterion remains immutable. Each `verification readback` may attach only its
new evidence and receipt id; it cannot change the intended check, expected
state, entity fingerprints, source operation, or required scope.

Get `$FINGERPRINT` from a finding's `fingerprint` field, via `prog delta` or an
envelope's `findings`.

### Options

| Option | Effect |
|---|---|
| `--check` | Human-readable description of the intended check. Required. |
| `--scope` | Scope the check covers, such as `target` or `regression-suite`. Required. |
| `--comparison-family` | Canonical invocation family expected for evidence, such as `checkout-log`. Must match the evidence observation's own `--comparison-family` (set via `prog run`) or the obligation evaluates to `stale`. |
| `--origin-observation` | Baseline observation holding the finding that must disappear. |
| `--expected-absent-fingerprint` | The finding fingerprint that must be gone. |
| `--evidence-observation` | The observation used to evaluate the obligation. |
| `--expected-argv` | Exact argv the evidence must represent. Structured data, never a shell string. |
| `--source-operation` | Source-native operation the evidence must represent. |
| `--required-state` | `any`, `workspace-unchanged`, `source-unchanged`, or `workspace-and-source-unchanged`. |
| `--optional` | Advisory only; does not block readiness. |
| `--advisory-argv` | A displayed hint. Never auto-run, and running it does not satisfy the obligation. |
| `--declared-by` | `user`, `recipe`, `normalizer`, or `harness`. |

`--origin-observation` and `--expected-absent-fingerprint` must be supplied
together; supplying one alone evaluates to `unknown`.

### Only users can require

`--declared-by` defaults to `user`. **Only `user` declarations can be
`required`** — `recipe`, `normalizer`, and `harness` declarations are advisory by
contract, regardless of flags. A component cannot declare an obligation that
authorizes its own success.

For the same reason, `--advisory-argv` is displayed and never executed, and its
`does_not_satisfy` field names the obligation it cannot discharge on its own.

## Evaluating readiness

```sh
prog session show --readiness
```

```json
{
  "schema": "prog.verification",
  "configured": true,
  "ready": false,
  "evaluations": [
    {
      "obligation": { "id": "checkout-fixed", "required": true },
      "status": "pending",
      "reasons": ["no evidence observation has been attached"]
    }
  ],
  "blockers": ["checkout-fixed: no evidence observation has been attached"]
}
```

`ready` is true only when every **required** obligation has status `passed`.
`configured` is false when no obligations are declared at all — an unconfigured
session is not a passing one, and a loop should treat the two differently.

`prog session obligation-list` returns the same report shape.

### Statuses

| Status | Meaning |
|---|---|
| `passed` | The expected finding is absent under a comparable, complete observation. |
| `failed` | The evidence command did not exit successfully. |
| `pending` | No evidence observation has been attached yet. |
| `persisting` | The finding is still present in the evidence. |
| `new` | The finding is gone, but the evidence contains **new** regression findings. Reported under `new_regressions`. |
| `not_observed` | The evidence did not cover the region where the finding would appear. |
| `stale` | Workspace or source state changed since capture, or the evidence does not match a declared constraint. |
| `unverifiable` | The evidence is missing, evicted, incomplete, or truncated. |
| `unknown` | Comparability could not be established. |

Note that `passed` is one status out of nine, and seven of the others are ways of
saying "not proven". A truncated log yields `unverifiable`, not `passed`. A
narrower rerun yields `not_observed`, not `passed`. Fixing the error but
introducing a new one yields `new`, not `passed`.

## How a pass is decided

Declarations redact check descriptions and advisory reasons before storage and
display. Recognized secrets in expected/advisory argv, source operations, scope,
comparison family, and identity fields cause a `bad_args` rejection. Use
secret-free declarations: replacing part of exact argv or a scope constraint
would change what evidence can satisfy the check. This boundary also applies to
library callers of `Store::put_obligation`, which returns the safe stored record.

When an obligation names both an origin observation and an expected-absent
fingerprint, evaluation runs a [conservative delta](delta.md) between the origin
and the evidence, then maps the finding's delta status:

```
delta resolved     -> passed        (unless new regressions exist -> new)
delta persisting   -> persisting
delta new          -> new
delta not_observed -> not_observed
delta unknown      -> unknown
```

So an obligation inherits every guarantee from `prog delta`: it can only pass
when absence is provable. See [`delta.md`](delta.md) for the full precondition
list.

When an obligation names neither, evaluation falls back to the evidence
command's exit status at `/command/success`.

Before any of that, evaluation rejects evidence that is unavailable, evicted,
incomplete, truncated, from a changed workspace, or from a mismatched operation.

## Full loop example

```sh
prog session start --goal "fix checkout timeout"

# 1. Capture the failing baseline with a declared comparison contract.
BASE=$(prog run --comparison-family checkout-log \
  --selection-scope checkout --selection-exhaustive -- ./run-checkout.sh \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["observation"]["observation_id"])')

# 2. Identify the finding that must disappear.
FP=$(prog delta "$BASE" "$BASE" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["findings"][0]["fingerprint"])')

# 3. Make the fix, then capture an identical verification run.
SUBJ=$(prog run --comparison-family checkout-log \
  --selection-scope checkout --selection-exhaustive -- ./run-checkout.sh \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["observation"]["observation_id"])')

# 4. Declare the obligation against that evidence and evaluate.
prog session obligation-add checkout-fixed \
  --check "checkout error no longer present" --scope checkout \
  --origin-observation "$BASE" --expected-absent-fingerprint "$FP" \
  --evidence-observation "$SUBJ"

prog session show --readiness
```

A calling loop should gate on `ready`, and on `blockers` for the explanation.
`prog` reports readiness; it does not merge, deploy, or approve anything.

## Example harness consumer

[`fixtures/harness/readiness_consumer.py`](../fixtures/harness/readiness_consumer.py)
is a tested reference consumer for hosts that choose to gate loop completion.
`prog` remains advisory: the harness owns the pass/block decision and invokes
the consumer explicitly:

```bash
prog session show --readiness | python3 fixtures/harness/readiness_consumer.py
```

The example passes only a configured `prog.verification` report whose
`ready` value is true and whose blocker list is empty. Unknown schema,
malformed input, and inconsistent readiness fields block conservatively. Its
JSON response retains at most eight blockers of 512 characters each; exit
codes are 0 for pass, 1 for an ordinary evidence blocker, and 2 for invalid or
inconsistent input. It does not run checks, infer obligations, or grant
permission to merge, deploy, send, refund, or approve.

## Related

- [Conservative observation delta](delta.md) — the comparison underneath a pass.
- [Evidence and observations](evidence.md) — observation records and lineage.
- [Replay evaluation](replay-eval.md) — the oracle gating readiness correctness.
- `prog meta VerificationObligation`, `prog meta ObligationEvaluation`,
  `prog meta ReadinessReport`, `prog meta ActionIntent`, and
  `prog meta ReadbackVerificationReceipt` — generated contract schemas.

# prog invariants

This table maps the RFC 0002 invariant set to executable tests. Property tests run as part of `cargo test`; generated proptest regression seeds are persisted beside the test modules.

| # | Invariant | Harness |
|---|---|---|
| I1 | Projection never invents values. Preview leaves must equal the source leaf at that path, be a marker, or be an explicit truncated prefix. | `crates/prog-core/tests/disclosure.rs::projection_never_fabricates_values` |
| I2 | Persistence-redacted data never reaches disk. | `crates/prog-core/tests/store.rs::persistence_redaction_is_idempotent_and_removes_secret_values`; composed with expansion in `redacted_payload_stays_redacted_through_store_and_expansion` |
| I3 | Expansion never escapes the cursor provenance boundary, segment-wise and escaping-aware. | `crates/prog-core/tests/disclosure.rs::pointer_containment_is_segment_based`; `expansion_rejects_generated_segment_siblings`; unit cases in `expand_rejects_paths_outside_cursor_boundary_segment_wise` |
| I4 | Redaction is idempotent. | `crates/prog-core/tests/store.rs::persistence_redaction_is_idempotent_and_removes_secret_values` |
| I5 | Shape join is commutative, associative, idempotent, monotone; `Unknown` is identity; enum-cap absorption is order-independent. | `crates/prog-core/tests/shape.rs::{join_is_commutative,join_is_associative,join_is_idempotent,unknown_is_join_identity,join_is_monotone_by_absorption,string_enum_absorption_is_associative_at_cap_boundary}` |
| I6 | Discovery never invokes non-read-only operations. | `crates/prog-cli/tests/cli.rs::probe_skips_effectless_operations_with_i6_warning`; policy refusal units in `crates/prog-core/tests/policy.rs::discovery_refuses_each_unsafe_effect_independently` |
| I7 | Mutating, shell-backed, and sensitive operations fail closed without flags/trust. | `crates/prog-core/tests/policy.rs::call_policy_requires_confirmation_and_shell_trust`; CLI integration in `crates/prog-cli/tests/cli.rs::call_validates_args_and_enforces_effect_policy` |
| I8 | Non-cacheable or sensitive results are never persisted. | `crates/prog-core/tests/store.rs::entries_respect_ttl_and_non_cacheable_sensitive_results_are_not_persisted`; `crates/prog-core/tests/policy.rs::cache_policy_respects_enabled_cacheable_and_sensitive_flags` |
| I9 | Stale, foreign, or incompatible cursors fail actionably and never return wrong data. | `crates/prog-core/tests/store.rs::cursors_fail_closed_for_missing_expired_and_redaction_mismatch`; CLI missing cursor coverage in `crates/prog-cli/tests/cli.rs::missing_call_and_expand_inputs_return_structured_errors` |

## Property strategy

The arbitrary JSON strategy in `crates/prog-core/tests/disclosure.rs` is bounded by depth and width and includes long strings, redaction sentinels, unicode text, escaped pointer keys, and sensitive-looking field names. The arbitrary shape strategy in `crates/prog-core/tests/shape.rs` generates nested shapes and exact enum-cap value sets to keep the known string-enum absorption boundary under test.

## CI

`.github/workflows/ci.yml` runs the normal gate:

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo run -- --help`

Because the property harnesses are ordinary Rust tests, they run in the same CI job as unit and integration tests.

## Kani evaluation

The pure functions targeted for future model checking are:

- `prog_core::pointer::{parse,is_within}`
- `prog_core::disclosure::{project,expand,slice_value}`
- `prog_core::redaction::RedactionPolicy::apply_persistence`
- `prog_core::shape::join`

Kani harnesses are not enabled in this PR because the repository has no pinned Kani toolchain or CI install path; adding one would make the standard gate depend on a non-Cargo setup. The proptest harnesses are intentionally written against pure, dependency-free core functions so they can be moved to feature-gated Kani/PropProof harnesses without rewriting the laws.

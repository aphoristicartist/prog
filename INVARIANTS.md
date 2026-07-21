# prog invariants

This table maps the RFC 0002 invariant set to executable tests. Property tests run as part of `cargo test`; generated proptest regression seeds are persisted beside the test modules.

| # | Invariant | Harness |
|---|---|---|
| I1 | Projection never invents values. Preview leaves must equal the source leaf at that path, be a marker, or be an explicit truncated prefix. | `crates/prog-core/tests/disclosure.rs::projection_never_fabricates_values` |
| I2 | Persistence-redacted data never reaches disk. | `crates/prog-core/tests/store.rs::persistence_redaction_is_idempotent_and_removes_secret_values`; composed with expansion in `redacted_payload_stays_redacted_through_store_and_expansion`; API boundary in `crates/prog-core/tests/lifecycle.rs::payload_typestate_requires_redaction_before_persistence`; value-pattern redaction of secrets embedded in string values in `crates/prog-core/tests/redaction.rs::{value_embedded_sensitive_name_value_pair_equals_is_redacted,value_embedded_sensitive_name_value_pair_colon_is_redacted,value_scan_never_persists_embedded_high_confidence_secret}` |
| I3 | Expansion never escapes the cursor provenance boundary, segment-wise and escaping-aware. | `crates/prog-core/tests/disclosure.rs::pointer_containment_is_segment_based`; `expansion_rejects_generated_segment_siblings`; unit cases in `expand_rejects_paths_outside_cursor_boundary_segment_wise`; scoped cursor capability tests in `crates/prog-core/tests/lifecycle.rs::{scoped_slice_validates_json_pointer_syntax_and_scope,validated_cursor_creates_expansion_scope_capability}` |
| I4 | Redaction is idempotent. | `crates/prog-core/tests/store.rs::persistence_redaction_is_idempotent_and_removes_secret_values`; value-scan idempotency (redaction markers are never reclassified as secrets) in `crates/prog-core/src/redaction.rs::tests::apply_persistence_detailed_is_pure_and_idempotent` and `crates/prog-core/tests/redaction.rs::value_scan_is_idempotent` |
| I5 | Shape join is commutative, associative, idempotent, monotone; `Unknown` is identity; enum-cap absorption is order-independent. | `crates/prog-core/tests/shape.rs::{join_is_commutative,join_is_associative,join_is_idempotent,unknown_is_join_identity,join_is_monotone_by_absorption,string_enum_absorption_is_associative_at_cap_boundary}` |
| I6 | Discovery never invokes non-read-only operations. | `crates/prog-cli/tests/cli.rs::probe_skips_effectless_operations_with_i6_warning`; policy refusal units in `crates/prog-core/tests/policy.rs::discovery_refuses_each_unsafe_effect_independently`. Discovery now evaluates `effective_effects(op, trust)`: a *proven* read-only op is probeable under default `trust.auto_upgrade`, and is skipped with the I6 warning when `trust.auto_upgrade=false` (re-gated) — see `crates/prog-core/tests/policy.rs::auto_upgrade_escape_hatch_re_gates_proven_read_only`. |
| I7 | Mutating, shell-backed, and sensitive operations fail closed without flags/trust. | `crates/prog-core/tests/policy.rs::call_policy_requires_confirmation_and_shell_trust`; CLI integration in `crates/prog-cli/tests/cli.rs::call_validates_args_and_enforces_effect_policy`. Graded-evidence executable coverage: `crates/prog-cli/tests/cli.rs::call_openapi_get_records_auto_upgrade_audit_in_observation_trust` (only *proven* read-only evidence relaxes confirmation) and `call_openapi_get_requires_yes_when_auto_upgrade_disabled_on_profile` (escape hatch re-gates even *proven*). `assumed`/`unproven` stay gated; the relaxation law is `crates/prog-core/tests/policy.rs::effective_effects_relaxes_only_proven_read_only_under_auto_upgrade`. |
| I8 | Non-cacheable or sensitive results are never persisted. | `crates/prog-core/tests/store.rs::entries_respect_ttl_and_non_cacheable_sensitive_results_are_not_persisted`; `crates/prog-core/tests/policy.rs::cache_policy_respects_enabled_cacheable_and_sensitive_flags` |
| I9 | Stale, foreign, or incompatible cursors fail actionably and never return wrong data. | `crates/prog-core/tests/store.rs::cursors_fail_closed_for_missing_expired_and_redaction_mismatch`; CLI missing cursor coverage in `crates/prog-cli/tests/cli.rs::missing_call_and_expand_inputs_return_structured_errors` |
| I10 | Findings ranking is pure, deterministic, and order-independent of input key order. | `crates/prog-core/tests/findings_proptest.rs::{ranked_findings_is_pure_and_deterministic,ranking_is_order_independent_of_key_order,ranks_are_contiguous_and_confidences_bounded}`; golden snapshots in `crates/prog-core/tests/fixtures/findings/*.expected.json` |
| I11 | Auto-pagination never escapes the effect policy or the envelope budget: only read-only/GET operations are followed; PageCaps (pages/bytes/wall) always stop with a continuation; every page is redacted -> inferred -> stored -> projected; the final envelope stays within `max_envelope_bytes`. | `crates/prog-core/tests/pagination.rs::pagination_respects_effect_policy_and_envelope_budget`; CLI end-to-end in `crates/prog-cli/tests/cli.rs::pagination_follows_readonly_and_stops_at_caps`; effect gate in `crates/prog-cli/tests/cli.rs::prog_call_pages_skipped_for_mutating_operation_emits_warning`; page-cursor fail-closed reuse in `crates/prog-core/tests/pagination.rs::page_cursors_fail_closed_on_redaction_mismatch_and_foreign_source` |
| I12 | Inspect, search, evidence, and lens findings read only persisted redacted payloads, remain inside cursor scope, and stay bounded. | `crates/prog-core/tests/navigation.rs::{cached_search_supports_text_regex_key_kind_and_scope,search_and_evidence_are_bounded_and_preserve_redaction,lens_findings_resolve_existing_wildcards_and_reject_path_escape}`; CLI workflow in `crates/prog-cli/tests/cli.rs::evidence_navigation_workflow_is_offline_scoped_bounded_and_session_backed` |
| I13 | Session trails contain metadata references only, survive store reopen, cap retained events, and purge with cache privacy state. | `crates/prog-core/tests/store.rs::session_trail_is_persistent_bounded_and_purged_with_cache` |
| I14 | A `resolved` delta classification implies the finding's evidence is absent from the subject's persisted payload — never merely absent from a bounded derivation window (head/tail slice, rank cap, or traversal cap) over a payload that still contains it. | `crates/prog-core/src/delta.rs::tests::assess_is_not_provable_when_capture_was_derivation_windowed`; CLI end-to-end in `crates/prog-cli/tests/cli.rs::delta_never_reports_resolved_for_a_finding_that_moved_into_the_derivation_window` |

## Property strategy

The arbitrary JSON strategy in `crates/prog-core/tests/disclosure.rs` is bounded by depth and width and includes long strings, redaction sentinels, unicode text, escaped pointer keys, and sensitive-looking field names. The arbitrary shape strategy in `crates/prog-core/tests/shape.rs` generates nested shapes and exact enum-cap value sets to keep the known string-enum absorption boundary under test.

## Typestate boundaries

Payloads that come from APIs, CLIs, MCP servers, imported examples, or observed artifacts enter core code as `RawPayload` and must be converted to `RedactedPayload` before they can be stored. `Store::put_payload` does not accept plain `serde_json::Value`. Stored payloads come back as `PersistedPayload`, and cursor-backed expansion requires a `ValidatedCursor` plus `ScopedSlice`, so raw cursor strings and unvalidated JSON Pointer strings cannot reach the expansion function directly.

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
- `prog_core::redaction::RedactionPolicy::apply_persistence_detailed`
- `prog_core::shape::join`
- `prog_core::pagination::{extract_pagination_hints,next_args_from_hints,merge_page_shapes}`

Kani harnesses are not enabled in this PR because the repository has no pinned Kani toolchain or CI install path; adding one would make the standard gate depend on a non-Cargo setup. The proptest harnesses are intentionally written against pure, dependency-free core functions so they can be moved to feature-gated Kani/PropProof harnesses without rewriting the laws.

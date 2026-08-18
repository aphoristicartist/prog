# Replay eval

This deterministic harness replays whole multi-iteration agent observation trajectories, not single envelopes, and gates every conservative-delta and verification-readiness correctness claim behind an oracle that must never observe a false `resolved`, false-fresh, or false-`passed` classification. It is not a model-quality benchmark.

Regenerate this report and the raw metrics with `PROG_REPLAY_EVAL_BLESS=1 cargo test -p prog-cli --test replay_eval -- --nocapture`.

Strategies marked unavailable (`evidence_packet`, `ranked_retrieval`) are reported as unavailable, never simulated: issues #116 and #118 have not landed.

The fixture inventory distinguishes generated, recorded public-live, and optional credentialed inputs. Credentialed capture is never required in CI, and neither raw credentials nor credentialed payloads are committed.

**This report makes no aggregate savings claim.** The byte/token/call columns exist to make cost and no-benefit controls visible. Token estimates use the named `bytes/4-ceiling` estimator over delivered bytes. Token/call savings evidence lives in `docs/token-economics.md`, `docs/task-success-eval.md`, and `docs/competitive-baselines.md`, which use realistic payload sizes. This report's claim is narrower and, for the loop kernel, more load-bearing: every delta, fingerprint, and readiness classification below is correct across a real multi-iteration trajectory.

## Summary

12 scenarios, 57/57 correctness checks passing; 6/11 comparison pairs can prove absence; 34/35 compared findings have fingerprints; 0 false freshness/resolution/readiness decisions.

## Fixture sources

| Kind | Checked in | CI required | Description |
|---|---:|---:|---|
| generated | true | true | deterministic fixtures generated locally by the harness |
| recorded_public_live | true | true | redacted recording of a public, unauthenticated endpoint |
| credentialed_optional | false | false | optional local capture; credentials and raw payloads are never committed |

## multi_iteration_resolution (`multi_iteration_resolution`)

Fixture source: `generated`. Wall time: 1393 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 119 | 30 | 3 |
| simple_truncation | true | 119 | 30 | 3 |
| prog_envelope | true | 25210 | 6303 | 3 |
| prog_delta | true | 13676 | 3419 | 5 |
| evidence_packet | false | 0 | 0 | 0 |
| ranked_retrieval | false | 0 | 0 | 0 |

Evidence available: true; first-view hit: true; comparison coverage: 2/2; fingerprint coverage: 6/6; budget compliant: true; redaction compliant: true; false decisions: 0.

| Delta status | Expected | Correct |
|---|---:|---:|
| new | 1 | 1 |
| persisting | 3 | 3 |
| resolved | 1 | 1 |

Checks:

- `alpha_persists_despite_line_position_shift`: pass
- `alpha_persists_iteration_2_to_3`: pass
- `beta_resolved_after_iteration_2`: pass
- `fingerprint_stable_across_three_iterations`: pass
- `gamma_new_at_iteration_2`: pass
- `gamma_persists_iteration_2_to_3`: pass
- `iteration1_to_2_can_prove_absence`: pass
- `small_payload_envelopes_report_raw_cheaper`: pass

## pytest_multi_iteration_failure_loop (`pytest_loop`)

Fixture source: `generated`. Wall time: 344 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 1416 | 354 | 2 |
| simple_truncation | true | 1416 | 354 | 2 |
| prog_envelope | true | 20200 | 5050 | 3 |
| prog_delta | true | 22776 | 5694 | 3 |
| evidence_packet | false | 0 | 0 | 0 |
| ranked_retrieval | false | 0 | 0 | 0 |

Evidence available: true; first-view hit: true; comparison coverage: 1/1; fingerprint coverage: 6/6; budget compliant: true; redaction compliant: true; false decisions: 0.

| Delta status | Expected | Correct |
|---|---:|---:|
| new | 1 | 1 |
| persisting | 1 | 1 |
| resolved | 1 | 1 |

Checks:

- `compacted_delta_preserves_complete_counts`: pass
- `complete_loop_can_prove_absence`: pass
- `delta_output_respects_disclosure_budget`: pass
- `loop_has_new_failure`: pass
- `loop_has_persisting_failure`: pass
- `loop_has_resolved_failure`: pass
- `new_failure_evidence_is_exactly_recoverable`: pass

## cargo_multi_iteration_failure_loop (`cargo_loop`)

Fixture source: `generated`. Wall time: 354 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 901 | 226 | 2 |
| simple_truncation | true | 901 | 226 | 2 |
| prog_envelope | true | 21171 | 5293 | 3 |
| prog_delta | true | 23588 | 5897 | 3 |
| evidence_packet | false | 0 | 0 | 0 |
| ranked_retrieval | false | 0 | 0 | 0 |

Evidence available: true; first-view hit: true; comparison coverage: 1/1; fingerprint coverage: 6/6; budget compliant: true; redaction compliant: true; false decisions: 0.

| Delta status | Expected | Correct |
|---|---:|---:|
| new | 1 | 1 |
| persisting | 1 | 1 |
| resolved | 1 | 1 |

Checks:

- `compacted_delta_preserves_complete_counts`: pass
- `complete_loop_can_prove_absence`: pass
- `delta_output_respects_disclosure_budget`: pass
- `loop_has_new_failure`: pass
- `loop_has_persisting_failure`: pass
- `loop_has_resolved_failure`: pass
- `new_failure_evidence_is_exactly_recoverable`: pass

## narrowed_rerun_no_false_resolved (`narrowed_rerun`)

Fixture source: `generated`. Wall time: 293 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| prog_delta | true | 3636 | 909 | 3 |

Evidence available: true; first-view hit: true; comparison coverage: 0/1; fingerprint coverage: 3/3; budget compliant: true; redaction compliant: true; false decisions: 0.

| Delta status | Expected | Correct |
|---|---:|---:|
| not_observed | 1 | 1 |

Checks:

- `can_prove_absence_is_false`: pass
- `missing_finding_marked_not_observed`: pass
- `missing_finding_not_marked_resolved`: pass
- `small_payload_envelopes_report_raw_cheaper`: pass

## realistic_payload_delta (`correctness_and_cost`)

Fixture source: `generated`. Wall time: 1817 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 238656 | 59664 | 2 |
| prog_delta | true | 22425 | 5607 | 3 |

Evidence available: true; first-view hit: false; comparison coverage: 1/1; fingerprint coverage: 3/3; budget compliant: true; redaction compliant: true; false decisions: 0.

| Delta status | Expected | Correct |
|---|---:|---:|
| persisting | 1 | 1 |
| resolved | 1 | 1 |

Checks:

- `full_capture_proves_absence`: pass
- `prog_delta_cheaper_than_raw_reread`: pass
- `removed_event_is_resolved`: pass
- `unchanged_event_persists`: pass

## no_benefit_tiny_payload_control (`no_benefit_control`)

Fixture source: `generated`. Wall time: 139 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 3 | 1 | 1 |
| prog_envelope | true | 4801 | 1201 | 1 |

Evidence available: true; first-view hit: true; comparison coverage: 0/0; fingerprint coverage: 0/0; budget compliant: true; redaction compliant: true; false decisions: 0.

Checks:

- `raw_cheaper_than_prog_for_tiny_payload`: pass
- `small_payload_envelope_reports_raw_cheaper`: pass

## stale_evidence_readiness_after_workspace_touch (`stale_workspace_state`)

Fixture source: `generated`. Wall time: 571 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| prog_verification_ledger | true | 947 | 237 | 3 |

Evidence available: true; first-view hit: true; comparison coverage: 0/0; fingerprint coverage: 0/0; budget compliant: true; redaction compliant: true; false decisions: 0.

Checks:

- `evidence_marked_stale_after_workspace_edit`: pass
- `fresh_evidence_reads_passed_before_edit`: pass
- `stale_reason_names_workspace`: pass

## derivation_window_moved_finding (`derivation_window_moved_finding`)

Fixture source: `generated`. Wall time: 299 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 678 | 170 | 2 |
| simple_truncation | true | 678 | 170 | 2 |
| prog_envelope | true | 15909 | 3978 | 2 |
| prog_delta | true | 9138 | 2285 | 3 |
| evidence_packet | false | 0 | 0 | 0 |
| ranked_retrieval | false | 0 | 0 | 0 |

Evidence available: true; first-view hit: false; comparison coverage: 1/1; fingerprint coverage: 2/2; budget compliant: true; redaction compliant: true; false decisions: 0.

| Delta status | Expected | Correct |
|---|---:|---:|
| persisting | 1 | 1 |

Checks:

- `full_text_capture_is_provable`: pass
- `moved_finding_is_not_falsely_resolved`: pass
- `moved_finding_remains_persisting`: pass
- `small_payload_envelopes_report_raw_cheaper`: pass

## noisy_log_one_changing_causal_event (`noisy_repeated_log`)

Fixture source: `generated`. Wall time: 347 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 1526 | 382 | 2 |
| simple_truncation | true | 1526 | 382 | 2 |
| prog_envelope | true | 22429 | 5608 | 3 |
| prog_delta | true | 11138 | 2785 | 3 |
| evidence_packet | false | 0 | 0 | 0 |
| ranked_retrieval | false | 0 | 0 | 0 |

Evidence available: true; first-view hit: true; comparison coverage: 0/1; fingerprint coverage: 2/2; budget compliant: true; redaction compliant: true; false decisions: 0.

| Delta status | Expected | Correct |
|---|---:|---:|
| new | 1 | 1 |
| unknown | 1 | 1 |

Checks:

- `new_causal_event_detected`: pass
- `only_causal_event_changes_in_fixture`: pass
- `redacted_capture_withholds_resolution`: pass
- `secret_is_redacted_from_initial_views_and_evidence`: pass

## compiler_diagnostics_reordered_and_shifted (`compiler_static_analysis`)

Fixture source: `generated`. Wall time: 235 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 844 | 211 | 2 |
| simple_truncation | true | 844 | 211 | 2 |
| prog_envelope | true | 15908 | 3977 | 2 |
| prog_delta | true | 10275 | 2569 | 3 |
| evidence_packet | false | 0 | 0 | 0 |
| ranked_retrieval | false | 0 | 0 | 0 |

Evidence available: true; first-view hit: true; comparison coverage: 0/1; fingerprint coverage: 6/6; budget compliant: true; redaction compliant: true; false decisions: 0.

| Delta status | Expected | Correct |
|---|---:|---:|
| persisting | 2 | 2 |

Checks:

- `location_shifts_do_not_change_fingerprints`: pass
- `persisting_diagnostics_move_array_positions`: pass
- `reordered_diagnostics_are_not_new_or_resolved`: pass
- `two_diagnostics_persist_after_reorder`: pass

## http_error_and_repeated_public_entity (`http_api_snapshot`)

Fixture source: `recorded_public_live`. Wall time: 409 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 390 | 98 | 3 |
| simple_truncation | true | 390 | 98 | 3 |
| prog_envelope | true | 15433 | 3859 | 4 |
| prog_delta | true | 8998 | 2250 | 3 |
| evidence_packet | false | 0 | 0 | 0 |
| ranked_retrieval | false | 0 | 0 | 0 |

Evidence available: true; first-view hit: true; comparison coverage: 0/1; fingerprint coverage: 0/1; budget compliant: true; redaction compliant: true; false decisions: 0.

Checks:

- `http_error_is_returned_and_persisted_as_evidence`: pass
- `http_error_secret_value_is_redacted`: pass
- `public_recording_is_redacted_and_checked_in`: pass
- `repeated_entity_snapshot_exposes_changed_fields`: pass
- `unknown_http_source_state_never_claims_resolution`: pass

## paginated_api_unchanged_and_changed_pages (`paginated_api`)

Fixture source: `generated`. Wall time: 453 ms (informational; excluded from deterministic correctness baselines).

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 146 | 37 | 4 |
| simple_truncation | true | 146 | 37 | 4 |
| prog_envelope | true | 13749 | 3438 | 4 |
| prog_delta | true | 10434 | 2609 | 3 |
| evidence_packet | false | 0 | 0 | 0 |
| ranked_retrieval | false | 0 | 0 | 0 |

Evidence available: true; first-view hit: true; comparison coverage: 0/1; fingerprint coverage: 0/0; budget compliant: true; redaction compliant: true; false decisions: 0.

Checks:

- `both_trajectories_fetch_two_pages`: pass
- `changed_downstream_page_hits_first_view_and_remains_navigable`: pass
- `changed_second_page_is_exactly_recoverable`: pass
- `unchanged_first_page_delta_has_no_false_resolution`: pass
- `unchanged_first_page_remains_identical`: pass


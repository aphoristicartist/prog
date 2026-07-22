# Replay eval

This deterministic harness replays whole multi-iteration agent observation trajectories, not single envelopes, and gates every conservative-delta and verification-readiness correctness claim behind an oracle that must never observe a false `resolved`, false-fresh, or false-`passed` classification. It is not a model-quality benchmark.

Regenerate this report and the raw metrics with `PROG_REPLAY_EVAL_BLESS=1 cargo test -p prog-cli --test replay_eval -- --nocapture`.

Strategies marked unavailable (`evidence_packet`, `ranked_retrieval`) are reported as unavailable, never simulated: issues #116 and #118 have not landed.

This is a baseline slice of #121's full scenario matrix. The HTTP/API snapshot, pagination, and noisy-log-with-one-changing-event categories remain future work.

**This report makes no savings claim.** Its scenario payloads are deliberately tiny (a handful of synthetic lines) so the suite stays fast and deterministic; at that scale `prog`'s envelope overhead legitimately costs more than raw output, matching the project's documented small-payload caveat. The byte/token/call columns exist to make that cost visible, not to claim a win. Token/call savings evidence lives in `docs/token-economics.md`, `docs/task-success-eval.md`, and `docs/competitive-baselines.md`, which use realistic payload sizes. This report's claim is narrower and, for the loop kernel, more load-bearing: every delta, fingerprint, and readiness classification below is correct across a real multi-iteration trajectory.

## Summary

5 scenarios, 16/16 correctness checks passing.

## multi_iteration_resolution (`multi_iteration_resolution`)

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 119 | 30 | 3 |
| simple_truncation | true | 119 | 30 | 3 |
| prog_envelope | true | 31257 | 7815 | 3 |
| prog_delta | true | 17743 | 4436 | 5 |
| evidence_packet | false | 0 | 0 | 0 |
| ranked_retrieval | false | 0 | 0 | 0 |

Checks:

- `alpha_persists_despite_line_position_shift`: pass
- `alpha_persists_iteration_2_to_3`: pass
- `beta_resolved_after_iteration_2`: pass
- `fingerprint_stable_across_three_iterations`: pass
- `gamma_new_at_iteration_2`: pass
- `gamma_persists_iteration_2_to_3`: pass
- `iteration1_to_2_can_prove_absence`: pass

## narrowed_rerun_no_false_resolved (`narrowed_rerun`)

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| prog_delta | true | 4918 | 1230 | 3 |

Checks:

- `can_prove_absence_is_false`: pass
- `missing_finding_marked_not_observed`: pass
- `missing_finding_not_marked_resolved`: pass

## no_benefit_tiny_payload_control (`no_benefit_control`)

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 3 | 1 | 1 |
| prog_envelope | true | 4451 | 1113 | 1 |

Checks:

- `raw_cheaper_than_prog_for_tiny_payload`: pass

## stale_evidence_readiness_after_workspace_touch (`stale_workspace_state`)

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| prog_verification_ledger | true | 947 | 237 | 3 |

Checks:

- `evidence_marked_stale_after_workspace_edit`: pass
- `fresh_evidence_reads_passed_before_edit`: pass
- `stale_reason_names_workspace`: pass

## derivation_window_moved_finding (`derivation_window_moved_finding`)

| Strategy | Available | Delivered bytes | Est. tokens | Calls |
|---|---:|---:|---:|---:|
| raw | true | 678 | 170 | 2 |
| simple_truncation | true | 678 | 170 | 2 |
| prog_envelope | true | 23206 | 5802 | 2 |
| prog_delta | true | 14533 | 3634 | 3 |
| evidence_packet | false | 0 | 0 | 0 |
| ranked_retrieval | false | 0 | 0 | 0 |

Checks:

- `assessment_is_non_provable_due_to_derivation_window`: pass
- `moved_finding_is_not_falsely_resolved`: pass


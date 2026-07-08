# Changelog

## Unreleased

- Added the generic findings ranking engine boundary: a stable `build_inspect_response(payload, &InspectRequest)` assembler plus `InspectRequest`/`CommandHintConfig` input types, honest command hints (`FindingCommandHints.evidence` is `None` by default until `prog evidence` ships; `NAV_ALL` opts back in), three new signal kinds (`compile_error`, `test_name`, `diff_hunk`) with strict rustc precedence, a `docs/findings.md` ranking reference, and determinism/order-independence/contiguity proptests with golden snapshots (#89).
- Added `prog call --pages N` upstream auto-pagination: follows cursor/page pagination for read-only operations under hard page/byte/time caps, prefetching pages into the local cache (#69).
- Added semantic table inference in `prog observe` for CSV/TSV (RFC 4180), GitHub markdown tables, and aligned/whitespace tables, exposed as bounded `/rows`-expandable payloads (#70).
- Activated graded-evidence trust auto-upgrade: importers stamp an `evidence_grade` (`proven`/`assumed`/`unproven`) on derived operations; imported read-only ops are stored confirmation-gated and relaxed to `requires_confirmation=false` at call/discovery time when the descriptor is *proven* read-only and `trust.auto_upgrade` is enabled (default). Mutating/shell/sensitive ops and `assumed`/`unproven` evidence are never relaxed; `trust.auto_upgrade=false` re-gates even *proven* ops. Each upgrade records its evidence chain under `observation.trust.extra.auto_upgrade` (#72).
- Added value-pattern redaction so secrets embedded in string values (Bearer tokens, PEM blocks, JWTs, sensitive URL parameters) are redacted before persistence (#73).
- Added tunable default redaction: a built-in allowlist (e.g. `max_tokens`, `session_timeout`), expanded secret keywords (`access_key`, `signing_key`, `pwd`), and per-source `RedactionConfig` with env overrides (#74).
- Hardened redaction for non-string (number/bool) declared-sensitive args and validated profile ids against path traversal.

- Added filtered path discovery and ranked expansion `next_actions` with exact cached `prog expand` argv.
- Added observation metadata for envelope completeness, freshness, trust, safety, and payload status.
- Added `prog run` for profile-free command capture with redacted cached stdout, stderr, failure sections, and optional preserved exit codes.
- Added `prog init --agent codex --project` for project-local skill and hook installation with dry-run and no-overwrite behavior.
- Added `EvidenceRef` metadata so agents can cite cursor/path-backed observations without pasting raw payloads.
- Added `prog cost` for profile-driven raw-vs-prog expensive-model cost planning.
- Added positioning docs comparing `prog` with native filters, truncation, RTK-style hooks, MCP gateways, and large-context models.
- Added first-party lens packs for command captures, text logs, NDJSON events, JSON item collections, and GitHub issue triage.
- Added `prog source add-http` and `prog source add-cli` to create simple source profiles without hand-authored seed JSON.
- Added bounded source-profile importers for OpenAPI, JSON Schema, MCP schemas, CLI help, and checked-in examples.
- Added deterministic task-success evals comparing raw, simple truncation, call-only, and targeted expansion strategies.
- Added competitive baseline evals against raw context, truncation, native field selection, RTK-style grep filtering, Caveman-style terse output, and repeated cache-backed expansion.
- Added a real-world demo suite for GitHub review, kubectl, CloudWatch, Jira, and MCP incident workflows with checked-in metrics.
- Added a deterministic observation parser/indexer pipeline with parser metadata for JSON, NDJSON, SARIF, JUnit XML, HTML, unified diffs, and text fallback.
- Added internal typestate boundaries for redacted payload persistence and scoped cursor-backed expansion.
- Added RFC 0003, defining observation lenses as the general progressive-disclosure model for agent artifacts.
- Added LensManifest v1 contracts, repo-local lens loading, and lens-driven call previews.
- Added progressive-disclosure docs, fixture walkthroughs, cache and safety notes, JSON contract documentation, and a token economics report.
- Added local HTTP, CLI, and MCP fixtures for copy-pasteable acceptance examples.

Release entries will use this file once versioned packages are cut.

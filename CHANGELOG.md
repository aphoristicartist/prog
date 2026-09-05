# Changelog

## Unreleased

- Fixed the `prog-cli` crate tarball so its embedded agent skill is packaged
  inside the crate and verified by the release dry-run build.
- Routed public-benchmark A/B records through the existing actual-agent claim
  gate, with generated Wilson intervals, exact paired McNemar results, explicit
  false-completion/dropout accounting, and the SWE-bench contamination caveat.
  A checked-in synthetic dry run proves incomplete usage stays claim-ineligible
  before any credentialed benchmark spend (#238).
- Added JUnit report recipes for Vitest, Playwright, Bun, and Deno plus SARIF
  recipes for Ruff, Biome, and Semgrep. Each recipe exposes the exact argv,
  records the child status, observes one private temporary report through the
  existing `junit`/`sarif` lens, and removes the temporary artifact afterward
  (#241).
- Preregistered the Terminal-Bench 2.0 paired public pilot before any live
  model spend: the benchmark, harness, model, resource-bounded seeded subset,
  raw/prog arm order, stopping rule, analysis, and falsification conditions are
  machine-readable and CI-gated; the checked-in outcome remains explicitly
  pending and claim-ineligible (#236).
- Extended route guidance to the 2026 toolchain: vitest, Playwright, Deno,
  uv, Ruff, Biome, tsc, Jest, Terraform/OpenTofu, Trivy, Semgrep, and
  `gh` log/API commands now receive conservative progressive-disclosure
  guidance; adjacent subcommands and watch modes stay unclassified (#239).
- Added six first-party lenses for GitHub Actions logs, Terraform plans,
  Trivy reports, MCP JSON-RPC errors, LLM provider errors, and OpenTelemetry
  records, and enforced that the lens README documents exactly the shipped
  manifests (#240).
- Bundled the first-party lens pack in the executable so recipes and cursor
  follow-ups work outside the source checkout. Project lenses override bundled
  ids by default; explicit lens directories retain exclusive selection (#254).
- Added the conservative live-trial accounting and claim-gate contract for the
  actual-agent evaluation: provider/model metadata, all fixed and provider
  token fields, calls/reruns/latency, dropouts, per-trial graders, and ordered
  uncertainty intervals. Missing provider fields remain unavailable, and the
  report cannot become claim-eligible without complete multi-trial evidence
  (#139).
- Added a CI-gated budget for the fixed agent integration surface: top-level
  help, immediate command help, and the portable skill must remain within
  34,000 bytes before any task work begins (#120).
- Repositioned `prog` as an agent-harness extension and added an end-to-end
  integration surface: auto-detected `prog harness install`/`doctor`, a portable
  Agent Skill target, a Codex marketplace plugin, and a native DeepSeek Harness
  `tools/post-execute` package that bounds accepted results without rerunning
  tools. Host capabilities are declared conservatively, package contracts are
  checked in CI, and the local CLI remains the single JSON transport for
  disclosure, evidence, and status.
- Made run completion evidence truthful (#228): incomplete, timed-out,
  cancelled, or truncated execution can no longer masquerade as a complete
  capture. Redaction of stdout, stderr, argv, or provenance forces
  `capture.can_prove_absence: false`, so redacted evidence can never authorize
  a `resolved` delta. The completeness preflight now counts virtual lines for
  any exact `text` field, aligning the bound with the traversal that derives
  findings.
- Scoped call caches to source semantics (#229): the cache identity hashes the
  configured adapter, the selected operation, the resolved auth principal,
  redaction and cache policy, declared output schema, pagination, source-state,
  and args. Rotating a credential or editing execution semantics produces a
  different key; credential values exist only in the transient hash input.
  Mutating operations are never served from cache, and redacted provenance or
  prefetched pages mark the observation redacted and non-provable.
- Separated provider and selection completeness (#230): `provider.complete`
  now means bounded normalization of the captured diagnostics, while
  `selection.exhaustive` remains the only authority for absence. A failing
  Cargo run is selection-exhaustive only under exact package+harness
  targeting or `--no-fail-fast`; pytest early-stop and failed name filters
  stay non-exhaustive and cannot authorize `resolved`.
- Enforced network trust boundaries (#231): `trust.allow_network` is required
  before any network-backed call or discovery probe, even with `--yes`.
  Effects are grounded in the configured adapter, so editable profile
  metadata cannot understate a network or shell effect to bypass trust or
  reuse cache under false semantics. HTTP redirects are limited to the
  source origin, pagination continuations are same-origin forced GETs, and
  transport errors are stripped of credential-bearing URLs before they can
  reach structured output.

- Added a release-published container image (#214): a multi-stage Dockerfile
  pinned to the Rust 1.89 MSRV builds an unprivileged, CA-carrying
  `ghcr.io/aphoristicartist/prog` image; CI smokes the image (`--help`,
  `route`, `observe`) on every Dockerfile change and publishes multi-arch
  (amd64 + arm64) SLSA-attested tags on release tags. Homebrew tap and
  crates.io publication remain deliberate owner decisions.

## 0.1.1 - 2026-08-16

- Added safe, idempotent PATH setup to the verified curl installer for zsh,
  bash, sh, dash, and ksh profiles. Existing PATH entries are preserved without
  startup-file edits, `PROG_MODIFY_PATH=0` opts out, and unknown shells fall
  back to a manual instruction without undoing a verified installation.

## 0.1.0 - 2026-08-15

- Added non-coding read-back verification for externally executed mutations:
  immutable action intents, identity/version fingerprints, safe fresh reads,
  deterministic receipts, eventual-consistency handling, and direct readiness
  integration. The verifier cannot execute mutations and fails closed on stale,
  redacted, truncated, expired, conflicting, or unavailable evidence (#133).
- Added the deterministic replay half of the actual-agent claim gate. It runs
  narrowed coding and expired-validator entity trajectories through the real
  CLI, resolves cited evidence and slice hashes, and proves deliberately false
  completion claims are rejected. Reports explicitly remain ineligible for a
  performance claim until credentialed multi-trial model runs exist (#139).

- Hardened the first-release contract: local stores reset audibly on the
  canonical observation shape, inert public fields and legacy compatibility
  paths were removed, and `prog meta` plus contract documentation now enumerate
  the complete current schema surface (#142, #200).
- Made disclosure budgets monotonic across capture kinds, retained findings and
  degradation warnings under pressure, compacted reusable action templates, and
  added an honest `disclosure_verdict` for cases where raw output costs fewer
  bytes (#212, #213, #219).
- Made shared local stores safe for concurrent processes with bounded lock
  retry, typed retryable contention errors, and lock release during external
  waits; added concurrent capture and navigation integration tests (#211).
- Replaced hardcoded integration targets with checked-in manifests, added an
  append-only `agents-md` target, external manifest directories, and zero-write
  YAML/MDC/plain skill export (#215).
- Pinned supported CI/release runners and target triples, verified crate license
  contents and exact package manifests, expanded release-candidate smoke tests,
  and documented the deferred MCP-server decision (#140, #216).
- Added a curl-first installer that verifies checksums and GitHub attestations,
  plus an explicit confirmation-gated `prog update` command for verified atomic
  self-updates. Release-candidate CI installs through the same script on every
  supported target (#214).

- Pinned MSRV at Rust 1.89 (`rust-version = "1.89"` in the workspace manifest), propagated to all crates, and added a dedicated CI job that builds and tests on `rust-toolchain@1.89.0` (#167).
- Extended macOS CI to match Ubuntu: `cargo fmt --all --check` and `cargo clippy --all-targets --all-features -- -D warnings` now run before tests on both platforms (#167).
- Added a tag-triggered release pipeline (`release.yml`): cross-platform tarballs with SHA256SUMS, CycloneDX SBOM, build-provenance attestations, a `cargo package` leak guard, and a release-candidate smoke test that reopens the local store across process exits (#167).
- Added a version-consistency CI guard (`check-version-consistency.sh`) asserting CHANGELOG, Cargo.toml version, and git tag alignment on tag builds (#167).
- Documented supported platforms (Ubuntu, macOS supported; Windows unsupported, linking #140), the schema/store-reset policy, and release notes (#167).
- Added the complete evidence-navigation loop: initial envelope findings, offline `inspect`, cached text/regex/type `search` and `find`, compact `evidence` blocks, declarative lens finding providers, ten domain lens packs, deterministic recipes, task-level session trails, structured-output autodetection, and a five-scenario evidence-acquisition regression suite (#87, #90-#99).
- Fixed false findings for null error fields and command argv, duplicate flattened `kind` keys in run next-actions, and unbounded generic finding traversal.
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

Changes after a versioned package is cut remain under `Unreleased` until the
next release entry is prepared.

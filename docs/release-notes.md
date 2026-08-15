# Release notes

This document is the per-release reference for `prog`. It is the file GitHub
Releases point at for the durable, platform-level facts: what runs, what does
not, what resets the local store, and what the shipped benchmarks cover. For
per-version changes, see [`../CHANGELOG.md`](../CHANGELOG.md).

## Supported platforms

`prog` is supported and CI-verified on these exact platform families:

- **Ubuntu 22.04 or newer on x86_64**
  (`x86_64-unknown-linux-gnu`). Release binaries are built on Ubuntu 22.04 so
  the supported glibc floor is not inherited from a moving `ubuntu-latest`.
- **macOS 15 or newer on Apple Silicon** (`aarch64-apple-darwin`).
- **macOS 15 or newer on Intel** (`x86_64-apple-darwin`).

Every supported target runs the full formatting + Clippy + test gate. An MSRV
(`rust-toolchain@1.89.0`) build/test also runs on Ubuntu. Release tarballs are
cut for all three targets on every `v*` tag (see
[`.github/workflows/release.yml`](../.github/workflows/release.yml)).
Each tarball also contains `VERSION` and `TARGET` files, and the release-candidate
smoke job verifies both before executing the binary.

**Windows is not supported.** Process-group, permissions, and signal semantics
that `prog` relies on are not implemented for Windows, and there is no timeline
for it. This is tracked under the release-readiness parent
([#140](https://github.com/aphoristicartist/prog/issues/140)).

**Minimum Supported Rust Version (MSRV)** is pinned at **1.89**. The workspace
declares `rust-version = "1.89"` in `Cargo.toml`, and the `msrv` CI job builds
and tests the workspace on `rust-toolchain@1.89.0`. A bump is a breaking change
and will be called out in the CHANGELOG.

## Known limitations

- **No Windows support.** See above.
- **No `cargo publish` flow yet.** Releases ship as GitHub Release tarballs with
  checksums, SBOM, and build provenance. Publishing to crates.io is deferred
  beyond v0.1.0 and remains an explicit owner decision, not an automated
  release step.
- **Verified installer and explicit self-update.** `install.sh` requires both a
  SHA-256 match and a GitHub build-provenance attestation before extraction.
  `prog update --yes` verifies the release installer and archive before atomic
  replacement. There is no hidden background update.
- **Exact crate manifests are pinned.** CI fails when any packaged file is
  added or removed until `.github/package-contents/` is reviewed and updated;
  runtime stores, local `.prog/` directories, and workspace fixtures remain
  hard-banned.
- **Pre-release store resets.** The local observation store is wiped, not
  migrated, whenever an immutable-record invariant changes (see the
  schema/store-reset policy below). This is intentional during `0.x`.
- **Single-machine stores.** The `redb`-backed store is local to a `--dir`;
  there is no remote or networked store. Local processes may share one `--dir`:
  external waits do not retain the database lock, brief contention is retried,
  and exhausted contention is a typed retryable error.

## Schema / store-reset policy

The local observation store carries a **schema identity** — not a compatibility
version — at:

- `crates/prog-core/src/store.rs:39`
- `const STORE_SCHEMA: &str = "prog.store.readback_verification_contract";`

On open, if the persisted `store_schema` key does not equal `STORE_SCHEMA`,
`prog` **resets the local store** rather than migrating it. This is a deliberate
choice: during `0.x`, an invariant change (e.g. changing what an observation
record commits to) is treated as a hard break, and stale local data is discarded
so a mixed-shape store can never be read by a newer binary.

Practical implications:

- Upgrading `prog` across a `STORE_SCHEMA` change resets `--dir` and emits an
  explicit stderr notice naming the directory and number of records dropped.
  There is no data loss in the upstream sources (observations can be
  re-captured), but cached cursors and local lineage are gone.
- The schema identity string is stable within a release; it changes only when an
  immutable record invariant changes, and any such change is noted in the
  CHANGELOG.

## Benchmark scope

Shipped deterministic evaluations live under `fixtures/evals/` and are run as
ordinary Cargo integration tests (`cargo test --workspace --all-features`).
They cover:

- **Task-success evals** comparing raw, simple truncation, call-only, and
  targeted-expansion strategies (see
  [`task-success-eval.md`](task-success-eval.md)).
- **Competitive baselines** against raw context, truncation, native field
  selection, RTK-style grep filtering, Caveman-style terse output, and repeated
  cache-backed expansion (see
  [`competitive-baselines.md`](competitive-baselines.md)).
- **Token-economics fixtures** measuring envelope cost versus full-payload
  retrieval (see [`token-economics.md`](token-economics.md)).

These benchmarks are deterministic (no live network) and run in CI on every
push and pull request. They are correctness/determinism gates, not live
performance regressions; wall-clock performance is not asserted.

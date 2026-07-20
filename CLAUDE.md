# CLAUDE.md

Instructions for Claude Code when working on the `prog` codebase.

**Read [`AGENTS.md`](AGENTS.md) first.** It is the canonical orientation
document — architecture, invariants, build commands, and conventions. This file
holds only the Claude-specific additions. Keeping the substance in one file is
deliberate; do not duplicate it here, or the two will drift.

## Before you change anything

1. Read [`AGENTS.md`](AGENTS.md) for the layering rules and the
   conservative-answer rule.
2. Read [`INVARIANTS.md`](INVARIANTS.md) if you are touching `prog-core`.
   Thirteen invariants map to executable tests; do not weaken a test to make a
   change pass.

## Things that surprise agents in this repo

- **`crates/prog-cli/tests/docs_examples.rs` tests the README.** It re-runs the
  quickstart and asserts exact literal strings are present. Editing `README.md`
  can turn the suite red. Check that test before and after any README edit.
- **Measured numbers are generated, not written.** Regenerate with
  `PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture`.
  Never hand-edit a figure in `docs/token-economics.md`,
  `docs/evidence-acquisition.md`, or `fixtures/evals/*.json`.
- **Redaction is enforced by typestate**, not by remembering to call it. If the
  compiler is fighting you about `RawPayload` vs `RedactedPayload`, it is
  working as intended. Do not add a bypass.
- **`.claude/worktrees/` contains full checkouts of the repo.** Searches will
  return duplicate hits from there. Scope greps to `crates/`, `docs/`, and
  `lenses/`, or exclude `.claude/`.
- **The suite is slow** (one integration test runs ~97s; a 360-scenario
  transport matrix is included). Run the full workspace suite before declaring
  work complete, and allow several minutes.

## Working style for this repo

- **Verify before asserting.** This project's whole premise is not claiming more
  than the evidence supports; hold your own reports to the same bar. If a test
  fails, say so with the output. If you skipped a step, say that.
- **Deliver analysis before implementation.** For non-trivial work, present the
  plan and wait for an explicit go-ahead before writing project code.
- **Prefer the conservative change.** A patch that makes `prog` report success
  more readily is a regression, even when the tests stay green.

## Standard gate

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

MSRV is Rust 1.89 and is CI-enforced. Windows is unsupported by design.

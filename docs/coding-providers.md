# Coding-output providers

`prog run` applies a small, internal normalization layer to recognized pytest
and Cargo/rustc invocations. The provider is pure over captured argv, stdout,
and stderr: it never executes another command, and the original redacted
capture remains stored and cursor-addressable if matching or parsing fails.

The normalized value is stored at `/provider`. Its observation record exposes
the provider and parser identities plus the exact selection and completeness
assessment:

```bash
prog run -- pytest -q
prog expand pc1_... --path /provider
prog cache observations --limit 1
```

## Supported inputs

| Provider | Recognized invocations | Preferred input | Text fallback |
|---|---|---|---|
| `pytest.v1` | `pytest`, `py.test`, `python -m pytest` | Captured standard pytest result output | Node IDs, statuses, terminal summary, targets, and early-stop markers |
| `cargo_rustc.v1` | `rustc`; Cargo `bench`, `build`, `check`, `clippy`, `rustc`, and `test` | Cargo/rustc JSON diagnostics | rustc diagnostics and stable libtest lines |

`prog recipe cargo-test` adds `--message-format=json` to a Cargo test argv when
the option is absent. The expanded argv is recorded in the recipe envelope.
User-supplied argv is never converted into a shell string.

## Completeness

Provider completeness is narrower than process completion. A complete capture
can still be marked incomplete when, for example, pytest stopped early, a
capture was truncated, Cargo JSON was malformed, or a Cargo test invocation
does not identify one exact harness. Incomplete provider output changes the
observation's capture stop reason to `derivation_windowed`; it cannot prove a
finding resolved.

Targets become explicit selection scopes. A targeted pytest run is exhaustive
only for that target, never for a broader suite. Cargo target flags likewise
become namespaced scopes. Delta comparison still requires the normal
invocation, scope, provider/parser, capture, workspace, and source-state proof.

## Bounds and fallback

Provider work is capped at 1 MiB of input, 10,000 lines, 512 normalized items,
1 MiB of normalized output, 32 spans per structured diagnostic, and 2,048
characters per retained string. Hitting any bound makes the result incomplete.

Malformed and unsupported output remains available through `/stdout/text`,
`/stderr/text`, and `/failure_sections`. Generic deterministic findings remain
the fallback; provider failure never removes captured evidence or causes a
second execution.

The checked-in golden matrix lives under
`crates/prog-core/tests/fixtures/providers/` and includes success, failure,
negative, malformed, truncated, reordered, Unicode, line-shift,
tool-output-variant, and no-match cases. Property tests pin deterministic
bounded behavior.

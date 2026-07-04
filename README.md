# prog

`prog` is a Rust progressive-disclosure gateway for noisy HTTP APIs, local CLIs, and MCP servers.

The implementation is intentionally staged from the RFCs in `docs/rfcs/`. The first milestone is the workspace and CLI shell: every command exists, machine-readable errors are JSON on stdout, and future issues can replace placeholders without changing the public command tree.

## Workspace

The repository uses three crates:

- `prog-core`: shared contracts, errors, disclosure logic, cache, redaction, and safety policy.
- `prog-adapters`: HTTP, CLI, and MCP upstream adapters behind one boundary.
- `prog-cli`: the `prog` binary.

## Development Checks

Run these before opening implementation PRs:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The CLI help should expose the full shell:

```bash
cargo run -- --help
```

## RFCs

- [RFC 0001: Prog Progressive Disclosure Gateway](docs/rfcs/0001-progressive-disclosure-gateway.md)
- [RFC 0002: Type Theory, Formal Methods, and the Reflexive Meta-Tower](docs/rfcs/0002-type-theory-formal-methods-and-reflexivity.md)

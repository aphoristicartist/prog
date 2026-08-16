# Install and update

The first-party install channel is a verified GitHub Release binary. It does
not require Rust, root access, a package manager, or a writable system prefix.
It requires `curl`, `tar`, a SHA-256 implementation, and GitHub CLI (`gh`).

## Latest release

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aphoristicartist/prog/releases/latest/download/install.sh | sh
```

The default destination is `~/.local/bin/prog`. Set an explicit user-writable
destination when needed:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aphoristicartist/prog/releases/latest/download/install.sh \
  | PROG_INSTALL_DIR="$HOME/bin" sh
```

If the chosen directory is not already on `PATH`, the installer appends one
idempotent entry to the detected startup file:

| Login shell | Startup file |
|---|---|
| zsh | `~/.zshrc` |
| bash on macOS | `~/.bash_profile` |
| bash on Linux | `~/.bashrc` |
| sh, dash, or ksh | `~/.profile` |

Open a new terminal after installation; a script executed through `curl | sh`
cannot modify the environment of the parent shell that launched it. If the
install directory is already present in the current `PATH`, no startup file is
changed. To opt out explicitly:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aphoristicartist/prog/releases/latest/download/install.sh \
  | PROG_MODIFY_PATH=0 sh
```

`PROG_MODIFY_PATH` accepts only `0` or `1`. For an unknown login shell, an
unavailable profile writer, or an unsafe multiline install path, installation
still succeeds but the profile is left untouched and a manual instruction is
printed.

## Exact release

Use the installer attached to that release and bind the expected version:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aphoristicartist/prog/releases/download/v0.1.1/install.sh \
  | PROG_VERSION=v0.1.1 sh
```

The installation fails unless the archive's embedded `VERSION` and `TARGET`
match the requested release and detected platform.

## Verification contract

Before extraction, `install.sh`:

1. accepts only HTTPS release URLs (a file URL is available only behind an
   explicit test-only opt-in);
2. selects one of the three supported release target triples;
3. verifies the archive against the release's `SHA256SUMS`;
4. runs `gh attestation verify` against the `aphoristicartist` owner; and
5. validates the archive's embedded `VERSION` and `TARGET` provenance.

Any missing tool, malformed manifest, checksum mismatch, failed attestation,
unsupported platform, or provenance mismatch stops before the destination
binary is replaced. A successful install writes a `.prog-install` marker beside
the binary so the self-updater can distinguish a curl-managed binary from a
Homebrew, Cargo, or system-package installation.

The curl pipeline necessarily trusts the installer bytes delivered by GitHub
over TLS. Release automation separately attests `install.sh`; an already
installed `prog` verifies that installer attestation before using it during
self-update.

## Explicit automatic update

```sh
prog update --yes
```

This resolves the latest release and automatically downloads the correct
platform archive. It verifies the downloaded installer attestation, then the
installer verifies the archive checksum and build attestation before an atomic
replacement. To install a specific version:

```sh
prog update --yes --target-version v0.1.1
```

There is deliberately no hidden background update. A self-update is a networked
filesystem mutation, so the same fail-closed convention as other mutating
operations applies: `--yes` is mandatory. When the adjacent managed-install
marker is absent, pass `--install-dir` only if replacing that destination is
intentional.

## Manual download

Every release contains three platform tarballs, `SHA256SUMS`, `install.sh`,
CycloneDX SBOMs, and GitHub build-provenance attestations. After downloading the
assets into one directory:

```sh
shasum -a 256 -c SHA256SUMS
for archive in prog-*.tar.gz; do
  gh attestation verify "$archive" --owner aphoristicartist
done
```

Linux users may use `sha256sum -c SHA256SUMS --ignore-missing` instead.

## Source build

Developers with Rust 1.89 or newer can install from a checkout:

```sh
cargo install --path crates/prog-cli
```

There is no crates.io publication in v0.1.1. The package name is `prog-cli`
while the installed binary name is `prog`; publishing that package remains an
explicit future owner decision rather than an automated release side effect.

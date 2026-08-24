#!/usr/bin/env bash
# Verify that every crate can be packaged and that its exact file manifest is
# intentional. Run from any directory; generated archives stay under target/.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
manifest_dir="$repo_root/.github/package-contents"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

cd "$repo_root"
# Compile each generated tarball as well as checking its contents. This catches
# workspace-relative include paths that work from a checkout but not a package.
cargo package --workspace --locked --allow-dirty

version="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"
[[ -n "$version" ]] || { echo "could not read workspace version" >&2; exit 1; }

for crate in prog-core prog-adapters prog-cli; do
  expected="$manifest_dir/$crate.txt"
  actual="$tmp_dir/$crate.txt"
  archive="target/package/$crate-$version.crate"

  [[ -f "$expected" ]] || { echo "missing package manifest: $expected" >&2; exit 1; }
  [[ -f "$archive" ]] || { echo "missing package archive: $archive" >&2; exit 1; }

  # The workspace package pass above has already resolved the locked graph.
  # Keep the per-crate manifest reads deterministic and network-free.
  cargo package -p "$crate" --locked --allow-dirty --offline --list > "$actual"
  if ! diff -u "$expected" "$actual"; then
    echo "package contents changed for $crate; review and update $expected intentionally" >&2
    exit 1
  fi

  if [[ "$(tar -tzf "$archive" | grep -Ec '/LICENSE$')" -ne 1 ]]; then
    echo "expected exactly one LICENSE in $archive" >&2
    exit 1
  fi
  if tar -tzf "$archive" | grep -E '\.redb$|\.prog/'; then
    echo "runtime artifact leaked into $archive" >&2
    exit 1
  fi
  if tar -tzf "$archive" | grep -E 'fixtures/' | grep -vE 'tests/fixtures/'; then
    echo "workspace fixtures leaked into $archive" >&2
    exit 1
  fi

  echo "$crate package contents: OK"
done

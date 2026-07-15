#!/usr/bin/env bash
# check-version-consistency.sh
#
# Guards against drift between the Cargo.toml workspace version, the CHANGELOG,
# and (on tag builds) the git tag. Exits non-zero on drift.
#
# Contract:
#   * Always: the CHANGELOG must contain a heading for the current workspace
#     version OR a `## Unreleased` heading (the latter is always permitted so
#     non-tag CI runs stay green while a release is being staged).
#   * On a tag build (GITHUB_REF = refs/tags/v<X>): the tag must equal
#     v<workspace-version> AND the CHANGELOG must contain a `## <version>`
#     heading for that version (no `## Unreleased`-only escape hatch).
#
# Usage: run from the repository root. No arguments. Reads GITHUB_REF from the
# environment; when unset (local invocation), the tag path is skipped.

set -euo pipefail

err() {
  echo "check-version-consistency: $*" >&2
  exit 1
}

# Locate the workspace Cargo.toml from the repo root (script lives under
# .github/scripts/). Resolve the repo root from this file's location so the
# script works regardless of the caller's working directory.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
cargo_toml="$repo_root/Cargo.toml"
changelog="$repo_root/CHANGELOG.md"

[[ -f "$cargo_toml" ]] || err "workspace Cargo.toml not found at $cargo_toml"
[[ -f "$changelog" ]] || err "CHANGELOG.md not found at $changelog"

# Extract workspace.package.version. awk: find the [workspace.package] table,
# then capture the version line within it before any other table starts.
version="$(
  awk '
    /^\[workspace\.package\]/ { in_table = 1; next }
    /^\[/ { in_table = 0 }
    in_table && /^version[[:space:]]*=/ {
      sub(/^version[[:space:]]*=[[:space:]]*"?/, "")
      sub(/"[[:space:]]*$/, "")
      gsub(/"/, "")
      print
      exit
    }
  ' "$cargo_toml"
)"

[[ -n "$version" ]] || err "could not read workspace.package.version from $cargo_toml"
echo "workspace version: $version"

# Determine whether this is a tag run and, if so, the tag value.
github_ref="${GITHUB_REF:-}"
tag=""
if [[ "$github_ref" == refs/tags/v* ]]; then
  tag="${github_ref#refs/tags/}"
fi

if [[ -n "$tag" ]]; then
  # Tag run: tag must equal v<version>, and a matching CHANGELOG heading is
  # required (## Unreleased alone is NOT sufficient on a tag).
  expected_tag="v$version"
  [[ "$tag" == "$expected_tag" ]] \
    || err "tag '$tag' does not match workspace version '$expected_tag'"
  echo "tag: $tag (matches workspace version)"

  if ! grep -qE "^## ${version}\b" "$changelog"; then
    err "CHANGELOG.md has no '## ${version}' heading for tag $tag"
  fi
  echo "CHANGELOG: found '## ${version}' heading"
else
  # Non-tag run: accept either an explicit heading for the current version or a
  # `## Unreleased` heading (the latter is the normal state between releases).
  if grep -qE "^## ${version}\b" "$changelog"; then
    echo "CHANGELOG: found '## ${version}' heading"
  elif grep -qE "^## Unreleased\b" "$changelog"; then
    echo "CHANGELOG: found '## Unreleased' heading"
  else
    err "CHANGELOG.md has neither a '## ${version}' nor a '## Unreleased' heading"
  fi
fi

echo "version consistency: OK"

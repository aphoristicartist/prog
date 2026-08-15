#!/bin/sh
# Install the latest verified prog release without requiring a Rust toolchain.
#
# The script deliberately requires GitHub CLI attestation verification in
# addition to a SHA-256 match. A downloaded binary is never extracted or
# installed when either proof fails.

set -eu

repo="aphoristicartist/prog"
owner="aphoristicartist"
install_dir="${PROG_INSTALL_DIR:-${HOME}/.local/bin}"
requested_version="${PROG_VERSION:-}"
requested_target="${PROG_TARGET:-}"
release_url="${PROG_RELEASE_URL:-}"
allow_file_url="${PROG_ALLOW_FILE_URL:-0}"

fail() {
  printf 'prog installer: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os:$arch" in
    Linux:x86_64|Linux:amd64)
      printf '%s\n' 'x86_64-unknown-linux-gnu'
      ;;
    Darwin:arm64|Darwin:aarch64)
      printf '%s\n' 'aarch64-apple-darwin'
      ;;
    Darwin:x86_64|Darwin:amd64)
      printf '%s\n' 'x86_64-apple-darwin'
      ;;
    *)
      fail "unsupported platform: $os $arch (supported: Linux x86_64, macOS arm64/x86_64)"
      ;;
  esac
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    fail 'required SHA-256 tool not found: install sha256sum or shasum'
  fi
}

download() {
  url="$1"
  output="$2"
  case "$url" in
    https://*) allowed_protocol='=https' ;;
    file://*)
      [ "$allow_file_url" = "1" ] || fail 'file release URLs require PROG_ALLOW_FILE_URL=1'
      allowed_protocol='=file'
      ;;
    *)
      fail "refusing non-HTTPS release URL: $url"
      ;;
  esac
  curl --proto "$allowed_protocol" --proto-redir "$allowed_protocol" --tlsv1.2 -fsSL "$url" -o "$output"
}

validate_version() {
  case "$1" in
    v*) version_body="${1#v}" ;;
    *) fail "invalid PROG_VERSION: $1" ;;
  esac

  version_core="${version_body%%-*}"
  version_rest="${version_core#*.}"
  version_major="${version_core%%.*}"
  [ "$version_rest" != "$version_core" ] || fail "invalid PROG_VERSION: $1"
  version_minor="${version_rest%%.*}"
  version_patch="${version_rest#*.}"
  [ "$version_patch" != "$version_rest" ] || fail "invalid PROG_VERSION: $1"

  case "$version_major" in ''|*[!0-9]*) fail "invalid PROG_VERSION: $1" ;; esac
  case "$version_minor" in ''|*[!0-9]*) fail "invalid PROG_VERSION: $1" ;; esac
  case "$version_patch" in ''|*[!0-9]*|*.*) fail "invalid PROG_VERSION: $1" ;; esac
  if [ "$version_body" != "$version_core" ]; then
    version_prerelease="${version_body#"$version_core"-}"
    case "$version_prerelease" in
      ''|.*|*.|*..*|*[!0-9A-Za-z.-]*) fail "invalid PROG_VERSION: $1" ;;
    esac
  fi
}

need curl
need gh
need tar
need awk
need uname
need mktemp

target="${requested_target:-$(detect_target)}"
case "$target" in
  x86_64-unknown-linux-gnu|aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) fail "unsupported release target: $target" ;;
esac

if [ -z "$release_url" ]; then
  if [ -n "$requested_version" ]; then
    validate_version "$requested_version"
    release_url="https://github.com/${repo}/releases/download/${requested_version}"
  else
    release_url="https://github.com/${repo}/releases/latest/download"
  fi
fi

archive="prog-${target}.tar.gz"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/prog-install.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

download "${release_url}/SHA256SUMS" "$tmp_dir/SHA256SUMS"
download "${release_url}/${archive}" "$tmp_dir/${archive}"

expected_hash="$(awk -v name="$archive" '$2 == name || $2 == "*" name { print $1 }' "$tmp_dir/SHA256SUMS")"
[ -n "$expected_hash" ] || fail "SHA256SUMS has no entry for $archive"
[ "$(printf '%s\n' "$expected_hash" | wc -l | tr -d ' ')" = "1" ] \
  || fail "SHA256SUMS has multiple entries for $archive"
actual_hash="$(sha256_file "$tmp_dir/${archive}")"
[ "$actual_hash" = "$expected_hash" ] \
  || fail "checksum verification failed for $archive"

if ! gh attestation verify "$tmp_dir/${archive}" --owner "$owner" >/dev/null; then
  fail "GitHub build-provenance verification failed for $archive"
fi

tar -xzf "$tmp_dir/${archive}" -C "$tmp_dir"
staging="$tmp_dir/prog-${target}"
[ -x "$staging/prog" ] || fail "archive does not contain an executable prog binary"
[ -f "$staging/VERSION" ] || fail 'archive does not contain VERSION provenance'
[ -f "$staging/TARGET" ] || fail 'archive does not contain TARGET provenance'

installed_version="$(tr -d '\r\n' < "$staging/VERSION")"
installed_target="$(tr -d '\r\n' < "$staging/TARGET")"
[ "$installed_target" = "$target" ] \
  || fail "archive target mismatch: expected $target, got $installed_target"
if [ -n "$requested_version" ]; then
  [ "v${installed_version}" = "$requested_version" ] \
    || fail "archive version mismatch: expected $requested_version, got v${installed_version}"
fi

mkdir -p "$install_dir"
install_tmp="$install_dir/.prog.install.$$"
marker_tmp="$install_dir/.prog-install.tmp.$$"
trap 'rm -rf "$tmp_dir"; rm -f "$install_tmp" "$marker_tmp"' EXIT HUP INT TERM
cp "$staging/prog" "$install_tmp"
chmod 0755 "$install_tmp"
printf 'repository=%s\nversion=%s\ntarget=%s\n' "$repo" "$installed_version" "$installed_target" > "$marker_tmp"
chmod 0644 "$marker_tmp"
mv -f "$install_tmp" "$install_dir/prog"
mv -f "$marker_tmp" "$install_dir/.prog-install"

printf 'prog %s installed to %s/prog (%s; checksum and GitHub attestation verified)\n' \
  "$installed_version" "$install_dir" "$installed_target" >&2

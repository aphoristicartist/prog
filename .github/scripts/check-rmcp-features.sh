#!/usr/bin/env bash
# check-rmcp-features.sh
#
# deny.toml ignores RUSTSEC-2026-0457 (rmcp OAuth client) and RUSTSEC-2026-0462
# (rmcp Streamable HTTP clients) because prog compiles neither code path. This
# guard keeps that true: it fails when the resolved rmcp version changes (the
# ignores must then be revisited) or when any feature that pulls in the
# vulnerable code is enabled, for any workspace crate with all features.
#
# Usage: run from the repository root. No arguments.

set -euo pipefail

pinned="2.1.0"
tree="$(cargo tree --workspace --all-features --target all -e features -i rmcp 2>&1)" || {
    printf 'check-rmcp-features: cargo tree failed:\n%s\n' "$tree" >&2
    exit 1
}

version="$(printf '%s\n' "$tree" | sed -n 's/^rmcp v\([0-9][^ ]*\).*/\1/p' | head -n 1)"
if [ "$version" != "$pinned" ]; then
    printf 'check-rmcp-features: rmcp resolved to %s, expected %s; revisit the deny.toml ignores for RUSTSEC-2026-0457 and RUSTSEC-2026-0462\n' "${version:-<none>}" "$pinned" >&2
    exit 1
fi

forbidden="$(printf '%s\n' "$tree" | grep -oE 'rmcp feature "(auth|auth-client-credentials-jwt|transport-streamable-http-client[^"]*|reqwest[^"]*|__reqwest)"' | sort -u || true)"
if [ -n "$forbidden" ]; then
    printf 'check-rmcp-features: features covered by ignored advisories are enabled:\n%s\n' "$forbidden" >&2
    exit 1
fi

printf 'check-rmcp-features: rmcp %s without auth or Streamable HTTP client features\n' "$version"

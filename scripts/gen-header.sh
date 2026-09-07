#!/usr/bin/env bash
# Regenerates the C header the Swift package compiles against from crates/rc-git-ffi.
# Pass --verify to fail when the committed header is stale; CI and the pre-commit hook do that.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HEADER="$ROOT/mac/Sources/CRcGit/include/rc_git.h"
CONFIG="$ROOT/crates/rc-git-ffi/cbindgen.toml"

command -v cbindgen >/dev/null || {
  echo "error: cbindgen not found; run: brew install cbindgen" >&2
  exit 1
}

mkdir -p "$(dirname "$HEADER")"
cd "$ROOT"
if [[ "${1:-}" == "--verify" ]]; then
  cbindgen --config "$CONFIG" --crate rc-git-ffi --output "$HEADER" --verify --quiet
  echo "header is up to date: ${HEADER#"$ROOT"/}"
else
  cbindgen --config "$CONFIG" --crate rc-git-ffi --output "$HEADER" --quiet
  echo "wrote ${HEADER#"$ROOT"/}"
fi

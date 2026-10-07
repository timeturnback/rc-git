#!/usr/bin/env bash
# Regenerates the C header the Swift package compiles against from crates/rc-git-ffi.
# Pass --verify to fail when the committed header is stale; CI and the pre-commit hook do that.
# Verify never writes. When the output differs and the installed cbindgen is not
# CBINDGEN_VERSION, neither mode touches the header: the difference may be the release's.
set -euo pipefail

CBINDGEN_VERSION=0.29.4
INSTALL_HINT="cargo install cbindgen --version $CBINDGEN_VERSION --locked"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HEADER="$ROOT/mac/Sources/CRcGit/include/rc_git.h"
CONFIG="$ROOT/crates/rc-git-ffi/cbindgen.toml"
RELATIVE="${HEADER#"$ROOT"/}"

verify=0
case "$#:${1:-}" in
0:) ;;
1:--verify) verify=1 ;;
*)
  echo "usage: gen-header.sh [--verify]" >&2
  exit 2
  ;;
esac

command -v cbindgen >/dev/null || {
  echo "error: cbindgen not found; install it with: $INSTALL_HINT" >&2
  exit 1
}

generated="$(mktemp)"
trap 'rm -f "$generated"' EXIT

cd "$ROOT"
status=0
cbindgen --config "$CONFIG" --crate rc-git-ffi --quiet >"$generated" || status=$?

if [[ $status -eq 0 ]] && cmp -s "$generated" "$HEADER"; then
  echo "header is up to date: $RELATIVE"
  exit 0
fi

found="$(cbindgen --version)"
found="${found##* }"
if [[ "$found" != "$CBINDGEN_VERSION" ]]; then
  echo "error: cbindgen $found found, header is generated with $CBINDGEN_VERSION;" \
    "install it with: $INSTALL_HINT" >&2
  exit 1
fi

if [[ $status -ne 0 ]]; then
  echo "error: cbindgen failed (exit $status) generating $RELATIVE" >&2
  exit "$status"
fi

if [[ $verify -eq 1 ]]; then
  # diff exits non-zero exactly when it shows the staleness reported below.
  diff -u "$RELATIVE" "$generated" >&2 || true
  echo "error: $RELATIVE is stale; run scripts/gen-header.sh and commit it" >&2
  exit 1
fi

mkdir -p "$(dirname "$HEADER")"
cp "$generated" "$HEADER"
echo "wrote $RELATIVE"

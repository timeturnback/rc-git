#!/usr/bin/env bash
# Builds the Rust static library, regenerates the C header, and builds the Swift package.
# Run it once before `swift build` or `swift test` in mac/: both link ../target/release.
#
#   scripts/build.sh [release|debug]   Swift build configuration, release by default; the Rust
#                                      library is always built release
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
usage() { echo "usage: build.sh [release|debug]"; }
(($# <= 1)) || {
  usage >&2
  exit 2
}
CONFIG="${1:-release}"
case "$CONFIG" in
release | debug) ;;
-h | --help)
  usage
  exit 0
  ;;
*)
  usage >&2
  exit 2
  ;;
esac
cd "$ROOT"

# Package.swift links ../target/release, so pin cargo there over any CARGO_TARGET_DIR or config.
CARGO_TARGET_DIR="$ROOT/target" cargo build --release -p rc-git-ffi
scripts/gen-header.sh
swift build --package-path mac -c "$CONFIG"
echo "built $(swift build --package-path mac -c "$CONFIG" --show-bin-path)/RcGitApp"

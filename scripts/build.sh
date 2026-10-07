#!/usr/bin/env bash
# Builds the Rust static library, regenerates the C header, and builds the Swift package.
# Run it once before `swift build` or `swift test` in mac/: both link ../target/app.
#
#   scripts/build.sh [release|debug]   build configuration, release by default; debug builds the
#                                      Rust library with the faster app-dev profile
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

scripts/rust-lib.sh "$CONFIG"
scripts/gen-header.sh
swift build --package-path mac -c "$CONFIG"
echo "built $(swift build --package-path mac -c "$CONFIG" --show-bin-path)/RcGitApp"

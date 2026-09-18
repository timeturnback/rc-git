#!/usr/bin/env bash
# Builds the Rust static library, regenerates the C header, and builds the Swift package.
# Run it once before `swift build` or `swift test` in mac/: both link ../target/release.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG="${1:-release}"
cd "$ROOT"

# Match the Swift package's platform floor so ring's assembly objects are not built
# for the host SDK version and warn at link time.
MACOSX_DEPLOYMENT_TARGET=15.0 cargo build --release -p rc-git-ffi
scripts/gen-header.sh
swift build --package-path mac -c "$CONFIG"
echo "built $(swift build --package-path mac -c "$CONFIG" --show-bin-path)/RcGitApp"

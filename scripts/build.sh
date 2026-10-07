#!/usr/bin/env bash
# Builds the Rust static library, regenerates the C header, and builds the Swift package.
# Run it once before `swift build` or `swift test` in mac/: both link ../target/release.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG="${1:-release}"
cd "$ROOT"

cargo build --release -p rc-git-ffi
scripts/gen-header.sh
swift build --package-path mac -c "$CONFIG"
echo "built $(swift build --package-path mac -c "$CONFIG" --show-bin-path)/RcGitApp"

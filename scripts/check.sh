#!/usr/bin/env bash
# The full gate, identical to CI. Pass --fast to skip the Swift build and tests.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

fast=0
[[ "${1:-}" == "--fast" ]] && fast=1

step() { printf '\n==> %s\n' "$*"; }
require() {
  command -v "$1" >/dev/null || {
    echo "error: $1 not found; install with: $2" >&2
    exit 1
  }
}

require cargo "rustup (https://rustup.rs)"
require cargo-deny "brew install cargo-deny"
require cbindgen "brew install cbindgen"
require shellcheck "brew install shellcheck"
require shfmt "brew install shfmt"
require actionlint "brew install actionlint"
require zizmor "brew install zizmor"

step "cargo fmt"
cargo fmt --all -- --check
step "cargo clippy"
cargo clippy --workspace --all-targets --all-features -- -D warnings
step "cargo test"
cargo test --workspace
step "cargo doc"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --quiet
step "cargo deny"
cargo deny check
step "ffi header"
scripts/gen-header.sh --verify
step "shell scripts"
shellcheck scripts/*.sh
shfmt -d scripts/*.sh
step "github actions"
actionlint
zizmor .github/workflows

if ((fast == 0)); then
  step "rust release library"
  # Same platform floor as scripts/build.sh: without it ring's assembly objects are built
  # for the host SDK and the Swift link warns on every one of them.
  MACOSX_DEPLOYMENT_TARGET=15.0 cargo build --release -p rc-git-ffi
  step "swift-format"
  xcrun swift-format lint --strict --recursive \
    mac/Sources mac/Tests mac/Package.swift scripts/make-icon.swift
  step "swift build"
  swift build --package-path mac
  step "swift test"
  swift test --package-path mac
fi

printf '\nall checks passed\n'

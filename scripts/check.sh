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
require cbindgen "cargo install cbindgen --version 0.29.4 --locked"
require shellcheck "brew install shellcheck"
require shfmt "brew install shfmt"
require actionlint "brew install actionlint"
require zizmor "brew install zizmor"

step "cargo fmt"
cargo fmt --all -- --check
step "cargo clippy"
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
step "cargo test"
cargo test --locked --workspace
step "cargo doc"
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --quiet
step "cargo deny"
cargo deny --locked check
step "ffi header"
scripts/gen-header.sh --verify
step "shell scripts"
shellcheck scripts/*.sh
shfmt -d scripts/*.sh
step "github actions"
actionlint
zizmor .github/workflows
step "kit layering"
if grep -rnE '\bimport[[:space:]]+([A-Za-z_]+[[:space:]]+)?(AppKit|Cocoa|SwiftUI)\b' \
  mac/Sources/RcGitKit; then
  echo 'error: RcGitKit must not import a UI framework (see .claude/rules/swift.md)' >&2
  exit 1
fi

if ((fast == 0)); then
  step "macOS platform floor"
  cargo_floor="$(sed -n 's/^MACOSX_DEPLOYMENT_TARGET = { value = "\([0-9.]*\)".*/\1/p' \
    .cargo/config.toml)"
  package_floor="$(sed -n 's/.*\.macOS("\([0-9.]*\)").*/\1/p' mac/Package.swift)"
  plist_floor="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' mac/Info.plist)"
  if [[ -z "$cargo_floor" || "$cargo_floor" != "$package_floor" ||
    "$cargo_floor" != "$plist_floor" ]]; then
    echo "error: macOS floor differs: .cargo/config.toml '$cargo_floor'," \
      "mac/Package.swift '$package_floor', mac/Info.plist '$plist_floor'; set all three alike" >&2
    exit 1
  fi
  step "rust release library"
  cargo build --locked --release -p rc-git-ffi
  step "swift-format"
  xcrun swift-format lint --strict --recursive \
    mac/Sources mac/Tests mac/Package.swift scripts/make-icon.swift
  step "swift build"
  swift build --package-path mac
  step "swift test"
  swift test --package-path mac
fi

printf '\nall checks passed\n'

#!/usr/bin/env bash
# The checks CI runs, minus assembling the app bundle, which only CI does.
# Pass --fast to skip the Swift build and tests.
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
step "ffi header"
scripts/gen-header.sh --verify
step "shell scripts"
shellcheck scripts/*.sh
shfmt -d scripts/*.sh
step "github actions"
actionlint
zizmor --offline .github/workflows
step "kit layering"
if grep -rnE '\bimport[[:space:]]+([A-Za-z_]+[[:space:]]+)?(AppKit|Cocoa|SwiftUI)\b' \
  mac/Sources/RcGitKit; then
  echo 'error: RcGitKit must not import a UI framework (see .claude/rules/swift.md)' >&2
  exit 1
fi
step "swift line length"
# swift-format's lineLength skips unbreakable string literals; C.UTF-8 counts characters, not bytes.
if LC_ALL=C.UTF-8 grep -rnE --include='*.swift' '^.{101,}$' \
  mac/Sources mac/Tests mac/Package.swift scripts; then
  echo 'error: Swift lines over 100 columns; split long string literals with +' >&2
  exit 1
fi
step "cargo clippy"
# clippy reports a clippy.toml path that resolves to nothing as a config warning -D misses.
clippy_out="$(cargo clippy --locked --workspace --all-targets --all-features \
  -- -D warnings 2>&1)" || {
  printf '%s\n' "$clippy_out"
  exit 1
}
printf '%s\n' "$clippy_out"
if grep -q 'does not refer to a reachable' <<<"$clippy_out"; then
  echo 'error: clippy.toml names a path that resolves to nothing;' \
    'fix it or mark it allow-invalid with a reason' >&2
  exit 1
fi
step "cargo test"
cargo test --locked --workspace
step "cargo doc"
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --quiet
step "cargo deny"
cargo deny --locked check

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
  step "swift-format"
  xcrun swift-format lint --strict --recursive \
    mac/Sources mac/Tests mac/Package.swift scripts/make-icon.swift
  step "rust release library"
  cargo build --locked --release -p rc-git-ffi
  # Same flags on both, so swift test reuses the build; kept out of Package.swift and build.sh.
  step "swift build"
  swift build --package-path mac -Xswiftc -warnings-as-errors -Xlinker -fatal_warnings
  step "swift test"
  swift test --package-path mac -Xswiftc -warnings-as-errors -Xlinker -fatal_warnings
fi

printf '\nall checks passed\n'

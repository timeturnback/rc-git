#!/usr/bin/env bash
# The checks CI runs, minus assembling the app bundle, which only CI does.
# Pass --fast to skip the Swift build and tests.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() { echo "usage: check.sh [--fast]"; }
(($# <= 1)) || {
  usage >&2
  exit 2
}
fast=0
case "${1:-}" in
"") ;;
--fast) fast=1 ;;
-h | --help)
  usage
  exit 0
  ;;
*)
  usage >&2
  exit 2
  ;;
esac

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
step "lint tables"
# rc-git-ffi and rc-git-testkit hand-copy the workspace lint tables; each Cargo.toml says why.
lint_table() {
  awk -v section="[$2]" '$0 == section { f = 1; next } /^\[/ { f = 0 } f && /^[a-z_]+ =/' "$1" |
    sort
}
lint_drift() {
  echo "error: $1 drifted from the workspace lint table in Cargo.toml (diff above)" >&2
  exit 1
}
ffi=crates/rc-git-ffi/Cargo.toml
testkit=crates/rc-git-testkit/Cargo.toml
panic_lints='^(unwrap_used|expect_used|panic|panic_in_result_fn|unimplemented) ='
diff <(lint_table Cargo.toml workspace.lints.clippy) \
  <(lint_table "$ffi" lints.clippy | grep -v '^undocumented_unsafe_blocks =') ||
  lint_drift "$ffi [lints.clippy]"
diff <(lint_table Cargo.toml workspace.lints.rust | grep -v '^unsafe_code =') \
  <(lint_table "$ffi" lints.rust | grep -v '^unsafe_op_in_unsafe_fn =') ||
  lint_drift "$ffi [lints.rust]"
diff <(lint_table Cargo.toml workspace.lints.clippy | grep -Ev "$panic_lints") \
  <(lint_table "$testkit" lints.clippy) ||
  lint_drift "$testkit [lints.clippy]"
diff <(lint_table Cargo.toml workspace.lints.rust) <(lint_table "$testkit" lints.rust) ||
  lint_drift "$testkit [lints.rust]"
step "shell scripts"
shellcheck scripts/*.sh scripts/tests/*.sh
shfmt -d scripts/*.sh scripts/tests/*.sh
step "install-app rollback"
scripts/tests/install-app.test.sh
step "github actions"
actionlint
zizmor --offline .github
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
step "core standalone"
# Workspace builds unify the guard's transport features into gix; this builds core without them.
cargo check --locked -p rc-git-core --all-targets
step "cargo test"
cargo test --locked --workspace
step "cargo doc"
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --quiet
step "cargo deny"
cargo deny --locked check
step "dev-only features"
# Captured first: grep -q closing the pipe early would fail cargo tree under pipefail.
for pkg in rc-git-ffi rc-git-cli; do
  features="$(cargo tree --locked -e features,normal -i gix-transport -p "$pkg")"
  if [[ $features == *http-client-insecure-credentials* ]]; then
    echo "error: dev-only gix-transport/http-client-insecure-credentials reached the $pkg" \
      "build; keep it in [dev-dependencies]" >&2
    exit 1
  fi
done
step "bundle paths"
# AppBuild relaunches these paths after a self-update, so they must match what the scripts build.
app_build=mac/Sources/RcGitKit/AppBuild.swift
swift_const() { sed -n "s/.*static let $1 = \"\([^\"]*\)\".*/\1/p" "$app_build"; }
script_app() { sed -n "s|^ *APP=\"\$ROOT/\([^\"]*\)\"$|\1|p" "$1"; }
installedPath="$(swift_const installedPath)"
devBundleName="$(swift_const devBundleName)"
install_apps="$(sed -n 's|^APPS=.*:-\([^}]*\)}"|\1|p' scripts/install-app.sh)"
install_name="$(sed -n "s|^TARGET=\"\\\$APPS/\([^\"]*\)\"\$|\1|p" scripts/install-app.sh)"
install_target="$install_apps/$install_name"
install_app="$(script_app scripts/install-app.sh)"
build_apps="$(script_app scripts/build-app.sh)"
release_app="$(sed -n 1p <<<"$build_apps")"
dev_app="$(sed -n 2p <<<"$build_apps")"
if [[ -z "$installedPath" || -z "$install_apps" || -z "$install_name" ||
  "$install_target" != "$installedPath" ||
  -z "$devBundleName" || "$dev_app" != "$devBundleName" ||
  -z "$install_app" || "$install_app" != "$release_app" ]]; then
  echo "error: bundle paths differ: $app_build installedPath '$installedPath'," \
    "devBundleName '$devBundleName'; scripts/install-app.sh TARGET '$install_target'," \
    "APP '$install_app'; scripts/build-app.sh release APP '$release_app'," \
    "dev APP '$dev_app'; set them alike" >&2
  exit 1
fi
plist_keys="$(sed -n 's/.*string("\(RCGit[A-Za-z]*\)").*/\1/p' "$app_build")"
[[ -n "$plist_keys" ]] || {
  echo "error: found no string(\"RCGit...\") Info.plist reads in $app_build" >&2
  exit 1
}
for key in $plist_keys; do
  grep -q "^stamp $key " scripts/build-app.sh || {
    echo "error: $app_build reads Info.plist key $key," \
      "but scripts/build-app.sh never stamps it" >&2
    exit 1
  }
done

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

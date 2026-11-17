#!/usr/bin/env bash
# The checks CI runs, minus assembling the app bundle, which only CI does.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() { echo "usage: check.sh"; }
case "$#:${1:-}" in
0:) ;;
1:-h | 1:--help)
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
require shellcheck "brew install shellcheck"
require shfmt "brew install shfmt"
require actionlint "brew install actionlint"
require zizmor "brew install zizmor"

step "cargo fmt"
cargo fmt --all -- --check
step "lint tables"
# rc-git-mac and rc-git-testkit hand-copy the workspace lint tables; each
# Cargo.toml says why.
lint_table() {
  awk -v section="[$2]" '$0 == section { f = 1; next } /^\[/ { f = 0 } f && /^[a-z_]+ =/' "$1" |
    sort
}
lint_drift() {
  echo "error: $1 drifted from the workspace lint table in Cargo.toml (diff above)" >&2
  exit 1
}
mac=crates/rc-git-mac/Cargo.toml
testkit=crates/rc-git-testkit/Cargo.toml
panic_lints='^(unwrap_used|expect_used|panic|panic_in_result_fn|unimplemented) ='
diff <(lint_table Cargo.toml workspace.lints.clippy) \
  <(lint_table "$mac" lints.clippy | grep -v '^undocumented_unsafe_blocks =') ||
  lint_drift "$mac [lints.clippy]"
diff <(lint_table Cargo.toml workspace.lints.rust | grep -v '^unsafe_code =') \
  <(lint_table "$mac" lints.rust | grep -v '^unsafe_op_in_unsafe_fn =') ||
  lint_drift "$mac [lints.rust]"
diff <(lint_table Cargo.toml workspace.lints.clippy | grep -Ev "$panic_lints") \
  <(lint_table "$testkit" lints.clippy) ||
  lint_drift "$testkit [lints.clippy]"
diff <(lint_table Cargo.toml workspace.lints.rust) <(lint_table "$testkit" lints.rust) ||
  lint_drift "$testkit [lints.rust]"
step "shell scripts"
shellcheck scripts/*.sh scripts/tests/*.sh
shfmt -d scripts/*.sh scripts/tests/*.sh
step "bounded runner"
scripts/tests/bounded.test.sh
step "changelog check"
scripts/tests/changelog-check.test.sh
step "github actions"
actionlint
zizmor --offline .github
step "guard write funnel"
# Writes live on the guard's Writer (built only from a Confirmation); network I/O in remote/.
if grep -rln 'clippy::disallowed_methods' crates/rc-git-guard/src |
  grep -vE '^crates/rc-git-guard/src/actions/(write\.rs|remote\.rs|remote/)'; then
  echo 'error: a disallowed-methods opt-out sits outside actions/write.rs and actions/remote;' \
    'make the write a Writer method (see .claude/rules/guard.md)' >&2
  exit 1
fi
step "rust doc comment width"
# rustfmt never wraps comments, so a doc line over max_width passes cargo fmt --check.
# perl -CSD counts characters; macOS awk counts bytes, so a line holding ⌘ read as too long.
# shellcheck disable=SC2016 # the single-quoted $ARGV and $_ are perl's, run through xargs
long_docs="$(git ls-files -z 'crates/*.rs' | xargs -0 perl -CSD -ne \
  'chomp; print "$ARGV:$.: $_\n" if length($_) > 100 && m{^\s*//[/!]}; close ARGV if eof')"
if [[ -n $long_docs ]]; then
  printf '%s\n' "$long_docs"
  echo 'error: Rust doc comment lines over 100 columns; rewrap them' >&2
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
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --document-private-items --quiet
step "cargo deny"
cargo deny --locked check
step "dev-only features"
# Captured first: grep -q closing the pipe early would fail cargo tree under pipefail.
for pkg in rc-git-app rc-git-cli; do
  features="$(cargo tree --locked -e features,normal -i gix-transport -p "$pkg")"
  if [[ $features == *http-client-insecure-credentials* ]]; then
    echo "error: dev-only gix-transport/http-client-insecure-credentials reached the $pkg" \
      "build; keep it in [dev-dependencies]" >&2
    exit 1
  fi
done
step "bundle stamp"
# AppBuild relaunches BUNDLE_NAME after a rebuild and reads the keys build-app.sh stamps.
app_build=crates/rc-git-kit/src/app_build.rs
bundle_name="$(sed -n 's/.*const BUNDLE_NAME: &.static str = "\([^"]*\)".*/\1/p' "$app_build")"
# shellcheck disable=SC2016 # a literal $ROOT, matched in the script text
script_app="$(sed -n 's|^APP="\$ROOT/\([^"]*\)"$|\1|p' scripts/build-app.sh)"
if [[ -z $bundle_name || $bundle_name != "$script_app" ]]; then
  echo "error: $app_build BUNDLE_NAME '$bundle_name' differs from" \
    "scripts/build-app.sh APP '$script_app'; set them alike" >&2
  exit 1
fi
plist_keys="$(sed -n 's/.*string("\(RCGit[A-Za-z]*\)").*/\1/p' "$app_build")"
[[ -n $plist_keys ]] || {
  echo "error: found no string(\"RCGit...\") Info.plist reads in $app_build" >&2
  exit 1
}
for key in $plist_keys; do
  grep "^stamp $key " scripts/build-app.sh >/dev/null || {
    echo "error: $app_build reads Info.plist key $key," \
      "but scripts/build-app.sh never stamps it" >&2
    exit 1
  }
done
step "macOS platform floor"
cargo_floor="$(sed -n 's/^MACOSX_DEPLOYMENT_TARGET = { value = "\([0-9.]*\)".*/\1/p' \
  .cargo/config.toml)"
plist_floor="$(plutil -extract LSMinimumSystemVersion raw crates/rc-git-app/Info.plist)"
if [[ -z $cargo_floor || $cargo_floor != "$plist_floor" ]]; then
  echo "error: macOS floor differs: .cargo/config.toml '$cargo_floor'," \
    "crates/rc-git-app/Info.plist '$plist_floor'; set both alike" >&2
  exit 1
fi

printf '\nall checks passed\n'

#!/usr/bin/env bash
# Builds librc_git_ffi.a and copies it to target/app, the one directory mac/Package.swift links.
#
#   scripts/rust-lib.sh <release|debug> [cargo args...]
#
# release uses [profile.release] (fat LTO, the shipped build); debug uses [profile.app-dev], the
# same profile without LTO and with incremental codegen, for the dev bundle's edit-run loop.
# Extra arguments go to cargo, e.g. --locked from the gate and CI.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
usage() { echo "usage: rust-lib.sh <release|debug> [cargo args...]"; }
(($# >= 1)) || {
  usage >&2
  exit 2
}
case "$1" in
release) profile=release ;;
debug) profile=app-dev ;;
-h | --help)
  usage
  exit 0
  ;;
*)
  usage >&2
  exit 2
  ;;
esac
shift
cd "$ROOT"

# Pinned to $ROOT/target, where Package.swift looks, over any CARGO_TARGET_DIR or cargo config.
CARGO_TARGET_DIR="$ROOT/target" cargo build "$@" --profile "$profile" -p rc-git-ffi
# A copy, not a symlink: the Swift build relinks only when the file it links changes.
mkdir -p target/app
cp "target/$profile/librc_git_ffi.a" target/app/librc_git_ffi.a
echo "linked target/$profile/librc_git_ffi.a as target/app/librc_git_ffi.a"

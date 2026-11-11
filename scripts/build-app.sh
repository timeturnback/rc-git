#!/usr/bin/env bash
# Assembles an app bundle from the SwiftPM binary. SwiftPM cannot emit an app bundle, and the
# bundle is what gives the app its name in the Dock and lets Finder open folders with it.
#
#   scripts/build-app.sh              release bundle -> RCGit.app
#   scripts/build-app.sh --dev        dev bundle     -> RCGit-dev.app
#   scripts/build-app.sh --gpui       GPUI bundle    -> RCGit-gpui.app (--dev: app-dev profile)
#
# The bundles carry different identifiers, so they keep separate tabs, recents, and window
# state, and different icons, so the Dock never confuses them. All are stamped with the
# commit and the checkout they were built from; that stamp is what the app's update window
# reads — "Update rc-git" in the release app, "Rebuild and Relaunch" in the dev one, which
# runs this script with --dev. The GPUI bundle is the Rust app the migration builds: its own
# identifier keeps it off the Swift app's tabs, and it wears the dev icon.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VARIANT=release
CONFIG=""
GPUI=false

usage() { echo "usage: build-app.sh [--dev] [--gpui] [--config release|debug]"; }

while [ $# -gt 0 ]; do
  case "$1" in
  --dev) VARIANT=dev ;;
  --gpui) GPUI=true ;;
  --config)
    case "${2:-}" in
    release | debug) CONFIG="$2" ;;
    *)
      usage >&2
      exit 2
      ;;
    esac
    shift
    ;;
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
done

case "$VARIANT" in
release)
  APP="$ROOT/RCGit.app"
  BUNDLE_ID="com.timeturnback.rcgit"
  NAME="rc-git"
  : "${CONFIG:=release}"
  ;;
dev)
  APP="$ROOT/RCGit-dev.app"
  BUNDLE_ID="com.timeturnback.rcgit.dev"
  NAME="rc-git (dev)"
  # Dev is for the edit-run loop, so it defaults to the build that compiles fastest.
  : "${CONFIG:=debug}"
  ;;
esac

ICON_VARIANT="$VARIANT"
STAMP_VARIANT="$VARIANT"
if [ "$GPUI" = true ]; then
  APP="$ROOT/RCGit-gpui.app"
  BUNDLE_ID="com.timeturnback.rcgit.gpui"
  NAME="rc-git (gpui)"
  ICON_VARIANT=dev
  STAMP_VARIANT=gpui
fi

ICON="$ROOT/build/icons/rcgit-$ICON_VARIANT.icns"
if [ ! -f "$ICON" ] || [ "$ROOT/scripts/make-icon.swift" -nt "$ICON" ]; then
  swift "$ROOT/scripts/make-icon.swift" --variant "$ICON_VARIANT" --output "$ICON" >/dev/null
fi

# Read before the build, so a checkout, commit or stash made while it runs cannot land in the
# stamp. A tree git cannot read (an export, a dubious-ownership refusal) still builds, but says
# so: its stamp names no commit and cannot be matched by the update window.
git_err="$(mktemp)"
git_state() { git -C "$ROOT" "$@" 2>"$git_err"; }
dirty=false
if commit="$(git_state rev-parse HEAD)" &&
  count="$(git_state rev-list --count HEAD)" &&
  porcelain="$(git_state --no-optional-locks status --porcelain --untracked-files=no)"; then
  if [ -n "$porcelain" ]; then dirty=true; fi
else
  reason="$(head -n 1 "$git_err")"
  echo "warning: cannot read git state ($reason); stamping commit unknown, build 0" >&2
  commit=unknown
  count=0
fi
rm -f "$git_err"

if [ "$GPUI" = true ]; then
  # debug maps to app-dev, as scripts/rust-lib.sh does: release minus fat LTO.
  case "$CONFIG" in
  release) PROFILE=release ;;
  debug) PROFILE=app-dev ;;
  esac
  cargo build --manifest-path "$ROOT/Cargo.toml" -p rc-git-app --profile "$PROFILE"
  BIN="$ROOT/target/$PROFILE/RCGit"
else
  "$ROOT/scripts/build.sh" "$CONFIG"
  BIN="$(swift build --package-path "$ROOT/mac" -c "$CONFIG" --show-bin-path)/RcGitApp"
fi
[ -x "$BIN" ] || {
  echo "error: binary not found at $BIN" >&2
  exit 1
}

# Assemble under build/ and swap at the end. "Rebuild and Relaunch" runs this script from
# the dev bundle it is replacing, and a bundle deleted out from under a live process loses
# every resource it has not already loaded. Plain -r (not -rf) so a wrong path fails loudly.
STAGE="$ROOT/build/stage/$(basename "$APP")"
BACKUP="$ROOT/build/stage/previous-$(basename "$APP")"
[ ! -d "$STAGE" ] || rm -r "$STAGE"
[ ! -d "$BACKUP" ] || rm -r "$BACKUP"
mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources"
cp "$BIN" "$STAGE/Contents/MacOS/RCGit"
cp "$ROOT/mac/Info.plist" "$STAGE/Contents/Info.plist"
cp "$ICON" "$STAGE/Contents/Resources/AppIcon.icns"

plist="$STAGE/Contents/Info.plist"

# Sets a key whether or not the template already carries it. plutil takes the value as its
# own argv word, so quotes and backslashes in a path are stored byte for byte.
stamped_keys=()
stamped_values=()
stamp() {
  local key="$1" value="$2"
  plutil -replace "$key" -string "$value" "$plist"
  stamped_keys+=("$key")
  stamped_values+=("$value")
}

verify_stamps() {
  local i key actual
  for i in "${!stamped_keys[@]}"; do
    key="${stamped_keys[$i]}"
    actual="$(plutil -extract "$key" raw "$plist")" || actual=""
    if [ "$actual" != "${stamped_values[$i]}" ]; then
      echo "error: Info.plist key $key reads back as '$actual', expected '${stamped_values[$i]}'" >&2
      exit 1
    fi
  done
}

# The version's patch is the commit count, so a commit on master is the only thing that
# moves it and nothing has to be bumped by hand. mac/Info.plist carries the major and minor;
# CFBundleVersion stays the bare count, which is the monotonic build number macOS wants.
series="$(plutil -extract CFBundleShortVersionString raw "$plist" 2>/dev/null || echo 0.1.0)"

stamp CFBundleIdentifier "$BUNDLE_ID"
stamp CFBundleName "$NAME"
stamp CFBundleDisplayName "$NAME"
stamp CFBundleShortVersionString "${series%.*}.$count"
stamp CFBundleVersion "$count"
stamp RCGitVariant "$STAMP_VARIANT"
stamp RCGitSourceRoot "$ROOT"
stamp RCGitSourceCommit "$commit"
stamp RCGitSourceDirty "$dirty"
stamp RCGitBuildDate "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
verify_stamps

# Ad-hoc signature: enough for local use, no developer account needed.
codesign --force --sign - "$STAGE" || {
  echo "error: ad-hoc codesign of $STAGE failed; kept the existing $APP" >&2
  exit 1
}

# The swap: two renames on the same filesystem. The old bundle moves aside under build/, the
# new one takes its place, and only then is the old one deleted. If the second rename fails,
# the old bundle moves back.
[ ! -d "$APP" ] || mv "$APP" "$BACKUP"
if ! mv "$STAGE" "$APP"; then
  echo "error: could not move $STAGE into place; restoring the previous $APP" >&2
  [ ! -d "$BACKUP" ] || mv "$BACKUP" "$APP"
  exit 1
fi
[ ! -d "$BACKUP" ] || rm -r "$BACKUP"

echo "built $APP"

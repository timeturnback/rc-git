#!/usr/bin/env bash
# Assembles an app bundle from the SwiftPM binary. SwiftPM cannot emit an app bundle, and the
# bundle is what gives the app its name in the Dock and lets Finder open folders with it.
#
#   scripts/build-app.sh              release bundle -> RCGit.app
#   scripts/build-app.sh --dev        dev bundle     -> RCGit-dev.app
#
# The two bundles carry different identifiers, so they keep separate tabs, recents, and
# window state, and different icons, so the Dock never confuses them. Both are stamped with
# the commit and the checkout they were built from; that stamp is what the app's update
# window reads — "Update rc-git" in the release app, "Rebuild and Relaunch" in the dev one,
# which runs this script with --dev.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VARIANT=release
CONFIG=""

usage() { echo "usage: build-app.sh [--dev] [--config release|debug]"; }

while [ $# -gt 0 ]; do
  case "$1" in
  --dev) VARIANT=dev ;;
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

ICON="$ROOT/build/icons/rcgit-$VARIANT.icns"
if [ ! -f "$ICON" ] || [ "$ROOT/scripts/make-icon.swift" -nt "$ICON" ]; then
  swift "$ROOT/scripts/make-icon.swift" --variant "$VARIANT" --output "$ICON" >/dev/null
fi

"$ROOT/scripts/build.sh" "$CONFIG"
BIN="$(swift build --package-path "$ROOT/mac" -c "$CONFIG" --show-bin-path)/RcGitApp"
[ -x "$BIN" ] || {
  echo "error: binary not found at $BIN" >&2
  exit 1
}

# Assemble under build/ and swap at the end. "Rebuild and Relaunch" runs this script from
# the dev bundle it is replacing, and a bundle deleted out from under a live process loses
# every resource it has not already loaded. Plain -r (not -rf) so a wrong path fails loudly.
STAGE="$ROOT/build/stage/$(basename "$APP")"
[ -d "$STAGE" ] && rm -r "$STAGE"
mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources"
cp "$BIN" "$STAGE/Contents/MacOS/RCGit"
cp "$ROOT/mac/Info.plist" "$STAGE/Contents/Info.plist"
cp "$ICON" "$STAGE/Contents/Resources/AppIcon.icns"

plist="$STAGE/Contents/Info.plist"

# Sets a key whether or not the template already carries it.
stamp() {
  local key="$1" value="$2"
  /usr/libexec/PlistBuddy -c "Add :$key string $value" "$plist" >/dev/null 2>&1 ||
    /usr/libexec/PlistBuddy -c "Set :$key $value" "$plist"
}

commit="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
count="$(git -C "$ROOT" rev-list --count HEAD 2>/dev/null || echo 0)"
dirty=false
[ -n "$(git -C "$ROOT" status --porcelain --untracked-files=no 2>/dev/null)" ] && dirty=true

# The version's patch is the commit count, so a commit on master is the only thing that
# moves it and nothing has to be bumped by hand. mac/Info.plist carries the major and minor;
# CFBundleVersion stays the bare count, which is the monotonic build number macOS wants.
series="$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$plist" 2>/dev/null ||
  echo 0.1.0)"

stamp CFBundleIdentifier "$BUNDLE_ID"
stamp CFBundleName "$NAME"
stamp CFBundleDisplayName "$NAME"
stamp CFBundleShortVersionString "${series%.*}.$count"
stamp CFBundleVersion "$count"
stamp RCGitVariant "$VARIANT"
stamp RCGitSourceRoot "$ROOT"
stamp RCGitSourceCommit "$commit"
stamp RCGitSourceDirty "$dirty"
stamp RCGitBuildDate "$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Ad-hoc signature: enough for local use, no developer account needed.
codesign --force --sign - "$STAGE" >/dev/null 2>&1 || {
  echo "warning: ad-hoc codesign failed; the app may not launch" >&2
}

# The swap. A rename on the same filesystem, so the window where the bundle is missing is
# as short as it can be.
[ -d "$APP" ] && rm -r "$APP"
mv "$STAGE" "$APP"

echo "built $APP"

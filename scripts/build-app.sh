#!/usr/bin/env bash
# Assembles RCGit.app from the SwiftPM binary. SwiftPM cannot emit an app bundle, and the bundle
# is what gives the app its name in the Dock and lets Finder open folders with it.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG="${1:-release}"
APP="$ROOT/RCGit.app"

"$ROOT/scripts/build.sh" "$CONFIG"
BIN="$(swift build --package-path "$ROOT/mac" -c "$CONFIG" --show-bin-path)/RcGitApp"
[ -x "$BIN" ] || {
  echo "error: binary not found at $BIN" >&2
  exit 1
}

# Replace any previous bundle. Plain -r (not -rf) so a wrong path fails loudly.
[ -d "$APP" ] && rm -r "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/RCGit"
cp "$ROOT/mac/Info.plist" "$APP/Contents/Info.plist"

# Ad-hoc signature: enough for local use, no developer account needed.
codesign --force --sign - "$APP" >/dev/null 2>&1 || {
  echo "warning: ad-hoc codesign failed; the app may not launch" >&2
}

echo "built $APP"

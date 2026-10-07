#!/usr/bin/env bash
# Builds the release bundle and installs it as /Applications/RCGit.app. Run it once to
# install; the app's "Update rc-git" window runs the same script for every update after
# that, which is why the swap is written to survive a failure: the old bundle moves aside
# first and moves back if the new one cannot take its place.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPS="/Applications"
TARGET="$APPS/RCGit.app"
APP="$ROOT/RCGit.app"

usage() {
  echo "usage: install-app.sh"
  echo "Builds the release bundle and replaces $TARGET with it."
}
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

[ -w "$APPS" ] || {
  echo "error: $APPS is not writable by $(whoami); install as an administrator" >&2
  exit 1
}

"$ROOT/scripts/build-app.sh"

staging="$APPS/.rcgit-staging-$$"
backup="$APPS/.rcgit-previous-$$"
trap '[ -d "$staging" ] && rm -r "$staging"' EXIT

echo "==> installing $TARGET"
ditto "$APP" "$staging"
[ -d "$TARGET" ] && mv "$TARGET" "$backup"
if ! mv "$staging" "$TARGET"; then
  echo "error: could not move the new bundle into place" >&2
  [ -d "$backup" ] && mv "$backup" "$TARGET"
  exit 1
fi
[ -d "$backup" ] && rm -r "$backup"

echo "installed $TARGET"

#!/usr/bin/env bash
# Builds the release bundle and installs it as /Applications/RCGit.app. Run it once to
# install; the app's "Update rc-git" window runs the same script for every update after
# that, which is why the swap is written to survive a failure: the old bundle moves aside
# first and moves back if the new one cannot take its place, or if the script is interrupted
# (TERM, INT, any exit) between the two renames. SIGKILL and power loss cannot run a trap, so
# they can still leave the previous bundle at $APPS/.rcgit-previous-<pid>.
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
cleanup() {
  [ ! -d "$staging" ] || rm -r "$staging"
  if [ ! -e "$TARGET" ] && [ -d "$backup" ]; then
    mv "$backup" "$TARGET"
  fi
}
trap cleanup EXIT

echo "==> installing $TARGET"
ditto "$APP" "$staging"
[ ! -d "$TARGET" ] || mv "$TARGET" "$backup"
if ! mv "$staging" "$TARGET"; then
  echo "error: could not move the new bundle into place; restoring the previous one" >&2
  exit 1
fi
if [ -d "$backup" ] && ! rm -r "$backup"; then
  echo "warning: installed $TARGET, but could not remove the previous bundle at $backup" >&2
fi

echo "installed $TARGET"

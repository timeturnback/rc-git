#!/usr/bin/env bash
# Runs a copy of scripts/install-app.sh against a sandbox Applications folder, with a stub
# build-app.sh, and checks the bundle swap: a fresh install, a replacement, and the rollback
# when the new bundle cannot move into place, when the backup cannot be removed, and when a
# TERM lands between the two renames. Needs no toolchain; finishes in seconds.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
base="$ROOT/build/tests/install-app"
mkdir -p "$base"
sandbox="$(mktemp -d "$base/run.XXXXXX")"
trap 'rm -r "$sandbox"' EXIT

apps="$sandbox/Applications"
stubs="$sandbox/stubs"
mkdir -p "$sandbox/scripts" "$apps" "$stubs"
cp "$ROOT/scripts/install-app.sh" "$sandbox/scripts/install-app.sh"
cat >"$sandbox/scripts/build-app.sh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$root/RCGit.app/Contents"
printf '%s\n' "$STAMP" >"$root/RCGit.app/Contents/stamp"
STUB
chmod +x "$sandbox/scripts/build-app.sh"

failures=0
fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

# stub <command> <case>: writes stdin as an executable <command> in $stubs/<case>, the directory
# install() puts first on PATH. Not called inside $( ): bash 3.2 mis-parses a heredoc there.
stub() {
  mkdir -p "$stubs/$2"
  cat >"$stubs/$2/$1"
  chmod +x "$stubs/$2/$1"
}

# install <stamp> [case]: runs the sandboxed install-app.sh, with that case's stubs first on PATH;
# output goes to out.log, the status is the script's.
install() {
  local path="$PATH"
  [ -z "${2:-}" ] || path="$stubs/$2:$PATH"
  STAMP="$1" RCGIT_APPS_DIR="$apps" PATH="$path" \
    "$sandbox/scripts/install-app.sh" >"$sandbox/out.log" 2>&1
}

installed_stamp() { cat "$apps/RCGit.app/Contents/stamp" 2>/dev/null || echo "<missing>"; }

expect_stamp() {
  local got
  got="$(installed_stamp)"
  [ "$got" = "$2" ] || fail "$1: installed stamp is '$got', expected '$2'"
}

expect_no_leftovers() {
  local left
  left="$(find "$apps" -mindepth 1 -maxdepth 1 -name '.rcgit-*')"
  [ -z "$left" ] || fail "$1: left behind: $left"
}

echo "==> fresh install"
install one || fail "fresh install exited $?: $(cat "$sandbox/out.log")"
expect_stamp "fresh install" one
expect_no_leftovers "fresh install"

echo "==> replacing install"
install two || fail "replacing install exited $?: $(cat "$sandbox/out.log")"
expect_stamp "replacing install" two
expect_no_leftovers "replacing install"

echo "==> new bundle cannot move into place"
stub mv staging <<'STUB'
#!/usr/bin/env bash
case "$1" in */.rcgit-staging-*) exit 1 ;; esac
exec /bin/mv "$@"
STUB
if install three staging; then
  fail "failed rename: install-app.sh exited 0"
fi
expect_stamp "failed rename" two
expect_no_leftovers "failed rename"

echo "==> previous bundle cannot be removed"
stub rm backup <<'STUB'
#!/usr/bin/env bash
case "${!#}" in */.rcgit-previous-*) exit 1 ;; esac
exec /bin/rm "$@"
STUB
install four backup ||
  fail "failed backup removal exited $?: $(cat "$sandbox/out.log")"
expect_stamp "failed backup removal" four
grep -q '^warning: .*could not remove the previous bundle' "$sandbox/out.log" ||
  fail "failed backup removal: no warning in: $(cat "$sandbox/out.log")"
find "$apps" -mindepth 1 -maxdepth 1 -name '.rcgit-previous-*' -exec /bin/rm -r {} +
expect_no_leftovers "failed backup removal"

echo "==> TERM between the two renames"
stub mv term <<'STUB'
#!/usr/bin/env bash
/bin/mv "$@"
case "${!#}" in */.rcgit-previous-*) kill -TERM "$PPID" ;; esac
STUB
# The subshell keeps bash's "Terminated" job notice out of the test output.
if (install five term) 2>/dev/null; then
  fail "TERM: install-app.sh exited 0"
fi
expect_stamp "TERM" four
expect_no_leftovers "TERM"

if ((failures > 0)); then
  echo "install-app.test.sh: $failures failure(s)" >&2
  exit 1
fi
echo "install-app.test.sh: all cases passed"

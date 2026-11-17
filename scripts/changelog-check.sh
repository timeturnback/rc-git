#!/usr/bin/env bash
# Refuses a feat or fix commit that leaves CHANGELOG.md alone, so every change a user would
# notice reaches the changelog Help ▸ Changelog shows. A commit whose parent has no
# CHANGELOG.md predates the changelog and is skipped.
#
#   scripts/changelog-check.sh            the commits HEAD has and no remote-tracking ref has
#   scripts/changelog-check.sh <range>    any revision range, e.g. origin/master..HEAD
#   scripts/changelog-check.sh -h|--help  print usage
#
# Runs before each push (.pre-commit-config.yaml), while an offending commit is still local and
# can be amended. The hook checks HEAD, not the pushed ref: the global dispatcher forwards no
# refs, so `git push origin other-branch` stays unchecked there. CI runs it on the pushed or
# pull-request range as a backstop.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage="usage: changelog-check.sh [<revision-range>]"
case "${1:-}" in
-h | --help)
  echo "$usage"
  exit 0
  ;;
-*)
  echo "$usage" >&2
  exit 2
  ;;
esac
if [ "$#" -gt 1 ]; then
  echo "$usage" >&2
  exit 2
fi

if [ "$#" -gt 0 ]; then
  revs=("$1")
else
  revs=(HEAD --not --remotes)
fi

# Assigned first: a range git cannot resolve fails here with git's status, instead of reading
# as a range with nothing in it.
pattern='^(feat|fix)(\([^)]*\))?!?:'
commits="$(git log --no-merges --format='%H%x09%s' "${revs[@]}")"
missing=""

while IFS=$'\t' read -r sha subject; do
  [[ $subject =~ $pattern ]] || continue
  git cat-file -e "$sha^:CHANGELOG.md" 2>/dev/null || continue
  changed="$(git diff-tree --no-commit-id --name-only -r "$sha" -- CHANGELOG.md)"
  [ -n "$changed" ] && continue
  missing+="  $(git rev-parse --short "$sha") $subject"$'\n'
done <<<"$commits"

if [ -n "$missing" ]; then
  echo "error: these feat/fix commits do not update CHANGELOG.md:" >&2
  printf '%s' "$missing" >&2
  echo "Add each one's entry to CHANGELOG.md in that commit (amend, or fixup and rebase)," >&2
  echo "then push again." >&2
  exit 1
fi

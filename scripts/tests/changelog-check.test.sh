#!/usr/bin/env bash
# Runs scripts/changelog-check.sh against throwaway repositories. The script cds to its own
# repository root, so each case copies it into the scripts/ folder of a scratch repository and
# runs that copy. Git runs with no user or system config, no hooks and a fixed identity.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
base="$ROOT/build/tests/changelog-check"
mkdir -p "$base"
sandbox="$(mktemp -d "$base/run.XXXXXX")"
trap 'rm -r "$sandbox"' EXIT

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com

failures=0
fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

g() { git -C "$repo" -c core.hooksPath=/dev/null "$@"; }

# fresh <case> [no-changelog]: a repository holding the real script in its first commit, plus
# CHANGELOG.md unless told otherwise.
fresh() {
  repo="$sandbox/$1"
  mkdir -p "$repo/scripts"
  cp "$ROOT/scripts/changelog-check.sh" "$repo/scripts/"
  if [[ ${2:-} != no-changelog ]]; then
    printf '# Changelog\n' >"$repo/CHANGELOG.md"
  fi
  g init -q -b master
  g add -A
  g commit -qm init
}

# commit <subject> <file>: appends the subject to the file and commits it.
commit() {
  printf '%s\n' "$1" >>"$repo/$2"
  g add -A
  g commit -qm "$1"
}

# check [range]: runs the copy, keeping its status and stderr.
check() {
  status=0
  (cd "$repo" && ./scripts/changelog-check.sh "$@") >"$repo.out" 2>"$repo.err" || status=$?
}

reported() { grep -F " $1" "$repo.err" >/dev/null; }

fresh refused
commit 'fix(sidebar): keep the selection' a.txt
commit 'feat!: drop the old format' b.txt
check HEAD~2..HEAD
((status == 1)) || fail "refused: exit $status, want 1"
reported 'fix(sidebar): keep the selection' || fail "refused: the scoped fix is not named"
reported 'feat!: drop the old format' || fail "refused: the breaking feat is not named"

fresh passes
commit 'chore: feat: tidy' a.txt
commit 'feat: add search' CHANGELOG.md
check HEAD~2..HEAD
((status == 0)) || fail "passes: exit $status, want 0: $(cat "$repo.err")"

fresh merges
g checkout -qb side
commit 'docs: side work' a.txt
g checkout -q master
g merge -q --no-ff -m 'fix: merge side' side
check HEAD~1..HEAD
((status == 0)) || fail "merges: a merge commit is checked, exit $status"

fresh before no-changelog
commit 'feat: early work' a.txt
check HEAD~1..HEAD
((status == 0)) || fail "before: a commit before the changelog is refused, exit $status"

fresh first
commit 'fix: add the changelog' CHANGELOG.md
commit 'feat: right after' a.txt
check HEAD~2..HEAD
((status == 1)) || fail "first: exit $status, want 1"
reported 'feat: right after' || fail "first: the commit after the changelog is not named"
if reported 'fix: add the changelog'; then
  fail "first: the commit that adds the changelog is reported"
fi

fresh badrange
check nope..HEAD
((status == 128)) || fail "badrange: exit $status, want git's 128"

fresh upstream
commit 'fix: first push' a.txt
check
((status == 1)) || fail "upstream: exit $status, want 1"
reported 'fix: first push' || fail "upstream: the unpushed fix is not named"

fresh pushed
commit 'fix: already pushed' a.txt
g update-ref refs/remotes/origin/master HEAD
commit 'fix: still local' b.txt
check
((status == 1)) || fail "pushed: exit $status, want 1"
reported 'fix: still local' || fail "pushed: the local fix is not named"
if reported 'fix: already pushed'; then
  fail "pushed: a commit on a remote-tracking ref is reported"
fi

fresh usage
check -x
((status == 2)) || fail "usage: an unknown flag exits $status, want 2"
check a..b c..d
((status == 2)) || fail "usage: two ranges exit $status, want 2"

if ((failures > 0)); then
  echo "$failures changelog-check.sh check(s) failed" >&2
  exit 1
fi
echo "changelog-check.sh: all checks passed"

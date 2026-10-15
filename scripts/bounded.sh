#!/usr/bin/env bash
# Runs a command under a wall-clock bound; scripts/check.sh runs `swift test` through it so a
# hung test fails the gate instead of stalling it. On timeout it names the swift-testing suites
# and tests that started and never finished, and exits 124. Every process group the command's
# processes ran in is killed when this script exits, so no swiftpm-testing-helper (which leads
# a group of its own) outlives the run.
set -euo pipefail

usage() { echo "usage: bounded.sh <seconds> <command> [args...]"; }
seconds='^[1-9][0-9]*$'
(($# >= 2)) && [[ $1 =~ $seconds ]] || {
  usage >&2
  exit 2
}
limit=$1
shift
cmd="$*"

log="$(mktemp -t bounded)"
job=""
groups=""
own_group="$(ps -o pgid= -p $$ | tr -d ' ')"

# Adds the process group of every live descendant of the job to $groups.
track() {
  local frontier=$job children pid pgid
  while [[ -n ${frontier// /} ]]; do
    children=""
    for pid in $frontier; do
      pgid="$(ps -o pgid= -p "$pid" | tr -d ' ' || true)"
      if [[ -n $pgid && $pgid != "$own_group" && " $groups " != *" $pgid "* ]]; then
        groups="$groups $pgid"
      fi
      children="$children $(pgrep -P "$pid" | tr '\n' ' ' || true)"
    done
    frontier=$children
  done
}

# signal_groups <signal>: sends it to every tracked group that still has a process.
signal_groups() {
  local group
  for group in $groups; do
    pkill "-$1" -g "$group" || true
  done
}

# shellcheck disable=SC2329 # run by the EXIT trap below; shellcheck misses that here
cleanup() {
  local group left
  for group in $groups; do
    left="$(pgrep -g "$group" | tr '\n' ' ' || true)"
    if [[ -n ${left// /} ]]; then
      echo "warning: killing processes $cmd left behind: $left" >&2
      pkill -KILL -g "$group" || true
    fi
  done
  rm -f "$log"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# A pseudo-terminal keeps the command's output line-buffered: live on screen, and complete in
# the log when it is killed. Job control gives the job a process group of its own.
set -m
(script -q /dev/null "$@" </dev/null 2>&1 | tee "$log") &
job=$!
set +m

# A finished test prints "<mark> Test name() passed|failed|skipped ..."; one still running only
# printed "<mark> Test name() started.". Counts, since two suites can share a test name.
still_running() {
  awk '
    { gsub(/\r/, ""); gsub(/\033\[[0-9;]*m/, "") }
    / (Suite|Test) .* started\.$/ {
      sub(/^[^ ]* /, ""); sub(/ started\.$/, ""); open[$0]++; next
    }
    / (Suite|Test) .* (passed|failed|skipped)/ {
      sub(/^[^ ]* /, ""); sub(/ (passed|failed|skipped).*$/, ""); open[$0]--
    }
    END { for (name in open) if (open[name] > 0) print "  " name }
  ' "$log" | sort
}

deadline=$((SECONDS + limit))
while kill -0 "$job" 2>/dev/null; do
  track
  if ((SECONDS >= deadline)); then
    running="$(still_running)"
    echo "error: $cmd still running after ${limit}s; killing it" >&2
    if [[ -n $running ]]; then
      printf 'started and never finished:\n%s\n' "$running" >&2
    else
      echo "no test was mid-run; the hang is in the build or the runner itself" >&2
    fi
    signal_groups TERM
    sleep 2
    exit 124
  fi
  sleep 1
done
status=0
wait "$job" || status=$?
exit "$status"

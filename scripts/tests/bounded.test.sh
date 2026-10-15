#!/usr/bin/env bash
# Runs scripts/bounded.sh against stub commands: a pass and a failure keep their exit status, a
# hang fails with 124 inside its bound and names the test still running, and a process the
# command leaves behind is killed in both cases. Needs no toolchain; finishes in seconds.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
base="$ROOT/build/tests/bounded"
mkdir -p "$base"
sandbox="$(mktemp -d "$base/run.XXXXXX")"
trap 'rm -r "$sandbox"' EXIT
bounded="$ROOT/scripts/bounded.sh"

failures=0
fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

# A stand-in for swift test: prints swift-testing progress lines and starts a sleeper that, like
# swiftpm-testing-helper, leads a process group of its own and records its pid. Then it runs
# for 5 s (time for bounded.sh to see the sleeper on a loaded machine) and exits with $1, or
# given "hang", never returns.
cat >"$sandbox/fake-test" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
echo '◇ Suite Fake started.'
echo '◇ Test finishes() started.'
echo '✔ Test finishes() passed after 0.001 seconds.'
perl -MPOSIX -e 'setpgid(0, 0); exec @ARGV' sleep 300 >/dev/null 2>&1 &
echo $! >"$LEFTOVER"
if [[ $1 == hang ]]; then
  echo '◇ Test hangs() started.'
  sleep 300
fi
sleep 5
exit "$1"
STUB
chmod +x "$sandbox/fake-test"

# run <case> <bound> <arg>: runs bounded.sh, keeping its status, stderr and elapsed seconds.
run() {
  export LEFTOVER="$sandbox/$1.pid"
  local start=$SECONDS
  status=0
  "$bounded" "$2" "$sandbox/fake-test" "$3" >"$sandbox/$1.out" 2>"$sandbox/$1.err" || status=$?
  elapsed=$((SECONDS - start))
}

# leftover_killed <case>: the sleeper the command left behind is gone.
leftover_killed() {
  local pid
  pid="$(cat "$sandbox/$1.pid")"
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid"
    fail "$1: leftover process $pid outlived the run"
  fi
}

run pass 30 0
((status == 0)) || fail "pass: exit $status, want 0"
grep -q 'Test finishes() passed' "$sandbox/pass.out" || fail "pass: test output not passed through"
grep -q 'left behind' "$sandbox/pass.err" || fail "pass: no warning about the leftover process"
leftover_killed pass

run failure 30 3
((status == 3)) || fail "failure: exit $status, want 3"
leftover_killed failure

run hang 2 hang
((status == 124)) || fail "hang: exit $status, want 124"
# Far under the stub's 300 s hang, with room for a loaded machine.
((elapsed <= 60)) || fail "hang: took ${elapsed}s against a 2s bound"
grep -q 'Test hangs()' "$sandbox/hang.err" || fail "hang: the running test is not named"
grep -q 'Suite Fake' "$sandbox/hang.err" || fail "hang: the running suite is not named"
if grep -q 'Test finishes()' "$sandbox/hang.err"; then
  fail "hang: a finished test is reported as running"
fi
leftover_killed hang

status=0
"$bounded" 0 true 2>/dev/null || status=$?
((status == 2)) || fail "usage: a zero bound exits $status, want 2"
status=0
"$bounded" 5 2>/dev/null || status=$?
((status == 2)) || fail "usage: a missing command exits $status, want 2"

if ((failures > 0)); then
  echo "$failures bounded.sh check(s) failed" >&2
  exit 1
fi
echo "bounded.sh: all checks passed"

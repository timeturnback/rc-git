//! The runner against real `/bin/sh` (and perl where one process must sleep between lines).
//!
//! A spawn takes seconds on a loaded machine with endpoint agents, so every limit a child
//! must outrun before it prints leaves seconds for its start, elapsed times run from the
//! child's first line rather than from the spawn, and scripts sleep in one process instead of
//! spawning `sleep` per step. Bounds sit halfway to the long sleeps, far from the kill's few
//! seconds, so a loaded machine cannot flip them.

use std::sync::Mutex;
use std::thread;

use super::*;

fn directory() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

fn spec(executable: &str, arguments: &[&str]) -> CommandSpec {
    CommandSpec::new(executable, arguments, &directory())
}

fn sh(script: &str) -> CommandSpec {
    spec("/bin/sh", &["-c", script])
}

/// Runs `command` and times it from its first line of output, when it reports it started; a
/// command that never prints is timed from the spawn.
fn launch(command: &CommandSpec) -> (Outcome, Duration) {
    let spawned = Instant::now();
    let first = Mutex::new(None::<Instant>);
    let on_output = |_: &[String]| {
        first.lock().unwrap().get_or_insert_with(Instant::now);
    };
    let outcome = ShellRunner.run(command, &CancelToken::new(), Some(&on_output));
    let finished = Instant::now();
    let start = first.lock().unwrap().unwrap_or(spawned);
    (outcome, finished - start)
}

/// Whether `pid` is gone, polled for up to ten seconds: an orphan killed by a signal is
/// reaped by launchd, not at once.
fn is_gone(pid: i32) -> bool {
    for _ in 0..200 {
        if send(pid, 0).is_err() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn first_pid(outcome: &Outcome) -> i32 {
    outcome
        .lines
        .first()
        .and_then(|line| line.parse().ok())
        .unwrap()
}

#[test]
fn a_command_past_its_limit_is_killed() {
    let mut command = sh("echo started; exec sleep 60");
    command.timeout = Some(Duration::from_secs(5));
    let (outcome, elapsed) = launch(&command);
    assert_eq!(outcome.stopped, Some(Stop::TimedOut));
    assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
}

#[test]
fn a_silent_command_is_stopped_by_the_idle_clock() {
    let mut command = sh("echo waiting; exec sleep 60");
    command.timeout = Some(Duration::from_secs(60));
    command.idle_timeout = Some(Duration::from_secs(5));
    let (outcome, elapsed) = launch(&command);
    assert_eq!(outcome.stopped, Some(Stop::Idle));
    assert_eq!(outcome.lines, ["waiting"]);
    assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
}

#[test]
fn steady_output_keeps_the_idle_clock_armed() {
    let script = "$| = 1; for (1..10) { print qq($_\\n); select(undef, undef, undef, 0.8) }";
    let mut command = spec("/usr/bin/perl", &["-e", script]);
    command.idle_timeout = Some(Duration::from_secs(5));
    let (outcome, _) = launch(&command);
    assert_eq!(outcome.stopped, None);
    assert_eq!(outcome.status, 0);
    assert_eq!(outcome.lines.len(), 10);
}

#[test]
fn cancelling_kills_the_process_group() {
    let cancel = Arc::new(CancelToken::new());
    let canceller = Arc::clone(&cancel);
    let on_output = move |_: &[String]| canceller.signal();
    let outcome = ShellRunner.run(&sh("sleep 30 & echo $!; wait"), &cancel, Some(&on_output));
    assert_eq!(outcome.stopped, Some(Stop::Cancelled));
    assert!(is_gone(first_pid(&outcome)));
}

#[test]
fn a_timeout_kills_a_grandchild_that_ignores_sigterm() {
    let mut command = sh("(trap '' TERM; exec sleep 60) & echo $!; wait");
    command.timeout = Some(Duration::from_secs(5));
    let (outcome, elapsed) = launch(&command);
    assert_eq!(outcome.stopped, Some(Stop::TimedOut));
    assert!(is_gone(first_pid(&outcome)));
    assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
}

#[test]
fn a_leader_that_left_its_group_is_still_killed() {
    let script = "$| = 1; setpgid(0, getpgrp(getppid())) or die $!; print qq(moved\\n); sleep 60";
    let mut command = spec("/usr/bin/perl", &["-MPOSIX", "-e", script]);
    command.timeout = Some(Duration::from_secs(5));
    let (outcome, elapsed) = launch(&command);
    assert_eq!(outcome.lines, ["moved"]);
    assert_eq!(outcome.stopped, Some(Stop::TimedOut));
    assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
}

#[test]
fn a_command_started_already_cancelled_is_stopped() {
    let start = Instant::now();
    let cancel = CancelToken::new();
    cancel.signal();
    let outcome = ShellRunner.run(&sh("sleep 30"), &cancel, None);
    assert_eq!(outcome.stopped, Some(Stop::Cancelled));
    assert!(start.elapsed() < Duration::from_secs(15));
}

#[test]
fn a_grandchild_holding_the_pipe_does_not_hold_the_run() {
    let (outcome, elapsed) = launch(&sh("(sleep 45 & echo $!); echo hi"));
    let grandchild = first_pid(&outcome);
    let _ = send(grandchild, SIGKILL);
    assert_eq!(outcome.lines[1..], ["hi"]);
    assert_eq!(outcome.stopped, None);
    assert!(elapsed < Duration::from_secs(20), "{elapsed:?}");
}

#[test]
fn a_last_line_without_a_newline_is_kept() {
    let seen = Mutex::new(Vec::new());
    let on_output = |lines: &[String]| seen.lock().unwrap().extend(lines.iter().cloned());
    let outcome = ShellRunner.run(&sh("printf tail"), &CancelToken::new(), Some(&on_output));
    assert_eq!(outcome.lines, ["tail"]);
    assert_eq!(outcome.status, 0);
    assert_eq!(*seen.lock().unwrap(), ["tail"]);
}

#[test]
fn stdout_and_stderr_arrive_merged() {
    let (outcome, _) = launch(&sh("echo out; echo err >&2; echo out2"));
    assert_eq!(outcome.lines, ["out", "err", "out2"]);
}

#[test]
fn a_failing_command_reports_its_status() {
    let (outcome, _) = launch(&sh("echo nope; exit 3"));
    assert_eq!(outcome.status, 3);
    assert_eq!(outcome.lines, ["nope"]);
    assert_eq!(outcome.stopped, None);
    assert!(!outcome.succeeded());
    assert_eq!(outcome.failure(), "failed (status 3)");
}

#[test]
fn a_command_killed_by_a_signal_says_so() {
    let (outcome, _) = launch(&sh("echo dying; kill -9 $$"));
    assert_eq!(outcome.termination, Termination::Signal);
    assert_eq!(outcome.status, SIGKILL);
    assert_eq!(outcome.lines, ["dying"]);
    assert_eq!(outcome.stopped, None);
    assert!(!outcome.succeeded());
    assert_eq!(outcome.failure(), "was killed by signal 9 (SIGKILL)");
}

#[test]
fn a_missing_working_directory_is_named() {
    let missing = format!("{}/rc-git-moved-{}", directory(), std::process::id());
    let command = CommandSpec::new("/bin/sh", &["-c", "true"], &missing);
    let outcome = ShellRunner.run(&command, &CancelToken::new(), None);
    assert_eq!(outcome.status, -1);
    assert!(!outcome.succeeded());
    let prefix = format!("working directory {missing} does not exist: ");
    assert!(outcome.lines[0].starts_with(&prefix), "{:?}", outcome.lines);
}

#[test]
fn a_missing_executable_is_named() {
    let seen = Mutex::new(Vec::new());
    let on_output = |lines: &[String]| seen.lock().unwrap().extend(lines.iter().cloned());
    let command = spec("/nonexistent/rc-git-shell", &[]);
    let outcome = ShellRunner.run(&command, &CancelToken::new(), Some(&on_output));
    assert_eq!(outcome.status, -1);
    assert!(
        outcome.lines[0].starts_with("cannot run /nonexistent/rc-git-shell: "),
        "{:?}",
        outcome.lines
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the cause reaches the log too"
    );
}

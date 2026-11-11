//! [`ProcessRunner`] on `std::process` (`Shell.swift`): the child starts as the leader of its
//! own process group, stdout and stderr share one pipe read on a thread of its own, and a
//! wall-clock limit, an idle limit and a cancel each kill the whole group: `SIGTERM`, then
//! `SIGKILL` after [`KILL_GRACE`]. A script that `exec`s or spawns cargo takes them down with
//! it instead of leaving them holding the build lock. The wait for the last bytes is bounded
//! by [`DRAIN_GRACE`], since a grandchild that inherited the pipe can keep it open for good.

use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use rc_git_kit::guard_bridge::CancelToken;
use rc_git_kit::platform::process::{
    CommandSpec, DRAIN_GRACE, KILL_GRACE, LineCollector, OnOutput, Outcome, ProcessRunner, Stop,
    Termination, start_failure,
};

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;
const ESRCH: i32 = 3;
/// How often the run checks its clocks, the cancel flag and the child.
const TICK: Duration = Duration::from_millis(20);

// `kill(2)` lives in libSystem, which every binary links.
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

/// Sends `signal` to `target` (a pid, or a negated group id); whether `kill` succeeded and
/// the errno it set when it did not.
fn send(target: i32, signal: i32) -> Result<(), i32> {
    // SAFETY: `kill` takes plain integers and touches no memory of ours.
    let result = unsafe { kill(target, signal) };
    if result == 0 {
        return Ok(());
    }
    Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
}

/// The live runner.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShellRunner;

impl ProcessRunner for ShellRunner {
    fn run(
        &self,
        command: &CommandSpec,
        cancel: &CancelToken,
        on_output: Option<OnOutput<'_>>,
    ) -> Outcome {
        let collector = Arc::new(LineCollector::new());
        let (child, batches) = match spawn(command, &collector) {
            Ok(started) => started,
            Err(line) => {
                let outcome = Outcome::not_started(line);
                if let Some(on_output) = on_output {
                    on_output(&outcome.lines);
                }
                return outcome;
            }
        };
        let mut run = Run {
            child,
            collector,
            batches,
            killer: Killer::default(),
            exit: None,
            wait_error: None,
        };
        run.supervise(command, cancel, on_output);
        drain(&run.batches, on_output);
        run.outcome(on_output)
    }
}

/// Starts `command` in a group of its own with one pipe for both output streams, and a
/// thread reading it to EOF. Returns the child and the channel the reader sends each batch
/// of complete lines on; the channel disconnects at EOF.
fn spawn(
    command: &CommandSpec,
    collector: &Arc<LineCollector>,
) -> Result<(Child, Receiver<Vec<String>>), String> {
    let (mut reader, writer) = std::io::pipe()
        .map_err(|error| format!("cannot open a pipe for {}: {error}", command.executable))?;
    let stderr = writer
        .try_clone()
        .map_err(|error| format!("cannot open a pipe for {}: {error}", command.executable))?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the bounded runner is the one sanctioned spawn: the updater's probe and build"
    )]
    let mut process = Command::new(&command.executable);
    process
        .args(&command.arguments)
        .current_dir(&command.directory)
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(stderr)
        .process_group(0);
    let spawned = process.spawn();
    // The command holds the parent's copies of the write end; only once they close can the
    // reader see EOF when the child and its descendants are done.
    drop(process);
    let child = spawned.map_err(|error| {
        start_failure(&command.executable, &command.directory, &error.to_string())
    })?;
    let (sender, batches) = mpsc::channel();
    let sink = Arc::clone(collector);
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let lines = sink.take(&buffer[..read]);
                    if !lines.is_empty() && sender.send(lines).is_err() {
                        break;
                    }
                }
            }
        }
    });
    Ok((child, batches))
}

/// Hands every batch that arrives to `on_output` until the reader hits EOF or the drain
/// grace runs out, whichever is first.
fn drain(batches: &Receiver<Vec<String>>, on_output: Option<OnOutput<'_>>) {
    let deadline = Instant::now() + DRAIN_GRACE;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match batches.recv_timeout(left) {
            Ok(lines) => {
                if let Some(on_output) = on_output {
                    on_output(&lines);
                }
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Kills one process group, once: `SIGTERM`, then `SIGKILL` for whatever is left after the
/// grace. The first stop records why; later ones change nothing.
#[derive(Debug, Default)]
struct Killer {
    reason: Option<Stop>,
    terminated_at: Option<Instant>,
    finished: bool,
}

impl Killer {
    /// Starts the kill for `reason` unless one is under way.
    fn stop(&mut self, reason: Stop, pid: i32, reaped: bool) {
        if self.reason.is_some() {
            return;
        }
        self.reason = Some(reason);
        self.terminated_at = Some(Instant::now());
        signal_group(pid, SIGTERM, reaped);
    }

    /// Moves a kill under way along: `SIGKILL` once the grace has passed while anything is
    /// left, done once nothing is.
    fn advance(&mut self, pid: i32, reaped: bool) {
        let Some(terminated_at) = self.terminated_at else {
            return;
        };
        if self.finished {
            return;
        }
        let group_alive = send(-pid, 0).is_ok();
        if !group_alive && reaped {
            self.finished = true;
        } else if terminated_at.elapsed() >= KILL_GRACE {
            signal_group(pid, SIGKILL, reaped);
            self.finished = true;
        }
    }

    /// Whether the run may return: no kill started, or the started one is done.
    fn settled(&self) -> bool {
        self.terminated_at.is_none() || self.finished
    }
}

/// Signals the group. The group outlives its leader while a grandchild is in it, and a group
/// id is not reused while the group exists, so a signal sent after the leader exited reaches
/// only what it left behind. A leader that moved itself into another group leaves its own
/// empty, so `ESRCH` falls back to the pid while it is unreaped (its pid is still ours).
fn signal_group(pid: i32, signal: i32, reaped: bool) {
    if let Err(ESRCH) = send(-pid, signal)
        && !reaped
    {
        let _ = send(pid, signal);
    }
}

struct Run {
    child: Child,
    collector: Arc<LineCollector>,
    batches: Receiver<Vec<String>>,
    killer: Killer,
    exit: Option<ExitStatus>,
    wait_error: Option<String>,
}

impl Run {
    fn pid(&self) -> i32 {
        i32::try_from(self.child.id()).unwrap_or(i32::MAX)
    }

    /// Ticks until the leader is reaped and any kill is done, forwarding output and stopping
    /// the group on a limit or a cancel.
    fn supervise(
        &mut self,
        command: &CommandSpec,
        cancel: &CancelToken,
        on_output: Option<OnOutput<'_>>,
    ) {
        let started = Instant::now();
        loop {
            self.reap(command);
            let reaped = self.exit.is_some() || self.wait_error.is_some();
            if !reaped && let Some(reason) = due_stop(command, cancel, &self.collector, started) {
                self.killer.stop(reason, self.pid(), reaped);
            }
            self.killer.advance(self.pid(), reaped);
            if reaped && self.killer.settled() {
                return;
            }
            if let Ok(lines) = self.batches.recv_timeout(TICK)
                && let Some(on_output) = on_output
            {
                on_output(&lines);
            }
        }
    }

    fn reap(&mut self, command: &CommandSpec) {
        if self.exit.is_some() || self.wait_error.is_some() {
            return;
        }
        match self.child.try_wait() {
            Ok(status) => self.exit = status,
            Err(error) => {
                self.wait_error = Some(format!("cannot wait for {}: {error}", command.executable));
            }
        }
    }

    fn outcome(self, on_output: Option<OnOutput<'_>>) -> Outcome {
        let (trailing, mut lines) = self.collector.finish();
        if let (Some(trailing), Some(on_output)) = (trailing, on_output) {
            on_output(&[trailing]);
        }
        let (status, termination) = match self.exit {
            Some(exit) => match (exit.code(), exit.signal()) {
                (Some(code), _) => (code, Termination::Exit),
                (None, Some(signal)) => (signal, Termination::Signal),
                (None, None) => (-1, Termination::Exit),
            },
            None => (-1, Termination::Exit),
        };
        if let Some(error) = self.wait_error {
            lines.push(error);
        }
        Outcome {
            status,
            termination,
            lines,
            stopped: self.killer.reason,
        }
    }
}

/// The limit a running command has hit, if any: a cancel first, then the wall clock, then the
/// idle clock.
fn due_stop(
    command: &CommandSpec,
    cancel: &CancelToken,
    collector: &LineCollector,
    started: Instant,
) -> Option<Stop> {
    if cancel.is_signalled() {
        return Some(Stop::Cancelled);
    }
    if command
        .timeout
        .is_some_and(|limit| started.elapsed() >= limit)
    {
        return Some(Stop::TimedOut);
    }
    if command
        .idle_timeout
        .is_some_and(|limit| collector.last_activity().elapsed() >= limit)
    {
        return Some(Stop::Idle);
    }
    None
}

#[cfg(test)]
mod tests;

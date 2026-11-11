//! Port of the pure half of `Shell.swift`: what a bounded command run is asked to do, what it
//! reports, and the line splitter. The runner that spawns lives in `rc-git-mac`; tests and the
//! updater model's tests use [`ScriptedRunner`].
//!
//! The contract every runner keeps: merged stdout and stderr streamed one batch of lines at a
//! time; a wall-clock limit, an idle limit and a cancel that kill the whole process group
//! (`SIGTERM`, then `SIGKILL` after a grace); and a bounded wait for the last bytes. Nothing
//! waits forever.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::guard_bridge::CancelToken;

/// How long the last bytes get to arrive after the process exits. A grandchild that inherited
/// the pipe keeps it open, so this wait is bounded like every other.
pub const DRAIN_GRACE: Duration = Duration::from_secs(2);
/// How long a killed process group gets to die before `SIGKILL`.
pub const KILL_GRACE: Duration = Duration::from_secs(2);

/// One command to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// The executable's absolute path.
    pub executable: String,
    /// Its arguments.
    pub arguments: Vec<String>,
    /// The working directory.
    pub directory: String,
    /// Kill the command once it has run this long.
    pub timeout: Option<Duration>,
    /// Kill the command once it has printed nothing for this long.
    pub idle_timeout: Option<Duration>,
}

impl CommandSpec {
    /// `executable` with `arguments` in `directory`, with no limits.
    #[must_use]
    pub fn new(executable: &str, arguments: &[&str], directory: &str) -> Self {
        Self {
            executable: executable.to_owned(),
            arguments: arguments.iter().map(|arg| (*arg).to_owned()).collect(),
            directory: directory.to_owned(),
            timeout: None,
            idle_timeout: None,
        }
    }
}

/// Why a command was stopped before it exited on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stop {
    /// It ran past its wall-clock limit.
    TimedOut,
    /// It printed nothing for its idle limit.
    Idle,
    /// The caller cancelled it.
    Cancelled,
}

/// Whether the process exited or a signal ended it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Termination {
    /// It exited; `status` is the exit status (or -1 when it never started).
    Exit,
    /// A signal ended it; `status` is the signal number.
    Signal,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The exit status, or the signal number when `termination` is [`Termination::Signal`].
    pub status: i32,
    /// Whether the process exited or a signal ended it.
    pub termination: Termination,
    /// Every line printed on stdout and stderr, in arrival order.
    pub lines: Vec<String>,
    /// Why the command was killed; `None` when it exited on its own.
    pub stopped: Option<Stop>,
}

impl Outcome {
    /// Whether the process exited on its own with status 0.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.termination == Termination::Exit && self.status == 0
    }

    /// How a run that did not succeed ended, worded to follow the command's name: "failed
    /// (status 3)" or "was killed by signal 9 (SIGKILL)". A signal number reads like an exit
    /// status, so a build the system killed would otherwise look like one that failed by
    /// itself.
    #[must_use]
    pub fn failure(&self) -> String {
        match self.termination {
            Termination::Exit => format!("failed (status {})", self.status),
            Termination::Signal => format!(
                "was killed by signal {} ({})",
                self.status,
                signal_name(self.status)
            ),
        }
    }

    /// The outcome of a command that never started: status -1 and one line saying why.
    #[must_use]
    pub fn not_started(line: String) -> Self {
        Self {
            status: -1,
            termination: Termination::Exit,
            lines: vec![line],
            stopped: None,
        }
    }
}

/// The conventional name of a signal number on macOS, `SIGKILL` for 9; `SIG<n>` otherwise.
#[must_use]
pub fn signal_name(number: i32) -> String {
    let name = match number {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        10 => "SIGBUS",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        _ => return format!("SIG{number}"),
    };
    name.to_owned()
}

/// What a run that could not start blames. A missing working directory fails the start with
/// the same error as a missing executable, so it is named first.
#[must_use]
pub fn start_failure(executable: &str, directory: &str, error: &str) -> String {
    if std::path::Path::new(directory).is_dir() {
        format!("cannot run {executable}: {error}")
    } else {
        format!("working directory {directory} does not exist: {error}")
    }
}

/// Where a run's output batches go.
pub type OnOutput<'a> = &'a (dyn Fn(&[String]) + Sync);

/// Runs commands under the bounds in the module docs.
pub trait ProcessRunner: Send + Sync {
    /// Runs `command` to the end, blocking the calling thread. A command still running past
    /// its limits, or when `cancel` is signalled, is killed with its process group, and the
    /// outcome says which. `on_output` gets the complete lines of each read as one batch.
    /// Never fails: a command that cannot start is an [`Outcome`] with status -1 and a line
    /// naming the cause.
    fn run(
        &self,
        command: &CommandSpec,
        cancel: &CancelToken,
        on_output: Option<OnOutput<'_>>,
    ) -> Outcome;
}

/// Splits streamed bytes into lines. Buffers bytes, not text, so a multi-byte character split
/// across two reads survives. Shared between the reading thread and the watchdog.
pub struct LineCollector {
    state: Mutex<Collected>,
}

struct Collected {
    buffer: Vec<u8>,
    lines: Vec<String>,
    last_read: Instant,
}

impl Default for LineCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for LineCollector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("LineCollector")
            .field("lines", &state.lines.len())
            .field("buffered", &state.buffer.len())
            .finish()
    }
}

impl LineCollector {
    /// An empty collector whose idle clock starts now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(Collected {
                buffer: Vec::new(),
                lines: Vec::new(),
                last_read: Instant::now(),
            }),
        }
    }

    /// Takes the bytes of one read; returns the lines it completed, for `on_output`.
    pub fn take(&self, data: &[u8]) -> Vec<String> {
        if data.is_empty() {
            return Vec::new();
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.last_read = Instant::now();
        state.buffer.extend_from_slice(data);
        let mut complete = Vec::new();
        while let Some(newline) = state.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = state.buffer.drain(..=newline).collect();
            complete.push(String::from_utf8_lossy(&line[..newline]).into_owned());
        }
        state.lines.extend(complete.iter().cloned());
        complete
    }

    /// When the last bytes arrived, or when the collector was made if none have.
    #[must_use]
    pub fn last_activity(&self) -> Instant {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last_read
    }

    /// Flushes a last line without a newline; returns it (for `on_output`) and every line.
    pub fn finish(&self) -> (Option<String>, Vec<String>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut trailing = None;
        if !state.buffer.is_empty() {
            let line = String::from_utf8_lossy(&state.buffer).into_owned();
            state.buffer.clear();
            state.lines.push(line.clone());
            trailing = Some(line);
        }
        (trailing, state.lines.clone())
    }
}

/// A [`ProcessRunner`] that runs nothing: it records each command and answers with the next
/// queued outcome, handing its lines to `on_output`. With nothing queued it answers as a
/// command that could not start.
#[derive(Debug, Default)]
pub struct ScriptedRunner {
    state: Mutex<Scripted>,
}

#[derive(Debug, Default)]
struct Scripted {
    outcomes: VecDeque<Outcome>,
    commands: Vec<CommandSpec>,
}

impl ScriptedRunner {
    /// A runner with nothing queued.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues the answer to the next run.
    pub fn push(&self, outcome: Outcome) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .outcomes
            .push_back(outcome);
    }

    /// Every command run so far, in order.
    #[must_use]
    pub fn commands(&self) -> Vec<CommandSpec> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .commands
            .clone()
    }
}

impl ProcessRunner for ScriptedRunner {
    fn run(
        &self,
        command: &CommandSpec,
        cancel: &CancelToken,
        on_output: Option<OnOutput<'_>>,
    ) -> Outcome {
        let next = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.commands.push(command.clone());
            state.outcomes.pop_front()
        };
        let mut outcome = next.unwrap_or_else(|| {
            Outcome::not_started(format!(
                "cannot run {}: nothing scripted",
                command.executable
            ))
        });
        if cancel.is_signalled() && outcome.stopped.is_none() {
            outcome.stopped = Some(Stop::Cancelled);
        }
        if let Some(on_output) = on_output
            && !outcome.lines.is_empty()
        {
            on_output(&outcome.lines);
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_split_across_two_reads_survives() {
        let collector = LineCollector::new();
        let bytes = "é".as_bytes();
        assert_eq!(bytes.len(), 2);
        assert!(collector.take(&bytes[..1]).is_empty());
        assert!(collector.take(&bytes[1..]).is_empty());
        assert_eq!(collector.take(b"\n"), ["é"]);
        assert_eq!(collector.finish(), (None, vec!["é".to_owned()]));
    }

    #[test]
    fn one_read_completes_several_lines_and_keeps_the_tail() {
        let collector = LineCollector::new();
        assert_eq!(collector.take(b"a\nb\nta"), ["a", "b"]);
        assert_eq!(collector.take(b""), Vec::<String>::new());
        let (trailing, lines) = collector.finish();
        assert_eq!(trailing.as_deref(), Some("ta"));
        assert_eq!(lines, ["a", "b", "ta"]);
    }

    #[test]
    fn a_read_moves_the_idle_clock() {
        let collector = LineCollector::new();
        let before = collector.last_activity();
        std::thread::sleep(Duration::from_millis(5));
        collector.take(b"x");
        assert!(collector.last_activity() > before);
    }

    #[test]
    fn failures_are_worded_by_how_the_run_ended() {
        let exited = Outcome {
            status: 3,
            termination: Termination::Exit,
            lines: Vec::new(),
            stopped: None,
        };
        assert!(!exited.succeeded());
        assert_eq!(exited.failure(), "failed (status 3)");
        let killed = Outcome {
            status: 9,
            termination: Termination::Signal,
            ..exited.clone()
        };
        assert!(!killed.succeeded());
        assert_eq!(killed.failure(), "was killed by signal 9 (SIGKILL)");
        let fine = Outcome {
            status: 0,
            ..exited
        };
        assert!(fine.succeeded());
    }

    #[test]
    fn an_unlisted_signal_is_named_by_its_number() {
        assert_eq!(signal_name(15), "SIGTERM");
        assert_eq!(signal_name(31), "SIG31");
    }

    #[test]
    fn a_start_failure_blames_a_missing_directory_first() {
        let line = start_failure("/bin/sh", "/nonexistent-rc-git-dir", "No such file");
        assert_eq!(
            line,
            "working directory /nonexistent-rc-git-dir does not exist: No such file"
        );
        let line = start_failure("/nope/sh", "/", "No such file");
        assert_eq!(line, "cannot run /nope/sh: No such file");
    }

    #[test]
    fn the_scripted_runner_records_and_answers_in_order() {
        let runner = ScriptedRunner::new();
        runner.push(Outcome {
            status: 0,
            termination: Termination::Exit,
            lines: vec!["built".to_owned()],
            stopped: None,
        });
        let seen = Mutex::new(Vec::new());
        let on_output = |lines: &[String]| {
            seen.lock().unwrap().extend(lines.iter().cloned());
        };
        let command = CommandSpec::new("/bin/sh", &["-c", "true"], "/");
        let first = runner.run(&command, &CancelToken::new(), Some(&on_output));
        assert!(first.succeeded());
        assert_eq!(*seen.lock().unwrap(), ["built"]);
        let second = runner.run(&command, &CancelToken::new(), None);
        assert_eq!(second.status, -1);
        assert_eq!(runner.commands().len(), 2);
    }

    #[test]
    fn the_scripted_runner_reports_a_cancel() {
        let runner = ScriptedRunner::new();
        runner.push(Outcome {
            status: 15,
            termination: Termination::Signal,
            lines: Vec::new(),
            stopped: None,
        });
        let cancel = CancelToken::new();
        cancel.signal();
        let outcome = runner.run(&CommandSpec::new("/bin/sh", &[], "/"), &cancel, None);
        assert_eq!(outcome.stopped, Some(Stop::Cancelled));
    }
}

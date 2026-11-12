//! Opens a folder in another application, the way the Finder's "Open With" does:
//! `/usr/bin/open -a <application> <path>` through the bounded [`ProcessRunner`]. The Swift
//! app opened Terminal through `NSWorkspace.open(_:withApplicationAt:configuration:)` in
//! `MainWindowController.openTerminal(at:)`; `open -a` reaches the same Launch Services call
//! without `AppKit`.

use std::time::Duration;

use rc_git_kit::guard_bridge::CancelToken;
use rc_git_kit::platform::process::{CommandSpec, ProcessRunner};

/// The application the diverged sheet opens a checkout in.
pub const TERMINAL: &str = "Terminal";

/// The most `open` may take to hand the folder to Launch Services.
pub const OPEN_LIMIT: Duration = Duration::from_secs(30);

const OPEN: &str = "/usr/bin/open";

/// Opens `path` in `application`. `None` on success, otherwise why not, naming both. Blocks
/// for at most [`OPEN_LIMIT`].
#[must_use]
pub fn open_with(runner: &dyn ProcessRunner, application: &str, path: &str) -> Option<String> {
    let mut command = CommandSpec::new(OPEN, &["-a", application, path], "/");
    command.timeout = Some(OPEN_LIMIT);
    let outcome = runner.run(&command, &CancelToken::new(), None);
    if outcome.stopped.is_none() && outcome.succeeded() {
        return None;
    }
    let reason = match outcome.stopped {
        Some(_) => format!("did not answer in {}s", OPEN_LIMIT.as_secs()),
        None => outcome.failure(),
    };
    let last = outcome
        .lines
        .iter()
        .rev()
        .find(|line| !line.is_empty())
        .map_or("no output", String::as_str);
    Some(format!(
        "cannot open {path} in {application}: open {reason}: {last}"
    ))
}

#[cfg(test)]
mod tests {
    use rc_git_kit::platform::process::{Outcome, ScriptedRunner, Stop, Termination};

    use super::*;

    fn exited(status: i32, lines: &[&str]) -> Outcome {
        Outcome {
            status,
            termination: Termination::Exit,
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            stopped: None,
        }
    }

    #[test]
    fn opens_the_folder_in_the_named_application() {
        let runner = ScriptedRunner::new();
        runner.push(exited(0, &[]));
        assert_eq!(open_with(&runner, TERMINAL, "/repo dir"), None);
        let commands = runner.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].executable, "/usr/bin/open");
        assert_eq!(commands[0].arguments, ["-a", "Terminal", "/repo dir"]);
        assert_eq!(commands[0].timeout, Some(OPEN_LIMIT));
    }

    #[test]
    fn a_failure_names_the_folder_the_application_and_the_reason() {
        let runner = ScriptedRunner::new();
        runner.push(exited(
            1,
            &["Unable to find application named 'Terminal'", ""],
        ));
        assert_eq!(
            open_with(&runner, TERMINAL, "/repo").as_deref(),
            Some(
                "cannot open /repo in Terminal: open failed (status 1): Unable to find \
                 application named 'Terminal'"
            )
        );
    }

    #[test]
    fn a_hung_open_is_reported_as_stopped() {
        let runner = ScriptedRunner::new();
        let mut outcome = exited(0, &[]);
        outcome.stopped = Some(Stop::TimedOut);
        runner.push(outcome);
        assert_eq!(
            open_with(&runner, TERMINAL, "/repo").as_deref(),
            Some("cannot open /repo in Terminal: open did not answer in 30s: no output")
        );
    }

    #[test]
    fn an_open_that_cannot_start_is_a_failure() {
        let runner = ScriptedRunner::new();
        let message = open_with(&runner, TERMINAL, "/repo");
        assert!(
            message
                .as_deref()
                .is_some_and(|message| message.contains("nothing scripted")),
            "{message:?}"
        );
    }
}

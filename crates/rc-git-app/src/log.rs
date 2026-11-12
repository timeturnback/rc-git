//! The app's one log sink: a failure no window shows (a settings write, a background read the
//! model reports, an open that has no window to alert in) is written here with what failed.

/// Logs `message` as an error.
#[expect(
    clippy::print_stderr,
    reason = "the app's one log sink; stderr reaches Console and the terminal that launched it"
)]
pub fn error(message: &str) {
    eprintln!("rc-git: error: {message}");
}

/// Logs output worth keeping while it runs (an update's build output), below a notice.
#[expect(
    clippy::print_stderr,
    reason = "the app's one log sink; stderr reaches Console and the terminal that launched it"
)]
pub fn info(message: &str) {
    eprintln!("rc-git: info: {message}");
}

/// Logs a step or state transition.
#[expect(
    clippy::print_stderr,
    reason = "the app's one log sink; stderr reaches Console and the terminal that launched it"
)]
pub fn notice(message: &str) {
    eprintln!("rc-git: {message}");
}

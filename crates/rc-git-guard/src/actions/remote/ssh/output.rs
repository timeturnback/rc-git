//! The exec'd git service's output as the transport reads it. A russh channel delivers
//! stdout, stderr and the exit status on one queue, so a single reader takes all three:
//! stdout goes to the caller, the tail of stderr and the exit status are kept, and a
//! command that ends badly turns its stderr into the read error. That text is what says
//! why a server refused ("ERROR: Repository not found."); without it the caller only
//! sees the git stream end early.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::time::Duration;

use russh::{ChannelMsg, ChannelReadHalf};

/// How much of the server's stderr is kept: the end, where the reason sits.
const STDERR_TAIL: usize = 4096;
/// The extended-data type code SSH uses for stderr.
const STDERR_EXT: u32 = 1;

/// The git command on the server ended badly and said why on stderr. It travels inside
/// the [`io::Error`] that ends the stream, through gix's wrapping, so the remote layer can
/// lift it back out and show the server's words instead of gix's generic I/O message.
#[derive(Clone, Debug, thiserror::Error)]
#[error("the server said: {message} ({})", exit_status_text(*.exit_status))]
pub(in crate::actions::remote) struct ServerRefusal {
    /// The server's stderr, cleaned to one line of plain text by [`server_message`].
    message: String,
    exit_status: Option<u32>,
}

impl ServerRefusal {
    /// The refusal carried by `error` or anywhere in its source chain, if there is one.
    pub(in crate::actions::remote) fn find<'a>(
        error: &'a (dyn std::error::Error + 'static),
    ) -> Option<&'a Self> {
        let mut current = Some(error);
        while let Some(err) = current {
            let carried = err
                .downcast_ref::<io::Error>()
                .and_then(io::Error::get_ref)
                .and_then(|inner| inner.downcast_ref::<Self>());
            if carried.is_some() {
                return carried;
            }
            current = err.source();
        }
        None
    }
}

fn exit_status_text(exit_status: Option<u32>) -> String {
    exit_status.map_or_else(
        || "no exit status".to_owned(),
        |code| format!("exit status {code}"),
    )
}

/// Reads stdout off an exec'd channel, keeping the last [`STDERR_TAIL`] bytes of stderr
/// and the exit status for the end of the stream.
pub(super) struct ChannelReader {
    half: ChannelReadHalf,
    /// Stdout bytes received but not yet handed to the caller.
    pending: VecDeque<u8>,
    stderr: Vec<u8>,
    exit_status: Option<u32>,
    /// Set once the end of the stream has been reported; every later read returns 0.
    ended: bool,
    /// How long to wait after the server's EOF for its exit status and close.
    shutdown: Duration,
}

impl ChannelReader {
    pub(super) fn new(half: ChannelReadHalf, shutdown: Duration) -> Self {
        Self {
            half,
            pending: VecDeque::new(),
            stderr: Vec::new(),
            exit_status: None,
            ended: false,
            shutdown,
        }
    }

    /// Reads the next stdout bytes into `buf`, returning 0 at the end of the stream.
    ///
    /// # Errors
    /// An [`io::ErrorKind::Other`] error carrying a [`ServerRefusal`] when the command
    /// ended with a non-zero or missing exit status and wrote to stderr.
    pub(super) async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if !self.pending.is_empty() {
                return self.pending.read(buf);
            }
            if self.ended {
                return Ok(0);
            }
            match self.half.wait().await {
                Some(ChannelMsg::Data { data }) => self.pending.extend(data.iter()),
                Some(ChannelMsg::Eof) => {
                    self.await_close().await;
                    return self.finish();
                }
                Some(ChannelMsg::Close) | None => return self.finish(),
                Some(other) => self.record(other),
            }
        }
    }

    /// Keeps the stderr and exit status a message carries; drops everything else.
    fn record(&mut self, message: ChannelMsg) {
        match message {
            ChannelMsg::ExtendedData { data, ext } if ext == STDERR_EXT => {
                self.stderr.extend_from_slice(&data);
                let excess = self.stderr.len().saturating_sub(STDERR_TAIL);
                self.stderr.drain(..excess);
            }
            ChannelMsg::ExitStatus { exit_status } => self.exit_status = Some(exit_status),
            _ => {}
        }
    }

    /// Servers send the exit status after their EOF, so keep reading until the channel
    /// closes, for at most the shutdown budget.
    async fn await_close(&mut self) {
        let deadline = tokio::time::Instant::now() + self.shutdown;
        loop {
            match tokio::time::timeout_at(deadline, self.half.wait()).await {
                Ok(Some(ChannelMsg::Close) | None) | Err(_) => return,
                Ok(Some(message)) => self.record(message),
            }
        }
    }

    /// Ends the stream: clean when the command succeeded or said nothing, otherwise an
    /// error carrying what it wrote to stderr.
    fn finish(&mut self) -> io::Result<usize> {
        self.ended = true;
        if self.exit_status == Some(0) {
            return Ok(0);
        }
        let Some(message) = server_message(&self.stderr) else {
            return Ok(0);
        };
        Err(io::Error::other(ServerRefusal {
            message,
            exit_status: self.exit_status,
        }))
    }
}

/// The server's stderr as one line of plain text, or `None` when it holds nothing
/// printable. The server controls these bytes, so ANSI escape sequences and every
/// control character are removed and whitespace runs collapse to one space.
fn server_message(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            skip_escape(&mut chars);
        } else if c.is_control() {
            plain.push(' ');
        } else {
            plain.push(c);
        }
    }
    let message = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    (!message.is_empty()).then_some(message)
}

/// Consumes the rest of an escape sequence whose ESC was just read: a CSI sequence
/// (`ESC [`, parameters, final byte `@`..`~`), or intermediates (space..`/`) and the
/// one character after them, as in `ESC ( B`.
fn skip_escape(chars: &mut std::str::Chars<'_>) {
    let mut next = chars.next();
    if next == Some('[') {
        for c in chars.by_ref() {
            if ('@'..='~').contains(&c) {
                return;
            }
        }
        return;
    }
    while next.is_some_and(|c| (' '..='/').contains(&c)) {
        next = chars.next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_message_is_kept_on_one_line() {
        assert_eq!(
            server_message(b"ERROR: Repository not found.\n").as_deref(),
            Some("ERROR: Repository not found.")
        );
        assert_eq!(
            server_message(b"line one\r\n\tline two\n").as_deref(),
            Some("line one line two")
        );
    }

    #[test]
    fn escapes_and_control_characters_are_removed() {
        let raw = b"\x1b[1;31mERROR:\x1b[0m denied\x07\x1b(B now\x00";
        assert_eq!(server_message(raw).as_deref(), Some("ERROR: denied now"));
    }

    #[test]
    fn a_refusal_is_found_through_the_io_error_that_carries_it() {
        let refusal = ServerRefusal {
            message: "ERROR: Repository not found.".to_owned(),
            exit_status: Some(1),
        };
        let carried = io::Error::other(refusal);
        let found = ServerRefusal::find(&carried).expect("found");
        assert_eq!(
            found.to_string(),
            "the server said: ERROR: Repository not found. (exit status 1)"
        );
        let silent = ServerRefusal {
            message: "fatal: hung up".to_owned(),
            exit_status: None,
        };
        assert_eq!(
            silent.to_string(),
            "the server said: fatal: hung up (no exit status)"
        );
        assert!(ServerRefusal::find(&io::Error::other("unrelated")).is_none());
    }

    #[test]
    fn nothing_printable_is_no_message() {
        assert_eq!(server_message(b""), None);
        assert_eq!(server_message(b" \n\x1b[0m\r\n"), None);
    }
}

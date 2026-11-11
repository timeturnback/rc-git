//! Port of `Highlighting.swift` minus its UTF-16 mapper: syntax spans for a file's bytes, by
//! the language its name implies. Pure and repository-free: bytes in, byte spans out, which is
//! what GPUI styles text by. `HighlightMapper` has no successor.
//!
//! Failures are data, not log lines: a caller shows plain text for any [`Plain`] and logs a
//! [`Plain::Failed`] at its boundary, once per message through [`is_first_failure`], so a
//! grammar or query that stopped loading after a dependency bump stays visible without a log
//! crate here.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock, PoisonError};

use rc_git_highlight::{Language, Span};

use crate::guard_bridge::CancelToken;

/// Cap on what gets parsed: highlighting is decoration, and a parse of tens of megabytes would
/// fight the viewer for memory. Above this, files stay plain.
pub const BYTE_LIMIT: usize = 2 * 1024 * 1024;

/// Why a file stays plain text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Plain {
    /// The name maps to no known language.
    UnknownLanguage,
    /// The input is over [`BYTE_LIMIT`].
    TooLarge,
    /// The caller's cancel token stopped the parse; nothing failed worth logging.
    Cancelled,
    /// The grammar failed to load or the parse failed; the message says which and why.
    Failed(String),
}

/// Spans for `data`, read as the language the file name `name` implies, ordered by start and
/// never overlapping. A file named like a fence label, such as `rust`, stays plain.
/// Signalling `cancel` from another thread stops the parse.
///
/// # Errors
/// Why the file stays plain: an unknown language, input over [`BYTE_LIMIT`], a cancel, or a
/// failed grammar or parse.
pub fn highlight(
    name: &str,
    data: &[u8],
    cancel: Option<&CancelToken>,
) -> Result<Vec<Span>, Plain> {
    let language = Language::from_file_name(name).ok_or(Plain::UnknownLanguage)?;
    run(language, data, cancel)
}

/// Spans for `data`, read as the language a markdown fence's info string `fence` names, such
/// as `rust`.
///
/// # Errors
/// As [`highlight`].
pub fn highlight_fence(fence: &str, data: &[u8]) -> Result<Vec<Span>, Plain> {
    let language = Language::from_fence(fence).ok_or(Plain::UnknownLanguage)?;
    run(language, data, None)
}

fn run(language: Language, data: &[u8], cancel: Option<&CancelToken>) -> Result<Vec<Span>, Plain> {
    if data.len() > BYTE_LIMIT {
        return Err(Plain::TooLarge);
    }
    rc_git_highlight::highlight(data, language, cancel.map(CancelToken::highlight)).map_err(
        |error| {
            if cancel.is_some_and(CancelToken::is_signalled) {
                Plain::Cancelled
            } else {
                Plain::Failed(error.to_string())
            }
        },
    )
}

/// The language the highlighter reads a file as by its extension, such as `markdown` for
/// `README.md`; `None` when the extension names none. The same lookup [`highlight`] uses, so a
/// file highlighted as a language is the file every other surface treats as one.
#[must_use]
pub fn language_for_file_name(name: &str) -> Option<&'static str> {
    Language::from_file_name(name).map(Language::name)
}

/// Whether `message` is seen here for the first time in this process; records it when it is.
/// The host logs a failure only when this says so.
pub fn is_first_failure(message: &str) -> bool {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Mutex::default);
    let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
    seen.insert(message.to_owned())
}

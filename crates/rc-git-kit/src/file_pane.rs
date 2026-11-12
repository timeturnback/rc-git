//! What the file pane says for each [`FileState`] (`FileViewController.render`): the header's
//! name and detail, the placeholder in place of the text, and whether "Load anyway" is offered.
//! The view renders these and decides none of them.

use crate::browser::FileState;
use crate::diff_text::too_large_placeholder;
use crate::markdown::MarkdownDocument;
use crate::text_document::{FileReveal, can_force_load, format_bytes};

/// What the pane shows with no file open.
pub const NO_FILE_TEXT: &str = "Select a file";
/// The header detail while a file loads.
pub const LOADING_DETAIL: &str = "Loading\u{2026}";

/// The header over the text: the file's name and a quieter detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// The file name.
    pub title: String,
    /// Line count and size, the size alone, "Loading…", or nothing.
    pub detail: String,
}

/// The header for `state`, or `None` with no file: an empty strip over "Select a file" reads
/// as a missing title.
#[must_use]
pub fn header(state: &FileState) -> Option<Header> {
    let (entry, detail) = match state {
        FileState::Empty => return None,
        FileState::Loading(entry) => (entry, LOADING_DETAIL.to_owned()),
        FileState::Text(entry, document) => {
            let size = u64::try_from(document.byte_count).unwrap_or(u64::MAX);
            let mut detail = format!(
                "{} lines \u{b7} {}",
                document.line_count,
                format_bytes(size)
            );
            if document.is_lossy {
                detail.push_str(" \u{b7} not valid UTF-8");
            }
            (entry, detail)
        }
        FileState::Binary(entry, size) | FileState::TooLarge { entry, size, .. } => {
            (entry, format_bytes(*size))
        }
        FileState::Failed(entry, _) => (entry, String::new()),
    };
    Some(Header {
        title: entry.name_lossy().into_owned(),
        detail,
    })
}

/// The text in place of the file's contents, or `None` while text is on screen or loading.
#[must_use]
pub fn placeholder(state: &FileState) -> Option<String> {
    match state {
        FileState::Empty => Some(NO_FILE_TEXT.to_owned()),
        FileState::Loading(_) | FileState::Text(..) => None,
        FileState::Binary(entry, size) => Some(format!(
            "{} is a binary file ({}).",
            entry.name_lossy(),
            format_bytes(*size)
        )),
        FileState::TooLarge { entry, size, limit } => {
            Some(too_large_placeholder(&entry.name_lossy(), *size, *limit))
        }
        FileState::Failed(_, message) => Some(message.clone()),
    }
}

/// The size "Load anyway" re-reads with, when the pane offers it: a file over its limit that
/// is still under the hard limit.
#[must_use]
pub fn load_anyway_size(state: &FileState) -> Option<u64> {
    match state {
        FileState::TooLarge { size, .. } if can_force_load(*size) => Some(*size),
        FileState::Empty
        | FileState::Loading(_)
        | FileState::Text(..)
        | FileState::Binary(..)
        | FileState::TooLarge { .. }
        | FileState::Failed(..) => None,
    }
}

/// Whether the text of `name`, `byte_count` bytes long, paints as the markdown preview: the
/// file offers one, the reader prefers it, and the place the pane is about to reveal does not
/// need the source (a content-search match is selected in the text the preview hides).
#[must_use]
pub fn shows_preview(
    name: &str,
    byte_count: usize,
    prefers_preview: bool,
    pending: Option<&FileReveal>,
) -> bool {
    MarkdownDocument::is_previewable(name, byte_count)
        && prefers_preview
        && !pending.is_some_and(FileReveal::needs_source)
}

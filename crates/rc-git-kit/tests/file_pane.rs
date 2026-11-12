//! What the file pane says per state.

use std::sync::Arc;

use rc_git_core::EntryKind;
use rc_git_kit::browser::{FileEntry, FileState};
use rc_git_kit::file_pane::{
    LOADING_DETAIL, NO_FILE_TEXT, header, load_anyway_size, placeholder, shows_preview,
};
use rc_git_kit::markdown::MarkdownDocument;
use rc_git_kit::text_document::{FileReveal, HARD_LIMIT, TextDocument};

fn entry() -> FileEntry {
    FileEntry {
        name: "notes.txt".into(),
        oid: None,
        kind: EntryKind::Blob,
    }
}

fn text(bytes: &[u8], valid: bool) -> FileState {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(bytes.iter().enumerate().filter_map(|(index, byte)| {
            (*byte == b'\n' && index + 1 < bytes.len()).then_some(index + 1)
        }))
        .collect();
    FileState::Text(
        entry(),
        Arc::new(TextDocument::new(bytes.to_vec(), &starts, valid)),
    )
}

#[test]
fn no_file_has_no_header_and_asks_for_one() {
    assert_eq!(header(&FileState::Empty), None);
    assert_eq!(
        placeholder(&FileState::Empty).as_deref(),
        Some(NO_FILE_TEXT)
    );
}

#[test]
fn a_loading_file_names_itself_and_shows_no_placeholder() {
    let state = FileState::Loading(entry());
    let header = header(&state).unwrap_or_else(|| panic!("a header"));
    assert_eq!(header.title, "notes.txt");
    assert_eq!(header.detail, LOADING_DETAIL);
    assert_eq!(placeholder(&state), None);
}

#[test]
fn text_counts_lines_and_bytes_and_flags_lossy_text() {
    let header_of = |state: &FileState| header(state).map(|header| header.detail);
    assert_eq!(
        header_of(&text(b"a\nb\n", true)).as_deref(),
        Some("2 lines \u{b7} 4 B")
    );
    assert_eq!(
        header_of(&text(b"a\xff\n", false)).as_deref(),
        Some("1 lines \u{b7} 3 B \u{b7} not valid UTF-8")
    );
}

#[test]
fn binary_and_failed_files_say_why_there_is_no_text() {
    let binary = FileState::Binary(entry(), 2048);
    assert_eq!(
        placeholder(&binary).as_deref(),
        Some("notes.txt is a binary file (2.0 KB).")
    );
    let failed = FileState::Failed(entry(), "read blob: missing".to_owned());
    assert_eq!(placeholder(&failed).as_deref(), Some("read blob: missing"));
    assert_eq!(
        header(&failed).map(|header| header.detail),
        Some(String::new())
    );
}

#[test]
fn load_anyway_is_offered_only_under_the_hard_limit() {
    let over = FileState::TooLarge {
        entry: entry(),
        size: 10_000_000,
        limit: 5_000_000,
    };
    assert_eq!(load_anyway_size(&over), Some(10_000_000));
    assert!(placeholder(&over).is_some());
    let huge = FileState::TooLarge {
        entry: entry(),
        size: HARD_LIMIT + 1,
        limit: 5_000_000,
    };
    assert_eq!(load_anyway_size(&huge), None);
    let at_limit = FileState::TooLarge {
        entry: entry(),
        size: HARD_LIMIT,
        limit: 5_000_000,
    };
    assert_eq!(load_anyway_size(&at_limit), Some(HARD_LIMIT));
    assert_eq!(load_anyway_size(&FileState::Empty), None);
}

#[test]
fn markdown_previews_unless_the_reader_or_a_match_wants_the_source() {
    let fragment = FileReveal::Fragment("usage".to_owned());
    let found = FileReveal::Match {
        line: 3,
        byte_column: 0,
        byte_length: 2,
    };
    assert!(shows_preview("README.md", 10, true, None));
    assert!(shows_preview("README.md", 10, true, Some(&fragment)));
    assert!(!shows_preview("README.md", 10, true, Some(&found)));
    assert!(!shows_preview("README.md", 10, false, None));
    assert!(!shows_preview("notes.txt", 10, true, None));
    assert!(!shows_preview(
        "README.md",
        MarkdownDocument::PREVIEW_LIMIT + 1,
        true,
        None
    ));
}

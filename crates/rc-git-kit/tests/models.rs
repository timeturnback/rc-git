//! The value helpers left over from `Models.swift`, `WorktreeModels.swift` and
//! `DiffModels.swift`. `onlyAPlainNewlineGoesUnmarked` is ported from
//! `RepositoryIntegrationTests.swift`.

use rc_git_core::{ChangeKind, EntryKind, LineEnd, TreeChange};
use rc_git_kit::fake::oid;
use rc_git_kit::models::{
    branch_refusal, line_end_marker, new_blob, old_blob, path_refusal, submodule_summary,
};

fn change(entry: EntryKind, old_entry: Option<EntryKind>) -> TreeChange {
    TreeChange {
        kind: ChangeKind::Modified,
        path: "sub".into(),
        source_path: None,
        old: Some(oid(1)),
        new: Some(oid(2)),
        entry,
        old_entry,
    }
}

#[test]
fn only_a_plain_newline_goes_unmarked() {
    assert_eq!(line_end_marker(LineEnd::Lf), None);
    assert!(line_end_marker(LineEnd::CrLf).unwrap().contains("CRLF"));
    assert!(
        line_end_marker(LineEnd::None)
            .unwrap()
            .contains("no newline at end of file")
    );
}

#[test]
fn a_gitlink_side_is_no_blob() {
    let file = change(EntryKind::Blob, Some(EntryKind::Blob));
    assert_eq!(
        (old_blob(&file), new_blob(&file)),
        (Some(oid(1)), Some(oid(2)))
    );
    let became_submodule = change(EntryKind::Submodule, Some(EntryKind::Blob));
    assert_eq!(
        (old_blob(&became_submodule), new_blob(&became_submodule)),
        (Some(oid(1)), None)
    );
    let was_submodule = change(EntryKind::Blob, Some(EntryKind::Submodule));
    assert_eq!(old_blob(&was_submodule), None);
    let added = TreeChange {
        old: None,
        old_entry: None,
        ..file
    };
    assert_eq!(old_blob(&added), None);
}

#[test]
fn a_submodule_change_reads_as_git_words_it() {
    let both = change(EntryKind::Submodule, Some(EntryKind::Submodule));
    assert_eq!(
        submodule_summary(&both),
        format!("Subproject commit {} \u{2192} {}", oid(1), oid(2))
    );
    let added = TreeChange { old: None, ..both };
    assert!(submodule_summary(&added).starts_with("Subproject commit none \u{2192} "));
}

#[test]
fn refusals_name_the_action_and_the_inexact_name() {
    assert_eq!(
        path_refusal("blame", "a\u{FFFD}b"),
        "cannot blame 'a\u{FFFD}b': the path is not valid UTF-8"
    );
    assert_eq!(
        branch_refusal("switch to", "ma\u{FFFD}"),
        "cannot switch to 'ma\u{FFFD}': the branch name is not valid UTF-8"
    );
}

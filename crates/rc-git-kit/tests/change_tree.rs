//! Change list rows, flat and grouped in folders, and which files a Discard acts on. Port of
//! `ChangeTreeTests.swift`.

use rc_git_core::bstr::BString;
use rc_git_core::{ChangeKind, EntryKind, StatusEntry, TreeChange, WorktreeStatus};
use rc_git_kit::change_tree::{ChangeGrouping, ChangeNode, discard_paths, nodes};

fn entry(path: &str) -> StatusEntry {
    StatusEntry {
        path: BString::from(path),
        kind: EntryKind::Blob,
        status: WorktreeStatus::Modified,
        oid: None,
        size: 1,
    }
}

fn entries(paths: &[&str]) -> Vec<StatusEntry> {
    paths.iter().map(|path| entry(path)).collect()
}

fn names<E>(rows: &[ChangeNode<E>]) -> Vec<&str> {
    rows.iter().map(|row| row.name.as_str()).collect()
}

fn paths<E>(rows: &[ChangeNode<E>]) -> Vec<&str> {
    rows.iter().map(|row| row.path.as_str()).collect()
}

#[test]
fn flat_keeps_one_row_per_entry_labeled_with_the_whole_path() {
    let rows = nodes(
        &entries(&["a.txt", "mac/Sources/App.swift"]),
        ChangeGrouping::Flat,
    );
    assert_eq!(names(&rows), ["a.txt", "mac/Sources/App.swift"]);
    assert!(
        rows.iter()
            .all(|row| !row.is_folder() && row.children.is_empty())
    );
    assert_eq!(paths(&rows), ["a.txt", "mac/Sources/App.swift"]);
}

#[test]
fn folders_nest_files_and_list_folders_first() {
    let rows = nodes(
        &entries(&[
            "README.md",
            "mac/App.swift",
            "mac/Kit.swift",
            "mac/tests/KitTests.swift",
        ]),
        ChangeGrouping::Folders,
    );
    assert_eq!(names(&rows), ["mac", "README.md"]);
    let mac = &rows[0];
    assert!(mac.is_folder());
    assert_eq!(mac.path, "mac");
    assert_eq!(names(&mac.children), ["tests", "App.swift", "Kit.swift"]);
    assert_eq!(
        paths(&mac.children[0].children),
        ["mac/tests/KitTests.swift"]
    );
}

#[test]
fn subfolders_sort_in_natural_order() {
    let rows = nodes(
        &entries(&["v10/a", "V2/a", "v1/a", "b/a"]),
        ChangeGrouping::Folders,
    );
    assert_eq!(names(&rows), ["b", "v1", "V2", "v10"]);
}

#[test]
fn a_sole_subfolder_merges_into_one_row() {
    let rows = nodes(
        &entries(&[
            "docs/solutions/ui-bugs/one.md",
            "docs/solutions/ui-bugs/two.md",
        ]),
        ChangeGrouping::Folders,
    );
    assert_eq!(names(&rows), ["docs/solutions/ui-bugs"]);
    assert_eq!(rows[0].path, "docs/solutions/ui-bugs");
    assert_eq!(names(&rows[0].children), ["one.md", "two.md"]);
}

#[test]
fn a_folder_with_its_own_files_does_not_merge() {
    let rows = nodes(
        &entries(&["docs/index.md", "docs/solutions/one.md"]),
        ChangeGrouping::Folders,
    );
    assert_eq!(names(&rows), ["docs"]);
    assert_eq!(names(&rows[0].children), ["solutions", "index.md"]);
}

#[test]
fn file_paths_cover_every_file_beneath_a_row() {
    let rows = nodes(
        &entries(&["mac/App.swift", "mac/tests/KitTests.swift", "a.txt"]),
        ChangeGrouping::Folders,
    );
    assert_eq!(
        rows[0].file_paths(),
        ["mac/tests/KitTests.swift", "mac/App.swift"]
    );
    assert_eq!(rows[1].file_paths(), ["a.txt"]);
}

#[test]
fn a_file_and_a_folder_with_one_name_keep_distinct_keys() {
    let rows = nodes(&entries(&["a", "a/b"]), ChangeGrouping::Folders);
    assert_eq!(rows.len(), 2);
    let folder = rows.iter().find(|row| row.is_folder()).unwrap();
    let file = rows.iter().find(|row| !row.is_folder()).unwrap();
    assert_eq!(folder.path, "a");
    assert_eq!(file.path, "a");
    assert_ne!(folder.key(), file.key());
    assert_eq!(folder.file_paths(), ["a/b"]);
    assert_eq!(file.file_paths(), ["a"]);
}

#[test]
fn empty_report_has_no_rows() {
    assert!(nodes::<StatusEntry>(&[], ChangeGrouping::Flat).is_empty());
    assert!(nodes::<StatusEntry>(&[], ChangeGrouping::Folders).is_empty());
}

#[test]
fn an_empty_path_gets_no_folder_row() {
    assert!(nodes(&entries(&["", "/"]), ChangeGrouping::Folders).is_empty());
}

#[test]
fn tree_changes_group_by_their_path() {
    let change = TreeChange {
        kind: ChangeKind::Renamed,
        path: BString::from("mac/New.swift"),
        source_path: Some(BString::from("mac/Old.swift")),
        old: None,
        new: None,
        entry: EntryKind::Blob,
        old_entry: None,
    };
    let rows = nodes(std::slice::from_ref(&change), ChangeGrouping::Folders);
    assert_eq!(names(&rows), ["mac"]);
    assert_eq!(rows[0].children[0].entry.as_ref(), Some(&change));
}

#[test]
fn the_stored_grouping_round_trips_and_an_unknown_one_is_none() {
    for grouping in ChangeGrouping::ALL {
        assert_eq!(ChangeGrouping::parse(grouping.as_str()), Some(grouping));
    }
    assert_eq!(ChangeGrouping::Flat.as_str(), "flat");
    assert_eq!(ChangeGrouping::Folders.as_str(), "folders");
    assert_eq!(ChangeGrouping::parse("sideways"), None);
    assert_eq!(ChangeGrouping::parse(""), None);
}

fn flat_abc() -> Vec<ChangeNode<StatusEntry>> {
    nodes(&entries(&["a", "b", "c"]), ChangeGrouping::Flat)
}

#[test]
fn discard_takes_the_selection_when_the_clicked_row_is_in_it() {
    let rows = flat_abc();
    let paths = discard_paths(&[&rows[0], &rows[2]], Some(&rows[2]));
    assert_eq!(paths, ["a", "c"]);
}

#[test]
fn discard_takes_only_a_clicked_row_outside_the_selection() {
    let rows = flat_abc();
    let paths = discard_paths(&[&rows[0], &rows[2]], Some(&rows[1]));
    assert_eq!(paths, ["b"]);
}

#[test]
fn discard_with_no_clicked_row_takes_the_selection() {
    let rows = flat_abc();
    let paths = discard_paths(&[&rows[1], &rows[2]], None);
    assert_eq!(paths, ["b", "c"]);
}

#[test]
fn a_folder_row_discards_every_file_beneath_it() {
    let rows = nodes(
        &entries(&["mac/App.swift", "mac/tests/KitTests.swift", "a.txt"]),
        ChangeGrouping::Folders,
    );
    let folder = rows.iter().find(|row| row.is_folder()).unwrap();
    let paths = discard_paths(&[], Some(folder));
    assert_eq!(paths, ["mac/App.swift", "mac/tests/KitTests.swift"]);
}

#[test]
fn a_folder_and_its_child_selected_together_list_each_path_once() {
    let rows = nodes(
        &entries(&["mac/Kit.swift", "mac/App.swift", "a.txt"]),
        ChangeGrouping::Folders,
    );
    let folder = rows.iter().find(|row| row.is_folder()).unwrap();
    let child = folder.children.last().unwrap();
    let file = rows.last().unwrap();
    let paths = discard_paths(&[file, child, folder], Some(child));
    assert_eq!(paths, ["a.txt", "mac/App.swift", "mac/Kit.swift"]);
}

#[test]
fn a_clicked_file_sharing_a_folder_path_is_outside_a_selected_folder() {
    let rows = nodes(&entries(&["a", "a/b"]), ChangeGrouping::Folders);
    let folder = rows.iter().find(|row| row.is_folder()).unwrap();
    let file = rows.iter().find(|row| !row.is_folder()).unwrap();
    assert_eq!(discard_paths(&[folder], Some(file)), ["a"]);
}

#[test]
fn nothing_selected_discards_nothing() {
    assert!(discard_paths::<StatusEntry>(&[], None).is_empty());
}

//! The Files tree's lazy loads, stale-root drops and display-time hidden filter.

use rc_git_core::{EntryKind, TreeEntry};
use rc_git_kit::fake::oid;
use rc_git_kit::file_tree::{
    EMPTY_TEXT, FileTree, LOADING_TEXT, UNREADABLE_TEXT, ViewKind, ViewNode,
};
use rc_git_kit::hidden_files::is_hidden_name;
use rc_git_kit::models::BrowseRow;

fn row(name: &[u8], kind: EntryKind, seed: u8) -> BrowseRow {
    BrowseRow {
        entry: TreeEntry {
            name: name.into(),
            oid: oid(seed),
            kind,
        },
        status: None,
        missing: false,
    }
}

fn everything(_: &BrowseRow) -> bool {
    true
}

fn not_hidden(row: &BrowseRow) -> bool {
    !is_hidden_name(&row.entry.name)
}

fn labels(nodes: &[ViewNode]) -> Vec<String> {
    let mut out = Vec::new();
    for node in nodes {
        out.push(node.label.clone());
        if let ViewKind::Folder {
            expanded: true,
            children,
        } = &node.kind
        {
            for child in labels(children) {
                out.push(format!("  {child}"));
            }
        }
    }
    out
}

fn sample() -> FileTree {
    let mut tree = FileTree::new();
    tree.set_root(vec![
        row(b".git", EntryKind::Tree, 1),
        row(b"src", EntryKind::Tree, 2),
        row(b"README.md", EntryKind::Blob, 3),
    ]);
    tree
}

#[test]
fn a_first_expand_asks_for_one_read_and_a_second_asks_for_none() {
    let mut tree = sample();
    let load = tree.expand("1");
    let load = load.unwrap_or_else(|| panic!("the first expand reads"));
    assert_eq!(load.tree, oid(2));
    assert_eq!(load.path, "src");
    assert_eq!(tree.expand("1"), None);
    assert!(tree.finish(&load, Ok(vec![row(b"main.rs", EntryKind::Blob, 4)])));
    tree.collapse("1");
    assert_eq!(
        tree.expand("1"),
        None,
        "children stay read across a collapse"
    );
    assert_eq!(
        labels(&tree.view(&everything)),
        [".git", "src", "  main.rs", "README.md"]
    );
}

#[test]
fn a_file_never_expands() {
    let mut tree = sample();
    assert_eq!(tree.expand("2"), None);
    assert_eq!(tree.expand("9"), None);
    assert_eq!(tree.expand("x"), None);
}

#[test]
fn a_folder_shows_loading_then_its_children_or_why_not() {
    let mut tree = sample();
    let load = tree.expand("1").unwrap_or_else(|| panic!("reads"));
    assert_eq!(
        labels(&tree.view(&everything))[2],
        format!("  {LOADING_TEXT}")
    );
    tree.finish(&load, Err("read tree: missing".to_owned()));
    assert_eq!(
        labels(&tree.view(&everything))[2],
        format!("  {UNREADABLE_TEXT}")
    );
    let retry = tree.expand("1");
    assert!(
        retry.is_some(),
        "a failed read is retried on the next expand"
    );
}

#[test]
fn a_collapsed_folder_carries_a_placeholder_so_it_can_expand() {
    let tree = sample();
    let view = tree.view(&everything);
    let ViewKind::Folder { expanded, children } = &view[1].kind else {
        panic!("src is a folder");
    };
    assert!(!expanded);
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].kind, ViewKind::Placeholder);
    assert_eq!(children[0].id, "1/-");
}

#[test]
fn hidden_rows_are_filtered_at_display_and_kept() {
    let mut tree = sample();
    assert_eq!(labels(&tree.view(&not_hidden)), ["src", "README.md"]);
    assert_eq!(tree.node_count(), 3, "the hidden row stays read");
    let load = tree.expand("1").unwrap_or_else(|| panic!("reads"));
    tree.finish(&load, Ok(vec![row(b".DS_Store", EntryKind::Blob, 5)]));
    assert_eq!(
        labels(&tree.view(&not_hidden)),
        ["src", format!("  {EMPTY_TEXT}").as_str(), "README.md"]
    );
    assert_eq!(
        labels(&tree.view(&everything)),
        [".git", "src", "  .DS_Store", "README.md"]
    );
    assert_eq!(
        tree.view(&not_hidden)[0].id,
        "1",
        "ids name nodes, not positions"
    );
}

#[test]
fn a_read_for_an_older_root_is_dropped() {
    let mut tree = sample();
    let load = tree.expand("1").unwrap_or_else(|| panic!("reads"));
    tree.set_root(vec![
        row(b"docs", EntryKind::Tree, 8),
        row(b"src", EntryKind::Tree, 9),
    ]);
    tree.expand("1");
    assert!(!tree.finish(&load, Ok(vec![row(b"stale.rs", EntryKind::Blob, 4)])));
    assert_eq!(
        labels(&tree.view(&everything)),
        ["docs", "src", format!("  {LOADING_TEXT}").as_str()]
    );
}

#[test]
fn nested_paths_and_their_exactness_follow_the_parents() {
    let mut tree = sample();
    let load = tree.expand("1").unwrap_or_else(|| panic!("reads"));
    tree.finish(
        &load,
        Ok(vec![
            row(b"lib", EntryKind::Tree, 6),
            row(b"bad\xff.rs", EntryKind::Blob, 7),
        ]),
    );
    let nested = tree.expand("1/0").unwrap_or_else(|| panic!("reads"));
    assert_eq!(nested.path, "src/lib");
    tree.finish(&nested, Ok(vec![row(b"mod.rs", EntryKind::Blob, 8)]));
    let info = tree.info("1/0/0").unwrap_or_else(|| panic!("found"));
    assert_eq!(info.path, "src/lib/mod.rs");
    assert!(info.path_is_exact);
    let lossy = tree.info("1/1").unwrap_or_else(|| panic!("found"));
    assert_eq!(lossy.path, "src/bad\u{fffd}.rs");
    assert!(!lossy.path_is_exact);
}

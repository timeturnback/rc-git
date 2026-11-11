//! Port of `PathSearchTests.swift`: the fuzzy scorer and the path index.
#![expect(
    clippy::unwrap_used,
    reason = "fixture helpers outside #[test] fns unwrap infallible builds"
)]

use std::cell::Cell;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};

use rc_git_core::bstr::BString;
use rc_git_core::{EntryKind, Oid, TreeEntry, WorktreeFile, WorktreeFileList};
use rc_git_kit::path_search::{BuildError, Cancelled, PathIndex, PathIndexEntry, score};

fn oid(byte: u8) -> Oid {
    Oid::from_bytes(&[byte; 20]).unwrap()
}

fn entry(name: &[u8], id: Oid, kind: EntryKind) -> TreeEntry {
    TreeEntry {
        name: BString::from(name),
        oid: id,
        kind,
    }
}

const ROOT: u8 = 1;
const SUB: u8 = 2;
const DEEP: u8 = 3;

fn trees() -> HashMap<Oid, Vec<TreeEntry>> {
    HashMap::from([
        (
            oid(ROOT),
            vec![
                entry(b"src", oid(SUB), EntryKind::Tree),
                entry(b"README.md", oid(0x10), EntryKind::Blob),
                entry(b"vendor", oid(0x60), EntryKind::Submodule),
            ],
        ),
        (
            oid(SUB),
            vec![
                entry(b"nested", oid(DEEP), EntryKind::Tree),
                entry(b"main.rs", oid(0x11), EntryKind::Blob),
                entry(b"run.sh", oid(0x12), EntryKind::ExecutableBlob),
            ],
        ),
        (
            oid(DEEP),
            vec![entry(b"link", oid(0x13), EntryKind::Symlink)],
        ),
    ])
}

fn build(fixed: &HashMap<Oid, Vec<TreeEntry>>, limit: usize) -> PathIndex {
    let cancel = AtomicBool::new(false);
    PathIndex::build(oid(ROOT), limit, &cancel, |tree| {
        Ok::<_, Infallible>(fixed[&tree].clone())
    })
    .unwrap()
}

fn search(index: &PathIndex, query: &str, limit: usize, include_hidden: bool) -> Vec<String> {
    let cancel = AtomicBool::new(false);
    let found = index.search(query, limit, include_hidden, &cancel).unwrap();
    found.iter().map(|entry| entry.path.clone()).collect()
}

fn paths(index: &PathIndex) -> Vec<&str> {
    index
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect()
}

fn flat_index(paths: &[&str]) -> PathIndex {
    let entries = paths
        .iter()
        .map(|path| {
            PathIndexEntry::new(
                (*path).to_owned(),
                Some(oid(0x11)),
                EntryKind::Blob,
                false,
                true,
            )
        })
        .collect();
    PathIndex {
        root: oid(ROOT),
        entries,
        truncated: false,
    }
}

#[test]
fn only_subsequences_match() {
    assert!(score("src/main.rs", "mainrs").is_some());
    assert_eq!(score("src/main.rs", "xyz"), None);
    assert!(score("src/main.rs", "srm").is_some());
    assert_eq!(score("abc", "abcd"), None);
    assert_eq!(score("", "a"), None);
    assert_eq!(score("anything", ""), Some(0));
}

#[test]
fn matching_is_case_insensitive() {
    assert!(score("README.md", "readme").is_some());
    assert!(score("readme.md", "README").is_some());
}

#[test]
fn file_name_matches_beat_directory_matches() {
    let in_name = score("docs/guide.md", "guide").unwrap();
    let in_directory = score("guide/notes.md", "guide").unwrap();
    assert!(in_name > in_directory);
}

#[test]
fn file_named_like_its_folder_keeps_the_file_name_bonus() {
    let named = score("Sources/Model/Model.swift", "model").unwrap();
    let sibling = score("Sources/Model/View.swift", "model").unwrap();
    assert!(named > sibling);
}

#[test]
fn consecutive_runs_beat_scattered_matches() {
    let run = score("browser.swift", "brow").unwrap();
    let scattered = score("b_r_o_w_x.swift", "brow").unwrap();
    assert!(run > scattered);
}

#[test]
fn non_ascii_queries_match_whole_characters() {
    assert_eq!(score("tiếng/bá.md", "ạ"), None);
    assert_eq!(score("è©.txt", "é"), None);
    assert!(score("tiếng/bạn.md", "ạ").is_some());
    assert!(score("tiếng/bạn.md", "bn").is_some());
}

#[test]
fn build_flattens_every_file_and_skips_submodules() {
    let index = build(&trees(), PathIndex::MAX_ENTRIES);
    assert_eq!(
        paths(&index),
        ["src/nested/link", "src/main.rs", "src/run.sh", "README.md"],
        "folders first, each in full and in order, then a folder's own files"
    );
    assert!(!index.truncated);
    let main = index
        .entries
        .iter()
        .find(|entry| entry.path == "src/main.rs")
        .unwrap();
    assert_eq!(main.name(), "main.rs");
    assert_eq!(main.oid, Some(oid(0x11)));
}

#[test]
fn build_lists_sibling_folders_in_order_before_files() {
    let fixed = HashMap::from([
        (
            oid(ROOT),
            vec![
                entry(b"a", oid(0x20), EntryKind::Tree),
                entry(b"b", oid(0x21), EntryKind::Tree),
                entry(b"z.txt", oid(0x22), EntryKind::Blob),
            ],
        ),
        (oid(0x20), vec![entry(b"x", oid(0x23), EntryKind::Blob)]),
        (oid(0x21), vec![entry(b"y", oid(0x24), EntryKind::Blob)]),
    ]);
    let index = build(&fixed, PathIndex::MAX_ENTRIES);
    assert_eq!(search(&index, "", 100, true), ["a/x", "b/y", "z.txt"]);
    let capped = build(&fixed, 1);
    assert_eq!(paths(&capped), ["a/x"], "the cut keeps the first folder");
}

#[test]
fn inexact_name_marks_its_path_and_every_path_below_it() {
    let mut fixed = trees();
    let root = fixed.get_mut(&oid(ROOT)).unwrap();
    root[0] = entry(b"s\xffc", oid(SUB), EntryKind::Tree);
    root.push(entry(b"f\xffo", oid(0x14), EntryKind::Blob));
    let index = build(&fixed, PathIndex::MAX_ENTRIES);
    let mut inexact: Vec<&str> = index
        .entries
        .iter()
        .filter(|entry| !entry.has_exact_path)
        .map(|entry| entry.path.as_str())
        .collect();
    inexact.sort_unstable();
    assert_eq!(
        inexact,
        [
            "f\u{FFFD}o",
            "s\u{FFFD}c/main.rs",
            "s\u{FFFD}c/nested/link",
            "s\u{FFFD}c/run.sh"
        ]
    );
    let readme = index
        .entries
        .iter()
        .find(|entry| entry.path == "README.md")
        .unwrap();
    assert!(readme.has_exact_path);
}

#[test]
fn search_ranks_and_limits() {
    let index = build(&trees(), PathIndex::MAX_ENTRIES);
    assert_eq!(
        search(&index, "mainrs", 100, true).first().unwrap(),
        "src/main.rs"
    );
    assert!(search(&index, "zzz", 100, true).is_empty());
    assert_eq!(search(&index, "", 100, true).len(), index.entries.len());
    assert_eq!(search(&index, "", 2, true).len(), 2);
}

#[test]
fn ties_order_by_length_then_path() {
    let index = flat_index(&["src/main.rs", "b/main.rs", "a/main.rs"]);
    let ranked = ["a/main.rs", "b/main.rs", "src/main.rs"];
    assert_eq!(search(&index, "main", 100, true), ranked);
    assert_eq!(
        search(&index, " main ", 100, true),
        ranked,
        "the query is trimmed"
    );
    assert_eq!(
        search(&index, "\t main\u{A0}", 100, true),
        ranked,
        "tabs and Zs spaces trim"
    );
    assert_eq!(
        search(&index, "   ", 100, true).len(),
        3,
        "a blank query lists every entry"
    );
}

#[test]
fn ties_compare_length_in_utf8_bytes() {
    let index = flat_index(&["\u{E9}/main.rs", "ab/main.rs"]);
    assert_eq!(
        search(&index, "main", 100, true),
        ["ab/main.rs", "\u{E9}/main.rs"],
        "both paths are 10 bytes, so the tie falls through to path order"
    );
}

#[test]
fn search_puts_the_file_named_like_its_folder_first() {
    let entries = [
        "components/button/index.ts",
        "components/button/Button.tsx",
        "components/button/styles.css",
    ]
    .iter()
    .map(|path| PathIndexEntry::new((*path).to_owned(), None, EntryKind::Blob, false, true))
    .collect();
    let index = PathIndex {
        root: oid(ROOT),
        entries,
        truncated: false,
    };
    assert_eq!(
        search(&index, "button", 100, true).first().unwrap(),
        "components/button/Button.tsx"
    );
}

#[test]
fn hidden_files_are_marked_and_left_out_on_request() {
    let mut fixed = trees();
    let meta = oid(0x70);
    fixed.get_mut(&oid(ROOT)).unwrap().extend([
        entry(b".DS_Store", oid(0x71), EntryKind::Blob),
        entry(b".git", meta, EntryKind::Tree),
        entry(b".env", oid(0x72), EntryKind::Blob),
    ]);
    fixed.insert(meta, vec![entry(b"HEAD", oid(0x73), EntryKind::Blob)]);
    let index = build(&fixed, PathIndex::MAX_ENTRIES);
    let mut hidden: Vec<&str> = index
        .entries
        .iter()
        .filter(|entry| entry.hidden)
        .map(|entry| entry.path.as_str())
        .collect();
    hidden.sort_unstable();
    assert_eq!(
        hidden,
        [".DS_Store", ".git/HEAD"],
        "a hidden folder hides what is inside it"
    );

    assert!(search(&index, "", 100, true).contains(&".DS_Store".to_owned()));
    let mut visible = search(&index, "", 100, false);
    visible.sort_unstable();
    assert_eq!(
        visible,
        [
            ".env",
            "README.md",
            "src/main.rs",
            "src/nested/link",
            "src/run.sh"
        ]
    );
    assert!(search(&index, "head", 100, false).is_empty());
    assert_eq!(search(&index, "head", 100, true), [".git/HEAD"]);
}

#[test]
fn build_stops_at_the_limit_and_says_so() {
    let fixed = trees();
    let capped = build(&fixed, 2);
    assert_eq!(capped.entries.len(), 2);
    assert!(capped.truncated, "a walk cut at the limit is flagged");
    let whole = build(&fixed, 4);
    assert_eq!(whole.entries.len(), 4);
    assert!(
        !whole.truncated,
        "a walk that fits the limit exactly drops nothing"
    );
    let roomy = build(&fixed, 5);
    assert_eq!(roomy.entries.len(), 4);
    assert!(!roomy.truncated);
}

#[test]
fn build_stops_at_the_entry_cap() {
    fn flat(count: usize) -> PathIndex {
        let files: Vec<TreeEntry> = (0..count)
            .map(|n| entry(format!("f{n}").as_bytes(), oid(0x77), EntryKind::Blob))
            .collect();
        let cancel = AtomicBool::new(false);
        PathIndex::build(oid(ROOT), PathIndex::MAX_ENTRIES, &cancel, |_| {
            Ok::<_, Infallible>(files.clone())
        })
        .unwrap()
    }
    let over = flat(PathIndex::MAX_ENTRIES + 1);
    assert!(over.truncated);
    assert_eq!(over.entries.len(), PathIndex::MAX_ENTRIES);
    let exact = flat(PathIndex::MAX_ENTRIES);
    assert!(!exact.truncated, "nothing was dropped");
    assert_eq!(exact.entries.len(), PathIndex::MAX_ENTRIES);
    assert!(!flat(PathIndex::MAX_ENTRIES - 1).truncated);
}

#[test]
fn build_stops_at_the_next_directory_once_cancelled() {
    let fixed = trees();
    let cancel = AtomicBool::new(false);
    let reads = Cell::new(0);
    let result = PathIndex::build(oid(ROOT), PathIndex::MAX_ENTRIES, &cancel, |tree| {
        reads.set(reads.get() + 1);
        if reads.get() == 1 {
            cancel.store(true, Ordering::Relaxed);
        }
        Ok::<_, Infallible>(fixed.get(&tree).cloned().unwrap_or_default())
    });
    assert!(matches!(result, Err(BuildError::Cancelled)));
    assert_eq!(reads.get(), 1, "no directory is read after the cancel");
}

#[test]
fn a_failed_directory_read_names_the_tree() {
    #[derive(Debug, thiserror::Error)]
    #[error("tree missing")]
    struct Missing;
    let cancel = AtomicBool::new(false);
    let result = PathIndex::build(oid(ROOT), PathIndex::MAX_ENTRIES, &cancel, |tree| {
        trees().get(&tree).cloned().ok_or(Missing)
    });
    let index = result.map(|index| index.entries.len());
    assert_eq!(
        index.unwrap(),
        4,
        "every tree is present, so the walk succeeds"
    );
    let result = PathIndex::build(oid(0x99), PathIndex::MAX_ENTRIES, &cancel, |tree| {
        trees().get(&tree).cloned().ok_or(Missing)
    });
    let error = result.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("path index: list tree {}: tree missing", oid(0x99))
    );
}

#[test]
fn search_stops_once_cancelled() {
    let names: Vec<String> = (0..PathIndex::CANCELLATION_STRIDE * 2)
        .map(|n| format!("dir/file{n}.txt"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let index = flat_index(&refs);
    let cancel = AtomicBool::new(true);
    assert_eq!(index.search("file", 100, true, &cancel), Err(Cancelled));
}

#[test]
fn a_worktree_listing_becomes_an_index_without_blob_ids() {
    let list = WorktreeFileList {
        files: vec![
            WorktreeFile {
                path: ".env".into(),
                kind: EntryKind::Blob,
            },
            WorktreeFile {
                path: "sub/.DS_Store".into(),
                kind: EntryKind::Blob,
            },
            WorktreeFile {
                path: "run.sh".into(),
                kind: EntryKind::ExecutableBlob,
            },
        ],
        scanned: 5,
        truncated: true,
        ..WorktreeFileList::default()
    };
    let index = PathIndex::from_worktree_files(oid(ROOT), &list);
    assert_eq!(index.root, oid(ROOT));
    assert!(index.truncated);
    assert_eq!(paths(&index), [".env", "sub/.DS_Store", "run.sh"]);
    let hidden: Vec<bool> = index.entries.iter().map(|entry| entry.hidden).collect();
    assert_eq!(hidden, [false, true, false]);
    assert!(index.entries.iter().all(|entry| entry.oid.is_none()));
    assert_eq!(index.entries[2].kind, EntryKind::ExecutableBlob);
}

#[test]
fn search_matches_decomposed_names_by_whole_characters() {
    let list = WorktreeFileList {
        files: ["cafe\u{301}.txt", "\u{e8}\u{a9}.txt"]
            .iter()
            .map(|path| WorktreeFile {
                path: (*path).into(),
                kind: EntryKind::Blob,
            })
            .collect(),
        scanned: 2,
        truncated: false,
        ..WorktreeFileList::default()
    };
    let index = PathIndex::from_worktree_files(oid(ROOT), &list);
    let found = search(&index, "caf\u{e9}", 100, true);
    assert_eq!(
        found.len(),
        1,
        "a precomposed query finds the decomposed name"
    );
    assert_eq!(
        found[0], "cafe\u{301}.txt",
        "the stored path stays as it was on disk"
    );
    assert_eq!(index.entries[0].search_key(), "caf\u{e9}.txt");
    assert_eq!(
        search(&index, "\u{e9}", 100, true),
        ["cafe\u{301}.txt"],
        "the letter matches only the name that has it, never bytes of other letters"
    );
}

#[test]
fn build_reads_each_directory_once() {
    let mut wide = HashMap::new();
    let folders: Vec<TreeEntry> = (0..50u8)
        .map(|n| entry(format!("dir{n}").as_bytes(), oid(n + 4), EntryKind::Tree))
        .collect();
    for folder in &folders {
        let files = (0..100)
            .map(|n| entry(format!("f{n}").as_bytes(), oid(0x77), EntryKind::Blob))
            .collect();
        wide.insert(folder.oid, files);
    }
    wide.insert(oid(ROOT), folders);
    let reads = Cell::new(0);
    let cancel = AtomicBool::new(false);
    let index = PathIndex::build(oid(ROOT), PathIndex::MAX_ENTRIES, &cancel, |tree| {
        reads.set(reads.get() + 1);
        Ok::<_, Infallible>(wide[&tree].clone())
    })
    .unwrap();
    assert_eq!(reads.get(), 51, "one read per directory");
    assert_eq!(index.entries.len(), 5000);
}

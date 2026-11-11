//! Port of `PathIndexModelTests.swift`, the two model tests of `ContentSearchTests.swift`,
//! and the model half of `ImportResolverTests.swift` (`ModelImportTests`): how the model
//! shares, cancels and retries the walks behind path search, supersedes a content search,
//! hands a match to the pane, and opens what an import names.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{Entity, TestAppContext};
use rc_git_core::{
    EntryKind, FileSearch, Head, LineMatch, RefKind, RefListing, WorktreeFile, WorktreeFileList,
    WorktreeListing, WorktreeStatus,
};
use rc_git_kit::browser::{
    BlobOpen, BrowserModel, ContentSearchError, OpenPathOutcome, PATH_INDEX_CACHE_LIMIT,
};
use rc_git_kit::content_search::{ContentLineMatch, ContentReport};
use rc_git_kit::fake::{FakeRepository, branch, oid, text_blob};
use rc_git_kit::path_search::PathIndexEntry;
use rc_git_kit::path_search_session::PathSearchError;
use rc_git_kit::text_document::FileReveal;

use crate::support::{arrived, attach, disk, file_state, gate, model, text, tree, until};

const MAIN_COMMIT: u8 = 0x11;
const MAIN_TREE: u8 = 0x12;
const FEATURE_COMMIT: u8 = 0x21;
const FEATURE_TREE: u8 = 0x22;
const SRC_TREE: u8 = 0x30;
const README: u8 = 0x40;

fn head(target: u8) -> Head {
    Head {
        ref_name: Some("refs/heads/main".to_owned()),
        target: Some(oid(target)),
        exact_ref_name: true,
    }
}

fn repo() -> FakeRepository {
    let repo = FakeRepository::new();
    {
        let mut state = repo.state();
        state.listing = RefListing {
            refs: vec![
                branch("feature", oid(FEATURE_COMMIT), RefKind::LocalBranch),
                branch("main", oid(MAIN_COMMIT), RefKind::LocalBranch),
            ],
            broken: Vec::new(),
        };
        state.head = head(MAIN_COMMIT);
        state.tree_of.insert(oid(MAIN_COMMIT), oid(MAIN_TREE));
        state.tree_of.insert(oid(FEATURE_COMMIT), oid(FEATURE_TREE));
        state.trees.insert(
            oid(MAIN_TREE),
            vec![
                tree("src", oid(SRC_TREE), EntryKind::Tree),
                tree("README.md", oid(README), EntryKind::Blob),
            ],
        );
        state.trees.insert(
            oid(FEATURE_TREE),
            vec![tree("src", oid(SRC_TREE), EntryKind::Tree)],
        );
        state.trees.insert(
            oid(SRC_TREE),
            vec![tree("main.rs", oid(README), EntryKind::Blob)],
        );
        state.worktree_file_list = WorktreeFileList {
            files: vec![WorktreeFile {
                path: "README.md".into(),
                kind: EntryKind::Blob,
            }],
            scanned: 1,
            ..WorktreeFileList::default()
        };
    }
    repo
}

fn checkout(repo: &FakeRepository) {
    repo.state().work_dir = Some(PathBuf::from("/checkout"));
}

fn paths(entries: &[PathIndexEntry]) -> Vec<String> {
    entries.iter().map(|e| e.path.clone()).collect()
}

#[gpui::test]
async fn concurrent_searches_share_one_tree_walk(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state()
        .delays
        .insert(oid(MAIN_TREE), Duration::from_millis(300));
    let first = model.update(cx, |m, cx| m.search_paths("", 100, cx));
    let watched = repo.clone();
    until(cx, move || watched.state().tree_entry_calls >= 1).await;
    let second = model
        .update(cx, |m, cx| m.search_paths("", 100, cx))
        .await
        .unwrap();
    assert_eq!(paths(&second.entries), ["src/main.rs", "README.md"]);
    assert_eq!(
        first.await,
        Err(PathSearchError::Cancelled),
        "the newer query supersedes it"
    );
    assert_eq!(repo.state().tree_entry_calls, 2, "one read per directory");
}

#[gpui::test]
async fn path_index_cache_keeps_only_the_most_recent_roots(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    let limit = PATH_INDEX_CACHE_LIMIT.get();
    let roots: Vec<_> = (0..=limit)
        .map(|index| oid(0x50 + u8::try_from(index).unwrap()))
        .collect();
    for (index, root) in roots.iter().enumerate() {
        repo.state().trees.insert(
            *root,
            vec![tree(&format!("file{index}"), oid(README), EntryKind::Blob)],
        );
    }
    attach(cx, &model, &repo).await;
    for root in &roots {
        let root = *root;
        model
            .update(cx, |m, cx| m.path_index(root, cx))
            .await
            .unwrap();
        assert!(model.read_with(cx, |m, _| m.path_index_cache_count()) <= limit);
    }
    assert_eq!(
        model.read_with(cx, |m, _| m.path_index_cache_count()),
        limit
    );

    let calls = repo.state().tree_entry_calls;
    let recent = roots[limit];
    let index = model
        .update(cx, |m, cx| m.path_index(recent, cx))
        .await
        .unwrap();
    assert_eq!(paths(&index.entries), [format!("file{limit}")]);
    let evicted = roots[0];
    let index = model
        .update(cx, |m, cx| m.path_index(evicted, cx))
        .await
        .unwrap();
    assert_eq!(
        paths(&index.entries),
        ["file0"],
        "an evicted root is rebuilt"
    );
    assert_eq!(
        repo.state().tree_entry_calls,
        calls + 1,
        "the rebuild reads its one directory again: the walk stores no listing"
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.path_index_cache_count()),
        limit
    );
}

#[gpui::test]
async fn a_build_for_another_root_cancels_the_running_build(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state()
        .delays
        .insert(oid(MAIN_TREE), Duration::from_millis(300));
    let slow = model.update(cx, |m, cx| m.path_index(oid(MAIN_TREE), cx));
    let watched = repo.clone();
    until(cx, move || watched.state().tree_entry_calls >= 1).await;
    let other = model
        .update(cx, |m, cx| m.path_index(oid(FEATURE_TREE), cx))
        .await
        .unwrap();
    assert_eq!(paths(&other.entries), ["src/main.rs"]);
    assert_eq!(slow.await.err(), Some(PathSearchError::Cancelled));
}

#[gpui::test]
async fn selecting_another_ref_cancels_the_running_build(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state()
        .delays
        .insert(oid(MAIN_TREE), Duration::from_millis(300));
    let slow = model.update(cx, |m, cx| m.path_index(oid(MAIN_TREE), cx));
    let watched = repo.clone();
    until(cx, move || watched.state().tree_entry_calls >= 1).await;
    let feature = model.read_with(cx, |m, _| m.refs().refs[0].clone());
    model.update(cx, |m, cx| m.select(feature, cx)).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.root_tree()),
        Some(oid(FEATURE_TREE))
    );
    let result = model
        .update(cx, |m, cx| m.search_paths("", 100, cx))
        .await
        .unwrap();
    assert_eq!(paths(&result.entries), ["src/main.rs"]);
    assert_eq!(slow.await.err(), Some(PathSearchError::Cancelled));
}

#[gpui::test]
async fn concurrent_working_tree_searches_share_one_walk(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    attach(cx, &model, &repo).await;
    repo.state().worktree_list_delay = Duration::from_millis(300);
    let first = model.update(cx, |m, cx| m.search_paths("", 100, cx));
    let watched = repo.clone();
    until(cx, move || watched.state().worktree_list_calls >= 1).await;
    let second = model
        .update(cx, |m, cx| m.search_paths("", 100, cx))
        .await
        .unwrap();
    assert_eq!(paths(&second.entries), ["README.md"]);
    assert_eq!(first.await, Err(PathSearchError::Cancelled));
    assert_eq!(repo.state().worktree_list_calls, 1);
}

#[gpui::test]
async fn a_failed_working_tree_walk_is_retried_by_the_next_search(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    attach(cx, &model, &repo).await;
    repo.state().fail_next_worktree_list = true;
    let failed = model.update(cx, |m, cx| m.search_paths("", 100, cx)).await;
    let Err(PathSearchError::Failed(_)) = failed else {
        panic!("expected a failure, got {failed:?}");
    };
    let result = model
        .update(cx, |m, cx| m.search_paths("", 100, cx))
        .await
        .unwrap();
    assert_eq!(paths(&result.entries), ["README.md"]);
    assert_eq!(repo.state().worktree_list_calls, 2);
}

#[gpui::test]
async fn an_outdated_working_tree_walk_answers_its_query_but_is_not_kept(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    attach(cx, &model, &repo).await;
    repo.state().worktree_list_delay = Duration::from_millis(300);
    let slow = model.update(cx, |m, cx| m.search_paths("", 100, cx));
    let watched = repo.clone();
    until(cx, move || watched.state().worktree_list_calls >= 1).await;
    model.update(cx, |m, _| m.forget_worktree_paths());
    let answered = slow.await.unwrap();
    assert_eq!(paths(&answered.entries), ["README.md"]);
    repo.state().worktree_list_delay = Duration::ZERO;
    model
        .update(cx, |m, cx| m.search_paths("", 100, cx))
        .await
        .unwrap();
    assert_eq!(
        repo.state().worktree_list_calls,
        2,
        "the outdated walk is dropped"
    );
}

/// The `ContentSearchModelTests` tree: one blob shared by two paths, a symlink and a hidden
/// file.
fn content_repo() -> FakeRepository {
    let repo = FakeRepository::new();
    {
        let mut state = repo.state();
        state.listing = RefListing {
            refs: vec![branch("main", oid(MAIN_COMMIT), RefKind::LocalBranch)],
            broken: Vec::new(),
        };
        state.head = head(MAIN_COMMIT);
        state.tree_of.insert(oid(MAIN_COMMIT), oid(MAIN_TREE));
        state.trees.insert(
            oid(MAIN_TREE),
            vec![
                tree("src", oid(SRC_TREE), EntryKind::Tree),
                tree("README.md", oid(0x40), EntryKind::Blob),
                tree("link", oid(0x41), EntryKind::Symlink),
                tree(".DS_Store", oid(0x42), EntryKind::Blob),
            ],
        );
        state.trees.insert(
            oid(SRC_TREE),
            vec![
                tree("copy.md", oid(0x40), EntryKind::Blob),
                tree("main.rs", oid(0x41), EntryKind::Blob),
            ],
        );
    }
    repo
}

fn hit_line(line: usize, column: usize) -> LineMatch {
    LineMatch {
        line,
        column,
        excerpt: b"match".to_vec(),
        ranges: std::iter::once(0..5).collect(),
    }
}

#[gpui::test]
async fn a_newer_search_supersedes_the_running_one(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = content_repo();
    attach(cx, &model, &repo).await;
    let held = gate();
    {
        let mut state = repo.state();
        state.blob_searches.insert(
            oid(0x40),
            FileSearch::Searched {
                lines: vec![hit_line(0, 0)],
                truncated: false,
            },
        );
        state.content_search_gate = Some(Arc::clone(&held));
    }
    let stale = Rc::new(Cell::new(0));
    let counted = Rc::clone(&stale);
    let stale_report: ContentReport = Rc::new(move |_, _, _| counted.set(counted.get() + 1));
    let first = model.update(cx, |m, cx| m.search_content("old", stale_report, cx));
    arrived(cx, &held).await;
    let found = Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = Rc::clone(&found);
    let report: ContentReport = Rc::new(move |hits, _, _| {
        sink.borrow_mut()
            .extend(hits.into_iter().map(|hit| hit.entry.path));
    });
    let newer = model.update(cx, |m, cx| m.search_content("new", report, cx));
    held.open();
    assert_eq!(first.await, Err(ContentSearchError::Cancelled));
    newer.await.unwrap();
    cx.run_until_parked();
    assert_eq!(*found.borrow(), ["src/copy.md", "README.md"]);
    assert_eq!(stale.get(), 0, "the superseded search reports nothing");
}

#[gpui::test]
async fn opening_a_match_hands_the_pane_its_position(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = content_repo();
    attach(cx, &model, &repo).await;
    repo.state()
        .blobs
        .insert(oid(0x40), text_blob("one\ntwo match\n"));
    let entry = PathIndexEntry::new(
        "README.md".to_owned(),
        Some(oid(0x40)),
        EntryKind::Blob,
        false,
        true,
    );
    let found = ContentLineMatch::new(hit_line(1, 4), 5);
    model
        .update(cx, |m, cx| m.open_content_match(&entry, Some(&found), cx))
        .await;
    model.read_with(cx, |m, _| {
        assert_eq!(m.opened_file_path(), Some("README.md"));
        assert_eq!(
            m.pending_reveal(),
            Some(&FileReveal::Match {
                line: 1,
                byte_column: 4,
                byte_length: 5
            })
        );
    });
    model
        .update(cx, |m, cx| m.open_content_match(&entry, None, cx))
        .await;
    assert_eq!(
        model.read_with(cx, |m, _| m.pending_reveal().cloned()),
        None,
        "a file opened at the top scrolls nowhere"
    );
}

const APP: u8 = 0x21;
const BUTTON: u8 = 0x22;
const CONFIG: u8 = 0x23;
const IMPORT_SRC: u8 = 0x13;

/// The `ModelImportTests` tree: `src/App.tsx`, `src/Button.tsx` and a `tsconfig.json` that
/// maps `@/*` to `./src/*`.
fn import_repo() -> FakeRepository {
    let repo = FakeRepository::new();
    {
        let mut state = repo.state();
        state.listing = RefListing {
            refs: vec![branch("main", oid(MAIN_COMMIT), RefKind::LocalBranch)],
            broken: Vec::new(),
        };
        state.head = head(MAIN_COMMIT);
        state.tree_of.insert(oid(MAIN_COMMIT), oid(MAIN_TREE));
        state.trees.insert(
            oid(MAIN_TREE),
            vec![
                tree("src", oid(IMPORT_SRC), EntryKind::Tree),
                tree("tsconfig.json", oid(CONFIG), EntryKind::Blob),
            ],
        );
        state.trees.insert(
            oid(IMPORT_SRC),
            vec![
                tree("App.tsx", oid(APP), EntryKind::Blob),
                tree("Button.tsx", oid(BUTTON), EntryKind::Blob),
            ],
        );
        state
            .blobs
            .insert(oid(APP), text_blob("import Button from './Button';\n"));
        state.blobs.insert(
            oid(BUTTON),
            text_blob("export default function Button() {}\n"),
        );
        state.blobs.insert(
            oid(CONFIG),
            text_blob(r#"{"compilerOptions": {"paths": {"@/*": ["./src/*"]}}}"#),
        );
    }
    repo
}

async fn open_import(
    cx: &mut TestAppContext,
    model: &Entity<BrowserModel>,
    specifier: &str,
) -> OpenPathOutcome {
    model
        .update(cx, |m, cx| m.open_import(specifier, "src/App.tsx", cx))
        .await
}

fn opened(cx: &mut TestAppContext, model: &Entity<BrowserModel>) -> Option<String> {
    model.read_with(cx, |m, _| m.opened_file_path().map(str::to_owned))
}

#[gpui::test]
async fn a_relative_import_opens_the_file_it_names(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &import_repo()).await;
    assert_eq!(
        open_import(cx, &model, "./Button").await,
        OpenPathOutcome::Opened
    );
    assert_eq!(opened(cx, &model).as_deref(), Some("src/Button.tsx"));
    let (entry, _) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry.oid, Some(oid(BUTTON)));
}

#[gpui::test]
async fn an_aliased_import_opens_through_the_config(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &import_repo()).await;
    assert_eq!(
        open_import(cx, &model, "@/Button").await,
        OpenPathOutcome::Opened
    );
    assert_eq!(opened(cx, &model).as_deref(), Some("src/Button.tsx"));
}

#[gpui::test]
async fn a_package_import_opens_nothing_and_leaves_the_file_in_place(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &import_repo()).await;
    let outcome = model
        .update(cx, |m, cx| m.open_path("src/App.tsx", None, cx))
        .await;
    assert_eq!(outcome, OpenPathOutcome::Opened);
    assert_eq!(
        open_import(cx, &model, "react").await,
        OpenPathOutcome::Missing
    );
    assert_eq!(
        open_import(cx, &model, "./missing").await,
        OpenPathOutcome::Missing
    );
    assert_eq!(opened(cx, &model).as_deref(), Some("src/App.tsx"));
}

#[gpui::test]
async fn a_failed_tree_read_is_a_failure_not_a_missing_module(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = import_repo();
    attach(cx, &model, &repo).await;
    repo.state().fail_next_tree = true;
    let OpenPathOutcome::Failed(message) = open_import(cx, &model, "@/Button").await else {
        panic!("a failed read must not read as a missing module");
    };
    assert!(message.contains("read tree"));
    assert_eq!(opened(cx, &model), None, "nothing opened");
}

#[gpui::test]
async fn resolution_reads_each_directory_once(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = import_repo();
    attach(cx, &model, &repo).await;
    assert_eq!(
        open_import(cx, &model, "@/Button").await,
        OpenPathOutcome::Opened
    );
    let calls = repo.state().tree_entry_calls;
    assert_eq!(
        calls, 2,
        "the root and src, however many candidates were probed"
    );
    assert_eq!(
        open_import(cx, &model, "./Button").await,
        OpenPathOutcome::Opened
    );
    assert_eq!(
        repo.state().tree_entry_calls,
        calls,
        "served from the tree cache"
    );
}

#[gpui::test]
async fn imports_resolve_on_disk_while_browsing_the_working_tree(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = import_repo();
    checkout(&repo);
    {
        let mut state = repo.state();
        state.worktree_listings.insert(
            String::new(),
            WorktreeListing {
                entries: vec![disk(
                    "src",
                    EntryKind::Tree,
                    WorktreeStatus::Unchanged,
                    None,
                )],
                ..WorktreeListing::default()
            },
        );
        state.worktree_listings.insert(
            "src".to_owned(),
            WorktreeListing {
                entries: vec![
                    disk(
                        "App.tsx",
                        EntryKind::Blob,
                        WorktreeStatus::Unchanged,
                        Some(oid(APP)),
                    ),
                    disk("New.tsx", EntryKind::Blob, WorktreeStatus::Untracked, None),
                ],
                ..WorktreeListing::default()
            },
        );
        state.worktree_contents.insert(
            "src/New.tsx".to_owned(),
            text_blob("export const fresh = 1;\n"),
        );
    }
    attach(cx, &model, &repo).await;
    assert!(model.read_with(cx, |m, _| m.browsing_working_tree()));
    assert_eq!(
        open_import(cx, &model, "./New").await,
        OpenPathOutcome::Opened
    );
    assert_eq!(
        opened(cx, &model).as_deref(),
        Some("src/New.tsx"),
        "an untracked file is on disk only"
    );
    assert_eq!(
        open_import(cx, &model, "./Button").await,
        OpenPathOutcome::Missing,
        "not on disk"
    );
    assert_eq!(
        open_import(cx, &model, "./nope/deeper").await,
        OpenPathOutcome::Missing,
        "a directory that is not on disk"
    );
}

#[gpui::test]
async fn an_unreadable_directory_on_disk_is_a_failure_not_a_missing_module(
    cx: &mut TestAppContext,
) {
    let model = model(cx);
    let repo = import_repo();
    checkout(&repo);
    {
        let mut state = repo.state();
        state.worktree_listings.insert(
            String::new(),
            WorktreeListing {
                entries: vec![
                    disk("src", EntryKind::Tree, WorktreeStatus::Unchanged, None),
                    disk("lib", EntryKind::Tree, WorktreeStatus::Unchanged, None),
                ],
                ..WorktreeListing::default()
            },
        );
        state.worktree_listings.insert(
            "src".to_owned(),
            WorktreeListing {
                entries: vec![disk(
                    "App.tsx",
                    EntryKind::Blob,
                    WorktreeStatus::Unchanged,
                    Some(oid(APP)),
                )],
                ..WorktreeListing::default()
            },
        );
    }
    attach(cx, &model, &repo).await;
    let outcome = open_import(cx, &model, "../lib/x").await;
    let OpenPathOutcome::Failed(_) = outcome else {
        panic!("lib is on disk and could not be listed, got {outcome:?}");
    };
}

#[gpui::test]
async fn a_resolution_the_user_moved_past_opens_nothing(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = import_repo();
    attach(cx, &model, &repo).await;
    let held = gate();
    repo.state()
        .gates
        .insert(oid(IMPORT_SRC), Arc::clone(&held));
    let stale = model.update(cx, |m, cx| m.open_import("./Button", "src/App.tsx", cx));
    arrived(cx, &held).await;
    let config = tree("tsconfig.json", oid(CONFIG), EntryKind::Blob);
    model
        .update(cx, |m, cx| m.open_file(BlobOpen::new(&config), cx))
        .await;
    held.open();
    assert_eq!(
        stale.await,
        OpenPathOutcome::Opened,
        "a stale resolution is not a dead link"
    );
    let (entry, _) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(
        entry.name, "tsconfig.json",
        "the newer open stays on screen"
    );
}

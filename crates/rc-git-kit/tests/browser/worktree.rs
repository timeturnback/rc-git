//! Port of `WorktreeModelTests.swift`: browsing the working directory as a pseudo-ref, its
//! rows, opening files from disk, path search on disk, and following `HEAD` on refresh.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{Entity, TestAppContext};
use rc_git_core::{
    EntryKind, Head, Oid, RefKind, RefListing, TreeEntry, WorktreeFile, WorktreeFileList,
    WorktreeListing, WorktreeStatus,
};
use rc_git_kit::browser::{
    BlobOpen, BrowserEvent, BrowserModel, FileEntry, FileState, OpenPathOutcome, WorktreeFileOpen,
    rows,
};
use rc_git_kit::fake::{FakeRepository, branch, oid, text_blob};
use rc_git_kit::models::BrowseRow;
use rc_git_kit::path_search::PathIndexEntry;
use rc_git_kit::path_search_session::PathSearchError;

use crate::support::{attach, count, disk, events, file_state, model, text, tree};

const MAIN_COMMIT: u8 = 0x11;
const MAIN_TREE: u8 = 0x12;
const README: u8 = 0x40;
const GONE: u8 = 0x50;

fn repo() -> FakeRepository {
    let repo = FakeRepository::new();
    {
        let mut state = repo.state();
        state.listing = RefListing {
            refs: vec![branch("main", oid(MAIN_COMMIT), RefKind::LocalBranch)],
            broken: Vec::new(),
        };
        state.head = head(Some(oid(MAIN_COMMIT)));
        state.tree_of.insert(oid(MAIN_COMMIT), oid(MAIN_TREE));
        state.trees.insert(
            oid(MAIN_TREE),
            vec![tree("README.md", oid(README), EntryKind::Blob)],
        );
        state.worktree_listings.insert(
            String::new(),
            WorktreeListing {
                entries: vec![
                    disk(
                        "README.md",
                        EntryKind::Blob,
                        WorktreeStatus::Modified,
                        Some(oid(README)),
                    ),
                    disk("new.txt", EntryKind::Blob, WorktreeStatus::Untracked, None),
                    disk("src", EntryKind::Tree, WorktreeStatus::Unchanged, None),
                ],
                missing: vec![tree("gone.txt", oid(GONE), EntryKind::Blob)],
                warnings: Vec::new(),
            },
        );
        state
            .worktree_contents
            .insert("new.txt".to_owned(), text_blob("new\n"));
    }
    repo
}

fn head(target: Option<Oid>) -> Head {
    Head {
        ref_name: Some("refs/heads/main".to_owned()),
        target,
        exact_ref_name: true,
    }
}

fn checkout(repo: &FakeRepository) {
    repo.state().work_dir = Some(PathBuf::from("/checkout"));
}

fn files(paths: &[&str], truncated: bool) -> WorktreeFileList {
    WorktreeFileList {
        files: paths
            .iter()
            .map(|path| WorktreeFile {
                path: (*path).into(),
                kind: EntryKind::Blob,
            })
            .collect(),
        scanned: paths.len(),
        truncated,
        ..WorktreeFileList::default()
    }
}

async fn search(
    cx: &mut TestAppContext,
    model: &Entity<BrowserModel>,
    query: &str,
) -> Result<rc_git_kit::path_search_session::PathSearchResult, PathSearchError> {
    model
        .update(cx, |m, cx| m.search_paths(query, 100, cx))
        .await
}

fn paths(entries: &[PathIndexEntry]) -> Vec<String> {
    entries.iter().map(|e| e.path.clone()).collect()
}

fn row(entry: TreeEntry, status: Option<WorktreeStatus>, missing: bool) -> BrowseRow {
    BrowseRow {
        entry,
        status,
        missing,
    }
}

async fn select_working_tree(cx: &mut TestAppContext, model: &Entity<BrowserModel>) {
    model.update(cx, BrowserModel::select_working_tree).await;
}

#[gpui::test]
async fn a_checkout_opens_on_the_working_tree(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    attach(cx, &model, &repo).await;
    model.read_with(cx, |m, _| {
        assert!(m.browsing_working_tree());
        assert!(m.selected_ref().is_none());
        assert_eq!(m.root_tree(), Some(oid(MAIN_TREE)));
    });
}

#[gpui::test]
async fn an_unborn_head_opens_on_a_branch_instead(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    repo.state().head = head(None);
    attach(cx, &model, &repo).await;
    model.read_with(cx, |m, _| {
        assert!(!m.browsing_working_tree());
        assert_eq!(m.selected_ref().unwrap().short_name, "main");
    });
}

#[gpui::test]
fn tree_rows_hide_clutter_until_asked(cx: &mut TestAppContext) {
    let model = model(cx);
    let store = row(tree(".DS_Store", oid(1), EntryKind::Blob), None, false);
    let env = row(
        tree(".env", oid(2), EntryKind::Blob),
        Some(WorktreeStatus::Ignored),
        false,
    );
    model.read_with(cx, |m, _| {
        assert!(!m.is_visible(&store));
        assert!(m.is_visible(&env), "a gitignored dotfile stays visible");
    });
    model.update(cx, |m, cx| m.set_shows_hidden_files(true, cx));
    assert!(model.read_with(cx, |m, _| m.is_visible(&store)));
}

#[gpui::test]
async fn path_search_in_the_working_tree_walks_disk_once_per_session(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    repo.state().worktree_file_list = files(&[".DS_Store", ".env", "README.md"], false);
    attach(cx, &model, &repo).await;
    let visible = search(cx, &model, "").await.unwrap();
    assert_eq!(
        paths(&visible.entries),
        [".env", "README.md"],
        "gitignored .env is found"
    );
    model.update(cx, |m, cx| m.set_shows_hidden_files(true, cx));
    let all = search(cx, &model, "").await.unwrap();
    assert_eq!(paths(&all.entries), [".DS_Store", ".env", "README.md"]);
    assert_eq!(
        repo.state().worktree_list_calls,
        1,
        "keystrokes reuse the walk"
    );
    model.update(cx, |m, _| m.forget_worktree_paths());
    search(cx, &model, "env").await.unwrap();
    assert_eq!(repo.state().worktree_list_calls, 2);
}

#[gpui::test]
async fn a_burst_of_queries_during_one_index_build_runs_one_scan_for_the_last(
    cx: &mut TestAppContext,
) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state()
        .delays
        .insert(oid(MAIN_TREE), Duration::from_millis(300));
    let queries = [
        "r", "re", "rea", "read", "readm", "readme", "readme.", "x", "xy", "rd",
    ];
    let mut calls = Vec::new();
    for query in queries {
        calls.push(model.update(cx, |m, cx| m.search_paths(query, 100, cx)));
    }
    let last = calls.pop().unwrap();
    let result = last.await.unwrap();
    assert_eq!(paths(&result.entries), ["README.md"]);
    for call in calls {
        assert_eq!(call.await, Err(PathSearchError::Cancelled));
    }
    assert_eq!(
        model.read_with(cx, |m, _| m.path_search_scans()),
        1,
        "only the last query scans the index"
    );
    assert_eq!(
        repo.state().tree_entry_calls,
        1,
        "the queries share one build"
    );
}

#[gpui::test]
async fn path_search_on_a_branch_searches_its_tree(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    attach(cx, &model, &repo).await;
    let main = model.read_with(cx, |m, _| m.refs().refs[0].clone());
    model.update(cx, |m, cx| m.select(main, cx)).await;
    let result = search(cx, &model, "").await.unwrap();
    assert_eq!(paths(&result.entries), ["README.md"]);
    assert!(!result.truncated, "a ref whose walk finished is complete");
    assert_eq!(repo.state().worktree_list_calls, 0);
}

#[gpui::test]
async fn a_truncated_working_tree_walk_reaches_the_search_result(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    repo.state().worktree_file_list = files(&["README.md"], true);
    attach(cx, &model, &repo).await;
    let result = search(cx, &model, "").await.unwrap();
    assert_eq!(paths(&result.entries), ["README.md"]);
    assert!(
        result.truncated,
        "the panel must say matches may be missing"
    );
}

#[gpui::test]
async fn a_search_pick_opens_from_disk_in_the_working_tree(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    attach(cx, &model, &repo).await;
    let pick = PathIndexEntry::new("new.txt".to_owned(), None, EntryKind::Blob, false, true);
    model
        .update(cx, |m, cx| m.open_search_result(&pick, cx))
        .await;
    let (entry, document) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry.name, "new.txt");
    assert_eq!(document.text, "new\n");
    assert_eq!(
        model.read_with(cx, |m, _| m.opened_file_path().map(str::to_owned)),
        Some("new.txt".to_owned())
    );
}

#[gpui::test]
async fn selecting_the_working_tree_roots_at_head(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    select_working_tree(cx, &model).await;
    model.read_with(cx, |m, _| {
        assert!(m.browsing_working_tree());
        assert!(m.selected_ref().is_none());
        assert_eq!(m.root_tree(), Some(oid(MAIN_TREE)));
    });
    let main = model.read_with(cx, |m, _| m.refs().refs[0].clone());
    model.update(cx, |m, cx| m.select(main, cx)).await;
    assert!(
        !model.read_with(cx, |m, _| m.browsing_working_tree()),
        "picking a ref leaves the working tree"
    );
}

#[gpui::test]
async fn rows_merge_disk_and_missing_in_display_order(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    select_working_tree(cx, &model).await;
    let listed = model
        .update(cx, |m, cx| m.worktree_rows("", cx))
        .await
        .unwrap();
    let names: Vec<String> = listed.iter().map(|r| r.entry.name.to_string()).collect();
    assert_eq!(
        names,
        ["src", "gone.txt", "new.txt", "README.md"],
        "directories first, then names, deleted files in their place"
    );
    assert!(listed[1].missing);
    assert_eq!(listed[2].status, Some(WorktreeStatus::Untracked));
    assert_eq!(listed[3].status, Some(WorktreeStatus::Modified));
    assert_eq!(
        listed[3].entry.oid,
        oid(README),
        "a tracked file keeps its HEAD blob id"
    );
}

#[test]
fn rows_sort_directories_first_then_names_ignoring_case() {
    let listing = WorktreeListing {
        entries: vec![
            disk("b.txt", EntryKind::Blob, WorktreeStatus::Unchanged, None),
            disk("Zeta", EntryKind::Tree, WorktreeStatus::Unchanged, None),
            disk("A.txt", EntryKind::Blob, WorktreeStatus::Unchanged, None),
            disk("alpha", EntryKind::Tree, WorktreeStatus::Unchanged, None),
        ],
        missing: vec![
            tree("Lib", oid(0x60), EntryKind::Tree),
            tree("c.txt", oid(0x61), EntryKind::Blob),
        ],
        warnings: Vec::new(),
    };
    let sorted = rows(&listing);
    let names: Vec<String> = sorted.iter().map(|r| r.entry.name.to_string()).collect();
    assert_eq!(names, ["alpha", "Lib", "Zeta", "A.txt", "b.txt", "c.txt"]);
    let missing: Vec<bool> = sorted.iter().map(|r| r.missing).collect();
    assert_eq!(missing, [false, true, false, false, false, true]);
}

#[gpui::test]
async fn worktree_files_open_from_disk(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    select_working_tree(cx, &model).await;
    model
        .update(cx, |m, cx| {
            m.open_worktree_file(WorktreeFileOpen::new("new.txt", "new.txt"), cx)
        })
        .await;
    let (entry, document) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry.name, "new.txt");
    assert_eq!(document.text, "new\n");
    model
        .update(cx, |m, cx| {
            m.open_worktree_file(WorktreeFileOpen::new("gone.txt", "gone.txt"), cx)
        })
        .await;
    let FileState::Failed(_, message) = file_state(cx, &model) else {
        panic!("expected a failure");
    };
    assert!(message.contains("gone.txt"));
}

async fn open_path(
    cx: &mut TestAppContext,
    model: &Entity<BrowserModel>,
    path: &str,
) -> OpenPathOutcome {
    model.update(cx, |m, cx| m.open_path(path, None, cx)).await
}

#[gpui::test]
async fn a_dead_link_in_the_working_tree_leaves_the_open_file_in_place(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    attach(cx, &model, &repo).await;
    assert_eq!(
        open_path(cx, &model, "new.txt").await,
        OpenPathOutcome::Opened
    );
    let state = file_state(cx, &model);
    let events = events(cx, &model);
    for (dead, why) in [
        ("missing.md", "no such file"),
        ("gone.txt", "a deleted file is not on disk"),
        ("src", "a directory is not a file"),
        ("new.txt/deeper", "a file is not a directory"),
    ] {
        assert_eq!(
            open_path(cx, &model, dead).await,
            OpenPathOutcome::Missing,
            "{why}"
        );
    }
    cx.run_until_parked();
    assert_eq!(file_state(cx, &model), state);
    assert_eq!(
        model.read_with(cx, |m, _| m.opened_file_path().map(str::to_owned)),
        Some("new.txt".to_owned())
    );
    assert_eq!(count(&events, &BrowserEvent::LocationChanged), 0);
}

#[gpui::test]
async fn a_working_tree_listing_failure_is_not_a_dead_link(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    attach(cx, &model, &repo).await;
    assert_eq!(
        open_path(cx, &model, "new.txt").await,
        OpenPathOutcome::Opened
    );
    let events = events(cx, &model);
    let OpenPathOutcome::Failed(message) = open_path(cx, &model, "src/lib.rs").await else {
        panic!("expected a failure");
    };
    cx.run_until_parked();
    assert!(message.contains("list working tree"));
    assert_eq!(
        model.read_with(cx, |m, _| m.opened_file_path().map(str::to_owned)),
        Some("new.txt".to_owned())
    );
    assert_eq!(count(&events, &BrowserEvent::LocationChanged), 0);
}

#[gpui::test]
async fn a_working_tree_row_on_disk_opens_from_disk(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    select_working_tree(cx, &model).await;
    let on_disk = row(
        tree("new.txt", oid(0), EntryKind::Blob),
        Some(WorktreeStatus::Untracked),
        false,
    );
    model
        .update(cx, |m, cx| m.open_tree_row(&on_disk, "new.txt", true, cx))
        .await;
    let (entry, document) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry.name, "new.txt");
    assert_eq!(document.text, "new\n");
    assert_eq!(repo.state().read_blob_calls, 0);
}

fn gone() -> TreeEntry {
    tree("gone.txt", oid(GONE), EntryKind::Blob)
}

#[gpui::test]
async fn a_missing_working_tree_row_opens_its_blob_and_load_anyway_stays_there(
    cx: &mut TestAppContext,
) {
    let model = model(cx);
    let repo = repo();
    repo.state()
        .blobs
        .insert(oid(GONE), text_blob("deleted on disk\n"));
    attach(cx, &model, &repo).await;
    select_working_tree(cx, &model).await;
    let missing = row(gone(), None, true);
    model
        .update(cx, |m, cx| m.open_tree_row(&missing, "gone.txt", true, cx))
        .await;
    let (entry, document) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(
        entry,
        FileEntry::from(&gone()),
        "a file gone from disk reads its HEAD blob"
    );
    assert_eq!(document.text, "deleted on disk\n");
    model.update(cx, |m, cx| m.load_anyway(1 << 20, cx)).await;
    let (reread, _) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(reread, FileEntry::from(&gone()));
    assert_eq!(repo.state().read_blob_calls, 2);
}

#[gpui::test]
async fn a_row_of_a_commit_opens_its_blob(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    repo.state()
        .blobs
        .insert(oid(README), text_blob("readme\n"));
    attach(cx, &model, &repo).await;
    assert!(!model.read_with(cx, |m, _| m.browsing_working_tree()));
    let readme = row(tree("README.md", oid(README), EntryKind::Blob), None, false);
    model
        .update(cx, |m, cx| m.open_tree_row(&readme, "README.md", true, cx))
        .await;
    let (entry, document) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry.oid, Some(oid(README)));
    assert_eq!(document.text, "readme\n");
    assert_eq!(repo.state().read_blob_calls, 1);
}

#[gpui::test]
async fn load_anyway_rereads_a_too_large_disk_file_from_disk(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    repo.state()
        .worktree_contents
        .insert("big.txt".to_owned(), text_blob("big file\n"));
    attach(cx, &model, &repo).await;
    select_working_tree(cx, &model).await;
    model
        .update(cx, |m, cx| {
            m.open_worktree_file(WorktreeFileOpen::new("big.txt", "big.txt").limit(2), cx)
        })
        .await;
    let FileState::TooLarge { .. } = file_state(cx, &model) else {
        panic!("expected too large");
    };
    model.update(cx, |m, cx| m.load_anyway(1 << 20, cx)).await;
    let (entry, document) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry.name, "big.txt");
    assert_eq!(document.text, "big file\n");
    assert_eq!(repo.state().read_blob_calls, 0);
}

#[gpui::test]
async fn load_anyway_rereads_a_missing_row_from_its_blob(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    repo.state()
        .blobs
        .insert(oid(GONE), text_blob("deleted on disk\n"));
    attach(cx, &model, &repo).await;
    select_working_tree(cx, &model).await;
    model
        .update(cx, |m, cx| {
            m.open_file(BlobOpen::new(&gone()).at("gone.txt").limit(2), cx)
        })
        .await;
    assert_eq!(
        file_state(cx, &model),
        FileState::TooLarge {
            entry: FileEntry::from(&gone()),
            size: 16,
            limit: 2
        }
    );
    model.update(cx, |m, cx| m.load_anyway(1 << 20, cx)).await;
    let (entry, document) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(
        entry,
        FileEntry::from(&gone()),
        "its HEAD blob, not the disk"
    );
    assert_eq!(document.text, "deleted on disk\n");
    assert_eq!(
        model.read_with(cx, |m, _| m.opened_file_path().map(str::to_owned)),
        Some("gone.txt".to_owned())
    );
    assert_eq!(repo.state().read_blob_calls, 2);
}

#[gpui::test]
async fn refresh_follows_head_while_browsing_the_working_tree(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    select_working_tree(cx, &model).await;
    let events = events(cx, &model);
    model.update(cx, BrowserModel::refresh_refs).await;
    cx.run_until_parked();
    assert_eq!(
        count(&events, &BrowserEvent::RootChanged),
        0,
        "an unmoved HEAD does not reload"
    );
    let moved = oid(0x66);
    {
        let mut state = repo.state();
        state.head = head(Some(moved));
        state.tree_of.insert(moved, oid(0x67));
        state.trees.insert(oid(0x67), Vec::new());
        state.listing = RefListing {
            refs: vec![branch("main", moved, RefKind::LocalBranch)],
            broken: Vec::new(),
        };
    }
    model.update(cx, BrowserModel::refresh_refs).await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert!(m.browsing_working_tree(), "the pseudo-ref stays selected");
        assert_eq!(m.root_tree(), Some(oid(0x67)));
    });
    assert_eq!(count(&events, &BrowserEvent::RootChanged), 1);
}

fn orphan(repo: &FakeRepository) {
    repo.state().head = Head {
        ref_name: Some("refs/heads/orphan".to_owned()),
        target: None,
        exact_ref_name: true,
    };
}

#[gpui::test]
async fn refresh_to_an_unborn_head_leaves_the_working_tree(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    attach(cx, &model, &repo).await;
    select_working_tree(cx, &model).await;
    let events = events(cx, &model);
    orphan(&repo);
    model.update(cx, BrowserModel::refresh_refs).await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert!(
            !m.browsing_working_tree(),
            "an unborn HEAD has no working tree"
        );
        assert_eq!(
            m.selected_ref().unwrap().short_name,
            "main",
            "falls back the way attach chooses"
        );
        assert_eq!(m.root_tree(), Some(oid(MAIN_TREE)));
    });
    assert!(count(&events, &BrowserEvent::RefsChanged) >= 1);
}

#[gpui::test]
async fn refresh_to_an_unborn_head_with_no_refs_clears_the_root(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    checkout(&repo);
    attach(cx, &model, &repo).await;
    select_working_tree(cx, &model).await;
    let events = events(cx, &model);
    orphan(&repo);
    repo.state().listing = RefListing::default();
    model.update(cx, BrowserModel::refresh_refs).await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert!(!m.browsing_working_tree());
        assert!(m.selected_ref().is_none());
        assert!(m.root_tree().is_none());
    });
    assert_eq!(count(&events, &BrowserEvent::RefsChanged), 1);
    assert_eq!(count(&events, &BrowserEvent::RootChanged), 1);
}

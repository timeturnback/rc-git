//! Ported from `ContentSearchTests.swift`: the pure parts, the batched search over a fake
//! repository (`ContentSearchModelTests`, now on `search_blobs` and `search_worktree` rather
//! than through the browser model), and the panel session. The fake's search-call log carries
//! the assertions, since reading each file once is the job.
#![expect(clippy::unwrap_used, reason = "the helpers build fixed valid inputs")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_core::{EntryKind, FileSearch, LineMatch, TreeEntry, WorktreeFile, WorktreeFileList};
use rc_git_kit::content_search::{
    ContentLineMatch, ContentReport, ContentSearchEvent, ContentSearchHit, ContentSearchProgress,
    ContentSearchRequest, ContentSearchRow, ContentSearchSession, ContentSearchStatus,
    MAX_MATCHED_LINES, search_blobs, search_worktree, searchable, status_text,
};
use rc_git_kit::fake::{FakeRepository, oid};
use rc_git_kit::oneshot;
use rc_git_kit::path_search::{PathIndex, PathIndexEntry};
use rc_git_kit::repository::{RepositoryError, RepositoryReading};

/// One matching line at `line`, the query found at byte `column`.
fn found(line: usize) -> LineMatch {
    LineMatch {
        line,
        column: 0,
        excerpt: b"match".to_vec(),
        ranges: std::iter::once(0..5).collect(),
    }
}

fn hit_line(line: usize) -> ContentLineMatch {
    ContentLineMatch::new(found(line), 5)
}

fn searched(lines: Vec<LineMatch>, truncated: bool) -> FileSearch {
    FileSearch::Searched { lines, truncated }
}

#[test]
fn lossy_excerpts_and_ranges_outside_them_get_no_highlight() {
    let lossy = ContentLineMatch::new(
        LineMatch {
            line: 0,
            column: 0,
            excerpt: vec![0x61, 0xFF],
            ranges: std::iter::once(0..1).collect(),
        },
        1,
    );
    assert_eq!(lossy.excerpt, "a\u{FFFD}");
    assert!(lossy.highlights.is_empty());
    let outside = ContentLineMatch::new(
        LineMatch {
            line: 0,
            column: 0,
            excerpt: b"abc".to_vec(),
            ranges: vec![1..2, 2..9],
        },
        1,
    );
    assert_eq!(outside.highlights.len(), 1);
    assert_eq!(outside.highlights[0], 1..2);
    // é is two bytes: a range that cuts it is dropped rather than shown misaligned.
    let cut = ContentLineMatch::new(
        LineMatch {
            line: 0,
            column: 0,
            excerpt: "é x".as_bytes().to_vec(),
            ranges: vec![1..3, 3..4],
        },
        1,
    );
    assert_eq!(cut.highlights.len(), 1);
    assert_eq!(cut.highlights[0], 3..4);
}

#[test]
fn status_says_how_far_a_search_got() {
    let mut progress = ContentSearchProgress::new(0, false);
    assert_eq!(status_text(&progress, false), "Searching…");
    progress.total_files = 2_500;
    progress.searched_files = 1_000;
    assert_eq!(
        status_text(&progress, false),
        "Searching 1,000 of 2,500 files…"
    );
    assert_eq!(status_text(&progress, true), "No matches");
}

#[test]
fn the_finished_status_names_totals_and_what_was_left_out() {
    let mut progress = ContentSearchProgress::new(10, true);
    progress.matched_lines = 1;
    progress.matched_files = 1;
    progress.stopped_at_limit = true;
    progress.skipped_files = 2;
    progress.unreadable_files = 1;
    assert_eq!(
        status_text(&progress, true),
        "1 line in 1 file · stopped at 2,000 lines · only the first 200,000 entries searched \
         · 2 files binary or over 1 MB skipped · 1 file unreadable"
    );
    let mut progress = ContentSearchProgress::new(10, false);
    progress.matched_lines = 3;
    progress.matched_files = 2;
    assert_eq!(status_text(&progress, true), "3 lines in 2 files");
}

const TREE: u8 = 0x12;
const SRC: u8 = 0x30;
const SHARED: u8 = 0x40;
const OTHER: u8 = 0x41;
const HIDDEN: u8 = 0x42;

fn entry(name: &str, seed: u8, kind: EntryKind) -> TreeEntry {
    TreeEntry {
        name: name.into(),
        oid: oid(seed),
        kind,
    }
}

/// The Swift suite's tree: `src/copy.md` and `README.md` share one blob, plus a symlink and
/// a hidden file.
fn tree_repo() -> FakeRepository {
    let repo = FakeRepository::new();
    let mut state = repo.state();
    state.trees.insert(
        oid(TREE),
        vec![
            entry("src", SRC, EntryKind::Tree),
            entry("README.md", SHARED, EntryKind::Blob),
            entry("link", OTHER, EntryKind::Symlink),
            entry(".DS_Store", HIDDEN, EntryKind::Blob),
        ],
    );
    state.trees.insert(
        oid(SRC),
        vec![
            entry("copy.md", SHARED, EntryKind::Blob),
            entry("main.rs", OTHER, EntryKind::Blob),
        ],
    );
    drop(state);
    repo
}

fn request(repo: &FakeRepository, include_hidden: bool) -> ContentSearchRequest {
    let cancel = AtomicBool::new(false);
    let index = PathIndex::build(oid(TREE), PathIndex::MAX_ENTRIES, &cancel, |tree| {
        repo.tree_entries(tree)
    })
    .unwrap();
    ContentSearchRequest {
        query: "match".to_owned(),
        files: searchable(&index.entries, include_hidden),
        index_truncated: index.truncated,
    }
}

async fn run_blobs(
    cx: &mut TestAppContext,
    repo: &FakeRepository,
    request: ContentSearchRequest,
) -> (Vec<String>, Result<ContentSearchProgress, RepositoryError>) {
    cx.executor().allow_parking();
    let mut paths = Vec::new();
    let shared: Arc<dyn RepositoryReading> = Arc::new(repo.clone());
    let outcome = search_blobs(&cx.executor(), shared, request, |hits, _| {
        paths.extend(hits.into_iter().map(|hit| hit.entry.path));
    })
    .await;
    (paths, outcome)
}

#[gpui::test]
async fn each_blob_is_read_once_and_hits_arrive_in_file_order(cx: &mut TestAppContext) {
    let repo = tree_repo();
    repo.state()
        .blob_searches
        .insert(oid(SHARED), searched(vec![found(0)], false));
    let (paths, progress) = run_blobs(cx, &repo, request(&repo, false)).await;
    let progress = progress.unwrap();
    assert_eq!(paths, ["src/copy.md", "README.md"]);
    assert_eq!(
        repo.state().search_blob_calls,
        [vec![oid(SHARED), oid(OTHER)]],
        "two paths share one blob; the symlink and the hidden file are left out"
    );
    assert_eq!(progress.searched_files, 3);
    assert_eq!(progress.matched_files, 2);
    assert_eq!(progress.matched_lines, 2);
}

#[gpui::test]
async fn hidden_files_are_searched_while_shown(cx: &mut TestAppContext) {
    let repo = tree_repo();
    let (_, progress) = run_blobs(cx, &repo, request(&repo, true)).await;
    assert_eq!(
        repo.state().search_blob_calls,
        [vec![oid(SHARED), oid(OTHER), oid(HIDDEN)]]
    );
    assert_eq!(progress.unwrap().total_files, 4, ".DS_Store is in the list");
}

#[gpui::test]
async fn a_call_that_stops_at_its_budget_is_resumed_with_the_rest(cx: &mut TestAppContext) {
    let repo = tree_repo();
    repo.state().content_files_per_call = Some(1);
    repo.state()
        .blob_searches
        .insert(oid(OTHER), searched(vec![found(3)], false));
    let (paths, _) = run_blobs(cx, &repo, request(&repo, false)).await;
    assert_eq!(paths, ["src/main.rs"]);
    assert_eq!(
        repo.state().search_blob_calls,
        [vec![oid(SHARED), oid(OTHER)], vec![oid(OTHER)]]
    );
}

#[gpui::test]
async fn a_call_that_reads_nothing_fails_instead_of_looping(cx: &mut TestAppContext) {
    let repo = tree_repo();
    repo.state().content_files_per_call = Some(0);
    let (_, outcome) = run_blobs(cx, &repo, request(&repo, false)).await;
    assert!(
        outcome
            .unwrap_err()
            .message()
            .contains("read none of 2 files")
    );
    assert_eq!(repo.state().search_blob_calls.len(), 1);
}

#[gpui::test]
async fn the_search_stops_at_the_line_cap(cx: &mut TestAppContext) {
    let repo = tree_repo();
    repo.state().content_files_per_call = Some(1);
    let lines = (0..MAX_MATCHED_LINES).map(found).collect();
    repo.state()
        .blob_searches
        .insert(oid(SHARED), searched(lines, true));
    let (paths, progress) = run_blobs(cx, &repo, request(&repo, false)).await;
    assert_eq!(
        paths,
        ["src/copy.md"],
        "the file that reaches the cap is the last reported"
    );
    assert!(progress.unwrap().stopped_at_limit);
    assert_eq!(
        repo.state().search_blob_calls.len(),
        1,
        "nothing more is read"
    );
}

#[gpui::test]
async fn an_empty_query_searches_nothing(cx: &mut TestAppContext) {
    let repo = tree_repo();
    let mut empty = request(&repo, false);
    empty.query = String::new();
    let (_, outcome) = run_blobs(cx, &repo, empty).await;
    assert!(outcome.is_err());
    assert!(repo.state().search_blob_calls.is_empty());
}

#[gpui::test]
async fn skipped_and_unreadable_files_are_counted(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let repo = FakeRepository::new();
    let file = |path: &str, kind| WorktreeFile {
        path: path.into(),
        kind,
    };
    let list = WorktreeFileList {
        files: vec![
            file("a.bin", EntryKind::Blob),
            file("big.txt", EntryKind::Blob),
            file("gone.txt", EntryKind::Blob),
            file("link", EntryKind::Symlink),
        ],
        scanned: 4,
        truncated: true,
        ..WorktreeFileList::default()
    };
    let index = PathIndex::from_worktree_files(oid(TREE), &list);
    {
        let mut state = repo.state();
        state
            .worktree_searches
            .insert("a.bin".to_owned(), FileSearch::Binary);
        state.worktree_searches.insert(
            "big.txt".to_owned(),
            FileSearch::TooLarge { size: 9, limit: 1 },
        );
        state.worktree_searches.insert(
            "gone.txt".to_owned(),
            FileSearch::Unreadable {
                error: "missing".to_owned(),
            },
        );
    }
    let request = ContentSearchRequest {
        query: "match".to_owned(),
        files: searchable(&index.entries, false),
        index_truncated: index.truncated,
    };
    let shared: Arc<dyn RepositoryReading> = Arc::new(repo.clone());
    let mut reported = 0;
    let progress = search_worktree(&cx.executor(), shared, request, |hits, _| {
        reported += hits.len();
    })
    .await
    .unwrap();
    assert_eq!(reported, 0);
    assert_eq!(
        repo.state().search_worktree_calls,
        [vec![
            "a.bin".to_owned(),
            "big.txt".to_owned(),
            "gone.txt".to_owned()
        ]]
    );
    assert_eq!(progress.skipped_files, 2);
    assert_eq!(progress.unreadable_files, 1);
    assert!(progress.index_truncated);
    assert!(repo.state().search_blob_calls.is_empty(), "disk, not blobs");
}

fn index_entry() -> PathIndexEntry {
    PathIndexEntry::new(
        "a.txt".to_owned(),
        Some(oid(1)),
        EntryKind::Blob,
        false,
        true,
    )
}

fn hit(lines: usize) -> ContentSearchHit {
    ContentSearchHit {
        entry: index_entry(),
        lines: (0..lines).map(hit_line).collect(),
        truncated: false,
    }
}

type Opened = Rc<RefCell<Vec<Option<ContentLineMatch>>>>;

fn session(cx: &mut TestAppContext) -> (Entity<ContentSearchSession>, Opened) {
    let session = cx.new(|_| ContentSearchSession::new());
    let opened: Opened = Rc::default();
    let log = Rc::clone(&opened);
    cx.update(|cx| {
        cx.subscribe(&session, move |_, event, _| {
            if let ContentSearchEvent::Open { line, .. } = event {
                log.borrow_mut().push(line.clone());
            }
        })
        .detach();
    });
    (session, opened)
}

/// A provider that reports `hits` at once and finishes.
fn answering(hits: Vec<ContentSearchHit>) -> rc_git_kit::content_search::ContentSearchProvider {
    Rc::new(move |_, report: ContentReport, cx| {
        let mut progress = ContentSearchProgress::new(1, false);
        progress.searched_files = 1;
        report(hits.clone(), progress, cx);
        gpui::Task::ready(Ok(progress))
    })
}

#[gpui::test]
fn return_searches_a_new_query_and_opens_the_selection_of_the_one_on_screen(
    cx: &mut TestAppContext,
) {
    let (session, opened) = session(cx);
    session.update(cx, |s, _| s.set_provider(answering(vec![hit(2)])));
    assert!(session.update(cx, |s, cx| s.submit("match", None, cx)));
    cx.run_until_parked();
    session.read_with(cx, |s, _| {
        assert_eq!(
            s.rows(),
            [
                ContentSearchRow::File(0),
                ContentSearchRow::Line { file: 0, line: 0 },
                ContentSearchRow::Line { file: 0, line: 1 },
            ]
        );
        assert!(matches!(s.status(), ContentSearchStatus::Finished(_)));
    });
    assert!(
        !session.update(cx, |s, cx| s.submit("match", Some(2), cx)),
        "the same query opens"
    );
    assert_eq!(*opened.borrow(), [Some(hit_line(1))]);
    session.update(cx, |s, cx| s.open(0, cx));
    assert_eq!(
        *opened.borrow(),
        [Some(hit_line(1)), Some(hit_line(0))],
        "a file row opens at its first match"
    );
    session.update(cx, |s, cx| s.open(9, cx));
    assert_eq!(
        opened.borrow().len(),
        2,
        "a row past the list opens nothing"
    );
    assert!(
        session.update(cx, |s, cx| s.submit("other", Some(2), cx)),
        "an edited query searches again"
    );
}

#[gpui::test]
fn an_empty_query_clears_the_list_without_searching(cx: &mut TestAppContext) {
    let (session, _) = session(cx);
    let calls = Rc::new(RefCell::new(0));
    let counted = Rc::clone(&calls);
    session.update(cx, |s, _| {
        s.set_provider(Rc::new(move |_, _, _| {
            *counted.borrow_mut() += 1;
            gpui::Task::ready(Ok(ContentSearchProgress::default()))
        }));
    });
    assert!(!session.update(cx, |s, cx| s.search("", cx)));
    assert_eq!(*calls.borrow(), 0);
    assert_eq!(
        session.read_with(cx, |s, _| s.status()),
        ContentSearchStatus::Idle
    );
}

#[gpui::test]
fn a_failed_search_says_why(cx: &mut TestAppContext) {
    let (session, _) = session(cx);
    session.update(cx, |s, _| {
        s.set_provider(Rc::new(|_, _, _| {
            gpui::Task::ready(Err(RepositoryError::new("read blob: missing")))
        }));
    });
    session.update(cx, |s, cx| s.search("x", cx));
    cx.run_until_parked();
    assert_eq!(
        session.read_with(cx, |s, _| s.status()),
        ContentSearchStatus::Failed("read blob: missing".to_owned())
    );
}

#[gpui::test]
fn only_the_newest_query_fills_the_list(cx: &mut TestAppContext) {
    let (session, _) = session(cx);
    let (release, held) = oneshot::channel::<()>();
    let held = Rc::new(RefCell::new(Some(held)));
    session.update(cx, |s, _| {
        s.set_provider(Rc::new(move |query, report: ContentReport, cx| {
            let gate = if query == "old" {
                held.borrow_mut().take()
            } else {
                None
            };
            let line = if query == "old" { 7 } else { 1 };
            // The old query's work outlives the session's own task, as a provider's detached
            // scan would: only the generation check keeps its late report out.
            cx.spawn(async move |cx| {
                if let Some(gate) = gate {
                    gate.await;
                }
                let found = ContentSearchHit {
                    entry: index_entry(),
                    lines: vec![hit_line(line)],
                    truncated: false,
                };
                let progress = ContentSearchProgress::new(1, false);
                cx.update(|cx| report(vec![found], progress, cx));
            })
            .detach();
            gpui::Task::ready(Ok(ContentSearchProgress::new(1, false)))
        }));
    });
    session.update(cx, |s, cx| s.search("old", cx));
    session.update(cx, |s, cx| s.search("new", cx));
    cx.run_until_parked();
    release.send(());
    cx.run_until_parked();
    session.read_with(cx, |s, _| {
        let lines: Vec<Vec<usize>> = s
            .hits()
            .iter()
            .map(|hit| hit.lines.iter().map(|line| line.line).collect())
            .collect();
        assert_eq!(lines, [vec![1]]);
        assert_eq!(s.searched_query(), Some("new"));
    });
}

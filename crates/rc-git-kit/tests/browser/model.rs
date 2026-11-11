//! Port of `BrowserModelTests.swift`: attaching, the tree cache, opening files and links,
//! blame, and the stale-load guards of the file and root streams.

use std::sync::Arc;

use gpui::TestAppContext;
use rc_git_core::{
    Blame, BlameRange, BlobContent, DEFAULT_BLOB_LIMIT, EntryKind, Head, RefKind, RefListing,
};
use rc_git_kit::browser::{
    BlobOpen, BrowserEvent, FileState, OpenPathOutcome, WorktreeFileOpen, initial_ref,
};
use rc_git_kit::fake::{FakeRepository, branch, commit_summary, oid, text_blob};
use rc_git_kit::highlighting::BYTE_LIMIT;
use rc_git_kit::models::BlameState;
use rc_git_kit::text_document::{FileReveal, HARD_LIMIT, TextDocument};

use crate::support::{
    arrived, attach, count, errors, events, file_state, gate, model, record, text, tree,
};

const MAIN_COMMIT: u8 = 0x11;
const MAIN_TREE: u8 = 0x12;
const FEATURE_COMMIT: u8 = 0x21;
const FEATURE_TREE: u8 = 0x22;
const SRC_TREE: u8 = 0x30;
const README: u8 = 0x40;
const BINARY: u8 = 0x41;
/// A name that arrived with U+FFFD replacements.
const INEXACT: &str = "f\u{FFFD}o";

fn repo() -> FakeRepository {
    let repo = FakeRepository::new();
    {
        let mut state = repo.state();
        state.listing = RefListing {
            refs: vec![
                branch("feature", oid(FEATURE_COMMIT), RefKind::LocalBranch),
                branch("main", oid(MAIN_COMMIT), RefKind::LocalBranch),
                branch("v1", oid(MAIN_COMMIT), RefKind::Tag),
            ],
            broken: Vec::new(),
        };
        state.head = Head {
            ref_name: Some("refs/heads/main".to_owned()),
            target: Some(oid(MAIN_COMMIT)),
            exact_ref_name: true,
        };
        state.tree_of.insert(oid(MAIN_COMMIT), oid(MAIN_TREE));
        state.tree_of.insert(oid(FEATURE_COMMIT), oid(FEATURE_TREE));
        state.trees.insert(
            oid(MAIN_TREE),
            vec![
                tree("src", oid(SRC_TREE), EntryKind::Tree),
                tree("README.md", oid(README), EntryKind::Blob),
                tree("logo.png", oid(BINARY), EntryKind::Blob),
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
        state.blobs.insert(oid(README), text_blob("# hi\n\nbody\n"));
        state
            .blobs
            .insert(oid(BINARY), BlobContent::Binary { size: 42 });
    }
    repo
}

fn stub_blame(repo: &FakeRepository, path: &str) {
    let mut state = repo.state();
    state.blames.insert(
        path.to_owned(),
        Blame::Ranges(vec![BlameRange {
            commit: oid(MAIN_COMMIT),
            start_line: 1,
            line_count: 1,
            boundary: false,
        }]),
    );
    state.summaries.insert(
        oid(MAIN_COMMIT),
        commit_summary(oid(MAIN_COMMIT), &[], "main"),
    );
}

#[gpui::test]
async fn attach_selects_head_branch_and_loads_its_root(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    let seen = record(cx, &model, BrowserEvent::RefsChanged, |model| {
        (
            model.refs().refs.len(),
            model.selected_ref().map(|r| r.short_name.clone()),
        )
    });
    let events = events(cx, &model);
    attach(cx, &model, &repo).await;
    model.read_with(cx, |model, _| {
        assert_eq!(model.refs().refs.len(), 3);
        assert_eq!(
            model.head().and_then(|h| h.ref_name.clone()).as_deref(),
            Some("refs/heads/main")
        );
        assert_eq!(model.selected_ref().unwrap().short_name, "main");
        assert_eq!(model.root_tree(), Some(oid(MAIN_TREE)));
        assert!(!model.refs_stale());
    });
    let last = seen.borrow().last().cloned().unwrap();
    assert_eq!(last.0, 3, "the last refs event sees the full listing");
    assert_eq!(last.1.as_deref(), Some("main"), "and the selection");
    assert_eq!(count(&events, &BrowserEvent::RootChanged), 1);
}

#[test]
fn initial_ref_falls_back_from_detached_head() {
    let listing = repo().state().listing.clone();
    let head = |name: Option<&str>, target| Head {
        ref_name: name.map(str::to_owned),
        target,
        exact_ref_name: true,
    };
    let pick = |listing: &RefListing, head: Head| initial_ref(listing, &head).map(|r| r.short_name);
    assert_eq!(
        pick(
            &listing,
            head(Some("refs/heads/feature"), Some(oid(FEATURE_COMMIT)))
        )
        .as_deref(),
        Some("feature")
    );
    assert_eq!(
        pick(&listing, head(None, Some(oid(MAIN_COMMIT)))).as_deref(),
        Some("main"),
        "first ref pointing at the same commit"
    );
    assert_eq!(
        pick(&listing, head(None, Some(oid(0x99)))).as_deref(),
        Some("feature"),
        "first local branch"
    );
    assert_eq!(pick(&RefListing::default(), head(None, None)), None);
    let no_local = RefListing {
        refs: vec![
            branch("origin/main", oid(FEATURE_COMMIT), RefKind::RemoteBranch),
            branch("v1", oid(MAIN_COMMIT), RefKind::Tag),
        ],
        broken: Vec::new(),
    };
    assert_eq!(
        pick(&no_local, head(None, None)).as_deref(),
        Some("origin/main"),
        "first ref of any kind"
    );
}

#[gpui::test]
async fn children_are_cached_by_tree_id_across_refs(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let first = model
        .update(cx, |m, cx| m.children(oid(SRC_TREE), cx))
        .await
        .unwrap();
    let second = model
        .update(cx, |m, cx| m.children(oid(SRC_TREE), cx))
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(repo.state().tree_entry_calls, 1, "served from the cache");

    let feature = model.read_with(cx, |m, _| m.refs().refs[0].clone());
    model.update(cx, |m, cx| m.select(feature, cx)).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.root_tree()),
        Some(oid(FEATURE_TREE))
    );
    model
        .update(cx, |m, cx| m.children(oid(SRC_TREE), cx))
        .await
        .unwrap();
    assert_eq!(
        repo.state().tree_entry_calls,
        1,
        "the same tree id under another ref is the same directory"
    );
    assert_eq!(model.read_with(cx, |m, _| m.trees().count()), 1);
}

#[gpui::test]
async fn children_errors_surface_as_repository_errors(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().fail_next_tree = true;
    let error = model
        .update(cx, |m, cx| m.children(oid(SRC_TREE), cx))
        .await
        .unwrap_err();
    assert!(error.message().contains("read tree"));
    assert!(
        model
            .read_with(cx, |m, _| m.trees().cached(oid(SRC_TREE)))
            .is_none(),
        "a failed load is not cached"
    );
}

#[gpui::test]
async fn path_search_reads_through_the_tree_cache_without_filling_it(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    model
        .update(cx, |m, cx| m.children(oid(MAIN_TREE), cx))
        .await
        .unwrap();
    let cached = model.read_with(cx, |m, _| m.trees().count());
    let result = model
        .update(cx, |m, cx| m.search_paths("", 100, cx))
        .await
        .unwrap();
    assert!(result.entries.iter().any(|e| e.path == "src/main.rs"));
    assert_eq!(
        model.read_with(cx, |m, _| m.trees().count()),
        cached,
        "the walk stores no listing"
    );
    assert!(
        model
            .read_with(cx, |m, _| m.trees().cached(oid(SRC_TREE)))
            .is_none()
    );
}

#[gpui::test]
async fn tree_cache_evicts_the_oldest_listings_past_its_child_limit(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    let limit = model.read_with(cx, |m, _| m.trees().child_limit());
    let half = limit / 2 + 1;
    let directories = [oid(0x60), oid(0x61), oid(0x62)];
    for directory in directories {
        let entries = (0..half)
            .map(|index| tree(&format!("f{index}"), oid(README), EntryKind::Blob))
            .collect();
        repo.state().trees.insert(directory, entries);
    }
    attach(cx, &model, &repo).await;
    for directory in directories {
        model
            .update(cx, |m, cx| m.children(directory, cx))
            .await
            .unwrap();
        assert!(model.read_with(cx, |m, _| m.trees().child_count()) <= limit);
    }
    assert_eq!(
        model.read_with(cx, |m, _| m.trees().count()),
        1,
        "two listings of over half the limit cannot share it"
    );
    assert!(
        model
            .read_with(cx, |m, _| m.trees().cached(directories[2]))
            .is_some()
    );
    let calls = repo.state().tree_entry_calls;
    model
        .update(cx, |m, cx| m.children(directories[0], cx))
        .await
        .unwrap();
    assert_eq!(
        repo.state().tree_entry_calls,
        calls + 1,
        "an evicted listing is read again"
    );
}

async fn open_path(
    cx: &mut TestAppContext,
    model: &gpui::Entity<rc_git_kit::browser::BrowserModel>,
    path: &str,
    fragment: Option<&str>,
) -> OpenPathOutcome {
    let fragment = fragment.map(str::to_owned);
    model
        .update(cx, |m, cx| m.open_path(path, fragment, cx))
        .await
}

#[gpui::test]
async fn open_path_walks_the_tree_and_reports_dead_links(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    assert_eq!(
        open_path(cx, &model, "src/main.rs", None).await,
        OpenPathOutcome::Opened
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.opened_file_path().map(str::to_owned)),
        Some("src/main.rs".to_owned())
    );
    let (entry, _) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry.name, "main.rs");
    for dead in ["src/missing.rs", "src", "src/main.rs/deeper"] {
        assert_eq!(
            open_path(cx, &model, dead, None).await,
            OpenPathOutcome::Missing,
            "{dead}"
        );
    }
    assert_eq!(
        model.read_with(cx, |m, _| m.opened_file_path().map(str::to_owned)),
        Some("src/main.rs".to_owned())
    );
}

#[gpui::test]
async fn open_path_that_fails_drops_its_fragment(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    assert_eq!(
        open_path(cx, &model, "README.md", Some("usage")).await,
        OpenPathOutcome::Opened
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.pending_reveal().cloned()),
        Some(FileReveal::Fragment("usage".to_owned())),
        "a link that opens keeps its fragment"
    );
    assert_eq!(
        open_path(cx, &model, "missing.md", Some("install")).await,
        OpenPathOutcome::Missing
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.pending_reveal().cloned()),
        None,
        "a dead link leaves no fragment behind"
    );
    assert_eq!(
        open_path(cx, &model, "src/main.rs", None).await,
        OpenPathOutcome::Opened
    );
    assert_eq!(model.update(cx, |m, _| m.take_pending_reveal()), None);
}

#[gpui::test]
async fn opening_another_file_clears_an_untaken_fragment(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    open_path(cx, &model, "README.md", Some("usage")).await;
    assert_eq!(
        model.update(cx, |m, _| m.take_pending_reveal()),
        Some(FileReveal::Fragment("usage".to_owned()))
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.pending_reveal().cloned()),
        None,
        "taking the fragment forgets it"
    );
    open_path(cx, &model, "README.md", Some("usage")).await;
    let entry = tree("README.md", oid(README), EntryKind::Blob);
    model
        .update(cx, |m, cx| {
            m.open_file(BlobOpen::new(&entry).at("README.md"), cx)
        })
        .await;
    assert_eq!(
        model.read_with(cx, |m, _| m.pending_reveal().cloned()),
        None,
        "a tree open is not the link's file"
    );
}

#[gpui::test]
async fn open_path_reports_a_failed_tree_read_apart_from_a_dead_link(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    open_path(cx, &model, "README.md", None).await;
    let before = model.read_with(cx, |m, _| m.location());
    let events = events(cx, &model);
    repo.state().fail_next_tree = true;
    let OpenPathOutcome::Failed(message) = open_path(cx, &model, "src/main.rs", None).await else {
        panic!("expected a failure");
    };
    assert!(
        message.contains("read tree"),
        "the core message names the operation"
    );
    assert_eq!(model.read_with(cx, |m, _| m.location()), before);
    assert_eq!(count(&events, &BrowserEvent::LocationChanged), 0);
}

#[gpui::test]
async fn highlights_and_links_stop_just_past_the_byte_limit(cx: &mut TestAppContext) {
    let model = model(cx);
    let head = "import a from './a';\n";
    let fill = BYTE_LIMIT - head.len();
    let at_limit = TextDocument::new(
        format!("{head}{}", " ".repeat(fill)).into_bytes(),
        &[0],
        true,
    );
    let over = TextDocument::new(
        format!("{head}{}", " ".repeat(fill + 1)).into_bytes(),
        &[0],
        true,
    );
    assert_eq!(at_limit.byte_count, BYTE_LIMIT);
    let store = model.read_with(cx, |m, _| m.highlight_store().clone());
    let spans = store
        .update(cx, |s, cx| {
            s.highlights("big.ts", Some(oid(0x51)), &at_limit, cx)
        })
        .await;
    assert!(spans.is_some());
    let links = store
        .update(cx, |s, cx| {
            s.import_links("big.ts", Some(oid(0x51)), &at_limit, cx)
        })
        .await;
    assert_eq!(links.map(|links| links.len()), Some(2));
    let spans = store
        .update(cx, |s, cx| {
            s.highlights("over.ts", Some(oid(0x52)), &over, cx)
        })
        .await;
    assert!(spans.is_none());
    let links = store
        .update(cx, |s, cx| {
            s.import_links("over.ts", Some(oid(0x52)), &over, cx)
        })
        .await;
    assert!(links.is_none());
}

#[gpui::test]
async fn load_anyway_reads_the_working_tree_file(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    let big = "x".repeat(usize::try_from(DEFAULT_BLOB_LIMIT).unwrap() + 1);
    repo.state()
        .worktree_contents
        .insert("big.txt".to_owned(), text_blob(&big));
    attach(cx, &model, &repo).await;
    model
        .update(cx, rc_git_kit::browser::BrowserModel::select_working_tree)
        .await;
    model
        .update(cx, |m, cx| {
            m.open_worktree_file(WorktreeFileOpen::new("big.txt", "big.txt"), cx)
        })
        .await;
    let FileState::TooLarge { size, limit, .. } = file_state(cx, &model) else {
        panic!("expected too large");
    };
    assert_eq!(limit, DEFAULT_BLOB_LIMIT);
    model.update(cx, |m, cx| m.load_anyway(size, cx)).await;
    let (entry, document) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry.name, "big.txt");
    assert_eq!(document.byte_count, big.len());
    assert_eq!(repo.state().read_blob_calls, 0, "the disk file is no blob");
}

#[gpui::test]
async fn load_anyway_never_passes_the_hard_limit(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    let huge = oid(0x7e);
    repo.state()
        .blobs
        .insert(huge, text_blob("stands for a file over the hard limit"));
    repo.state().blob_sizes.insert(huge, HARD_LIMIT + 1);
    attach(cx, &model, &repo).await;
    let entry = tree("huge.bin", huge, EntryKind::Blob);
    model
        .update(cx, |m, cx| m.open_file(BlobOpen::new(&entry), cx))
        .await;
    model
        .update(cx, |m, cx| m.load_anyway(HARD_LIMIT + 1, cx))
        .await;
    let FileState::TooLarge { size, limit, .. } = file_state(cx, &model) else {
        panic!("expected too large");
    };
    assert_eq!(size, HARD_LIMIT + 1);
    assert_eq!(limit, HARD_LIMIT);
}

#[gpui::test]
async fn open_file_produces_text_binary_and_too_large_states(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let readme = tree("README.md", oid(README), EntryKind::Blob);
    model
        .update(cx, |m, cx| m.open_file(BlobOpen::new(&readme), cx))
        .await;
    let (entry, document) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry, (&readme).into());
    assert_eq!(document.line_count, 3);

    let logo = tree("logo.png", oid(BINARY), EntryKind::Blob);
    model
        .update(cx, |m, cx| m.open_file(BlobOpen::new(&logo), cx))
        .await;
    assert_eq!(
        file_state(cx, &model),
        FileState::Binary((&logo).into(), 42)
    );

    model
        .update(cx, |m, cx| m.open_file(BlobOpen::new(&readme).limit(2), cx))
        .await;
    assert_eq!(
        file_state(cx, &model),
        FileState::TooLarge {
            entry: (&readme).into(),
            size: 11,
            limit: 2
        }
    );

    let missing = tree("gone", oid(0x77), EntryKind::Blob);
    model
        .update(cx, |m, cx| m.open_file(BlobOpen::new(&missing), cx))
        .await;
    let FileState::Failed(failed, message) = file_state(cx, &model) else {
        panic!("expected a failure");
    };
    assert_eq!(failed, (&missing).into());
    assert!(message.contains("read blob"));

    let src = tree("src", oid(SRC_TREE), EntryKind::Tree);
    model
        .update(cx, |m, cx| m.open_file(BlobOpen::new(&src), cx))
        .await;
    assert_eq!(
        file_state(cx, &model),
        FileState::Failed((&missing).into(), message),
        "directories are ignored"
    );
}

#[gpui::test]
async fn stale_file_load_is_dropped(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let slow = oid(0x50);
    let held = gate();
    repo.state().blobs.insert(slow, text_blob("slow"));
    repo.state().gates.insert(slow, Arc::clone(&held));
    let slow_entry = tree("slow.txt", slow, EntryKind::Blob);
    let fast_entry = tree("README.md", oid(README), EntryKind::Blob);
    let states = record(cx, &model, BrowserEvent::FileChanged, |m| {
        m.file_state().clone()
    });
    let slow_load = model.update(cx, |m, cx| m.open_file(BlobOpen::new(&slow_entry), cx));
    arrived(cx, &held).await;
    model
        .update(cx, |m, cx| m.open_file(BlobOpen::new(&fast_entry), cx))
        .await;
    held.open();
    slow_load.await;
    cx.run_until_parked();

    let (entry, _) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry, (&fast_entry).into(), "the later request wins");
    let states = states.borrow();
    assert!(states.contains(&FileState::Loading((&slow_entry).into())));
    assert!(
        !states
            .iter()
            .any(|state| text(state).is_some_and(|(entry, _)| entry.oid == Some(slow))),
        "the slow file never rendered"
    );
}

#[gpui::test]
async fn inexact_name_blame_is_refused_instead_of_walking_the_twin(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    stub_blame(&repo, INEXACT);
    let entry = rc_git_core::TreeEntry {
        name: b"f\xffo".as_slice().into(),
        oid: oid(README),
        kind: EntryKind::Blob,
    };
    model
        .update(cx, |m, cx| {
            m.open_file(BlobOpen::new(&entry).at(INEXACT), cx)
        })
        .await;
    model
        .update(cx, rc_git_kit::browser::BrowserModel::request_blame)
        .await;
    assert_eq!(
        model.read_with(cx, |m, _| m.blame().clone()),
        BlameState::Failed("cannot blame 'f\u{FFFD}o': the path is not valid UTF-8".to_owned())
    );
    assert!(
        repo.state().blame_limits.is_empty(),
        "the twin's ranges were never read"
    );
}

#[gpui::test]
async fn inexact_name_in_a_folder_refuses_blame_for_every_file_below(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let path = format!("{INEXACT}/main.rs");
    stub_blame(&repo, &path);
    let main = tree("main.rs", oid(README), EntryKind::Blob);
    model
        .update(cx, |m, cx| {
            m.open_file(BlobOpen::new(&main).at(path).inexact(), cx)
        })
        .await;
    assert!(!model.read_with(cx, |m, _| m.opened_path_is_exact()));
    model
        .update(cx, rc_git_kit::browser::BrowserModel::request_blame)
        .await;
    let BlameState::Failed(message) = model.read_with(cx, |m, _| m.blame().clone()) else {
        panic!("expected a refusal");
    };
    assert!(message.ends_with("the path is not valid UTF-8"));
    assert!(repo.state().blame_limits.is_empty());

    let readme = tree("README.md", oid(README), EntryKind::Blob);
    model
        .update(cx, |m, cx| {
            m.open_file(BlobOpen::new(&readme).at("README.md"), cx)
        })
        .await;
    assert!(
        model.read_with(cx, |m, _| m.opened_path_is_exact()),
        "an exact file opened later is blamed again"
    );
}

#[gpui::test]
async fn inexact_name_history_and_copy_are_refused(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let events = events(cx, &model);
    model
        .update(cx, |m, cx| m.show_path_history(INEXACT, false, cx))
        .await;
    assert_eq!(
        model.read_with(cx, |m, _| m.history_scope().clone()),
        rc_git_kit::back_forward::HistoryScope::All,
        "the scope never moved to the twin's path"
    );
    assert_eq!(
        model.update(cx, |m, cx| m.copyable_path(INEXACT, false, cx)),
        None
    );
    assert_eq!(
        model.update(cx, |m, cx| m.copyable_path("README.md", true, cx)),
        Some("README.md".to_owned())
    );
    cx.run_until_parked();
    assert_eq!(
        errors(&events),
        [
            "cannot show the history of 'f\u{FFFD}o': the path is not valid UTF-8",
            "cannot copy the path of 'f\u{FFFD}o': the path is not valid UTF-8",
        ]
    );
}

#[gpui::test]
async fn inexact_name_never_answers_a_link_by_path(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    repo.state().trees.insert(
        oid(MAIN_TREE),
        vec![
            rc_git_core::TreeEntry {
                name: b"f\xffo".as_slice().into(),
                oid: oid(BINARY),
                kind: EntryKind::Blob,
            },
            tree("README.md", oid(README), EntryKind::Blob),
        ],
    );
    attach(cx, &model, &repo).await;
    assert_eq!(
        open_path(cx, &model, INEXACT, None).await,
        OpenPathOutcome::Missing,
        "the U+FFFD spelling names no stored file"
    );
}

async fn open_with_path(
    cx: &mut TestAppContext,
    model: &gpui::Entity<rc_git_kit::browser::BrowserModel>,
    name: &str,
    path: &str,
) {
    let entry = tree(name, oid(README), EntryKind::Blob);
    let path = path.to_owned();
    model
        .update(cx, |m, cx| m.open_file(BlobOpen::new(&entry).at(path), cx))
        .await;
}

fn loading(path: &str) -> BlameState {
    BlameState::Loading {
        path: path.to_owned(),
        title: format!("Blaming {path}\u{2026}"),
    }
}

#[gpui::test]
async fn stale_blame_is_dropped_after_the_open_file_changes(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    stub_blame(&repo, "README.md");
    open_with_path(cx, &model, "README.md", "README.md").await;
    let states = record(cx, &model, BrowserEvent::BlameChanged, |m| {
        m.blame().clone()
    });
    let held = gate();
    repo.state().blame_gate = Some(Arc::clone(&held));
    let blame = model.update(cx, rc_git_kit::browser::BrowserModel::request_blame);
    arrived(cx, &held).await;
    open_with_path(cx, &model, "main.rs", "src/main.rs").await;
    held.open();
    blame.await;
    cx.run_until_parked();
    assert_eq!(
        *states.borrow(),
        [loading("README.md"), BlameState::Empty],
        "the README walk finished after the user opened another file"
    );
}

#[gpui::test]
async fn a_cancelled_blame_never_lands(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    stub_blame(&repo, "README.md");
    open_with_path(cx, &model, "README.md", "README.md").await;
    let states = record(cx, &model, BrowserEvent::BlameChanged, |m| {
        m.blame().clone()
    });
    let held = gate();
    repo.state().blame_gate = Some(Arc::clone(&held));
    let blame = model.update(cx, rc_git_kit::browser::BrowserModel::request_blame);
    arrived(cx, &held).await;
    model.update(cx, rc_git_kit::browser::BrowserModel::cancel_blame);
    held.open();
    blame.await;
    cx.run_until_parked();
    assert_eq!(
        *states.borrow(),
        [loading("README.md"), BlameState::Empty],
        "the walk finished after its window closed"
    );
    model
        .update(cx, rc_git_kit::browser::BrowserModel::request_blame)
        .await;
    assert_eq!(
        repo.state().blame_limits.len(),
        2,
        "a cancelled walk is no longer joined"
    );
}

#[gpui::test]
async fn a_superseded_blame_leaves_the_newer_walk_in_flight(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    stub_blame(&repo, "README.md");
    stub_blame(&repo, "src/main.rs");
    open_with_path(cx, &model, "README.md", "README.md").await;
    let states = record(cx, &model, BrowserEvent::BlameChanged, |m| {
        m.blame().clone()
    });
    let readme_gate = gate();
    repo.state().blame_gate = Some(Arc::clone(&readme_gate));
    let readme_blame = model.update(cx, rc_git_kit::browser::BrowserModel::request_blame);
    arrived(cx, &readme_gate).await;

    open_with_path(cx, &model, "main.rs", "src/main.rs").await;
    let main_gate = gate();
    repo.state().blame_gate = Some(Arc::clone(&main_gate));
    let main_blame = model.update(cx, rc_git_kit::browser::BrowserModel::request_blame);
    arrived(cx, &main_gate).await;
    readme_gate.open();
    readme_blame.await;
    cx.run_until_parked();
    assert_eq!(
        *states.borrow(),
        [loading("README.md"), loading("src/main.rs")],
        "the README walk was superseded by the main.rs request"
    );

    model
        .update(cx, rc_git_kit::browser::BrowserModel::request_blame)
        .await;
    main_gate.open();
    main_blame.await;
    cx.run_until_parked();
    let states = states.borrow();
    let BlameState::Loaded { path, spans, title } = states.last().cloned().unwrap() else {
        panic!("expected a loaded blame");
    };
    assert_eq!(path, "src/main.rs");
    assert_eq!(title, "Blame \u{2014} src/main.rs");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].commit.title, "main");
    assert_eq!(states.len(), 3);
    assert_eq!(
        repo.state().blame_limits.len(),
        2,
        "the repeat joined the main.rs walk"
    );
}

#[gpui::test]
async fn repeated_blame_requests_share_one_walk(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    stub_blame(&repo, "README.md");
    open_with_path(cx, &model, "README.md", "README.md").await;
    let events = events(cx, &model);
    let held = gate();
    repo.state().blame_gate = Some(Arc::clone(&held));
    let first = model.update(cx, rc_git_kit::browser::BrowserModel::request_blame);
    arrived(cx, &held).await;
    model
        .update(cx, rc_git_kit::browser::BrowserModel::request_blame)
        .await;
    model
        .update(cx, rc_git_kit::browser::BrowserModel::request_blame)
        .await;
    held.open();
    first.await;
    cx.run_until_parked();
    assert_eq!(repo.state().blame_limits.len(), 1);
    assert_eq!(
        count(&events, &BrowserEvent::BlameChanged),
        2,
        "one walk: loading, then loaded"
    );
    let BlameState::Loaded { path, spans, .. } = model.read_with(cx, |m, _| m.blame().clone())
    else {
        panic!("expected a loaded blame");
    };
    assert_eq!(path, "README.md");
    assert_eq!(spans.len(), 1);
}

#[gpui::test]
async fn stale_root_load_is_dropped(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let slow_commit = oid(0x60);
    let held = gate();
    {
        let mut state = repo.state();
        state.tree_of.insert(slow_commit, oid(0x61));
        state.trees.insert(oid(0x61), Vec::new());
        state.gates.insert(slow_commit, Arc::clone(&held));
    }
    let slow = model.update(cx, |m, cx| {
        m.select(branch("slow", slow_commit, RefKind::LocalBranch), cx)
    });
    arrived(cx, &held).await;
    let feature = model.read_with(cx, |m, _| m.refs().refs[0].clone());
    model.update(cx, |m, cx| m.select(feature, cx)).await;
    held.open();
    slow.await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.selected_ref().unwrap().short_name, "feature");
        assert_eq!(m.root_tree(), Some(oid(FEATURE_TREE)));
    });
}

#[gpui::test]
async fn selecting_a_ref_whose_tree_cannot_be_read_keeps_the_pane_consistent(
    cx: &mut TestAppContext,
) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    open_path(cx, &model, "README.md", None).await;
    let before = model.read_with(cx, |m, _| m.location());
    let events = events(cx, &model);
    let blob_tag = branch("blob", oid(0x70), RefKind::Tag);
    model.update(cx, |m, cx| m.select(blob_tag, cx)).await;
    cx.run_until_parked();
    let reported = errors(&events);
    assert_eq!(reported.len(), 1);
    assert!(reported[0].contains("expected a tree"));
    model.read_with(cx, |m, _| {
        assert_eq!(
            m.selected_ref().unwrap().short_name,
            "main",
            "the click does not take"
        );
        assert_eq!(m.root_tree(), Some(oid(MAIN_TREE)));
        assert_eq!(m.opened_file_path(), Some("README.md"));
        assert_eq!(m.location(), before);
    });

    let missing = commit_summary(oid(0x71), &[], "tree missing");
    model.update(cx, |m, cx| m.browse_commit(missing, cx)).await;
    cx.run_until_parked();
    assert_eq!(errors(&events).len(), 2);
    model.read_with(cx, |m, _| {
        assert!(m.browsed_commit().is_none());
        assert_eq!(m.selected_ref().unwrap().short_name, "main");
        assert_eq!(m.location(), before);
    });
}

#[gpui::test]
async fn attach_failure_reports_an_error(cx: &mut TestAppContext) {
    let model = model(cx);
    let broken = FakeRepository::new();
    let events = events(cx, &model);
    attach(cx, &model, &broken).await;
    cx.run_until_parked();
    assert!(model.read_with(cx, |m, _| m.selected_ref().is_none()));
    assert!(
        errors(&events).is_empty(),
        "an empty repository is no error"
    );

    broken.state().fail_next_refs = true;
    attach(cx, &model, &broken).await;
    cx.run_until_parked();
    assert_eq!(errors(&events), ["cannot list refs: transient lock"]);
}

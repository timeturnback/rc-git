//! A pull request's changes: the pure state machine, `PullRequestsModel` driving it over the
//! fake service, and the live service over real repositories and a loopback git server. No
//! fetch starts without the user's click; a head that moved shows no files; the diff is the
//! three-dot one, from the merge base to the stamp's head.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_core::{BlobDiff, ChangeKind, EntryKind, Oid, TreeChange};
use rc_git_kit::fake::{
    CallGate, FakePullRequests, linked, oid, pull_request, pull_request_detail,
};
use rc_git_kit::models::FileDiffState;
use rc_git_kit::platform::api_tokens::{ApiTokenStore, MemoryApiTokenStore};
use rc_git_kit::platform::credentials::MemoryCredentialStore;
use rc_git_kit::platform::host_keys::MemoryHostKeyStore;
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::pull_requests::changes::{
    ChangesLoad, ChangesPhase, ChangesRead, ChangesState, FetchProblem, HeadFetched,
    MERGE_BASE_SCAN, read_changes,
};
use rc_git_kit::pull_requests::live::LivePullRequests;
use rc_git_kit::pull_requests::model::PullRequestsModel;
use rc_git_kit::pull_requests::polling::DETAIL_INTERVAL;
use rc_git_kit::pull_requests::state::DetailState;
use rc_git_kit::pull_requests::{
    Cancel, CheckList, PullRequestService, PullRequestStamp, PullRequestState,
};
use rc_git_kit::repository::{LiveRepository, RepositoryReading};
use rc_git_testkit::{Fixture, HttpGitServer};

const REPO: &str = "/work/rc-git";

fn stamp(number: u64, head: Oid) -> PullRequestStamp {
    PullRequestStamp {
        number,
        head_oid: head,
        base_ref: "main".to_owned(),
        state: PullRequestState::Open,
    }
}

fn change(path: &str) -> TreeChange {
    TreeChange {
        kind: ChangeKind::Modified,
        path: path.into(),
        source_path: None,
        old: Some(oid(1)),
        new: Some(oid(2)),
        entry: EntryKind::Blob,
        old_entry: Some(EntryKind::Blob),
    }
}

fn files(base: Oid, paths: &[&str]) -> ChangesRead {
    ChangesRead::Files {
        base,
        tracking: "refs/remotes/origin/main".to_owned(),
        files: paths.iter().map(|path| change(path)).collect(),
    }
}

fn load_generation(load: &ChangesLoad) -> u64 {
    match load {
        ChangesLoad::Read { generation, .. } | ChangesLoad::Fetch { generation, .. } => *generation,
    }
}

// --- the state machine ---

#[test]
fn a_missing_head_waits_for_fetch_and_a_fetched_one_is_read_again() {
    let shown = stamp(7, oid(7));
    let (mut state, read) = ChangesState::new(shown.clone());
    assert!(matches!(read, ChangesLoad::Read { .. }));
    assert_eq!(state.fetch(), None, "no fetch while reading");
    state.finish_read(load_generation(&read), &shown, Ok(ChangesRead::HeadMissing));
    assert_eq!(state.phase(), &ChangesPhase::NeedsFetch { problem: None });

    let fetch = state.fetch().unwrap();
    assert!(matches!(&fetch, ChangesLoad::Fetch { stamp, .. } if *stamp == shown));
    assert!(state.is_fetching());
    let again = state
        .finish_fetch(load_generation(&fetch), &shown, Ok(HeadFetched::Fetched))
        .unwrap();
    assert_eq!(state.phase(), &ChangesPhase::Reading);
    state.finish_read(
        load_generation(&again),
        &shown,
        Ok(files(oid(3), &["a.rs"])),
    );
    assert!(matches!(state.phase(), ChangesPhase::Ready { files, .. } if files.len() == 1));
    assert_eq!(state.fetch(), None, "nothing to fetch once ready");
}

#[test]
fn a_moved_head_shows_no_files_and_reads_nothing() {
    let shown = stamp(7, oid(7));
    let (mut state, read) = ChangesState::new(shown.clone());
    state.finish_read(load_generation(&read), &shown, Ok(ChangesRead::HeadMissing));
    let fetch = state.fetch().unwrap();
    let next = state.finish_fetch(
        load_generation(&fetch),
        &shown,
        Ok(HeadFetched::Moved { found: oid(9) }),
    );
    assert_eq!(
        next, None,
        "a moved head is never read as the pull request's"
    );
    assert_eq!(state.phase(), &ChangesPhase::Moved { found: oid(9) });
    assert_eq!(state.select(0), None);
}

#[test]
fn answers_for_another_load_or_stamp_are_dropped() {
    let shown = stamp(7, oid(7));
    let (mut state, read) = ChangesState::new(shown.clone());
    let generation = load_generation(&read);
    let other = stamp(7, oid(8));
    state.finish_read(generation, &other, Ok(files(oid(3), &["x.rs"])));
    state.finish_read(generation + 1, &shown, Ok(files(oid(3), &["x.rs"])));
    assert_eq!(state.phase(), &ChangesPhase::Reading);
    state.finish_read(generation, &shown, Ok(ChangesRead::HeadMissing));

    let fetch = state.fetch().unwrap();
    assert!(state.cancel_fetch());
    assert_eq!(state.phase(), &ChangesPhase::NeedsFetch { problem: None });
    let late = state.finish_fetch(load_generation(&fetch), &shown, Ok(HeadFetched::Fetched));
    assert_eq!(late, None, "a cancelled fetch's answer is dropped");
    assert_eq!(state.phase(), &ChangesPhase::NeedsFetch { problem: None });

    let fetch = state.fetch().unwrap();
    let failed = FetchProblem::Failed("host refused".to_owned());
    state.finish_fetch(load_generation(&fetch), &shown, Err(failed));
    assert_eq!(
        state.phase(),
        &ChangesPhase::NeedsFetch {
            problem: Some("host refused".to_owned())
        }
    );
}

#[test]
fn a_file_diff_is_taken_only_for_the_selection_on_screen() {
    let shown = stamp(7, oid(7));
    let (mut state, read) = ChangesState::new(shown.clone());
    state.finish_read(
        load_generation(&read),
        &shown,
        Ok(files(oid(3), &["a.rs", "b.rs"])),
    );
    let first = state.select(0).unwrap();
    let second = state.select(1).unwrap();
    let diff = BlobDiff::Text { hunks: Vec::new() };
    state.finish_file(&first, Ok(diff.clone()));
    assert!(matches!(state.file_diff(), FileDiffState::Loading(c) if c.path == "b.rs"));
    state.finish_file(&second, Ok(diff));
    assert!(matches!(state.file_diff(), FileDiffState::Loaded(c, _) if c.path == "b.rs"));
    assert_eq!(state.select(5), None);

    let mut submodule = change("vendor/lib");
    submodule.entry = EntryKind::Submodule;
    submodule.old_entry = Some(EntryKind::Submodule);
    let (mut state, read) = ChangesState::new(shown.clone());
    let read_files = ChangesRead::Files {
        base: oid(3),
        tracking: "refs/remotes/origin/main".to_owned(),
        files: vec![submodule],
    };
    state.finish_read(load_generation(&read), &shown, Ok(read_files));
    assert_eq!(state.select(0), None, "a submodule settles without a read");
    assert!(matches!(state.file_diff(), FileDiffState::Submodule(_)));
}

#[test]
fn the_words_name_the_heads_and_neutralize_a_file_name() {
    use rc_git_kit::pull_requests::copy::{changed_file_label, changes_note};
    let shown = stamp(7, oid(0xab));
    let needs = changes_note(
        &ChangesPhase::NeedsFetch { problem: None },
        &shown,
        "origin",
    );
    assert!(
        needs.contains("refs/rc-git/pr/7/head") && needs.contains("abababa"),
        "{needs}"
    );
    let moved = changes_note(&ChangesPhase::Moved { found: oid(0xcd) }, &shown, "origin");
    assert!(
        moved.contains("cdcdcdc") && moved.contains("Nothing was fetched"),
        "{moved}"
    );
    let mut trick = change("placeholder");
    trick.path = "evil\u{202e}txt.exe\n".into();
    assert_eq!(changed_file_label(&trick), "evil\\u{202e}txt.exe\\u{a}");
}

// --- the model over the fake ---

struct Harness {
    fake: FakePullRequests,
    model: Entity<PullRequestsModel>,
}

fn harness(cx: &mut TestAppContext) -> Harness {
    cx.executor().allow_parking();
    let fake = FakePullRequests::new();
    let service: Arc<dyn PullRequestService> = Arc::new(fake.clone());
    let model = cx.new(|_| PullRequestsModel::new(service, Arc::new(|| 1_000)));
    Harness { fake, model }
}

impl Harness {
    fn busy(&self, cx: &mut TestAppContext) -> bool {
        self.model.read_with(cx, |m, _| {
            let changes_busy = m.tabs().iter().any(|tab| {
                m.changes(tab.number()).is_some_and(|changes| {
                    matches!(
                        changes.phase(),
                        ChangesPhase::Reading | ChangesPhase::Fetching
                    ) || matches!(changes.file_diff(), FileDiffState::Loading(_))
                })
            });
            m.link().is_none()
                || m.list().is_loading()
                || m.tabs().iter().any(DetailState::is_loading)
                || changes_busy
        })
    }

    fn settle(&self, cx: &mut TestAppContext) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            if !self.busy(cx) {
                return;
            }
            assert!(Instant::now() < deadline, "the reads never ended");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Scripts pull request 7 with head `head`.
    fn detail(&self, head: Oid) {
        let detail = pull_request_detail(pull_request(7, "Ship", head), "", CheckList::default());
        self.fake.state().details.insert(7, Ok(detail));
    }

    fn script_changes(&self, head: Oid, answers: Vec<Result<ChangesRead, String>>) {
        self.fake
            .state()
            .changes
            .insert(head, answers.into_iter().collect());
    }

    async fn open(&self, cx: &mut TestAppContext) {
        self.model
            .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
            .await;
        self.settle(cx);
        self.model.update(cx, |m, cx| m.open(7, cx));
        self.settle(cx);
    }

    fn phase(&self, cx: &mut TestAppContext) -> ChangesPhase {
        self.model.read_with(cx, |m, _| {
            m.selected_changes().map(|c| c.phase().clone()).unwrap()
        })
    }
}

#[gpui::test]
async fn a_local_head_shows_its_files_without_any_fetch(cx: &mut TestAppContext) {
    let h = harness(cx);
    h.detail(oid(7));
    h.script_changes(oid(7), vec![Ok(files(oid(3), &["src/lib.rs"]))]);
    h.fake.state().file_diffs.insert(
        "src/lib.rs".to_owned(),
        Ok(BlobDiff::Text { hunks: Vec::new() }),
    );
    h.open(cx).await;
    assert!(h.model.read_with(cx, |m, _| m.selected_changes().is_none()));
    h.model.update(cx, PullRequestsModel::show_changes);
    h.settle(cx);
    assert!(matches!(h.phase(cx), ChangesPhase::Ready { base, .. } if base == oid(3)));
    h.model.update(cx, |m, cx| m.select_changed_file(0, cx));
    h.settle(cx);
    h.model.read_with(cx, |m, _| {
        let changes = m.selected_changes().unwrap();
        assert!(
            matches!(changes.file_diff(), FileDiffState::Loaded(c, _) if c.path == "src/lib.rs")
        );
    });
    let state = h.fake.state();
    assert_eq!(state.fetch_calls.len(), 0);
    assert_eq!(state.changes_calls, vec![stamp(7, oid(7))]);
}

#[gpui::test]
async fn nothing_fetches_until_the_user_asks(cx: &mut TestAppContext) {
    let h = harness(cx);
    h.detail(oid(7));
    h.script_changes(
        oid(7),
        vec![Ok(ChangesRead::HeadMissing), Ok(files(oid(3), &["a.rs"]))],
    );
    h.fake
        .state()
        .fetches
        .insert(oid(7), Ok(HeadFetched::Fetched));
    h.open(cx).await;
    h.model.update(cx, PullRequestsModel::show_changes);
    h.settle(cx);
    assert_eq!(h.phase(cx), ChangesPhase::NeedsFetch { problem: None });
    for _ in 0..3 {
        h.model.update(cx, |m, cx| {
            m.show_changes(cx);
            m.refresh_detail(cx);
        });
        cx.executor().advance_clock(DETAIL_INTERVAL * 2);
        h.settle(cx);
    }
    assert_eq!(
        h.fake.state().fetch_calls.len(),
        0,
        "no fetch without the click"
    );

    h.model.update(cx, PullRequestsModel::fetch_changes);
    h.settle(cx);
    assert!(matches!(h.phase(cx), ChangesPhase::Ready { .. }));
    assert_eq!(h.fake.state().fetch_calls, vec![stamp(7, oid(7))]);
}

#[gpui::test]
async fn a_head_that_moved_shows_no_files_and_the_pull_request_is_read_again(
    cx: &mut TestAppContext,
) {
    let h = harness(cx);
    h.detail(oid(7));
    h.script_changes(oid(7), vec![Ok(ChangesRead::HeadMissing)]);
    h.script_changes(oid(9), vec![Ok(ChangesRead::HeadMissing)]);
    h.fake
        .state()
        .fetches
        .insert(oid(7), Ok(HeadFetched::Moved { found: oid(9) }));
    h.open(cx).await;
    h.model.update(cx, PullRequestsModel::show_changes);
    h.settle(cx);
    let details_before = h.fake.state().detail_calls.len();

    h.detail(oid(9));
    h.model.update(cx, PullRequestsModel::fetch_changes);
    h.settle(cx);
    let state = h.fake.state();
    assert_eq!(state.detail_calls.len(), details_before + 1, "read again");
    assert_eq!(
        state.changes_calls,
        vec![stamp(7, oid(7)), stamp(7, oid(9))],
        "no read of the old head after the fetch; the new read starts over"
    );
    drop(state);
    h.model.read_with(cx, |m, _| {
        let changes = m.selected_changes().unwrap();
        assert_eq!(changes.stamp().head_oid, oid(9));
        assert_eq!(changes.phase(), &ChangesPhase::NeedsFetch { problem: None });
    });
}

#[gpui::test]
async fn stop_shows_fetch_again_at_once_and_drops_the_late_answer(cx: &mut TestAppContext) {
    let h = harness(cx);
    h.detail(oid(7));
    h.script_changes(oid(7), vec![Ok(ChangesRead::HeadMissing)]);
    h.fake
        .state()
        .fetches
        .insert(oid(7), Ok(HeadFetched::Fetched));
    let gate = Arc::new(CallGate::new());
    h.fake.state().fetch_gate = Some(Arc::clone(&gate));
    h.open(cx).await;
    h.model.update(cx, PullRequestsModel::show_changes);
    h.settle(cx);
    h.model.update(cx, PullRequestsModel::fetch_changes);
    assert!(gate.wait_until_arrived());
    assert_eq!(h.phase(cx), ChangesPhase::Fetching);
    h.model.update(cx, PullRequestsModel::cancel_changes_fetch);
    assert_eq!(h.phase(cx), ChangesPhase::NeedsFetch { problem: None });
    gate.open();
    h.settle(cx);
    assert_eq!(h.phase(cx), ChangesPhase::NeedsFetch { problem: None });
    assert_eq!(
        h.fake.state().changes_calls.len(),
        1,
        "the late answer starts no read"
    );
}

// --- the live service over real repositories ---

/// An origin with `main` and a pull request branch `topic` that left `main` one commit before
/// its tip, served over HTTP with `topic` as `refs/pull/7/head`, and a clone made before the
/// pull request existed.
struct Served {
    origin: Fixture,
    _bare: Fixture,
    _server: HttpGitServer,
    clone: Fixture,
}

fn served() -> Served {
    let origin = Fixture::empty_dir();
    origin.git(&["init", "-q", "-b", "main"]);
    origin.write("shared.txt", b"one\n");
    origin.write("base-only.txt", b"base\n");
    origin.commit("root", "2024-01-01T00:00:00+0000");
    origin.write("shared.txt", b"two\n");
    origin.commit("main moves", "2024-01-02T00:00:00+0000");
    origin.git(&["checkout", "-q", "-b", "topic", "main~1"]);
    origin.write("topic.txt", b"topic\n");
    origin.write("shared.txt", b"one\ntopic\n");
    origin.commit("topic work", "2024-01-03T00:00:00+0000");
    origin.git(&["checkout", "-q", "main"]);
    origin.write("base-only.txt", b"base moved on\n");
    origin.commit("main moves again", "2024-01-04T00:00:00+0000");
    let (bare, bare_path) = origin.bare_copy();
    let target = bare_path.to_string_lossy().into_owned();
    let clone = Fixture::empty_dir();
    clone.git(&[
        "clone",
        "--quiet",
        "--no-local",
        "--single-branch",
        "-b",
        "main",
        &target,
        ".",
    ]);
    origin.git(&["push", "--quiet", &target, "topic:refs/pull/7/head"]);
    let server = HttpGitServer::serve(&bare_path);
    clone.git(&["remote", "set-url", "origin", &server.url()]);
    Served {
        origin,
        _bare: bare,
        _server: server,
        clone,
    }
}

fn live() -> LivePullRequests {
    let tokens: Arc<dyn ApiTokenStore> = Arc::new(MemoryApiTokenStore::new());
    LivePullRequests::new(Arc::new(MemorySettings::new()), tokens).with_git_credentials(
        Arc::new(MemoryCredentialStore::new()),
        Arc::new(MemoryHostKeyStore::new()),
    )
}

fn path_of(fixture: &Fixture) -> String {
    fixture.path().to_string_lossy().into_owned()
}

/// `git diff --name-only <base>...<head>` in `fixture`.
fn three_dot_names(fixture: &Fixture, base: &str, head: &str) -> Vec<String> {
    fixture
        .git_output(&["diff", "--name-only", &format!("{base}...{head}")])
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_live_service_fetches_on_request_then_diffs_from_the_merge_base() {
    let served = served();
    let service = live();
    let path = path_of(&served.clone);
    let head = Oid::from_hex(&served.origin.rev_parse("topic")).unwrap();
    let shown = stamp(7, head);
    assert_eq!(
        service.changes(&path, &linked(), &shown).unwrap(),
        ChangesRead::HeadMissing
    );

    let fetched = service.fetch_head(&path, &linked(), &shown, &Cancel::new());
    assert_eq!(fetched, Ok(HeadFetched::Fetched));
    let ChangesRead::Files {
        base,
        tracking,
        files,
    } = service.changes(&path, &linked(), &shown).unwrap()
    else {
        panic!("expected files");
    };
    assert_eq!(tracking, "refs/remotes/origin/main");
    let merge_base = served
        .clone
        .git_output(&["merge-base", "origin/main", &head.to_hex()]);
    assert_eq!(base.to_hex(), merge_base);
    let names: Vec<String> = files.iter().map(|change| change.path.to_string()).collect();
    assert_eq!(
        names,
        three_dot_names(&served.clone, "origin/main", &head.to_hex())
    );
    assert_eq!(
        names,
        ["shared.txt", "topic.txt"],
        "base-only.txt moved on main alone"
    );

    let shared = files.iter().find(|c| c.path == "shared.txt").unwrap();
    let diff = service.file_diff(&path, shared).unwrap();
    assert!(matches!(diff, BlobDiff::Text { hunks } if hunks.len() == 1));
}

#[test]
fn the_live_service_answers_moved_for_a_head_the_remote_no_longer_has() {
    let served = served();
    let path = path_of(&served.clone);
    let old = Oid::from_hex(&served.origin.rev_parse("main~1")).unwrap();
    let found = Oid::from_hex(&served.origin.rev_parse("topic")).unwrap();
    let fetched = live().fetch_head(&path, &linked(), &stamp(7, old), &Cancel::new());
    assert_eq!(fetched, Ok(HeadFetched::Moved { found }));
    assert!(!LiveRepository::open(&path).unwrap().has_object(found));
}

#[test]
fn the_live_service_reads_an_already_local_head_and_names_a_missing_base_copy() {
    let served = served();
    let path = path_of(&served.clone);
    served.clone.git(&["checkout", "-q", "-b", "mine"]);
    served.clone.write("mine.txt", b"mine\n");
    served.clone.commit("my work", "2024-01-05T00:00:00+0000");
    let head = Oid::from_hex(&served.clone.rev_parse("mine")).unwrap();
    served
        .clone
        .git(&["config", "branch.mine.remote", "origin"]);
    let read = live().changes(&path, &linked(), &stamp(7, head)).unwrap();
    assert!(
        matches!(&read, ChangesRead::Files { files, .. } if files.len() == 1),
        "{read:?}"
    );

    let mut elsewhere = stamp(7, head);
    elsewhere.base_ref = "release".to_owned();
    assert_eq!(
        live().changes(&path, &linked(), &elsewhere).unwrap(),
        ChangesRead::NoBaseCopy {
            branch: "release".to_owned(),
            tracking: None,
        },
        "a single-branch clone keeps no copy of release"
    );
}

#[test]
fn read_changes_walks_at_most_its_scan_and_reads_nothing_for_a_missing_head() {
    let served = served();
    let repository = LiveRepository::open(served.clone.path()).unwrap();
    let missing = stamp(7, oid(42));
    assert_eq!(
        read_changes(
            &repository,
            &missing,
            Some("refs/remotes/origin/main".to_owned())
        ),
        Ok(ChangesRead::HeadMissing)
    );
    let fake = rc_git_kit::fake::FakeRepository::default();
    let head = oid(7);
    {
        let mut state = fake.state();
        state.objects.insert(head);
        state
            .resolves
            .insert("refs/remotes/origin/main".to_owned(), oid(1));
        state.merge_bases.insert(
            (oid(1), head),
            rc_git_core::MergeBase::Incomplete { scanned: 12 },
        );
    }
    let read = read_changes(
        &fake,
        &stamp(7, head),
        Some("refs/remotes/origin/main".to_owned()),
    );
    assert_eq!(
        read,
        Ok(ChangesRead::NoMergeBase {
            tracking: "refs/remotes/origin/main".to_owned(),
            incomplete: true,
            scanned: 12,
        })
    );
    let state = fake.state();
    assert_eq!(
        state.merge_base_calls,
        vec![(oid(1), head, MERGE_BASE_SCAN)]
    );
    assert!(
        state.diff_tree_calls.is_empty(),
        "no files without a merge base"
    );
    let _ = Path::new(REPO);
}

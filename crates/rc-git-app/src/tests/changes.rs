//! The Changes mode and the sync and guard sheets against the fakes: a commit takes exactly the
//! ticked selection, a watcher burst costs one rescan, a credential prompt answered,
//! dismissed or dropped, a typed-phrase challenge that blocks until it matches, and Cancel Sync.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext, Entity, TestAppContext, VisualTestContext, WindowHandle};
use gpui_component::Root;
use rc_git_core::bstr::BString;
use rc_git_core::{
    BlobDiff, DiffHunk, DiffLine, DiffLineKind, EntryKind, LineEnd, StashEntry, StatusEntry,
    StatusReport, WorktreeStatus,
};
use rc_git_kit::changes::{ChangesModel, ChangesState};
use rc_git_kit::fake::{ActionCall, ActionKind, CallGate, FakeActions, FakeRepository, oid};
use rc_git_kit::guard_actions::{
    ActionError, Committed, Deleted, FetchOutcome, Identity, UpstreamStatus,
};
use rc_git_kit::guard_bridge::{CommitSelection, Credentials, HunkStamp};
use rc_git_kit::platform::api_tokens::MemoryApiTokenStore;
use rc_git_kit::platform::credentials::{CredentialStore, MemoryCredentialStore};
use rc_git_kit::platform::host_keys::MemoryHostKeyStore;
use rc_git_kit::platform::process::{Outcome, ScriptedRunner, Termination};
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::watch::ManualWatchers;
use rc_git_kit::sync::{SyncModel, SyncState, reconcile_command};

use crate::app::{Opened, Services, Shell};
use crate::changes::list::ChangeListView;
use crate::menus::{CancelSync, Fetch, ShowChanges};
use crate::sheets::OpenSheet;
use crate::sheets::credentials::CredentialSheet;
use crate::sheets::host_key::HostKeySheet;
use crate::tests::support::{MAIN_COMMIT, repo, wait_until};
use crate::window::RepoWindow;
use crate::window::sync::{COPY_COMMAND, OPEN_IN_TERMINAL, OpenRunner, TERMINAL_FAILURE_TITLE};
use rc_git_kit::changes::CommitAuthorModel;

const PATH: &str = "/nonexistent-rc-git-changes";

fn entry(path: &str, status: WorktreeStatus) -> StatusEntry {
    StatusEntry {
        path: BString::from(path),
        kind: EntryKind::Blob,
        status,
        oid: (status != WorktreeStatus::Untracked).then(|| oid(0x60)),
        size: 1,
    }
}

fn line(kind: DiffLineKind, text: &str) -> DiffLine {
    DiffLine {
        kind,
        text: BString::from(text),
        eol: LineEnd::Lf,
    }
}

fn hunk(start: u32) -> DiffHunk {
    DiffHunk {
        old_start: start,
        old_lines: 1,
        new_start: start,
        new_lines: 1,
        lines: vec![
            line(DiffLineKind::Removed, "old"),
            line(DiffLineKind::Added, "new"),
        ],
    }
}

/// [`repo`] with a checkout: three changed files, `a.txt` with two hunks, and one stash.
fn changes_repo() -> FakeRepository {
    let repo = repo();
    {
        let mut state = repo.state();
        state.work_dir = Some(PATH.into());
        state.status_report = StatusReport {
            entries: vec![
                entry("a.txt", WorktreeStatus::Modified),
                entry("b.txt", WorktreeStatus::Untracked),
                entry("c.txt", WorktreeStatus::Deleted),
            ],
            scanned: 3,
            ..StatusReport::default()
        };
        state.worktree_diffs.insert(
            "a.txt".to_owned(),
            BlobDiff::Text {
                hunks: vec![hunk(1), hunk(10)],
            },
        );
        state.stashes = vec![StashEntry {
            index: 0,
            oid: oid(0x70),
            message: "WIP on main".to_owned(),
        }];
    }
    repo
}

fn upstream() -> UpstreamStatus {
    UpstreamStatus {
        branch: "main".to_owned(),
        remote: "origin".to_owned(),
        upstream_ref: "refs/remotes/origin/main".to_owned(),
        url: "https://git.example.com/repo.git".to_owned(),
        host: "git.example.com".to_owned(),
        ahead: 0,
        behind: 0,
        has_upstream: true,
        published: true,
        uses_ssh: false,
    }
}

/// Fake actions that answer every call the Changes and sync tests make.
fn actions() -> FakeActions {
    let actions = FakeActions::new();
    {
        let mut state = actions.state();
        state.identity = Some(Identity {
            name: "Ann".to_owned(),
            email: "ann@example.com".to_owned(),
        });
        state.committed = Some(Committed {
            oid: oid(0x80),
            branch: "main".to_owned(),
            index_refreshed: true,
            reflog_written: true,
        });
        state.deleted = Some(Deleted {
            full_name: "refs/stash".to_owned(),
            backup_ref: None,
        });
        state.upstream = Some(upstream());
        state.fetched = Some(FetchOutcome {
            remote: "origin".to_owned(),
            updated_refs: Vec::new(),
        });
    }
    actions
}

struct Opened1 {
    window: Entity<RepoWindow>,
    handle: WindowHandle<Root>,
    repo: FakeRepository,
    actions: FakeActions,
    watchers: Arc<ManualWatchers>,
    credentials: Arc<MemoryCredentialStore>,
}

impl Opened1 {
    fn visual(&self, cx: &mut TestAppContext) -> VisualTestContext {
        VisualTestContext::from_window(self.handle.into(), cx)
    }

    fn changes(&self, cx: &TestAppContext) -> Entity<ChangesModel> {
        cx.read(|cx| {
            let window = self.window.read(cx);
            let parts = window
                .changes()
                .unwrap_or_else(|| panic!("no Changes models"));
            parts.model.clone()
        })
    }

    fn sync(&self, cx: &TestAppContext) -> Entity<SyncModel> {
        cx.read(|cx| self.window.read(cx).sync_model().cloned())
            .unwrap_or_else(|| panic!("no sync model"))
    }

    fn calls(&self, kind: ActionKind) -> Vec<ActionCall> {
        self.actions
            .state()
            .calls
            .iter()
            .filter(|call| call.kind() == kind)
            .cloned()
            .collect()
    }

    fn sheet(&self, cx: &TestAppContext) -> Option<OpenSheet> {
        cx.read(|cx| self.window.read(cx).sheet().cloned())
    }
}

/// Opens [`changes_repo`] over [`actions`] and shows Changes, waiting for the list.
fn open_changes(cx: &mut TestAppContext) -> Opened1 {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let repo = changes_repo();
    let actions = actions();
    let watchers = Arc::new(ManualWatchers::new());
    let credentials = Arc::new(MemoryCredentialStore::new());
    let (opener_repo, opener_actions) = (repo.clone(), actions.clone());
    let services = Services {
        settings: Arc::new(MemorySettings::new()),
        opener: Rc::new(move |path: &str| {
            if path != PATH {
                return Err(format!("{path} is not a repository"));
            }
            Ok(Opened {
                reading: Arc::new(opener_repo.clone()),
                key: format!("{path}/.git"),
                tab_path: path.to_owned(),
                dirs: Some((format!("{path}/.git"), format!("{path}/.git"))),
                actions: Some(Arc::new(opener_actions.clone())),
            })
        }),
        watchers: watchers.clone(),
        credentials: credentials.clone(),
        host_keys: Arc::new(MemoryHostKeyStore::new()),
        api_tokens: Arc::new(MemoryApiTokenStore::new()),
        home: "/Users/me".to_owned(),
        app_name: "rc-git".to_owned(),
    };
    let shell = cx.new(|cx| Shell::new(services, cx));
    let id = shell.update(cx, |shell, cx| shell.open(PATH, cx));
    assert!(id.is_some(), "the checkout opens");
    let tab = shell.read_with(cx, |shell, _| shell.windows().first().cloned());
    let tab = tab.unwrap_or_else(|| panic!("a window"));
    let (window, handle) = (tab.view, tab.handle);
    let opened = Opened1 {
        window,
        handle,
        repo,
        actions,
        watchers,
        credentials,
    };
    opened.visual(cx).dispatch_action(ShowChanges);
    let model = opened.changes(cx);
    wait_until(cx, |cx| {
        let model = model.clone();
        cx.read(|cx| matches!(model.read(cx).status_state(), ChangesState::Loaded(_)))
    });
    opened
}

#[gpui::test]
fn a_commit_sends_exactly_the_ticked_selections(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let parts = cx.read(|cx| {
        let window = opened.window.read(cx);
        let parts = window.changes().unwrap_or_else(|| panic!("no Changes"));
        (parts.list.clone(), parts.pane.clone(), parts.model.clone())
    });
    let (list, pane, model) = parts;
    assert_eq!(
        cx.read(|cx| list.read(cx).labels()),
        ["a.txt", "b.txt", "c.txt"]
    );
    let rebuilds = cx.read(|cx| list.read(cx).rebuilds());

    // Untick b.txt, show a.txt's diff and untick its first hunk.
    cx.update(|cx| list.update(cx, |list, cx| list.toggle_tick(1, cx)));
    let mut visual = opened.visual(cx);
    let press = gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: gpui::Point::default(),
        modifiers: gpui::Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    };
    list.update_in(&mut visual, |list, _, cx| list.press(0, &press, cx));
    let model_for_wait = model.clone();
    wait_until(cx, |cx| {
        let model = model_for_wait.clone();
        cx.read(|cx| {
            matches!(
                model.read(cx).diff_state(),
                rc_git_kit::changes::WorktreeDiffState::Loaded(..)
            )
        })
    });
    cx.update(|cx| pane.update(cx, |pane, cx| pane.toggle_hunk(0, cx)));
    assert!(cx.read(|cx| model.read(cx).is_partially_ticked("a.txt")));
    assert_eq!(
        cx.read(|cx| list.read(cx).rebuilds()),
        rebuilds,
        "a tick move redraws the rows it does not rebuild them"
    );

    let mut visual = opened.visual(cx);
    list.update_in(&mut visual, |list, window, cx| {
        list.set_message("Ship it", window, cx);
        list.commit(cx);
    });
    wait_until(cx, |_| !opened.calls(ActionKind::Commit).is_empty());
    let commits = opened.calls(ActionKind::Commit);
    let [
        ActionCall::Commit {
            message,
            selections,
            expected_head,
            ..
        },
    ] = commits.as_slice()
    else {
        panic!("one commit: {commits:?}");
    };
    assert_eq!(message, "Ship it");
    assert_eq!(*expected_head, Some(oid(MAIN_COMMIT)));
    assert_eq!(
        selections,
        &[
            CommitSelection::Hunks {
                path: "a.txt".to_owned(),
                hunks: vec![HunkStamp::from(&hunk(10))],
            },
            CommitSelection::Whole {
                path: "c.txt".to_owned(),
            },
        ]
    );
    wait_until(cx, |cx| cx.read(|cx| list.read(cx).message(cx).is_empty()));
}

#[gpui::test]
fn an_empty_message_is_refused_before_the_guard(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let list = cx.read(|cx| {
        let window = opened.window.read(cx);
        window.changes().map(|parts| parts.list.clone())
    });
    let list = list.unwrap_or_else(|| panic!("no list"));
    cx.update(|cx| list.update(cx, ChangeListView::commit));
    cx.run_until_parked();
    assert!(opened.calls(ActionKind::Commit).is_empty());
    assert!(cx.has_pending_prompt(), "the empty message is named");
}

#[gpui::test]
fn a_watcher_burst_costs_one_rescan(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let calls = opened.repo.state().status_calls;
    let gate = Arc::new(CallGate::new());
    opened.repo.state().status_gate = Some(gate.clone());
    let path = format!("{PATH}/a.txt");
    assert_eq!(
        opened.watchers.emit(&[&path]),
        1,
        "the worktree watcher fires"
    );
    cx.run_until_parked();
    assert!(gate.wait_until_arrived(), "the first rescan starts");
    for _ in 0..5 {
        opened.watchers.emit(&[&path]);
        cx.run_until_parked();
    }
    gate.open();
    let model = opened.changes(cx);
    wait_until(cx, |cx| {
        let model = model.clone();
        cx.read(|cx| !model.read(cx).is_scanning())
    });
    assert_eq!(
        opened.repo.state().status_calls,
        calls + 2,
        "a burst during one scan costs that scan and one more"
    );
}

#[gpui::test]
fn a_hidden_list_scans_nothing_until_shown(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    opened.visual(cx).dispatch_action(crate::menus::ShowFiles);
    cx.run_until_parked();
    let calls = opened.repo.state().status_calls;
    let path = format!("{PATH}/a.txt");
    for _ in 0..3 {
        opened.watchers.emit(&[&path]);
        cx.run_until_parked();
    }
    assert_eq!(opened.repo.state().status_calls, calls, "hidden: no scan");
    opened.visual(cx).dispatch_action(ShowChanges);
    let model = opened.changes(cx);
    wait_until(cx, |cx| {
        let model = model.clone();
        cx.read(|cx| !model.read(cx).is_scanning())
    });
    assert_eq!(
        opened.repo.state().status_calls,
        calls + 1,
        "shown: one catch-up scan"
    );
}

/// Starts a fetch whose first attempt the host refuses, and waits for the credentials sheet.
fn fetch_until_prompted(
    opened: &Opened1,
    cx: &mut TestAppContext,
) -> gpui::WeakEntity<crate::sheets::credentials::CredentialSheet> {
    opened.actions.state().fail_next.insert(
        ActionKind::Fetch,
        ActionError::AuthRequired {
            op: "fetch",
            host: "git.example.com".to_owned(),
        },
    );
    let sync = opened.sync(cx);
    wait_until(cx, |cx| {
        let sync = sync.clone();
        cx.read(|cx| sync.read(cx).remote_status().is_some())
    });
    opened.visual(cx).dispatch_action(Fetch);
    wait_until(cx, |cx| {
        matches!(opened.sheet(cx), Some(OpenSheet::Credentials(_)))
    });
    match opened.sheet(cx) {
        Some(OpenSheet::Credentials(sheet)) => sheet,
        _ => panic!("the credentials sheet is on screen"),
    }
}

#[gpui::test]
fn an_answered_credential_prompt_retries_with_it(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let sheet = fetch_until_prompted(&opened, cx);
    let sheet = sheet
        .upgrade()
        .unwrap_or_else(|| panic!("the sheet is alive"));
    let mut visual = opened.visual(cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        sheet.fill("", "", window, cx);
        sheet.submit(window, cx);
    });
    assert_eq!(
        cx.read(|cx| sheet.read(cx).problem()),
        Some("Enter a username."),
        "an empty sheet names what is missing and stays"
    );
    let mut visual = opened.visual(cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        sheet.fill("ann", "token", window, cx);
        sheet.submit(window, cx);
    });
    wait_until(cx, |_| opened.calls(ActionKind::Fetch).len() == 2);
    let fetches = opened.calls(ActionKind::Fetch);
    let Some(ActionCall::Fetch { remote }) = fetches.last() else {
        panic!("a retried fetch");
    };
    assert_eq!(
        remote.credentials,
        Some(Credentials {
            username: "ann".to_owned(),
            token: "token".to_owned(),
        })
    );
    let sync = opened.sync(cx);
    wait_until(cx, |cx| {
        let sync = sync.clone();
        cx.read(|cx| *sync.read(cx).state() == SyncState::Idle)
    });
    let stored = opened.credentials.credentials(
        "git.example.com",
        rc_git_kit::platform::credentials::CredentialKind::HttpsToken,
    );
    assert!(matches!(stored, Ok(Some(_))), "what worked is remembered");
}

#[gpui::test]
fn a_dropped_credential_prompt_fails_the_step_as_unavailable(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let sheet = fetch_until_prompted(&opened, cx);
    let mut visual = opened.visual(cx);
    visual.update(crate::sheets::close);
    for _ in 0..10 {
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
    }
    assert!(sheet.upgrade().is_none(), "the closed sheet is dropped");
    let sync = opened.sync(cx);
    wait_until(cx, |cx| {
        let sync = sync.clone();
        cx.read(|cx| matches!(sync.read(cx).state(), SyncState::Failed { .. }))
    });
    assert_eq!(opened.calls(ActionKind::Fetch).len(), 1, "no retry");
    let (title, _) = cx
        .pending_prompt()
        .unwrap_or_else(|| panic!("the failure is shown"));
    assert_eq!(title, rc_git_kit::guard_copy::SYNC_FAILURE_TITLE);
    cx.simulate_prompt_answer("OK");
    wait_until(cx, |cx| {
        let sync = sync.clone();
        cx.read(|cx| *sync.read(cx).state() == SyncState::Idle)
    });
}

#[gpui::test]
fn a_dismissed_credential_prompt_cancels_the_step(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let sheet = fetch_until_prompted(&opened, cx);
    let sheet = sheet
        .upgrade()
        .unwrap_or_else(|| panic!("the sheet is alive"));
    let mut visual = opened.visual(cx);
    sheet.update_in(&mut visual, CredentialSheet::cancel);
    let sync = opened.sync(cx);
    wait_until(cx, |cx| {
        let sync = sync.clone();
        cx.read(|cx| matches!(sync.read(cx).state(), SyncState::Failed { .. }))
    });
    assert_eq!(opened.calls(ActionKind::Fetch).len(), 1, "no retry");
}

#[gpui::test]
fn a_typed_phrase_challenge_blocks_until_it_matches(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let (list, model) = cx.read(|cx| {
        let window = opened.window.read(cx);
        let parts = window.changes().unwrap_or_else(|| panic!("no Changes"));
        (parts.list.clone(), parts.model.clone())
    });
    wait_until(cx, |cx| {
        let model = model.clone();
        cx.read(|cx| !model.read(cx).stashes().is_empty())
    });
    cx.update(|cx| {
        list.update(cx, |list, cx| {
            list.pick_stash(0, cx);
            list.drop_picked_stash(cx);
        });
    });
    wait_until(cx, |cx| {
        matches!(opened.sheet(cx), Some(OpenSheet::Challenge(_)))
    });
    let Some(OpenSheet::Challenge(sheet)) = opened.sheet(cx) else {
        panic!("the challenge is on screen");
    };
    let sheet = sheet
        .upgrade()
        .unwrap_or_else(|| panic!("the sheet is alive"));
    let mut visual = opened.visual(cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        sheet.type_phrase("dro", window, cx);
        assert!(!sheet.can_confirm(cx), "a partial phrase is refused");
        sheet.confirm(window, cx);
    });
    cx.run_until_parked();
    assert!(
        opened.calls(ActionKind::StashDrop).is_empty(),
        "nothing ran"
    );
    let mut visual = opened.visual(cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        sheet.type_phrase("drop", window, cx);
        assert!(sheet.can_confirm(cx));
        sheet.confirm(window, cx);
    });
    wait_until(cx, |_| !opened.calls(ActionKind::StashDrop).is_empty());
    let drops = opened.calls(ActionKind::StashDrop);
    let [ActionCall::StashDrop { entry, input }] = drops.as_slice() else {
        panic!("one drop: {drops:?}");
    };
    assert_eq!(entry.oid, oid(0x70));
    assert_eq!(input.as_deref(), Some("drop"));
}

#[gpui::test]
fn cancel_sync_idles_the_model(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let sync = opened.sync(cx);
    wait_until(cx, |cx| {
        let sync = sync.clone();
        cx.read(|cx| sync.read(cx).remote_status().is_some())
    });
    let gate = Arc::new(CallGate::new());
    opened
        .actions
        .state()
        .gates
        .insert(ActionKind::Fetch, gate.clone());
    opened.visual(cx).dispatch_action(Fetch);
    wait_until(cx, |_| opened.calls(ActionKind::Fetch).len() == 1);
    assert!(gate.wait_until_arrived(), "the fetch is running");
    assert!(cx.read(|cx| sync.read(cx).is_cancellable()));
    opened.visual(cx).dispatch_action(CancelSync);
    cx.run_until_parked();
    assert!(matches!(
        cx.read(|cx| sync.read(cx).state().clone()),
        SyncState::Cancelling(_)
    ));
    gate.open();
    wait_until(cx, |cx| {
        let sync = sync.clone();
        cx.read(|cx| *sync.read(cx).state() == SyncState::Idle)
    });
    assert!(!cx.has_pending_prompt(), "a cancel is not a failure");
    assert_eq!(opened.calls(ActionKind::Fetch).len(), 1);
    assert!(opened.calls(ActionKind::PullFastForward).is_empty());
}

#[gpui::test]
fn a_commit_with_nobody_to_record_asks_first_and_saves_the_answer(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    opened.actions.state().identity = None;
    let (list, author) = cx.read(|cx| {
        let window = opened.window.read(cx);
        let parts = window.changes().unwrap_or_else(|| panic!("no Changes"));
        (parts.list.clone(), parts.author.clone())
    });
    let author = author.unwrap_or_else(|| panic!("an author model"));
    cx.update(|cx| author.update(cx, CommitAuthorModel::refresh).detach());
    wait_until(cx, |cx| {
        let author = author.clone();
        cx.read(|cx| {
            author.read(cx).author() == Some(&rc_git_kit::commit_identity::CommitAuthor::Unset)
        })
    });
    let mut visual = opened.visual(cx);
    list.update_in(&mut visual, |list, window, cx| {
        list.set_message("First", window, cx);
        list.commit(cx);
    });
    wait_until(cx, |cx| {
        matches!(opened.sheet(cx), Some(OpenSheet::Identity(_)))
    });
    assert!(
        opened.calls(ActionKind::Commit).is_empty(),
        "nothing runs before the answer"
    );
    let Some(OpenSheet::Identity(sheet)) = opened.sheet(cx) else {
        panic!("the identity sheet is on screen");
    };
    let sheet = sheet
        .upgrade()
        .unwrap_or_else(|| panic!("the sheet is alive"));
    let mut visual = opened.visual(cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        sheet.fill("Ann", "ann@example.com", window, cx);
        sheet.save(window, cx);
    });
    wait_until(cx, |_| !opened.calls(ActionKind::Commit).is_empty());
    let commits = opened.calls(ActionKind::Commit);
    let [
        ActionCall::Commit {
            author: recorded, ..
        },
    ] = commits.as_slice()
    else {
        panic!("one commit: {commits:?}");
    };
    assert_eq!(
        recorded.fallback,
        Some(Identity {
            name: "Ann".to_owned(),
            email: "ann@example.com".to_owned(),
        })
    );
    wait_until(cx, |cx| {
        let author = author.clone();
        cx.read(|cx| author.read(cx).saved().is_some())
    });
}

#[gpui::test]
fn new_branch_asks_for_a_name_then_creates_it(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let entry =
        rc_git_kit::fake::branch("main", oid(MAIN_COMMIT), rc_git_core::RefKind::LocalBranch);
    let mut visual = opened.visual(cx);
    opened.window.update_in(&mut visual, |window, w, cx| {
        window.run_ref_action(
            rc_git_kit::ref_menu::RefMenuAction::NewBranch,
            &entry,
            w,
            cx,
        );
    });
    wait_until(cx, |cx| {
        matches!(opened.sheet(cx), Some(OpenSheet::RefName(_)))
    });
    let Some(OpenSheet::RefName(sheet)) = opened.sheet(cx) else {
        panic!("the name sheet is on screen");
    };
    let sheet = sheet
        .upgrade()
        .unwrap_or_else(|| panic!("the sheet is alive"));
    let mut visual = opened.visual(cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        sheet.fill("  ", window, cx);
        sheet.create(window, cx);
    });
    cx.run_until_parked();
    assert!(
        opened.calls(ActionKind::CreateBranch).is_empty(),
        "a blank name creates nothing"
    );
    let mut visual = opened.visual(cx);
    sheet.update_in(&mut visual, |sheet, window, cx| {
        sheet.fill(" topic ", window, cx);
        sheet.create(window, cx);
    });
    wait_until(cx, |_| !opened.calls(ActionKind::CreateBranch).is_empty());
    assert_eq!(
        opened.calls(ActionKind::CreateBranch),
        [ActionCall::CreateBranch {
            name: "topic".to_owned(),
            revspec: oid(MAIN_COMMIT).to_hex(),
        }]
    );
}

#[gpui::test]
fn a_trusted_host_key_retries_with_it(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    {
        let mut state = opened.actions.state();
        state.upstream = Some(UpstreamStatus {
            url: "git.example.com:repo.git".to_owned(),
            uses_ssh: true,
            ..upstream()
        });
        state.fail_next.insert(
            ActionKind::Fetch,
            ActionError::UnknownHostKey {
                op: "fetch",
                host: "git.example.com".to_owned(),
                key_type: "ssh-ed25519".to_owned(),
                fingerprint: "SHA256:abc".to_owned(),
            },
        );
    }
    let sync = opened.sync(cx);
    cx.update(|cx| sync.update(cx, SyncModel::refresh_status).detach());
    wait_until(cx, |cx| {
        let sync = sync.clone();
        cx.read(|cx| {
            sync.read(cx)
                .remote_status()
                .is_some_and(|status| status.uses_ssh)
        })
    });
    opened.visual(cx).dispatch_action(Fetch);
    wait_until(cx, |cx| {
        matches!(opened.sheet(cx), Some(OpenSheet::HostKey(_)))
    });
    let Some(OpenSheet::HostKey(sheet)) = opened.sheet(cx) else {
        panic!("the host-key sheet is on screen");
    };
    let sheet = sheet
        .upgrade()
        .unwrap_or_else(|| panic!("the sheet is alive"));
    let mut visual = opened.visual(cx);
    sheet.update_in(&mut visual, HostKeySheet::trust);
    wait_until(cx, |_| opened.calls(ActionKind::Fetch).len() == 2);
    let fetches = opened.calls(ActionKind::Fetch);
    let Some(ActionCall::Fetch { remote }) = fetches.last() else {
        panic!("a retried fetch");
    };
    assert_eq!(remote.accepted_host_key.as_deref(), Some("SHA256:abc"));
}

/// Pulls into a diverged branch and waits for the sheet that names the rebase.
fn pull_until_diverged(opened: &Opened1, cx: &mut TestAppContext) {
    opened.actions.state().fail_next.insert(
        ActionKind::PullFastForward,
        ActionError::Diverged {
            branch: "main".to_owned(),
            ahead: 1,
            behind: 1,
        },
    );
    let sync = opened.sync(cx);
    cx.update(|cx| sync.update(cx, SyncModel::pull).detach());
    wait_until(cx, |cx| cx.has_pending_prompt());
    let (title, _) = cx
        .pending_prompt()
        .unwrap_or_else(|| panic!("the diverged sheet is shown"));
    assert_eq!(title, rc_git_kit::guard_copy::DIVERGED_TITLE);
}

fn exited(status: i32, lines: &[&str]) -> Outcome {
    Outcome {
        status,
        termination: Termination::Exit,
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
        stopped: None,
    }
}

fn open_runner(cx: &mut TestAppContext) -> Arc<ScriptedRunner> {
    let runner = Arc::new(ScriptedRunner::new());
    let global = OpenRunner(runner.clone());
    cx.update(|cx| cx.set_global(global));
    runner
}

#[gpui::test]
fn open_in_terminal_copies_the_rebase_and_opens_the_checkout(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let runner = open_runner(cx);
    runner.push(exited(0, &[]));
    pull_until_diverged(&opened, cx);
    cx.simulate_prompt_answer(OPEN_IN_TERMINAL);
    wait_until(cx, |_| !runner.commands().is_empty());
    let commands = runner.commands();
    assert_eq!(commands[0].executable, "/usr/bin/open");
    assert_eq!(commands[0].arguments, ["-a", "Terminal", PATH]);
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some(reconcile_command(PATH).as_str()));
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt(), "an open that worked says nothing");
}

#[gpui::test]
fn a_terminal_that_does_not_open_is_reported(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let runner = open_runner(cx);
    runner.push(exited(1, &["Unable to find application named 'Terminal'"]));
    pull_until_diverged(&opened, cx);
    cx.simulate_prompt_answer(OPEN_IN_TERMINAL);
    wait_until(cx, |cx| cx.has_pending_prompt());
    let (title, detail) = cx
        .pending_prompt()
        .unwrap_or_else(|| panic!("the failure is shown"));
    assert_eq!(title, TERMINAL_FAILURE_TITLE);
    assert!(detail.contains("Unable to find application"), "{detail}");
}

#[gpui::test]
fn copy_command_opens_nothing(cx: &mut TestAppContext) {
    let opened = open_changes(cx);
    let runner = open_runner(cx);
    pull_until_diverged(&opened, cx);
    cx.simulate_prompt_answer(COPY_COMMAND);
    cx.run_until_parked();
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some(reconcile_command(PATH).as_str()));
    assert!(runner.commands().is_empty());
}

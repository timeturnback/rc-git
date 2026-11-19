//! Deleting a merged pull request's head branch from its tab: the control decided from the
//! guard's own blocker and the policy's protected names, the flow as a state machine, the words,
//! the model over the fake service (races ordered with `CallGate`s), and the live service
//! against the testkit's fake GitHub host and a fixture repository.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_kit::fake::{
    CallGate, FakePullRequests, delete_branch_plan, linked, merged_detail, oid, pull_request,
    pull_request_detail,
};
use rc_git_kit::github::accounts::{GitHubAccount, GitHubAccounts, SignInMethod};
use rc_git_kit::github::{ApiToken, GitHubHost, TokenSet};
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore, MemoryApiTokenStore};
use rc_git_kit::platform::settings::{MemorySettings, SettingsStore};
use rc_git_kit::pull_requests::copy::{
    branch_outcome_log, delete_branch_note, flow_note_alarms, flow_title,
};
use rc_git_kit::pull_requests::delete_branch::{
    BranchAsk, DeleteBranchControl, DeleteBranchFlow, DeleteBranchIntent, delete_branch_control,
    restore_destination,
};
use rc_git_kit::pull_requests::flow::{ActionStop, Ended, Phase, TabFlow};
use rc_git_kit::pull_requests::link::RepositoryLink;
use rc_git_kit::pull_requests::live::LivePullRequests;
use rc_git_kit::pull_requests::model::{FlowEvent, PullRequestsModel};
use rc_git_kit::pull_requests::state::DetailState;
use rc_git_kit::pull_requests::{
    ActionRefusal, BackupKeptStop, BranchAfter, BranchDeleteOutcome, BranchDeleteReport, CheckList,
    GitHubError, PullRequestDetail, PullRequestService, PullRequestStamp, PullRequestState,
    ReadError, Settled, ViewerAbilities, single_line,
};
use rc_git_testkit::Fixture;
use rc_git_testkit::github::{BranchScript, DeleteBehaviour, FakeToken, GitHubServer};

const REPO: &str = "/work/rc-git";

const REPOSITORY: &str = "octo/rc-git@github.com";

const BACKUP: &str = "refs/rc-git/backup/remote-branch/feature";

/// Pull request 7, merged at `oid(head)`, its head branch `feature` deletable.
fn detail(head: u8) -> PullRequestDetail {
    merged_detail(pull_request_detail(
        pull_request(7, "Ship it", oid(head)),
        "",
        CheckList::default(),
    ))
}

/// A tab whose last read answered `answer`.
fn tab(answer: Result<PullRequestDetail, ReadError>) -> DetailState {
    let mut tab = DetailState::new(7);
    let load = tab.refresh().unwrap();
    tab.finish(&load, answer, 1_000);
    tab
}

fn intent() -> DeleteBranchIntent {
    DeleteBranchIntent::new(REPO.to_owned(), &linked())
}

/// The report of a delete of `feature` at `oid(head)` that ended as `outcome`.
fn report(head: u8, outcome: BranchDeleteOutcome, local_branch: bool) -> BranchDeleteReport {
    BranchDeleteReport {
        branch: "feature".to_owned(),
        head_oid: oid(head),
        backup_ref: BACKUP.to_owned(),
        local_branch,
        outcome,
    }
}

#[test]
fn the_control_shows_only_for_a_merged_pull_request_s_own_deletable_branch() {
    assert_eq!(
        delete_branch_control(&DetailState::new(7), true),
        DeleteBranchControl::Hidden,
        "no current read"
    );
    let shown = tab(Ok(detail(7)));
    assert_eq!(
        delete_branch_control(&shown, true),
        DeleteBranchControl::Shown {
            branch: "feature".to_owned()
        }
    );
    assert_eq!(
        delete_branch_control(&shown, false),
        DeleteBranchControl::Hidden
    );
    let mut stale = tab(Ok(detail(7)));
    let load = stale.refresh().unwrap();
    stale.finish(&load, Err(ReadError::Failed("offline".to_owned())), 2_000);
    assert!(stale.detail().is_some(), "the old read stays on screen");
    assert_eq!(
        delete_branch_control(&stale, true),
        DeleteBranchControl::Hidden
    );
    for state in [PullRequestState::Open, PullRequestState::Closed] {
        let mut detail = detail(7);
        detail.summary.state = state;
        assert_eq!(
            delete_branch_control(&tab(Ok(detail)), true),
            DeleteBranchControl::Hidden,
            "{state:?}"
        );
    }
    let reason = |change: fn(&mut PullRequestDetail)| {
        let mut detail = detail(7);
        change(&mut detail);
        match delete_branch_control(&tab(Ok(detail)), true) {
            DeleteBranchControl::Unavailable { reason } => reason,
            other => panic!("{other:?}"),
        }
    };
    let fork = reason(|d| d.summary.cross_repository = true);
    assert!(fork.contains("another repository"), "{fork}");
    for no_right in [
        reason(|d| d.viewer = None),
        reason(|d| d.viewer = Some(ViewerAbilities::default())),
    ] {
        assert!(no_right.contains("may delete"), "{no_right}");
    }
    for protected in ["main", "MASTER", "release/1.0"] {
        let mut detail = detail(7);
        detail.summary.head_ref = protected.to_owned();
        let DeleteBranchControl::Unavailable { reason } =
            delete_branch_control(&tab(Ok(detail)), true)
        else {
            panic!("{protected} has a button");
        };
        assert!(reason.contains("protected branch"), "{reason}");
    }
    let mut hostile = detail(7);
    hostile.summary.head_ref = single_line("fea\u{202e}ture", 1024);
    let DeleteBranchControl::Unavailable { reason } =
        delete_branch_control(&tab(Ok(hostile)), true)
    else {
        panic!("an escaped name has a button");
    };
    assert!(
        reason.contains("cannot be typed") && !reason.contains('\u{202e}'),
        "{reason}"
    );
}

#[test]
fn a_delete_flow_takes_only_its_own_answers_and_sends_once_per_confirm() {
    let shown = detail(7);
    let (mut flow, load) =
        DeleteBranchFlow::begin(4, REPOSITORY.to_owned(), shown.stamp(), intent());
    let plan = delete_branch_plan(&shown);
    assert!(!flow.planned(3, Ok(plan.clone())), "another generation");
    assert_eq!(flow.phase(), &Phase::Planning);
    assert!(flow.planned(load.generation, Ok(plan.clone())));
    let send = flow.confirm("feature".to_owned()).unwrap();
    assert_eq!(send.typed, "feature", "the guard gets what was typed");
    assert_eq!(send.decision, plan.decision);
    assert_eq!(send.ask, intent());
    assert!(
        flow.confirm("feature".to_owned()).is_none(),
        "one send per confirm"
    );
    let deleted = report(
        7,
        BranchDeleteOutcome::Deleted {
            settled: Settled::Answer,
        },
        false,
    );
    assert!(!flow.finished(5, Ok(deleted.clone())));
    assert!(flow.finished(4, Ok(deleted)));

    let (mut followed, _) =
        DeleteBranchFlow::begin(9, REPOSITORY.to_owned(), shown.stamp(), intent());
    assert!(
        followed.follow(Some(&detail(8).stamp())),
        "another head ends it"
    );
    let Phase::Done(Ended::NotSent(stop)) = followed.phase() else {
        panic!("{:?}", followed.phase());
    };
    assert!(stop.is_changed());
    assert!(
        !followed.planned(9, Ok(plan)),
        "nothing lands after it ended"
    );
}

/// A delete flow of `detail(7)` that ended as `outcome`.
fn ended(outcome: BranchDeleteOutcome, local_branch: bool) -> DeleteBranchFlow {
    let shown = detail(7);
    let (mut flow, load) =
        DeleteBranchFlow::begin(1, REPOSITORY.to_owned(), shown.stamp(), intent());
    flow.planned(load.generation, Ok(delete_branch_plan(&shown)));
    flow.confirm("feature".to_owned()).unwrap();
    flow.finished(1, Ok(report(7, outcome, local_branch)));
    flow
}

#[test]
fn every_outcome_names_the_backup_ref_and_how_to_bring_the_branch_back() {
    let restore = format!("git push origin {BACKUP}:refs/heads/feature");
    let deleted = ended(
        BranchDeleteOutcome::Deleted {
            settled: Settled::Answer,
        },
        true,
    );
    let note = delete_branch_note(&deleted).unwrap();
    assert!(
        note.starts_with("Deleted \u{201c}feature\u{201d} on GitHub."),
        "{note}"
    );
    assert!(note.contains(BACKUP) && note.contains(&restore), "{note}");
    assert!(
        note.contains("still has a local branch named \u{201c}feature\u{201d}")
            && note.contains("sidebar"),
        "{note}"
    );
    assert!(!flow_note_alarms(&TabFlow::DeleteBranch(deleted.clone())));
    assert_eq!(flow_title(&TabFlow::DeleteBranch(deleted)), "Delete Branch");
    for outcome in [
        BranchDeleteOutcome::Refused {
            reason: "Reference does not exist".to_owned(),
        },
        BranchDeleteOutcome::NotDeleted {
            problem: "the connection dropped".to_owned(),
        },
        BranchDeleteOutcome::Unknown {
            problem: "the connection dropped".to_owned(),
            after: BranchAfter::Unread {
                reason: "offline".to_owned(),
            },
        },
    ] {
        let flow = ended(outcome.clone(), false);
        let note = delete_branch_note(&flow).unwrap();
        assert!(note.contains(BACKUP), "{outcome:?}: {note}");
        assert!(!note.contains("local branch"), "{note}");
        assert!(
            flow_note_alarms(&TabFlow::DeleteBranch(flow)),
            "{outcome:?}"
        );
    }
    let unknown = ended(
        BranchDeleteOutcome::Unknown {
            problem: "the connection dropped".to_owned(),
            after: BranchAfter::Changed { now: None },
        },
        false,
    );
    assert!(delete_branch_note(&unknown).unwrap().contains(&restore));
}

#[test]
fn the_restore_command_quotes_what_a_shell_would_read() {
    let shown = detail(7);
    let flow = {
        let (mut flow, load) = DeleteBranchFlow::begin(
            1,
            REPOSITORY.to_owned(),
            shown.stamp(),
            DeleteBranchIntent {
                repo_path: REPO.to_owned(),
                push_to: "up stream".to_owned(),
            },
        );
        flow.planned(load.generation, Ok(delete_branch_plan(&shown)));
        flow.confirm("feature".to_owned()).unwrap();
        let mut odd = report(
            7,
            BranchDeleteOutcome::Deleted {
                settled: Settled::Answer,
            },
            false,
        );
        odd.branch = "fix$(id)'x".to_owned();
        odd.backup_ref = "refs/rc-git/backup/remote-branch/fix$(id)'x".to_owned();
        flow.finished(1, Ok(odd));
        flow
    };
    let note = delete_branch_note(&flow).unwrap();
    assert!(
        note.ends_with(
            "git push 'up stream' 'refs/rc-git/backup/remote-branch/fix$(id)'\\''x:refs/heads/\
             fix$(id)'\\''x'"
        ),
        "{note}"
    );
}

#[test]
fn a_delete_stopped_after_its_backup_names_the_backup_and_the_untouched_branch() {
    for stop in [
        BackupKeptStop::Cancelled,
        BackupKeptStop::NotSent(Box::new(GitHubError::Transport {
            op: "delete the head branch",
            url: "https://github.com/…".to_owned(),
            reason: "Couldn't connect to server".to_owned(),
        })),
    ] {
        let shown = detail(7);
        let (mut flow, load) =
            DeleteBranchFlow::begin(1, REPOSITORY.to_owned(), shown.stamp(), intent());
        flow.planned(load.generation, Ok(delete_branch_plan(&shown)));
        flow.confirm("feature".to_owned()).unwrap();
        let refusal = ActionRefusal::BackupKept {
            pull_request: "octo/rc-git#7".to_owned(),
            branch: "\u{201c}feature\u{201d}".to_owned(),
            backup_ref: BACKUP.to_owned(),
            stop,
        };
        flow.finished(1, Err(ActionStop::Refused(refusal)));
        let note = delete_branch_note(&flow).unwrap();
        assert!(
            note.contains(BACKUP)
                && note.contains("nothing was sent and the branch on GitHub is untouched"),
            "{note}"
        );
        let Phase::Done(end) = flow.phase() else {
            panic!("done");
        };
        assert!(branch_outcome_log(&flow, end).contains(BACKUP));
    }
}

#[test]
fn the_restore_command_names_a_remote_only_when_its_name_is_a_plain_word() {
    let target = linked().target;
    let url = "https://github.com/octo/rc-git.git";
    for plain in ["origin", "up-stream", "fork_2", "a.b", "U1"] {
        assert_eq!(restore_destination(plain, &target), plain);
    }
    let long = "a".repeat(101);
    for hostile in [
        "--receive-pack=x",
        "-u",
        "ori\u{1b}[31mgin",
        "ori\ngin",
        "up stream",
        "$(id)",
        "or\u{ed}gin",
        "https://evil.example/x.git",
        ".origin",
        "",
        long.as_str(),
    ] {
        assert_eq!(restore_destination(hostile, &target), url, "{hostile:?}");
    }
    for remote in ["--receive-pack=touch /tmp/pwned", "ori\u{1b}[2Jgin\u{202e}"] {
        let mut hostile = linked();
        hostile.remote = remote.to_owned();
        let shown = detail(7);
        let (mut flow, load) = DeleteBranchFlow::begin(
            1,
            REPOSITORY.to_owned(),
            shown.stamp(),
            DeleteBranchIntent::new(REPO.to_owned(), &hostile),
        );
        flow.planned(load.generation, Ok(delete_branch_plan(&shown)));
        flow.confirm("feature".to_owned()).unwrap();
        let deleted = BranchDeleteOutcome::Deleted {
            settled: Settled::Answer,
        };
        flow.finished(1, Ok(report(7, deleted, false)));
        let note = delete_branch_note(&flow).unwrap();
        assert!(
            note.ends_with(&format!("git push {url} {BACKUP}:refs/heads/feature")),
            "{remote:?}: {note}"
        );
        assert!(
            !note.contains("receive-pack") && !note.contains(['\u{1b}', '\u{202e}']),
            "{note}"
        );
    }
}

#[test]
fn the_log_names_the_repository_number_branch_head_backup_and_outcome() {
    let flow = ended(
        BranchDeleteOutcome::Deleted {
            settled: Settled::Reread,
        },
        true,
    );
    let Phase::Done(end) = flow.phase() else {
        panic!("done");
    };
    assert_eq!(
        branch_outcome_log(&flow, end),
        format!(
            "delete head branch \u{201c}feature\u{201d} #7 of {REPOSITORY} head {} backup \
             {BACKUP}: deleted (read after a lost answer); backup ref {BACKUP} kept; local branch \
             kept",
            oid(7)
        )
    );
}

struct Harness {
    fake: FakePullRequests,
    model: Entity<PullRequestsModel>,
    events: Arc<Mutex<Vec<FlowEvent>>>,
    log: Arc<Mutex<Vec<String>>>,
}

/// A model over the fake, tab 7 reading `shown`, its delete planned as the guard would.
fn harness(cx: &mut TestAppContext, shown: &PullRequestDetail) -> Harness {
    cx.executor().allow_parking();
    let fake = FakePullRequests::new();
    {
        let mut state = fake.state();
        state.details.insert(7, Ok(shown.clone()));
        state.branch_plan = Ok(delete_branch_plan(&detail(7)));
        state.branch_delete = Ok(report(
            7,
            BranchDeleteOutcome::Deleted {
                settled: Settled::Answer,
            },
            false,
        ));
    }
    let service: Arc<dyn PullRequestService> = Arc::new(fake.clone());
    let model = cx.new(|_| PullRequestsModel::new(service, Arc::new(|| 1_000)));
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&model, move |_, event: &FlowEvent, _| {
            seen.lock().unwrap().push(*event);
        })
        .detach();
    });
    let log = Arc::new(Mutex::new(Vec::new()));
    let lines = Arc::clone(&log);
    model.update(cx, |m, _| {
        m.set_merge_log(Arc::new(move |line| {
            lines.lock().unwrap().push(line.to_owned());
        }));
    });
    Harness {
        fake,
        model,
        events,
        log,
    }
}

impl Harness {
    async fn open(&self, cx: &mut TestAppContext) {
        self.model
            .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
            .await;
        self.settle(cx);
        self.model.update(cx, |m, cx| m.open(7, cx));
        self.settle(cx);
    }

    /// Runs until no read, plan or unsent action is in flight.
    fn settle(&self, cx: &mut TestAppContext) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let busy = self.model.read_with(cx, |m, _| {
                m.link().is_none()
                    || m.list().is_loading()
                    || m.tabs().iter().any(DetailState::is_loading)
                    || m.tab_flow(7)
                        .is_some_and(|flow| flow.is_running() && flow.decision().is_none())
            });
            if !busy {
                return;
            }
            assert!(Instant::now() < deadline, "the work never ended");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn confirm(&self, cx: &mut TestAppContext, typed: &str) {
        self.model
            .update(cx, |m, cx| m.confirm_flow(7, typed.to_owned(), cx));
    }

    fn sends(&self) -> (usize, usize) {
        let state = self.fake.state();
        (state.branch_calls.len(), state.branches_sent)
    }
}

#[gpui::test]
async fn a_confirmed_delete_sends_once_with_what_was_typed_then_reads_again(
    cx: &mut TestAppContext,
) {
    let shown = detail(7);
    let h = harness(cx, &shown);
    h.open(cx).await;
    let details = h.fake.state().detail_calls.len();
    h.model.update(cx, PullRequestsModel::start_delete_branch);
    h.settle(cx);
    assert_eq!(*h.events.lock().unwrap(), [FlowEvent::Confirm(7)]);
    assert_eq!(
        h.fake.state().branch_plan_calls,
        [(REPO.to_owned(), shown.stamp())]
    );
    h.model.update(cx, PullRequestsModel::start_delete_branch);
    assert_eq!(
        h.fake.state().branch_plan_calls.len(),
        1,
        "one action at a time per tab"
    );
    h.confirm(cx, "feature");
    h.confirm(cx, "feature");
    h.settle(cx);
    assert_eq!(
        h.sends(),
        (1, 1),
        "one confirm, one send, however many clicks"
    );
    let call = h.fake.state().branch_calls[0].clone();
    assert_eq!(call.repo_path, REPO);
    assert_eq!(call.stamp, shown.stamp());
    assert_eq!(call.typed, "feature");
    assert_eq!(call.decision, Some(delete_branch_plan(&shown).decision));
    let flow = h
        .model
        .read_with(cx, |m, _| m.delete_branch_flow(7).unwrap().clone());
    assert!(matches!(
        flow.phase(),
        Phase::Done(Ended::Sent(BranchDeleteReport {
            outcome: BranchDeleteOutcome::Deleted { .. },
            ..
        }))
    ));
    assert_eq!(
        h.fake.state().detail_calls.len(),
        details + 1,
        "the tab reads again"
    );
    let log = h.log.lock().unwrap().clone();
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(
        log[0].ends_with(&format!("backup {BACKUP}: sending")),
        "{log:?}"
    );
    assert!(log[1].contains(": deleted (answer); backup ref"), "{log:?}");
}

#[gpui::test]
async fn nothing_starts_without_the_control(cx: &mut TestAppContext) {
    let mut open = detail(7);
    open.summary.state = PullRequestState::Open;
    let h = harness(cx, &open);
    h.open(cx).await;
    h.model.update(cx, PullRequestsModel::start_delete_branch);
    h.confirm(cx, "feature");
    h.settle(cx);
    assert!(h.model.read_with(cx, |m, _| m.tab_flow(7).is_none()));
    assert!(h.fake.state().branch_plan_calls.is_empty());
    assert_eq!(h.sends(), (0, 0));

    let mut protected = detail(7);
    protected.summary.head_ref = "main".to_owned();
    let p = harness(cx, &protected);
    p.open(cx).await;
    p.model.update(cx, PullRequestsModel::start_delete_branch);
    p.settle(cx);
    assert!(p.fake.state().branch_plan_calls.is_empty());
}

#[gpui::test]
async fn a_plan_answer_of_an_old_link_is_dropped(cx: &mut TestAppContext) {
    let shown = detail(7);
    let h = harness(cx, &shown);
    h.open(cx).await;
    let gate = Arc::new(CallGate::new());
    h.fake.state().plan_gate = Some(Arc::clone(&gate));
    h.model.update(cx, PullRequestsModel::start_delete_branch);
    assert!(gate.wait_until_arrived());
    let mut other = rc_git_kit::fake::linked();
    other.target = rc_git_kit::pull_requests::RepositoryTarget::on_host(
        &GitHubHost::github_com(),
        "https://github.com/octo/other",
    )
    .unwrap();
    h.fake.state().link = RepositoryLink::Linked(other);
    h.fake.state().plan_gate = None;
    h.model.update(cx, PullRequestsModel::relink).await;
    h.settle(cx);
    h.model.update(cx, |m, cx| m.open(7, cx));
    gate.open();
    h.settle(cx);
    assert!(
        h.model.read_with(cx, |m, _| m.tab_flow(7).is_none()),
        "the new link's #7 shows no delete"
    );
    assert!(!h.events.lock().unwrap().contains(&FlowEvent::Confirm(7)));
    h.confirm(cx, "feature");
    h.settle(cx);
    assert_eq!(h.sends(), (0, 0));
}

const TOKEN: &str = "ghp_live_branch_token_0123";
const REPO_ID: &str = "R_kgDOlive7";

/// A repository whose `origin` is `octo/rc-git` on `server`, an account for `server` with
/// [`TOKEN`] stored, and the live service over both.
fn live(server: &GitHubServer) -> (Fixture, LivePullRequests) {
    server.add_token(FakeToken {
        token: TOKEN.to_owned(),
        login: "octocat".to_owned(),
        scopes: None,
        expires: None,
    });
    let fixture = Fixture::standard();
    let url = format!("{}/octo/rc-git.git", server.origin());
    fixture.git(&["remote", "add", "origin", &url]);
    let settings = Arc::new(MemorySettings::new());
    let account = GitHubAccount {
        key: AccountKey::new(GitHubHost::parse(&server.host()).unwrap(), "octocat").unwrap(),
        method: SignInMethod::Token,
        client_id: None,
        scopes: None,
        token_expires: None,
    };
    GitHubAccounts::new(Arc::clone(&settings) as Arc<dyn SettingsStore>)
        .save(&account)
        .unwrap();
    let tokens = Arc::new(MemoryApiTokenStore::new());
    let set = TokenSet::lasting(ApiToken::new(TOKEN.to_owned()).unwrap());
    tokens.save_new_tokens(&account.key, &set).unwrap();
    let service = LivePullRequests::new(settings, tokens as Arc<dyn ApiTokenStore>)
        .with_ssh_dir(Some(Fixture::empty_dir().path().to_path_buf()));
    (fixture, service)
}

/// The head branch read's answer: pull request 7 merged at `head`, its branch `shipped` there.
fn branch_read(head: &str, at: Option<&str>) -> String {
    let head_ref = at.map_or_else(
        || "null".to_owned(),
        |at| {
            format!(
                r#"{{"name":"shipped","prefix":"refs/heads/",
                "target":{{"oid":"{at}"}},"repository":{{"name":"rc-git","owner":{{"login":"octo"}}}},
                "associatedPullRequests":{{"totalCount":0,"nodes":[]}}}}"#
            )
        },
    );
    format!(
        r#"{{"data":{{"repository":{{"id":"{REPO_ID}","defaultBranchRef":{{"name":"main"}},"pullRequest":{{
        "number":7,"title":"Ship","state":"MERGED","merged":true,"headRefName":"shipped",
        "headRefOid":"{head}","baseRefName":"main","isCrossRepository":false,
        "viewerCanDeleteHeadRef":true,"headRepository":{{"name":"rc-git","owner":{{"login":"octo"}}}},
        "headRef":{head_ref}}}}}}}}}"#
    )
}

/// Pull request 7's head branch `shipped` of `target` on `server`, at `head`, deleted once.
fn script_live_branch(
    server: &GitHubServer,
    target: &rc_git_kit::pull_requests::RepositoryTarget,
    head: &str,
) {
    use rc_git_guard::actions::github::testing::{
        based_on_request_body, delete_branch_request_body, head_branch_request_body,
    };
    server.script_branch(BranchScript {
        read: head_branch_request_body(target, 7),
        present: branch_read(head, Some(head)),
        present_at: head.to_owned(),
        changed: branch_read(head, Some(head)),
        changed_at: head.to_owned(),
        deleted: branch_read(head, None),
        based_on: based_on_request_body(target, "shipped"),
        based_on_answer: r#"{"data":{"repository":{"pullRequests":{"totalCount":0,"nodes":[]}}}}"#
            .to_owned(),
        mutation: delete_branch_request_body(REPO_ID, "shipped", head),
        behaviour: DeleteBehaviour::Delete,
    });
}

#[test]
fn the_live_service_backs_up_then_deletes_once_through_the_guard() {
    let server = GitHubServer::start();
    let (fixture, service) = live(&server);
    let repo_path = fixture.path().to_string_lossy().into_owned();
    let RepositoryLink::Linked(linked) = service.link(&repo_path) else {
        panic!("linked");
    };
    let head = fixture.rev_parse("HEAD");
    script_live_branch(&server, &linked.target, &head);
    let stamp = PullRequestStamp {
        number: 7,
        head_oid: rc_git_core::Oid::from_hex(&head).unwrap(),
        base_ref: "main".to_owned(),
        state: PullRequestState::Merged,
    };
    let plan = service
        .plan_delete_branch(
            &linked,
            &repo_path,
            &stamp,
            &rc_git_kit::pull_requests::Cancel::new(),
        )
        .unwrap();
    assert_eq!(plan.branch, "shipped");
    let unconfirmed = BranchAsk {
        repo_path: &repo_path,
        stamp: &stamp,
        decision: None,
        typed: "shipped",
        on_send: &|| {},
    };
    let cancel = rc_git_kit::pull_requests::Cancel::new();
    let refused = service
        .delete_branch(&linked, unconfirmed, &cancel)
        .unwrap_err();
    assert!(
        matches!(
            refused,
            ActionStop::Refused(ActionRefusal::NotConfirmed { .. })
        ),
        "{refused:?}"
    );
    let mistyped = BranchAsk {
        decision: Some(&plan.decision),
        typed: "Shipped",
        ..unconfirmed
    };
    let refused = service
        .delete_branch(&linked, mistyped, &cancel)
        .unwrap_err();
    assert!(
        matches!(refused, ActionStop::Refused(ActionRefusal::NotTyped { .. })),
        "{refused:?}"
    );
    assert_eq!(server.delete_requests(), 0);
    assert_eq!(
        fixture.git_output(&["for-each-ref", "refs/rc-git/backup/"]),
        "",
        "nothing written"
    );
    let sends = Mutex::new(0);
    let on_send = || *sends.lock().unwrap() += 1;
    let ask = BranchAsk {
        decision: Some(&plan.decision),
        on_send: &on_send,
        ..unconfirmed
    };
    let report = service.delete_branch(&linked, ask, &cancel).unwrap();
    assert_eq!(
        report.outcome,
        BranchDeleteOutcome::Deleted {
            settled: Settled::Answer
        }
    );
    assert_eq!(
        report.backup_ref,
        "refs/rc-git/backup/remote-branch/shipped"
    );
    assert_eq!(fixture.rev_parse(&report.backup_ref), head);
    assert_eq!(
        (
            server.delete_requests(),
            server.mutation_requests(),
            *sends.lock().unwrap()
        ),
        (1, 1, 1)
    );
}

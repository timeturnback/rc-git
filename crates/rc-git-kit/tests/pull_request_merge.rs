//! A tab's merge: the Merge control and the admin merge control decided from the guard's own
//! blockers, the merge flow as a state machine, the model over the fake service (races ordered
//! with `CallGate`s), and the live service merging against the testkit's fake GitHub host.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_kit::fake::{
    CallGate, FakePullRequests, admin_merge_plan, blocked_detail, merge_plan, oid, pull_request,
    pull_request_detail,
};
use rc_git_kit::github::accounts::{GitHubAccount, GitHubAccounts, SignInMethod};
use rc_git_kit::github::{ApiToken, GitHubHost, TokenSet};
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore, MemoryApiTokenStore};
use rc_git_kit::platform::settings::{MemorySettings, SettingsStore};
use rc_git_kit::pull_requests::copy::{
    merge_note, merge_note_alarms, merge_outcome_log, merge_stop_message,
};
use rc_git_kit::pull_requests::link::RepositoryLink;
use rc_git_kit::pull_requests::live::LivePullRequests;
use rc_git_kit::pull_requests::merge::{
    AdminMergeButton, CancelAnswer, MergeAsk, MergeButton, MergeEnd, MergeFlow, MergePhase,
    MergeStop, PlanAsk, admin_merge_button, allowed_methods, merge_button,
};
use rc_git_kit::pull_requests::model::{MergeEvent, PullRequestsModel};
use rc_git_kit::pull_requests::state::DetailState;
use rc_git_kit::pull_requests::{
    AfterLoss, Cancel, CheckList, MergeBlocker, MergeKind, MergeMethod, MergeMethods, MergeOutcome,
    MergeRefusal, MergeStateStatus, Mergeable, PullRequestDetail, PullRequestService,
    PullRequestState, ReadError, ReviewDecision, Settled, ViewerAbilities,
};
use rc_git_testkit::Fixture;
use rc_git_testkit::github::{FakeToken, GitHubServer, MergeBehaviour, MergeScript};

const REPO: &str = "/work/rc-git";

/// The repository the fake links to, as a merge names it.
const REPOSITORY: &str = "octo/rc-git@github.com";

fn detail(head: u8) -> PullRequestDetail {
    pull_request_detail(
        pull_request(7, "Ship it", oid(head)),
        "",
        CheckList::default(),
    )
}

/// A tab whose last read answered `answer`.
fn tab(answer: Result<PullRequestDetail, ReadError>) -> DetailState {
    let mut tab = DetailState::new(7);
    let load = tab.refresh().unwrap();
    tab.finish(&load, answer, 1_000);
    tab
}

fn reason(button: &MergeButton) -> &str {
    match button {
        MergeButton::Disabled { reason } => reason,
        MergeButton::Hidden | MergeButton::Enabled { .. } => "not disabled",
    }
}

#[test]
fn no_current_read_means_no_merge_control() {
    assert_eq!(
        merge_button(&DetailState::new(7), true),
        MergeButton::Hidden
    );
    let mut stale = tab(Ok(detail(7)));
    let load = stale.refresh().unwrap();
    stale.finish(&load, Err(ReadError::Failed("offline".to_owned())), 2_000);
    assert!(stale.detail().is_some(), "the old read stays on screen");
    assert_eq!(
        merge_button(&stale, true),
        MergeButton::Hidden,
        "a stale screen stands for nothing"
    );
}

#[test]
fn the_merge_control_says_why_it_takes_no_click() {
    let with = |change: fn(&mut PullRequestDetail)| {
        let mut detail = detail(7);
        change(&mut detail);
        merge_button(&tab(Ok(detail)), true)
    };
    assert_eq!(
        reason(&with(|d| d.summary.draft = true)),
        "Can't merge: it is a draft; mark it ready for review first."
    );
    assert_eq!(
        reason(&with(|d| d.mergeable = Mergeable::Conflicting)),
        "Can't merge: it conflicts with its base branch."
    );
    assert_eq!(
        reason(&with(|d| d.summary.state = PullRequestState::Closed)),
        "Can't merge: it is closed."
    );
    assert_eq!(
        reason(&with(|d| d.merge_state = Some(MergeStateStatus::Blocked))),
        "Can't merge: branch protection blocks the merge."
    );
    assert_eq!(
        reason(&with(|d| d.merge_methods = MergeMethods::default())),
        "Can't merge: the repository allows no merge method."
    );
    assert!(
        reason(&merge_button(&tab(Ok(detail(7))), false)).contains("sign in"),
        "signed out"
    );
    assert_eq!(
        with(|d| d.merge_methods.squash = false),
        MergeButton::Enabled {
            methods: vec![MergeMethod::Merge, MergeMethod::Rebase]
        }
    );
    assert_eq!(
        allowed_methods(&MergeMethods {
            merge: true,
            squash: true,
            rebase: true
        }),
        [MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase],
        "squash first"
    );
}

#[test]
fn a_flow_drops_answers_of_another_generation_and_stops_only_before_the_send() {
    let stamp = detail(7).stamp();
    let (mut flow, load) = MergeFlow::start(
        4,
        REPOSITORY.to_owned(),
        stamp.clone(),
        MergeMethod::Squash,
        MergeKind::Plain,
    );
    let plan = merge_plan(&detail(7), MergeMethod::Squash);
    assert!(!flow.planned(3, Ok(plan.clone())), "another generation");
    assert_eq!(flow.phase(), &MergePhase::Planning);
    assert!(flow.planned(load.generation, Ok(plan)));
    assert!(flow.confirm(String::new()).is_some());
    assert!(
        flow.confirm(String::new()).is_none(),
        "one merge per confirm"
    );
    assert_eq!(flow.cancel(), CancelAnswer::Signal);
    flow.sent(3);
    assert!(matches!(
        flow.phase(),
        MergePhase::Merging { sent: false, .. }
    ));
    flow.sent(4);
    assert_eq!(flow.cancel(), CancelAnswer::TooLate);
    let merged = Ok(MergeOutcome::Merged {
        merge_commit: Some(oid(9)),
        base: Some("main".to_owned()),
        settled: Settled::Answer,
    });
    assert!(!flow.finished(5, merged.clone()));
    assert!(flow.finished(4, merged));
    assert!(
        merge_note(&flow)
            .unwrap()
            .contains("unchanged until you fetch")
    );
}

#[test]
fn a_tab_read_that_no_longer_matches_ends_a_merge_before_its_send() {
    let stamp = detail(7).stamp();
    let (mut flow, load) = MergeFlow::start(
        1,
        REPOSITORY.to_owned(),
        stamp.clone(),
        MergeMethod::Squash,
        MergeKind::Plain,
    );
    flow.planned(
        load.generation,
        Ok(merge_plan(&detail(7), MergeMethod::Squash)),
    );
    assert!(!flow.follow(Some(&stamp)), "the same read keeps it");
    assert!(flow.follow(Some(&detail(8).stamp())));
    assert!(flow.confirm(String::new()).is_none(), "nothing to confirm");
    let MergePhase::Done(MergeEnd::NotSent(stop)) = flow.phase() else {
        panic!("{:?}", flow.phase());
    };
    assert!(stop.is_changed());
    let (mut failed, _) = MergeFlow::start(
        2,
        REPOSITORY.to_owned(),
        stamp,
        MergeMethod::Squash,
        MergeKind::Plain,
    );
    assert!(failed.follow(None), "a failed read stands for nothing");
}

#[test]
fn an_unknown_outcome_never_reads_as_not_merged() {
    let stamp = detail(7).stamp();
    let (mut flow, load) = MergeFlow::start(
        1,
        REPOSITORY.to_owned(),
        stamp,
        MergeMethod::Squash,
        MergeKind::Plain,
    );
    flow.planned(
        load.generation,
        Ok(merge_plan(&detail(7), MergeMethod::Squash)),
    );
    let load = flow.confirm(String::new()).unwrap();
    flow.finished(
        load.generation,
        Ok(MergeOutcome::Unknown {
            problem: "timed out".to_owned(),
            after: AfterLoss::StillOpen { head: oid(7) },
        }),
    );
    let note = merge_note(&flow).unwrap();
    assert!(note.contains("could not tell whether #7 merged"), "{note}");
    assert!(
        note.contains("Look at the pull request on GitHub"),
        "{note}"
    );
    assert!(!note.contains("try again,"), "{note}");
}

/// One change to a pull request's read.
type Change = fn(&mut PullRequestDetail);

/// [`detail`] at `head`, blocked by branch protection, the account allowed past it.
fn blocked(head: u8) -> PullRequestDetail {
    blocked_detail(detail(head))
}

#[test]
fn the_admin_control_exists_only_for_a_current_blocked_read_the_account_may_merge_past() {
    let shown = AdminMergeButton::Shown {
        methods: vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase],
    };
    assert_eq!(admin_merge_button(&tab(Ok(blocked(7))), true), shown);
    assert!(
        matches!(
            merge_button(&tab(Ok(blocked(7))), true),
            MergeButton::Disabled { .. }
        ),
        "Merge and the admin control are never both clickable"
    );
    let mut behind = blocked(7);
    behind.merge_state = Some(MergeStateStatus::Behind);
    assert_eq!(admin_merge_button(&tab(Ok(behind)), true), shown);

    assert_eq!(
        admin_merge_button(&DetailState::new(7), true),
        AdminMergeButton::Hidden,
        "no read yet"
    );
    let mut stale = tab(Ok(blocked(7)));
    let load = stale.refresh().unwrap();
    stale.finish(&load, Err(ReadError::Failed("offline".to_owned())), 2_000);
    assert!(stale.detail().is_some(), "the old read stays on screen");
    assert_eq!(
        admin_merge_button(&stale, true),
        AdminMergeButton::Hidden,
        "a stale screen stands for nothing"
    );
    assert_eq!(
        admin_merge_button(&tab(Ok(blocked(7))), false),
        AdminMergeButton::Hidden,
        "signed out"
    );
    let hidden = |change: fn(&mut PullRequestDetail)| {
        let mut detail = blocked(7);
        change(&mut detail);
        admin_merge_button(&tab(Ok(detail)), true)
    };
    let cases: [(&str, Change); 9] = [
        ("nothing to go past", |d| {
            d.merge_state = Some(MergeStateStatus::Clean);
        }),
        ("unstable merges plainly", |d| {
            d.merge_state = Some(MergeStateStatus::Unstable);
        }),
        ("no ability", |d| {
            d.viewer = Some(ViewerAbilities::default());
        }),
        ("ability unknown", |d| d.viewer = None),
        ("unknown state", |d| {
            d.merge_state = Some(MergeStateStatus::Unknown);
        }),
        ("no merge state", |d| d.merge_state = None),
        ("draft", |d| d.summary.draft = true),
        ("conflicting", |d| d.mergeable = Mergeable::Conflicting),
        ("no method", |d| d.merge_methods = MergeMethods::default()),
    ];
    for (why, change) in cases {
        assert_eq!(hidden(change), AdminMergeButton::Hidden, "{why}");
    }
}

#[test]
fn an_admin_flow_keeps_its_kind_and_hands_the_typed_text_on_untouched() {
    let stamp = blocked(7).stamp();
    let (mut flow, load) = MergeFlow::start(
        3,
        REPOSITORY.to_owned(),
        stamp.clone(),
        MergeMethod::Squash,
        MergeKind::Admin,
    );
    assert_eq!(load.kind, MergeKind::Admin);
    let plan = admin_merge_plan(&blocked(7), MergeMethod::Squash);
    assert!(!flow.planned(2, Ok(plan.clone())), "another generation");
    assert_eq!(flow.phase(), &MergePhase::Planning);
    assert!(flow.bypassed().is_none());
    assert!(flow.planned(3, Ok(plan.clone())));
    assert_eq!(flow.bypassed(), plan.bypassed.as_ref());
    let typed = "  #07 x\t".to_owned();
    let load = flow.confirm(typed.clone()).unwrap();
    assert_eq!(load.typed, typed, "the guard alone judges what was typed");
    assert_eq!(load.kind, MergeKind::Admin);
    assert_eq!(load.decision, plan.decision);
    assert!(!flow.finished(
        4,
        Ok(MergeOutcome::Refused {
            reason: "late".to_owned()
        })
    ));
    assert!(flow.finished(
        3,
        Ok(MergeOutcome::Refused {
            reason: "Not allowed.".to_owned()
        })
    ));
    let note = merge_note(&flow).unwrap();
    assert!(
        note.starts_with("GitHub refused the admin merge of #7: Not allowed. Nothing was merged."),
        "{note}"
    );
    assert!(note.contains("has not verified"), "{note}");
    let line = merge_outcome_log(
        &flow,
        &MergeEnd::Refused {
            reason: "Not allowed.".to_owned(),
        },
    );
    assert!(
        line.starts_with(&format!(
            "admin merge #7 of octo/rc-git@github.com head {} method squash bypassing [",
            oid(7)
        )),
        "{line}"
    );
    assert!(line.ends_with("]: refused: Not allowed."), "{line}");
}

struct Harness {
    fake: FakePullRequests,
    model: Entity<PullRequestsModel>,
    events: Arc<Mutex<Vec<MergeEvent>>>,
    log: Arc<Mutex<Vec<String>>>,
}

fn harness(cx: &mut TestAppContext) -> Harness {
    cx.executor().allow_parking();
    let fake = FakePullRequests::new();
    fake.state().details.insert(7, Ok(detail(7)));
    fake.state().plan = Ok(merge_plan(&detail(7), MergeMethod::Squash));
    fake.state().merge = Ok(MergeOutcome::Merged {
        merge_commit: Some(oid(9)),
        base: Some("main".to_owned()),
        settled: Settled::Answer,
    });
    let service: Arc<dyn PullRequestService> = Arc::new(fake.clone());
    let model = cx.new(|_| PullRequestsModel::new(service, Arc::new(|| 1_000)));
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&model, move |_, event: &MergeEvent, _| {
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

    /// Runs until no read, plan or unsent merge is in flight.
    fn settle(&self, cx: &mut TestAppContext) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let busy = self.model.read_with(cx, |m, _| {
                m.link().is_none()
                    || m.list().is_loading()
                    || m.tabs().iter().any(DetailState::is_loading)
                    || m.merge_flow(7).is_some_and(|flow| {
                        matches!(
                            flow.phase(),
                            MergePhase::Planning | MergePhase::Merging { .. }
                        )
                    })
            });
            if !busy {
                return;
            }
            assert!(Instant::now() < deadline, "the work never ended");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn phase(&self, cx: &mut TestAppContext) -> MergePhase {
        self.model
            .read_with(cx, |m, _| m.merge_flow(7).unwrap().phase().clone())
    }

    fn start(&self, cx: &mut TestAppContext) {
        self.model
            .update(cx, |m, cx| m.start_merge(MergeMethod::Squash, cx));
    }

    fn confirm(&self, cx: &mut TestAppContext) {
        self.model
            .update(cx, |m, cx| m.confirm_merge(7, String::new(), cx));
    }

    fn merges(&self) -> (usize, usize) {
        let state = self.fake.state();
        (state.merge_calls.len(), state.merges_sent)
    }
}

#[gpui::test]
async fn a_confirmed_merge_sends_once_then_reads_the_tab_and_the_list_again(
    cx: &mut TestAppContext,
) {
    let h = harness(cx);
    h.open(cx).await;
    h.start(cx);
    h.settle(cx);
    assert!(matches!(h.phase(cx), MergePhase::Confirming(_)));
    assert_eq!(*h.events.lock().unwrap(), [MergeEvent::Confirm(7)]);
    let (details, lists) = {
        let state = h.fake.state();
        (state.detail_calls.len(), state.list_calls.len())
    };
    h.confirm(cx);
    h.confirm(cx);
    h.settle(cx);
    assert_eq!(
        h.merges(),
        (1, 1),
        "one confirm, one merge, however many clicks"
    );
    let call = h.fake.state().merge_calls[0].clone();
    assert_eq!(call.stamp, detail(7).stamp());
    assert_eq!(call.method, MergeMethod::Squash);
    assert_eq!(
        call.decision,
        Some(merge_plan(&detail(7), MergeMethod::Squash).decision),
        "the decision the challenge showed"
    );
    assert!(matches!(
        h.phase(cx),
        MergePhase::Done(MergeEnd::Merged { .. })
    ));
    let state = h.fake.state();
    assert_eq!(state.detail_calls.len(), details + 1, "the tab reads again");
    assert_eq!(state.list_calls.len(), lists + 1, "and the list");
    assert_eq!(state.fetch_calls.len(), 0, "nothing local is fetched");
    drop(state);
    let log = h.log.lock().unwrap().clone();
    let head = oid(7).to_hex();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(
        log[0],
        format!("merge #7 of octo/rc-git@github.com head {head} method squash: sending")
    );
    assert!(
        log[1].starts_with(&format!(
            "merge #7 of octo/rc-git@github.com head {head} method squash: merged as "
        )),
        "{}",
        log[1]
    );
}

#[gpui::test]
async fn nothing_starts_without_a_current_read(cx: &mut TestAppContext) {
    let h = harness(cx);
    h.fake
        .state()
        .details
        .insert(7, Err(ReadError::Failed("offline".to_owned())));
    h.open(cx).await;
    h.start(cx);
    h.confirm(cx);
    h.settle(cx);
    assert!(h.model.read_with(cx, |m, _| m.merge_flow(7).is_none()));
    assert!(h.fake.state().plan_calls.is_empty());
    assert_eq!(h.merges(), (0, 0));
}

#[gpui::test]
async fn a_read_that_moves_the_head_while_the_challenge_is_up_withdraws_it(
    cx: &mut TestAppContext,
) {
    let h = harness(cx);
    h.open(cx).await;
    h.start(cx);
    h.settle(cx);
    assert!(matches!(h.phase(cx), MergePhase::Confirming(_)));
    h.fake.state().details.insert(7, Ok(detail(8)));
    h.model.update(cx, PullRequestsModel::refresh_detail);
    h.settle(cx);
    assert_eq!(
        *h.events.lock().unwrap(),
        [MergeEvent::Confirm(7), MergeEvent::Withdrawn(7)]
    );
    h.confirm(cx);
    h.settle(cx);
    assert_eq!(h.merges(), (0, 0), "the old challenge sends nothing");
    let MergePhase::Done(MergeEnd::NotSent(stop)) = h.phase(cx) else {
        panic!("withdrawn");
    };
    assert!(stop.is_changed());
}

#[gpui::test]
async fn a_plan_answer_after_a_cancel_is_dropped(cx: &mut TestAppContext) {
    let h = harness(cx);
    h.open(cx).await;
    let gate = Arc::new(CallGate::new());
    h.fake.state().plan_gate = Some(Arc::clone(&gate));
    h.start(cx);
    assert!(gate.wait_until_arrived());
    h.model.update(cx, |m, cx| m.cancel_merge(7, cx));
    gate.open();
    h.settle(cx);
    assert_eq!(
        h.phase(cx),
        MergePhase::Done(MergeEnd::NotSent(MergeStop::Cancelled))
    );
    assert_eq!(*h.events.lock().unwrap(), [MergeEvent::Withdrawn(7)]);
    h.confirm(cx);
    h.settle(cx);
    assert_eq!(h.merges(), (0, 0));
}

#[gpui::test]
async fn cancel_stops_a_merge_only_until_its_request_is_sent(cx: &mut TestAppContext) {
    let h = harness(cx);
    h.open(cx).await;
    let unsent = Arc::new(CallGate::new());
    h.fake.state().unsent_gate = Some(Arc::clone(&unsent));
    h.start(cx);
    h.settle(cx);
    h.confirm(cx);
    assert!(unsent.wait_until_arrived());
    h.model.update(cx, |m, cx| m.cancel_merge(7, cx));
    unsent.open();
    h.settle(cx);
    assert_eq!(h.merges(), (1, 0), "asked, never sent");
    let MergePhase::Done(MergeEnd::NotSent(stop)) = h.phase(cx) else {
        panic!("cancelled");
    };
    assert_eq!(merge_stop_message(&stop), "Cancelled; nothing was sent.");

    h.fake.state().unsent_gate = None;
    let sent = Arc::new(CallGate::new());
    h.fake.state().sent_gate = Some(Arc::clone(&sent));
    h.model.update(cx, |m, cx| m.dismiss_merge(7, cx));
    h.start(cx);
    h.settle(cx);
    h.confirm(cx);
    assert!(sent.wait_until_arrived());
    let deadline = Instant::now() + Duration::from_secs(20);
    while !matches!(h.phase(cx), MergePhase::Merging { sent: true, .. }) {
        assert!(Instant::now() < deadline, "the send was never seen");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        merge_note(
            &h.model
                .read_with(cx, |m, _| m.merge_flow(7).unwrap().clone())
        )
        .unwrap()
        .contains("cannot be stopped")
    );
    h.model.update(cx, |m, cx| m.cancel_merge(7, cx));
    sent.open();
    h.settle(cx);
    assert!(matches!(
        h.phase(cx),
        MergePhase::Done(MergeEnd::Merged { .. })
    ));
    assert_eq!(h.merges(), (2, 1));
}

const TOKEN: &str = "ghp_live_merge_token_0123";
const ID: &str = "PR_kwDOlive7";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// The detail read's answer for pull request 7 at [`HEAD`] in `state`.
fn pull(state: &str) -> String {
    format!(
        r#"{{"data":{{"repository":{{"mergeCommitAllowed":true,"squashMergeAllowed":true,
        "rebaseMergeAllowed":false,"autoMergeAllowed":false,"pullRequest":{{"id":"{ID}",
        "number":7,"title":"Ship","body":"","url":null,"isDraft":false,"state":"{state}",
        "updatedAt":"2026-11-18T09:00:00Z","author":{{"login":"mona"}},"headRefName":"topic",
        "headRefOid":"{HEAD}","baseRefName":"main","isCrossRepository":false,
        "headRepositoryOwner":{{"login":"octo"}},"reviewDecision":null,"mergeable":"MERGEABLE",
        "mergeStateStatus":"CLEAN","viewerCanMergeAsAdmin":false,"viewerCanEnableAutoMerge":false,
        "viewerCanDisableAutoMerge":false,"viewerCanDeleteHeadRef":false,"autoMergeRequest":null,
        "commits":{{"nodes":[{{"commit":{{"statusCheckRollup":null}}}}]}}}}}}}}}}"#
    )
}

/// A repository whose `origin` is `octo/rc-git` on `server`, an account for `server` with
/// [`TOKEN`] stored, and the live service over both.
fn live(server: &GitHubServer) -> (Fixture, LivePullRequests) {
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

#[test]
fn the_live_service_merges_once_through_the_guard() {
    use rc_git_guard::actions::github::testing::{detail_request_body, merge_request_body};
    let server = GitHubServer::start();
    server.add_token(FakeToken {
        token: TOKEN.to_owned(),
        login: "octocat".to_owned(),
        scopes: None,
        expires: None,
    });
    let (fixture, service) = live(&server);
    let RepositoryLink::Linked(linked) = service.link(&fixture.path().to_string_lossy()) else {
        panic!("linked");
    };
    let reply = format!(
        r#"{{"data":{{"mergePullRequest":{{"pullRequest":{{"number":7,"state":"MERGED",
        "merged":true,"headRefOid":"{HEAD}","mergeCommit":{{"oid":"{HEAD}"}}}}}}}}}}"#
    );
    server.script_merge(MergeScript {
        read: detail_request_body(&linked.target, 7, false),
        open: pull("OPEN"),
        moved: pull("OPEN"),
        merged: pull("MERGED"),
        mutation: merge_request_body(ID, HEAD, MergeMethod::Squash),
        reply,
        behaviour: MergeBehaviour::Merge,
    });
    let stamp = rc_git_kit::pull_requests::PullRequestStamp {
        number: 7,
        head_oid: rc_git_core::Oid::from_hex(HEAD).unwrap(),
        base_ref: "main".to_owned(),
        state: PullRequestState::Open,
    };
    let plan = service
        .plan_merge(
            &linked,
            PlanAsk {
                stamp: &stamp,
                method: MergeMethod::Squash,
                kind: MergeKind::Plain,
            },
            &Cancel::new(),
        )
        .unwrap();
    let unconfirmed = MergeAsk {
        stamp: &stamp,
        method: MergeMethod::Squash,
        kind: MergeKind::Plain,
        decision: None,
        typed: "",
        on_send: &|| {},
    };
    let refused = service
        .merge(&linked, unconfirmed, &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(
            refused,
            MergeStop::Refused(MergeRefusal::NotConfirmed { .. })
        ),
        "{refused:?}"
    );
    assert_eq!(server.mutation_requests(), 0);
    let sends = Mutex::new(0);
    let on_send = || *sends.lock().unwrap() += 1;
    let ask = MergeAsk {
        decision: Some(&plan.decision),
        on_send: &on_send,
        ..unconfirmed
    };
    let outcome = service.merge(&linked, ask, &Cancel::new()).unwrap();
    assert!(
        matches!(outcome, MergeOutcome::Merged { .. }),
        "{outcome:?}"
    );
    assert_eq!((server.mutation_requests(), *sends.lock().unwrap()), (1, 1));
}

/// A harness whose tab 7 reads `detail`, with the admin plan of [`blocked`] scripted.
fn admin_harness(cx: &mut TestAppContext, detail: PullRequestDetail) -> Harness {
    let h = harness(cx);
    let mut planned = blocked(7);
    planned.summary.review = Some(ReviewDecision::ChangesRequested);
    h.fake.state().details.insert(7, Ok(detail));
    h.fake.state().plan = Ok(admin_merge_plan(&planned, MergeMethod::Squash));
    h
}

#[gpui::test]
async fn an_admin_merge_starts_only_from_its_control_and_sends_the_typed_text_as_typed(
    cx: &mut TestAppContext,
) {
    let mut planned = blocked(7);
    planned.summary.review = Some(ReviewDecision::ChangesRequested);
    let h = admin_harness(cx, planned.clone());
    h.open(cx).await;
    h.start(cx);
    h.settle(cx);
    assert!(
        h.fake.state().plan_calls.is_empty(),
        "Merge takes no click while branch protection blocks it"
    );
    h.model
        .update(cx, |m, cx| m.start_admin_merge(MergeMethod::Squash, cx));
    h.settle(cx);
    assert_eq!(
        h.fake.state().plan_calls,
        [(planned.stamp(), MergeMethod::Squash, MergeKind::Admin)]
    );
    assert!(matches!(h.phase(cx), MergePhase::Confirming(_)));
    assert_eq!(*h.events.lock().unwrap(), [MergeEvent::Confirm(7)]);
    let typed = " 7 \u{202e}".to_owned();
    h.model
        .update(cx, |m, cx| m.confirm_merge(7, typed.clone(), cx));
    h.settle(cx);
    let state = h.fake.state();
    assert_eq!((state.merge_calls.len(), state.merges_sent), (1, 1));
    let call = state.merge_calls[0].clone();
    drop(state);
    assert_eq!(call.kind, MergeKind::Admin);
    assert_eq!(
        call.typed, typed,
        "passed through, unverified by the front end"
    );
    assert_eq!(
        call.decision,
        Some(admin_merge_plan(&planned, MergeMethod::Squash).decision)
    );
    let log = h.log.lock().unwrap().clone();
    assert_eq!(log.len(), 2, "{log:?}");
    let subject = format!(
        "admin merge #7 of octo/rc-git@github.com head {} method squash bypassing [failed \
         required checks: []; \
         pending required checks: []; checks of unreported requirement not passed: 0; checks not \
         read: 0; review: changes requested; behind base: false]",
        oid(7)
    );
    assert_eq!(log[0], format!("{subject}: sending"));
    assert!(
        log[1].starts_with(&format!("{subject}: merged as ")),
        "{}",
        log[1]
    );
}

#[gpui::test]
async fn no_admin_merge_starts_without_its_control(cx: &mut TestAppContext) {
    let mut unable = blocked(7);
    unable.viewer = Some(ViewerAbilities::default());
    for detail in [detail(7), unable] {
        let h = admin_harness(cx, detail);
        h.open(cx).await;
        h.model
            .update(cx, |m, cx| m.start_admin_merge(MergeMethod::Squash, cx));
        h.model
            .update(cx, |m, cx| m.confirm_merge(7, "7".to_owned(), cx));
        h.settle(cx);
        assert!(h.model.read_with(cx, |m, _| m.merge_flow(7).is_none()));
        assert!(h.fake.state().plan_calls.is_empty());
        assert_eq!(h.merges(), (0, 0));
    }
    let h = admin_harness(cx, blocked(7));
    h.fake
        .state()
        .details
        .insert(7, Err(ReadError::Failed("offline".to_owned())));
    h.open(cx).await;
    h.model
        .update(cx, |m, cx| m.start_admin_merge(MergeMethod::Squash, cx));
    h.settle(cx);
    assert!(h.fake.state().plan_calls.is_empty(), "no current read");
}

#[gpui::test]
async fn an_admin_plan_answer_after_a_cancel_is_dropped(cx: &mut TestAppContext) {
    let h = admin_harness(cx, blocked(7));
    h.open(cx).await;
    let gate = Arc::new(CallGate::new());
    h.fake.state().plan_gate = Some(Arc::clone(&gate));
    h.model
        .update(cx, |m, cx| m.start_admin_merge(MergeMethod::Squash, cx));
    assert!(gate.wait_until_arrived());
    h.model.update(cx, |m, cx| m.cancel_merge(7, cx));
    gate.open();
    h.settle(cx);
    assert_eq!(
        h.phase(cx),
        MergePhase::Done(MergeEnd::NotSent(MergeStop::Cancelled))
    );
    h.model
        .update(cx, |m, cx| m.confirm_merge(7, "7".to_owned(), cx));
    h.settle(cx);
    assert_eq!(h.merges(), (0, 0));
}

/// Pull request 7 on `server`, read through `target`: blocked by branch protection, the account
/// allowed past it, merging with a squash.
fn script_blocked_live(
    server: &GitHubServer,
    target: &rc_git_kit::pull_requests::RepositoryTarget,
) {
    use rc_git_guard::actions::github::testing::{detail_request_body, merge_request_body};
    let blocked_read = pull("OPEN").replace(
        r#""mergeStateStatus":"CLEAN","viewerCanMergeAsAdmin":false"#,
        r#""mergeStateStatus":"BLOCKED","viewerCanMergeAsAdmin":true"#,
    );
    assert_ne!(blocked_read, pull("OPEN"));
    let reply = format!(
        r#"{{"data":{{"mergePullRequest":{{"pullRequest":{{"number":7,"state":"MERGED",
        "merged":true,"headRefOid":"{HEAD}","mergeCommit":{{"oid":"{HEAD}"}}}}}}}}}}"#
    );
    server.script_merge(MergeScript {
        read: detail_request_body(target, 7, false),
        open: blocked_read.clone(),
        moved: blocked_read,
        merged: pull("MERGED"),
        mutation: merge_request_body(ID, HEAD, MergeMethod::Squash),
        reply,
        behaviour: MergeBehaviour::Merge,
    });
}

#[test]
fn the_live_service_sends_an_admin_merge_only_with_the_number_typed() {
    let server = GitHubServer::start();
    server.add_token(FakeToken {
        token: TOKEN.to_owned(),
        login: "octocat".to_owned(),
        scopes: None,
        expires: None,
    });
    let (fixture, service) = live(&server);
    let RepositoryLink::Linked(linked) = service.link(&fixture.path().to_string_lossy()) else {
        panic!("linked");
    };
    script_blocked_live(&server, &linked.target);
    let stamp = rc_git_kit::pull_requests::PullRequestStamp {
        number: 7,
        head_oid: rc_git_core::Oid::from_hex(HEAD).unwrap(),
        base_ref: "main".to_owned(),
        state: PullRequestState::Open,
    };
    let plain = service
        .plan_merge(
            &linked,
            PlanAsk {
                stamp: &stamp,
                method: MergeMethod::Squash,
                kind: MergeKind::Plain,
            },
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(
        matches!(
            plain,
            MergeStop::Refused(MergeRefusal::Blocked {
                blocker: MergeBlocker::Blocked,
                ..
            })
        ),
        "{plain:?}"
    );
    let plan = service
        .plan_merge(
            &linked,
            PlanAsk {
                stamp: &stamp,
                method: MergeMethod::Squash,
                kind: MergeKind::Admin,
            },
            &Cancel::new(),
        )
        .unwrap();
    let sends = Mutex::new(0);
    let on_send = || *sends.lock().unwrap() += 1;
    for typed in ["", "#7", "8"] {
        let ask = MergeAsk {
            stamp: &stamp,
            method: MergeMethod::Squash,
            kind: MergeKind::Admin,
            decision: Some(&plan.decision),
            typed,
            on_send: &on_send,
        };
        let refused = service.merge(&linked, ask, &Cancel::new()).unwrap_err();
        assert!(
            matches!(
                refused,
                MergeStop::Refused(MergeRefusal::NotConfirmed { .. })
            ),
            "{typed:?}: {refused:?}"
        );
    }
    assert_eq!((server.mutation_requests(), *sends.lock().unwrap()), (0, 0));
    let ask = MergeAsk {
        stamp: &stamp,
        method: MergeMethod::Squash,
        kind: MergeKind::Admin,
        decision: Some(&plan.decision),
        typed: "7",
        on_send: &on_send,
    };
    let outcome = service.merge(&linked, ask, &Cancel::new()).unwrap();
    assert!(
        matches!(outcome, MergeOutcome::Merged { .. }),
        "{outcome:?}"
    );
    assert_eq!((server.mutation_requests(), *sends.lock().unwrap()), (1, 1));
}

/// A flow of #7 confirmed and finished with `outcome`.
fn finished_with(outcome: MergeOutcome) -> MergeFlow {
    let stamp = detail(7).stamp();
    let (mut flow, load) = MergeFlow::start(
        1,
        REPOSITORY.to_owned(),
        stamp,
        MergeMethod::Squash,
        MergeKind::Plain,
    );
    flow.planned(
        load.generation,
        Ok(merge_plan(&detail(7), MergeMethod::Squash)),
    );
    let load = flow.confirm(String::new()).unwrap();
    assert!(flow.finished(load.generation, Ok(outcome)));
    flow
}

#[test]
fn a_merge_into_another_base_reads_as_a_warning_naming_both_branches() {
    let flow = finished_with(MergeOutcome::MergedIntoOtherBase {
        confirmed: "main".to_owned(),
        actual: "release".to_owned(),
        merge_commit: Some(oid(9)),
        settled: Settled::Answer,
    });
    let MergePhase::Done(end) = flow.phase() else {
        panic!("{:?}", flow.phase());
    };
    assert!(
        matches!(end, MergeEnd::MergedIntoOtherBase { confirmed, actual, .. }
            if confirmed == "main" && actual == "release"),
        "{end:?}"
    );
    let note = merge_note(&flow).unwrap();
    assert!(
        note.starts_with("Warning: #7 merged into release, not main, the base you confirmed"),
        "{note}"
    );
    assert!(merge_note_alarms(&flow), "shown as an alarm");
    let line = merge_outcome_log(&flow, end);
    assert!(
        line.contains("MERGED INTO ANOTHER BASE")
            && line.contains("into release, not the confirmed main"),
        "{line}"
    );

    let unnamed = finished_with(MergeOutcome::Merged {
        merge_commit: None,
        base: None,
        settled: Settled::Answer,
    });
    let note = merge_note(&unnamed).unwrap();
    assert!(
        note.starts_with("Merged #7. GitHub did not say which branch"),
        "{note}"
    );
    assert!(!note.contains("main"), "no base named as fact: {note}");
    let named = finished_with(MergeOutcome::Merged {
        merge_commit: None,
        base: Some("main".to_owned()),
        settled: Settled::Answer,
    });
    assert!(
        merge_note(&named)
            .unwrap()
            .starts_with("Merged #7 into main.")
    );
    assert!(!merge_note_alarms(&named) && !merge_note_alarms(&unnamed));
}

/// The fake's link, for another repository on the same host.
fn other_link() -> RepositoryLink {
    let mut other = rc_git_kit::fake::linked();
    other.target = rc_git_kit::pull_requests::RepositoryTarget::on_host(
        &GitHubHost::github_com(),
        "https://github.com/octo/other",
    )
    .unwrap();
    RepositoryLink::Linked(other)
}

#[gpui::test]
async fn a_merge_sent_before_a_relink_is_logged_as_its_own_and_shown_by_no_new_tab(
    cx: &mut TestAppContext,
) {
    let h = harness(cx);
    h.open(cx).await;
    let sent = Arc::new(CallGate::new());
    h.fake.state().sent_gate = Some(Arc::clone(&sent));
    h.start(cx);
    h.settle(cx);
    h.confirm(cx);
    assert!(sent.wait_until_arrived());
    let deadline = Instant::now() + Duration::from_secs(20);
    while !matches!(h.phase(cx), MergePhase::Merging { sent: true, .. }) {
        assert!(Instant::now() < deadline, "the send was never seen");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(1));
    }
    h.fake.state().link = other_link();
    h.model.update(cx, PullRequestsModel::relink).await;
    h.settle(cx);
    h.model.update(cx, |m, cx| m.open(7, cx));
    h.settle(cx);
    assert!(
        h.model.read_with(cx, |m, _| m.merge_flow(7).is_none()),
        "the new link's #7 has no merge"
    );
    let details = h.fake.state().detail_calls.len();
    sent.open();
    while h.log.lock().unwrap().len() < 2 {
        assert!(Instant::now() < deadline, "the merge never ended");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(1));
    }
    h.settle(cx);
    let log = h.log.lock().unwrap().clone();
    assert!(
        log[1].starts_with("merge #7 of octo/rc-git@github.com head "),
        "{log:?}"
    );
    assert!(h.model.read_with(cx, |m, _| m.merge_flow(7).is_none()));
    assert_eq!(
        h.fake.state().detail_calls.len(),
        details,
        "the old link's merge reads nothing of the new link"
    );
}

#[gpui::test]
async fn a_detail_read_asked_under_an_old_link_never_lands_in_the_new_link_s_tab(
    cx: &mut TestAppContext,
) {
    let h = harness(cx);
    h.model
        .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
        .await;
    h.settle(cx);
    let old = Arc::new(CallGate::new());
    h.fake.state().detail_gate = Some(Arc::clone(&old));
    // The old tab's first read, so its generation is the one the new tab's first read gets.
    h.model.update(cx, |m, cx| m.open(7, cx));
    assert!(old.wait_until_arrived());
    let new = Arc::new(CallGate::new());
    {
        let mut state = h.fake.state();
        state.link = other_link();
        state.detail_gate = Some(Arc::clone(&new));
        state.details.insert(7, Ok(detail(2)));
    }
    h.model.update(cx, PullRequestsModel::relink).await;
    cx.run_until_parked();
    h.model.update(cx, |m, cx| m.open(7, cx));
    assert!(new.wait_until_arrived());
    let tab = |cx: &mut TestAppContext| h.model.read_with(cx, |m, _| m.tabs()[0].clone());
    old.open();
    let deadline = Instant::now() + Duration::from_secs(20);
    while h.fake.state().details_answered < 1 {
        assert!(Instant::now() < deadline, "the old read never answered");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(1));
    }
    for _ in 0..50 {
        cx.run_until_parked();
        if !tab(cx).is_loading() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        tab(cx).is_loading() && tab(cx).detail().is_none(),
        "the old link's answer landed: {:?}",
        tab(cx)
    );
    new.open();
    h.settle(cx);
    assert_eq!(tab(cx).stamp().map(|stamp| stamp.head_oid), Some(oid(2)));
}

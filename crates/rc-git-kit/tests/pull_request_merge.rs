//! A tab's merge: the Merge control decided from the guard's own blockers, the merge flow as a
//! state machine, the model over the fake service (races ordered with `CallGate`s), and the
//! live service merging against the testkit's fake GitHub host.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_kit::fake::{
    CallGate, FakePullRequests, merge_plan, oid, pull_request, pull_request_detail,
};
use rc_git_kit::github::accounts::{GitHubAccount, GitHubAccounts, SignInMethod};
use rc_git_kit::github::{ApiToken, GitHubHost, TokenSet};
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore, MemoryApiTokenStore};
use rc_git_kit::platform::settings::{MemorySettings, SettingsStore};
use rc_git_kit::pull_requests::copy::{merge_note, merge_stop_message};
use rc_git_kit::pull_requests::link::RepositoryLink;
use rc_git_kit::pull_requests::live::LivePullRequests;
use rc_git_kit::pull_requests::merge::{
    CancelAnswer, MergeAsk, MergeButton, MergeEnd, MergeFlow, MergePhase, MergeStop,
    allowed_methods, merge_button,
};
use rc_git_kit::pull_requests::model::{MergeEvent, PullRequestsModel};
use rc_git_kit::pull_requests::state::DetailState;
use rc_git_kit::pull_requests::{
    AfterLoss, Cancel, CheckList, MergeMethod, MergeMethods, MergeOutcome, MergeRefusal,
    MergeStateStatus, Mergeable, PullRequestDetail, PullRequestService, PullRequestState,
    ReadError, Settled,
};
use rc_git_testkit::Fixture;
use rc_git_testkit::github::{FakeToken, GitHubServer, MergeBehaviour, MergeScript};

const REPO: &str = "/work/rc-git";

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
    tab.finish(load.generation, answer, 1_000);
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
    stale.finish(
        load.generation,
        Err(ReadError::Failed("offline".to_owned())),
        2_000,
    );
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
    let (mut flow, load) = MergeFlow::start(4, stamp.clone(), MergeMethod::Squash);
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
    let (mut flow, load) = MergeFlow::start(1, stamp.clone(), MergeMethod::Squash);
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
    let (mut failed, _) = MergeFlow::start(2, stamp, MergeMethod::Squash);
    assert!(failed.follow(None), "a failed read stands for nothing");
}

#[test]
fn an_unknown_outcome_never_reads_as_not_merged() {
    let stamp = detail(7).stamp();
    let (mut flow, load) = MergeFlow::start(1, stamp, MergeMethod::Squash);
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
        format!("merge octo/rc-git#7 head {head} method squash: sending")
    );
    assert!(
        log[1].starts_with(&format!(
            "merge octo/rc-git#7 head {head} method squash: merged as "
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
        .plan_merge(&linked, &stamp, MergeMethod::Squash, &Cancel::new())
        .unwrap();
    let unconfirmed = MergeAsk {
        stamp: &stamp,
        method: MergeMethod::Squash,
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

//! A tab's auto-merge change and re-run of failed jobs: the controls decided from the guard's
//! own blockers, the flows as state machines, the model over the fake service (races ordered
//! with `CallGate`s), and the live service against the testkit's fake GitHub host.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_kit::fake::{
    CallGate, FakePullRequests, auto_merge_on_detail, auto_merge_plan, auto_mergeable_detail,
    failing_detail, oid, pull_request, pull_request_detail, rerun_plan,
};
use rc_git_kit::github::accounts::{GitHubAccount, GitHubAccounts, SignInMethod};
use rc_git_kit::github::{ApiToken, GitHubHost, TokenSet};
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore, MemoryApiTokenStore};
use rc_git_kit::platform::settings::{MemorySettings, SettingsStore};
use rc_git_kit::pull_requests::auto_merge::{
    AutoMergeControl, AutoMergeFlow, AutoMergeIntent, auto_merge_control,
};
use rc_git_kit::pull_requests::copy::{
    auto_merge_note, flow_note_alarms, flow_title, rerun_note, rerun_outcome_log,
};
use rc_git_kit::pull_requests::flow::{ActionAsk, ActionStop, Ended, Phase, Stop, TabFlow};
use rc_git_kit::pull_requests::link::RepositoryLink;
use rc_git_kit::pull_requests::live::LivePullRequests;
use rc_git_kit::pull_requests::model::{FlowEvent, PullRequestsModel};
use rc_git_kit::pull_requests::rerun::{RerunControl, RerunFlow, RerunIntent, rerun_control};
use rc_git_kit::pull_requests::state::DetailState;
use rc_git_kit::pull_requests::{
    ActionRefusal, AutoMergeChange, AutoMergeOutcome, Cancel, CheckList, MergeMethod,
    MergeStateStatus, PullRequestDetail, PullRequestService, PullRequestStamp, PullRequestState,
    ReadError, RerunReport, RunId, RunOutcome, RunReport, Settled, ViewerAbilities,
};
use rc_git_testkit::Fixture;
use rc_git_testkit::github::{
    FakeToken, GitHubServer, MergeBehaviour, MergeScript, RerunBehaviour,
};

const REPO: &str = "/work/rc-git";

const REPOSITORY: &str = "octo/rc-git@github.com";

const SQUASH: AutoMergeChange = AutoMergeChange::Enable(MergeMethod::Squash);

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

/// A tab whose read failed after it showed `shown`.
fn stale(shown: PullRequestDetail) -> DetailState {
    let mut stale = tab(Ok(shown));
    let load = stale.refresh().unwrap();
    stale.finish(&load, Err(ReadError::Failed("offline".to_owned())), 2_000);
    stale
}

#[test]
fn no_current_read_means_no_auto_merge_or_rerun_control() {
    let blank = DetailState::new(7);
    assert_eq!(auto_merge_control(&blank, true), AutoMergeControl::Hidden);
    assert_eq!(rerun_control(&blank, true), RerunControl::Hidden);
    let on = stale(auto_merge_on_detail(detail(7)));
    assert!(on.detail().is_some(), "the old read stays on screen");
    assert_eq!(auto_merge_control(&on, true), AutoMergeControl::Hidden);
    let failing = stale(failing_detail(detail(7), &[41]));
    assert_eq!(rerun_control(&failing, true), RerunControl::Hidden);
}

#[test]
fn the_auto_merge_control_offers_only_what_the_guard_allows() {
    let with = |change: fn(&mut PullRequestDetail)| {
        let mut detail = auto_mergeable_detail(detail(7));
        change(&mut detail);
        auto_merge_control(&tab(Ok(detail)), true)
    };
    assert_eq!(
        with(|_| {}),
        AutoMergeControl::Offer {
            methods: vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase]
        }
    );
    for (why, change) in [
        (
            "it can merge now",
            (|d: &mut PullRequestDetail| d.merge_state = Some(MergeStateStatus::Clean))
                as fn(&mut PullRequestDetail),
        ),
        ("the repository does not allow it", |d| {
            d.auto_merge_allowed = Some(false);
        }),
        ("the host does not say", |d| d.auto_merge_allowed = None),
        ("the account may not", |d| {
            d.viewer = Some(ViewerAbilities::default());
        }),
        ("a draft", |d| d.summary.draft = true),
        ("closed", |d| d.summary.state = PullRequestState::Closed),
    ] {
        assert_eq!(with(change), AutoMergeControl::Hidden, "{why}");
    }
    let signed_out = auto_merge_control(&tab(Ok(auto_mergeable_detail(detail(7)))), false);
    assert_eq!(signed_out, AutoMergeControl::Hidden);
    let on = tab(Ok(auto_merge_on_detail(detail(7))));
    assert_eq!(
        auto_merge_control(&on, true),
        AutoMergeControl::On {
            method: Some(MergeMethod::Squash),
            enabled_by: Some("mona".to_owned()),
            can_turn_off: true
        }
    );
    assert_eq!(
        auto_merge_control(&on, false),
        AutoMergeControl::On {
            method: Some(MergeMethod::Squash),
            enabled_by: Some("mona".to_owned()),
            can_turn_off: false
        },
        "the state shows, the switch does not"
    );
    let mut no_right = auto_merge_on_detail(detail(7));
    no_right.viewer = Some(ViewerAbilities::default());
    assert!(matches!(
        auto_merge_control(&tab(Ok(no_right)), true),
        AutoMergeControl::On {
            can_turn_off: false,
            ..
        }
    ));
}

#[test]
fn the_rerun_control_shows_only_for_an_open_pull_request_with_failed_runs() {
    assert_eq!(
        rerun_control(&tab(Ok(detail(7))), true),
        RerunControl::Hidden
    );
    let failing = tab(Ok(failing_detail(detail(7), &[41, 42, 42])));
    assert_eq!(
        rerun_control(&failing, true),
        RerunControl::Shown {
            runs: 2,
            left_out: 0
        }
    );
    assert_eq!(rerun_control(&failing, false), RerunControl::Hidden);
    let runs: Vec<u64> = (100..112).collect();
    assert_eq!(
        rerun_control(&tab(Ok(failing_detail(detail(7), &runs))), true),
        RerunControl::Shown {
            runs: 10,
            left_out: 2
        }
    );
    let mut closed = failing_detail(detail(7), &[41]);
    closed.summary.state = PullRequestState::Merged;
    assert_eq!(rerun_control(&tab(Ok(closed)), true), RerunControl::Hidden);
    assert_eq!(
        rerun_control(&tab(Ok(failing_detail(detail(7), &[0]))), true),
        RerunControl::Hidden,
        "a run id the guard does not accept is no failed run"
    );
}

#[test]
fn a_flow_drops_answers_of_another_generation_and_follows_its_tab() {
    let shown = auto_mergeable_detail(detail(7));
    let (mut flow, load) = AutoMergeFlow::begin(
        4,
        REPOSITORY.to_owned(),
        shown.stamp(),
        AutoMergeIntent(SQUASH),
    );
    assert_eq!(load.generation, 4);
    let plan = auto_merge_plan(&shown, SQUASH);
    assert!(!flow.planned(3, Ok(plan.clone())), "another generation");
    assert_eq!(flow.phase(), &Phase::Planning);
    assert!(flow.planned(4, Ok(plan.clone())));
    assert!(flow.confirm(String::new()).is_some());
    assert!(
        flow.confirm(String::new()).is_none(),
        "one send per confirm"
    );
    assert!(!flow.finished(
        5,
        Ok(AutoMergeOutcome::Done {
            settled: Settled::Answer
        })
    ));
    assert!(flow.finished(
        4,
        Ok(AutoMergeOutcome::Done {
            settled: Settled::Answer
        })
    ));
    let note = auto_merge_note(&flow).unwrap();
    assert!(note.starts_with("Auto-merge is on for #7"), "{note}");

    let (mut rerun, _) = RerunFlow::begin(9, REPOSITORY.to_owned(), shown.stamp(), RerunIntent);
    assert!(
        rerun.follow(Some(&detail(8).stamp())),
        "another head ends it"
    );
    let Phase::Done(Ended::NotSent(stop)) = rerun.phase() else {
        panic!("{:?}", rerun.phase());
    };
    assert!(stop.is_changed());
    assert!(!rerun.planned(9, Ok(rerun_plan(&failing_detail(detail(7), &[41])))));
}

#[test]
fn a_challenge_to_turn_auto_merge_off_stands_through_a_push_or_a_retarget() {
    let shown = auto_merge_on_detail(auto_mergeable_detail(detail(7)));
    let pushed = PullRequestStamp {
        base_ref: "develop".to_owned(),
        ..detail(8).stamp()
    };
    let (mut off, load) = AutoMergeFlow::begin(
        1,
        REPOSITORY.to_owned(),
        shown.stamp(),
        AutoMergeIntent(AutoMergeChange::Disable),
    );
    off.planned(
        load.generation,
        Ok(auto_merge_plan(&shown, AutoMergeChange::Disable)),
    );
    assert!(!off.follow(Some(&pushed)), "a push and a retarget keep it");
    assert_eq!(off.phase(), &Phase::Confirming);
    let closed = PullRequestStamp {
        state: PullRequestState::Closed,
        ..shown.stamp()
    };
    assert!(off.follow(Some(&closed)), "a closed pull request ends it");
    let ready = auto_mergeable_detail(detail(7));
    let (mut on, load) = AutoMergeFlow::begin(
        2,
        REPOSITORY.to_owned(),
        ready.stamp(),
        AutoMergeIntent(SQUASH),
    );
    on.planned(load.generation, Ok(auto_merge_plan(&ready, SQUASH)));
    assert!(
        on.follow(Some(&pushed)),
        "turning it on stands for one head"
    );
}

#[test]
fn a_rerun_note_and_log_name_every_run_and_never_call_an_unknown_one_done() {
    let shown = failing_detail(detail(7), &[41, 42]);
    let (mut flow, load) = RerunFlow::begin(1, REPOSITORY.to_owned(), shown.stamp(), RerunIntent);
    flow.planned(load.generation, Ok(rerun_plan(&shown)));
    flow.confirm(String::new()).unwrap();
    let report = RerunReport {
        runs: vec![
            RunReport {
                run: RunId::new(41).unwrap(),
                outcome: RunOutcome::Requested,
            },
            RunReport {
                run: RunId::new(42).unwrap(),
                outcome: RunOutcome::Unknown {
                    problem: "the connection dropped".to_owned(),
                },
            },
        ],
    };
    flow.finished(1, Ok(report.clone()));
    let note = rerun_note(&flow).unwrap();
    assert!(
        note.contains("Run 41: re-run asked for.")
            && note.contains("Run 42: rc-git could not tell whether GitHub re-runs it"),
        "{note}"
    );
    assert!(flow_note_alarms(&TabFlow::Rerun(flow.clone())));
    let line = rerun_outcome_log(&flow, &Ended::Sent(report));
    assert_eq!(
        line,
        format!(
            "re-run failed jobs #7 of {REPOSITORY} head {} runs [41, 42]: run 41 requested; run \
             42 unknown: the connection dropped",
            oid(7)
        )
    );
    assert_eq!(flow_title(&TabFlow::Rerun(flow)), "Re-run Failed Jobs");
}

struct Harness {
    fake: FakePullRequests,
    model: Entity<PullRequestsModel>,
    events: Arc<Mutex<Vec<FlowEvent>>>,
    log: Arc<Mutex<Vec<String>>>,
}

/// A model over the fake, tab 7 reading `shown`, with `plan` scripted for auto-merge and re-run
/// plans.
fn harness(
    cx: &mut TestAppContext,
    shown: &PullRequestDetail,
    plan: rc_git_kit::pull_requests::ActionPlan,
) -> Harness {
    cx.executor().allow_parking();
    let fake = FakePullRequests::new();
    {
        let mut state = fake.state();
        state.details.insert(7, Ok(shown.clone()));
        state.action_plan = Ok(plan);
        state.auto_merge = Ok(AutoMergeOutcome::Done {
            settled: Settled::Answer,
        });
        state.rerun = Ok(RerunReport {
            runs: vec![RunReport {
                run: RunId::new(41).unwrap(),
                outcome: RunOutcome::Requested,
            }],
        });
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

    fn confirm(&self, cx: &mut TestAppContext) {
        self.model
            .update(cx, |m, cx| m.confirm_flow(7, String::new(), cx));
    }

    fn sends(&self) -> (usize, usize) {
        let state = self.fake.state();
        (state.action_calls.len(), state.actions_sent)
    }
}

#[gpui::test]
async fn turning_auto_merge_on_sends_once_then_reads_the_tab_again(cx: &mut TestAppContext) {
    let shown = auto_mergeable_detail(detail(7));
    let h = harness(cx, &shown, auto_merge_plan(&shown, SQUASH));
    h.open(cx).await;
    let details = h.fake.state().detail_calls.len();
    h.model.update(cx, |m, cx| {
        m.start_enable_auto_merge(MergeMethod::Squash, cx);
    });
    h.settle(cx);
    assert_eq!(*h.events.lock().unwrap(), [FlowEvent::Confirm(7)]);
    assert_eq!(
        h.fake.state().action_plan_calls,
        [(shown.stamp(), Some(SQUASH))]
    );
    h.model.update(cx, PullRequestsModel::start_rerun);
    assert_eq!(
        h.fake.state().action_plan_calls.len(),
        1,
        "one action at a time per tab"
    );
    h.confirm(cx);
    h.confirm(cx);
    h.settle(cx);
    assert_eq!(
        h.sends(),
        (1, 1),
        "one confirm, one send, however many clicks"
    );
    let call = h.fake.state().action_calls[0].clone();
    assert_eq!(call.change, Some(SQUASH));
    assert_eq!(call.stamp, shown.stamp());
    assert_eq!(
        call.decision,
        Some(auto_merge_plan(&shown, SQUASH).decision)
    );
    let flow = h
        .model
        .read_with(cx, |m, _| m.auto_merge_flow(7).unwrap().clone());
    assert!(matches!(
        flow.phase(),
        Phase::Done(Ended::Sent(AutoMergeOutcome::Done { .. }))
    ));
    assert_eq!(
        h.fake.state().detail_calls.len(),
        details + 1,
        "the tab reads again"
    );
    let log = h.log.lock().unwrap().clone();
    let head = oid(7).to_hex();
    assert_eq!(
        log,
        [
            format!(
                "turn on auto-merge with method squash #7 of {REPOSITORY} head {head}: sending"
            ),
            format!(
                "turn on auto-merge with method squash #7 of {REPOSITORY} head {head}: on \
                 (answer)"
            ),
        ]
    );
}

#[gpui::test]
async fn nothing_starts_without_its_control(cx: &mut TestAppContext) {
    let clean = detail(7);
    let h = harness(
        cx,
        &clean,
        auto_merge_plan(&auto_mergeable_detail(detail(7)), SQUASH),
    );
    h.open(cx).await;
    h.model.update(cx, |m, cx| {
        m.start_enable_auto_merge(MergeMethod::Squash, cx);
        m.start_disable_auto_merge(cx);
        m.start_rerun(cx);
    });
    h.confirm(cx);
    h.settle(cx);
    assert!(h.model.read_with(cx, |m, _| m.tab_flow(7).is_none()));
    assert!(h.fake.state().action_plan_calls.is_empty());
    assert_eq!(h.sends(), (0, 0));

    let offline = harness(
        cx,
        &clean,
        auto_merge_plan(&auto_mergeable_detail(detail(7)), SQUASH),
    );
    offline
        .fake
        .state()
        .details
        .insert(7, Err(ReadError::Failed("offline".to_owned())));
    offline.open(cx).await;
    offline.model.update(cx, PullRequestsModel::start_rerun);
    offline.settle(cx);
    assert!(offline.fake.state().action_plan_calls.is_empty());
}

#[gpui::test]
async fn a_confirmed_rerun_sends_once_and_turning_off_needs_it_on(cx: &mut TestAppContext) {
    let shown = failing_detail(detail(7), &[41]);
    let h = harness(cx, &shown, rerun_plan(&shown));
    h.open(cx).await;
    h.model
        .update(cx, PullRequestsModel::start_disable_auto_merge);
    h.settle(cx);
    assert!(
        h.fake.state().action_plan_calls.is_empty(),
        "auto-merge is off"
    );
    h.model.update(cx, PullRequestsModel::start_rerun);
    h.settle(cx);
    h.confirm(cx);
    h.settle(cx);
    assert_eq!(h.sends(), (1, 1));
    assert_eq!(h.fake.state().action_calls[0].change, None);
    let log = h.log.lock().unwrap().clone();
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(log[0].ends_with("runs [41]: sending"), "{log:?}");
    assert!(log[1].ends_with("runs [41]: run 41 requested"), "{log:?}");
}

#[gpui::test]
async fn a_plan_answer_of_an_old_link_is_dropped(cx: &mut TestAppContext) {
    let shown = failing_detail(detail(7), &[41]);
    let h = harness(cx, &shown, rerun_plan(&shown));
    h.open(cx).await;
    let gate = Arc::new(CallGate::new());
    h.fake.state().plan_gate = Some(Arc::clone(&gate));
    h.model.update(cx, PullRequestsModel::start_rerun);
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
        "the new link's #7 shows no re-run"
    );
    assert!(!h.events.lock().unwrap().contains(&FlowEvent::Confirm(7)));
    h.confirm(cx);
    h.settle(cx);
    assert_eq!(h.sends(), (0, 0));
}

#[gpui::test]
async fn cancel_stops_an_action_only_until_its_request_is_sent(cx: &mut TestAppContext) {
    let shown = auto_merge_on_detail(detail(7));
    let h = harness(
        cx,
        &shown,
        auto_merge_plan(&shown, AutoMergeChange::Disable),
    );
    h.open(cx).await;
    let unsent = Arc::new(CallGate::new());
    h.fake.state().unsent_gate = Some(Arc::clone(&unsent));
    h.model
        .update(cx, PullRequestsModel::start_disable_auto_merge);
    h.settle(cx);
    h.confirm(cx);
    assert!(unsent.wait_until_arrived());
    h.model.update(cx, |m, cx| m.cancel_flow(7, cx));
    unsent.open();
    let deadline = Instant::now() + Duration::from_secs(20);
    while h
        .model
        .read_with(cx, |m, _| m.tab_flow(7).is_some_and(TabFlow::is_running))
    {
        assert!(Instant::now() < deadline, "the cancel never landed");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(h.sends(), (1, 0), "asked, never sent");
    let flow = h
        .model
        .read_with(cx, |m, _| m.auto_merge_flow(7).unwrap().clone());
    assert_eq!(flow.phase(), &Phase::Done(Ended::NotSent(Stop::Cancelled)));
}

const TOKEN: &str = "ghp_live_action_token_0123";
const ID: &str = "PR_kwDOlive7";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// The detail read's answer for pull request 7 at [`HEAD`]: open, blocked, auto-merge allowed
/// and off, with a failed job of run 41.
fn pull() -> String {
    format!(
        r#"{{"data":{{"repository":{{"mergeCommitAllowed":true,"squashMergeAllowed":true,
        "rebaseMergeAllowed":false,"autoMergeAllowed":true,"pullRequest":{{"id":"{ID}",
        "number":7,"title":"Ship","body":"","url":null,"isDraft":false,"state":"OPEN",
        "updatedAt":"2026-11-18T09:00:00Z","author":{{"login":"mona"}},"headRefName":"topic",
        "headRefOid":"{HEAD}","baseRefName":"main","isCrossRepository":false,
        "headRepositoryOwner":{{"login":"octo"}},"reviewDecision":null,"mergeable":"MERGEABLE",
        "mergeStateStatus":"BLOCKED","viewerCanMergeAsAdmin":false,
        "viewerCanEnableAutoMerge":true,"viewerCanDisableAutoMerge":true,
        "viewerCanDeleteHeadRef":false,"autoMergeRequest":null,
        "commits":{{"nodes":[{{"commit":{{"statusCheckRollup":{{"state":"FAILURE",
        "contexts":{{"totalCount":1,"nodes":[{{"__typename":"CheckRun","name":"test",
        "status":"COMPLETED","conclusion":"FAILURE","detailsUrl":null,"databaseId":5,
        "isRequired":true,"checkSuite":{{"workflowRun":{{"databaseId":41}}}}}}]}}}}}}}}]}}}}}}}}}}"#
    )
}

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

fn stamp() -> PullRequestStamp {
    PullRequestStamp {
        number: 7,
        head_oid: rc_git_core::Oid::from_hex(HEAD).unwrap(),
        base_ref: "main".to_owned(),
        state: PullRequestState::Open,
    }
}

#[test]
fn the_live_service_turns_auto_merge_on_once_through_the_guard() {
    use rc_git_guard::actions::github::testing::{
        detail_request_body, enable_auto_merge_request_body,
    };
    let server = GitHubServer::start();
    let (fixture, service) = live(&server);
    let RepositoryLink::Linked(linked) = service.link(&fixture.path().to_string_lossy()) else {
        panic!("linked");
    };
    let on = pull().replace(
        r#""autoMergeRequest":null"#,
        r#""autoMergeRequest":{"mergeMethod":"SQUASH","enabledBy":{"login":"octocat"}}"#,
    );
    server.script_merge(MergeScript {
        read: detail_request_body(&linked.target, 7, false),
        open: pull(),
        moved: pull(),
        merged: on,
        mutation: enable_auto_merge_request_body(ID, HEAD, MergeMethod::Squash),
        reply: format!(
            r#"{{"data":{{"enablePullRequestAutoMerge":{{"pullRequest":{{"number":7,
            "state":"OPEN","headRefOid":"{HEAD}","baseRefName":"main",
            "autoMergeRequest":{{"mergeMethod":"SQUASH"}}}}}}}}}}"#
        ),
        behaviour: MergeBehaviour::Merge,
    });
    let stamp = stamp();
    let plan = service
        .plan_auto_merge(&linked, &stamp, SQUASH, &Cancel::new())
        .unwrap();
    let unconfirmed = ActionAsk {
        stamp: &stamp,
        decision: None,
        on_send: &|| {},
    };
    let refused = service
        .set_auto_merge(&linked, SQUASH, unconfirmed, &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(
            refused,
            ActionStop::Refused(ActionRefusal::NotConfirmed { .. })
        ),
        "{refused:?}"
    );
    let disable = service
        .set_auto_merge(
            &linked,
            AutoMergeChange::Disable,
            ActionAsk {
                decision: Some(&plan.decision),
                ..unconfirmed
            },
            &Cancel::new(),
        )
        .unwrap_err();
    assert!(
        matches!(disable, ActionStop::Refused(ActionRefusal::Blocked { .. })),
        "an enable answer never turns it off: {disable:?}"
    );
    assert_eq!(server.mutation_requests(), 0);
    let sends = Mutex::new(0);
    let on_send = || *sends.lock().unwrap() += 1;
    let ask = ActionAsk {
        decision: Some(&plan.decision),
        on_send: &on_send,
        ..unconfirmed
    };
    let outcome = service
        .set_auto_merge(&linked, SQUASH, ask, &Cancel::new())
        .unwrap();
    assert_eq!(
        outcome,
        AutoMergeOutcome::Done {
            settled: Settled::Answer
        }
    );
    assert_eq!(
        (
            server.mutations_named("RcGitEnableAutoMerge"),
            server.mutation_requests(),
            *sends.lock().unwrap()
        ),
        (1, 1, 1)
    );
}

#[test]
fn the_live_service_reruns_once_through_the_guard() {
    use rc_git_guard::actions::github::testing::detail_request_body;
    let server = GitHubServer::start();
    let (fixture, service) = live(&server);
    let RepositoryLink::Linked(linked) = service.link(&fixture.path().to_string_lossy()) else {
        panic!("linked");
    };
    server.script_graphql(&detail_request_body(&linked.target, 7, false), &pull());
    server.script_workflow_run(
        "octo/rc-git",
        41,
        "200 OK",
        &format!(r#"{{"id":41,"head_sha":"{HEAD}"}}"#),
    );
    server.script_rerun("octo/rc-git", 41, RerunBehaviour::Rerun);
    let stamp = stamp();
    let plan = service.plan_rerun(&linked, &stamp, &Cancel::new()).unwrap();
    let unconfirmed = ActionAsk {
        stamp: &stamp,
        decision: None,
        on_send: &|| {},
    };
    assert!(service.rerun(&linked, unconfirmed, &Cancel::new()).is_err());
    assert_eq!(server.mutation_requests(), 0);
    let ask = ActionAsk {
        decision: Some(&plan.decision),
        ..unconfirmed
    };
    let report = service.rerun(&linked, ask, &Cancel::new()).unwrap();
    assert_eq!(
        report.runs,
        [RunReport {
            run: RunId::new(41).unwrap(),
            outcome: RunOutcome::Requested
        }]
    );
    assert_eq!(server.rerun_requests("octo/rc-git", 41), 1);
    assert_eq!(server.mutation_requests(), 1);
}

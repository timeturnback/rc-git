//! A tab's job log: which checks offer it, the read as a state machine, the model over the fake
//! service (races ordered with `CallGate`s), the words, and the live service against the testkit's
//! fake GitHub host and its fake storage host.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_kit::fake::{
    CallGate, FakePullRequests, failing_detail, oid, pull_request, pull_request_detail,
};
use rc_git_kit::github::accounts::{GitHubAccount, GitHubAccounts, SignInMethod};
use rc_git_kit::github::{ApiToken, GitHubHost, TokenSet};
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore, MemoryApiTokenStore};
use rc_git_kit::platform::settings::{MemorySettings, SettingsStore};
use rc_git_kit::pull_requests::copy::{log_cut_notes, log_name_note, log_note, log_title};
use rc_git_kit::pull_requests::link::RepositoryLink;
use rc_git_kit::pull_requests::live::LivePullRequests;
use rc_git_kit::pull_requests::logs::{
    LogAsk, LogCheck, LogPhase, LogState, LogStop, ShownLog, log_check,
};
use rc_git_kit::pull_requests::model::PullRequestsModel;
use rc_git_kit::pull_requests::state::DetailState;
use rc_git_kit::pull_requests::{
    Cancel, CheckConclusion, CheckList, CheckProgress, CheckStatus, JobId, JobLog, LogCut,
    PullRequestDetail, PullRequestService, ReadError, RunId,
};
use rc_git_testkit::Fixture;
use rc_git_testkit::github::{
    FakeToken, GitHubServer, LogsAnswer, StorageBehaviour, StorageServer,
};

const REPO: &str = "/work/rc-git";

/// Pull request 7 at `head` with a failed job of run 41 (job 1041) and of run 42 (job 1042).
fn detail(head: u8) -> PullRequestDetail {
    let detail = pull_request_detail(
        pull_request(7, "Ship it", oid(head)),
        "",
        CheckList::default(),
    );
    failing_detail(detail, &[41, 42])
}

/// A tab whose last read answered `answer`.
fn tab(answer: Result<PullRequestDetail, ReadError>) -> DetailState {
    let mut tab = DetailState::new(7);
    let load = tab.refresh().unwrap();
    tab.finish(&load, answer, 1_000);
    tab
}

fn job(id: u64) -> JobId {
    JobId::new(id).unwrap()
}

fn check(job_id: u64, run: u64) -> LogCheck {
    LogCheck {
        name: format!("job {}", usize::from(run == 42)),
        run: RunId::new(run).unwrap(),
        job: job(job_id),
    }
}

/// The guard's log of `check` at `head`: two lines, cut at its start.
fn job_log(check: &LogCheck, head: u8) -> JobLog {
    JobLog {
        job: check.job,
        run: check.run,
        head: oid(head),
        name: check.name.clone(),
        text: "first\nsecond".to_owned(),
        lines: 2,
        cut: LogCut::Tail { kept: 2_097_100 },
        lines_left_out: 1_234,
        lines_shortened: 2,
    }
}

#[test]
fn only_a_completed_actions_job_on_a_current_read_offers_its_log() {
    let shown = detail(7);
    let current = tab(Ok(shown.clone()));
    let failed = &shown.checks.checks[0];
    assert_eq!(log_check(&current, failed), Some(check(1041, 41)));
    assert_eq!(
        log_check(&DetailState::new(7), failed),
        None,
        "nothing read"
    );
    let mut stale = current.clone();
    let load = stale.refresh().unwrap();
    stale.finish(&load, Err(ReadError::Failed("offline".to_owned())), 2_000);
    assert_eq!(
        log_check(&stale, failed),
        None,
        "a stale read offers nothing"
    );
    let mut others = shown.clone();
    let mut running = others.checks.checks[0].clone();
    running.progress = CheckProgress::Run {
        status: CheckStatus::InProgress,
        conclusion: None,
    };
    let mut passed = others.checks.checks[1].clone();
    passed.progress = CheckProgress::Run {
        status: CheckStatus::Completed,
        conclusion: Some(CheckConclusion::Success),
    };
    let mut other_app = others.checks.checks[0].clone();
    other_app.actions = None;
    let mut zero = others.checks.checks[0].clone();
    zero.actions.as_mut().unwrap().job = 0;
    others.checks.checks = vec![
        running.clone(),
        passed.clone(),
        other_app.clone(),
        zero.clone(),
    ];
    let others_tab = tab(Ok(others));
    assert_eq!(log_check(&others_tab, &running), None, "a running job");
    assert_eq!(
        log_check(&others_tab, &passed),
        Some(check(1042, 42)),
        "any conclusion"
    );
    assert_eq!(
        log_check(&others_tab, &other_app),
        None,
        "another app's check"
    );
    assert_eq!(
        log_check(&others_tab, &zero),
        None,
        "an id the guard refuses"
    );
    assert_eq!(
        log_check(&tab(Ok(detail(7))), &running),
        None,
        "a check not on screen"
    );
}

#[test]
fn a_log_state_takes_only_its_own_generation_once() {
    let mut log = LogState::begin(3, 7, oid(7), check(1041, 41));
    assert!(log.is_running());
    assert!(!log.matched(2));
    assert!(log.matched(3));
    assert_eq!(log.phase(), &LogPhase::Downloading);
    let shown = Arc::new(ShownLog::new(job_log(&check(1041, 41), 7)));
    assert!(!log.finish(2, Ok(Arc::clone(&shown))));
    assert!(log.finish(3, Err(LogStop::NotMatched("another commit".to_owned()))));
    assert!(
        !log.finish(3, Ok(shown)),
        "an ended read takes no second answer"
    );
    assert_eq!(
        log.phase(),
        &LogPhase::NotMatched("another commit".to_owned())
    );
    assert!(log.follows(oid(7)) && !log.follows(oid(8)));
}

#[test]
fn the_log_words_name_what_was_left_out() {
    let log = ShownLog::new(job_log(&check(1041, 41), 7));
    assert_eq!(log.document.text, "first\nsecond");
    assert_eq!(log.document.line_starts, [0, 6]);
    assert_eq!(
        log_cut_notes(log.cut, log.lines_left_out, log.lines_shortened),
        [
            "Showing the last 2.1 MB of the log; the earlier part was not downloaded.",
            "The first 1,234 lines downloaded are not shown.",
            "2 long lines are cut at 1,000 characters, ending in \u{2026}.",
        ]
    );
    let mut state = LogState::begin(1, 7, oid(7), check(1041, 41));
    assert_eq!(log_title(&state), "Log of \u{201c}job 0\u{201d}");
    assert!(
        log_note(&state, 0)
            .unwrap()
            .starts_with("Checking that \u{201c}job 0\u{201d} ran")
    );
    state.finish(
        1,
        Err(LogStop::NotMatched(
            "job 1041 ran on commit 0808080".to_owned(),
        )),
    );
    assert_eq!(
        log_note(&state, 0).unwrap(),
        "This check's log could not be matched to the pull request's head commit, so RCGit shows \
         none: job 1041 ran on commit 0808080."
    );
    let mut limited = LogState::begin(2, 7, oid(7), check(1041, 41));
    limited.finish(
        2,
        Err(LogStop::Read(ReadError::RateLimited { reset: Some(700) })),
    );
    assert_eq!(
        log_note(&limited, 100).unwrap(),
        "Couldn't read the log: the host's rate limit is spent for about 10 more minutes."
    );
}

#[test]
fn a_shown_log_is_titled_with_the_job_s_own_name_and_says_when_the_check_differs() {
    let mut same = LogState::begin(1, 7, oid(7), check(1041, 41));
    let log = job_log(&check(1041, 41), 7);
    same.finish(1, Ok(Arc::new(ShownLog::new(log.clone()))));
    assert_eq!(log_title(&same), "Log of \u{201c}job 0\u{201d}");
    assert_eq!(log_name_note(&same), None, "the names match");
    let mut renamed = LogState::begin(2, 7, oid(7), check(1041, 41));
    let other = JobLog {
        name: "deploy \\u{202e}".to_owned(),
        ..log
    };
    renamed.finish(2, Ok(Arc::new(ShownLog::new(other))));
    assert_eq!(
        log_title(&renamed),
        "Log of \u{201c}deploy \\u{202e}\u{201d}"
    );
    assert_eq!(
        log_name_note(&renamed).unwrap(),
        "GitHub names this job \u{201c}deploy \\u{202e}\u{201d}; its check on this page is named \
         \u{201c}job 0\u{201d}."
    );
    let reading = LogState::begin(3, 7, oid(7), check(1041, 41));
    assert_eq!(log_name_note(&reading), None, "no log shows yet");
}

/// A model over the fake, tab 7 reading `shown`, its log answering `log`.
fn harness(
    cx: &mut TestAppContext,
    shown: &PullRequestDetail,
) -> (FakePullRequests, Entity<PullRequestsModel>) {
    cx.executor().allow_parking();
    let fake = FakePullRequests::new();
    fake.state().details.insert(7, Ok(shown.clone()));
    let service: Arc<dyn PullRequestService> = Arc::new(fake.clone());
    let model = cx.new(|_| PullRequestsModel::new(service, Arc::new(|| 1_000)));
    (fake, model)
}

/// Runs until no read is in flight.
fn settle(model: &Entity<PullRequestsModel>, cx: &mut TestAppContext) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        let busy = model.read_with(cx, |m, _| {
            m.link().is_none()
                || m.list().is_loading()
                || m.tabs().iter().any(DetailState::is_loading)
                || m.tab_log(7).is_some_and(LogState::is_running)
        });
        if !busy {
            return;
        }
        assert!(Instant::now() < deadline, "the work never ended");
        std::thread::sleep(Duration::from_millis(1));
    }
}

async fn open(model: &Entity<PullRequestsModel>, cx: &mut TestAppContext) {
    model
        .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
        .await;
    settle(model, cx);
    model.update(cx, |m, cx| m.open(7, cx));
    settle(model, cx);
}

fn phase(model: &Entity<PullRequestsModel>, cx: &mut TestAppContext) -> Option<LogPhase> {
    model.read_with(cx, |m, _| m.tab_log(7).map(|log| log.phase().clone()))
}

#[gpui::test]
async fn a_click_reads_the_log_once_for_the_head_on_screen(cx: &mut TestAppContext) {
    let shown = detail(7);
    let (fake, model) = harness(cx, &shown);
    fake.state().log = Ok(job_log(&check(1041, 41), 7));
    model.update(cx, |m, cx| m.view_log(job(1041), cx));
    assert!(fake.state().log_calls.is_empty(), "no tab, no read");
    open(&model, cx).await;
    let offered = model.read_with(cx, |m, _| m.log_check(7, &shown.checks.checks[0]));
    assert_eq!(offered, Some(check(1041, 41)));
    model.update(cx, |m, cx| m.view_log(job(9999), cx));
    assert!(
        fake.state().log_calls.is_empty(),
        "a job not on screen reads nothing"
    );
    model.update(cx, |m, cx| m.view_log(job(1041), cx));
    settle(&model, cx);
    assert_eq!(fake.state().log_calls, [(oid(7), check(1041, 41))]);
    let Some(LogPhase::Shown(log)) = phase(&model, cx) else {
        panic!("{:?}", phase(&model, cx));
    };
    assert_eq!(log.document.text, "first\nsecond");
    model.update(cx, |m, cx| m.close_log(7, cx));
    assert_eq!(phase(&model, cx), None);
}

#[gpui::test]
async fn a_second_click_cancels_the_read_in_flight_and_only_its_answer_lands(
    cx: &mut TestAppContext,
) {
    let shown = detail(7);
    let (fake, model) = harness(cx, &shown);
    open(&model, cx).await;
    let gate = Arc::new(CallGate::new());
    fake.state().log_gate = Some(Arc::clone(&gate));
    fake.state().log = Ok(job_log(&check(1042, 42), 7));
    model.update(cx, |m, cx| m.view_log(job(1041), cx));
    assert!(gate.wait_until_arrived());
    model.update(cx, |m, cx| m.view_log(job(1042), cx));
    assert!(gate.wait_until_arrived());
    gate.open();
    settle(&model, cx);
    assert_eq!(fake.state().log_calls.len(), 2, "one read per click");
    let state = model.read_with(cx, |m, _| m.tab_log(7).cloned()).unwrap();
    assert_eq!(state.check(), &check(1042, 42));
    assert!(
        matches!(state.phase(), LogPhase::Shown(_)),
        "the cancelled first read never lands: {:?}",
        state.phase()
    );
}

#[gpui::test]
async fn a_stopped_read_or_a_closed_tab_drops_its_answer(cx: &mut TestAppContext) {
    let shown = detail(7);
    let (fake, model) = harness(cx, &shown);
    open(&model, cx).await;
    let gate = Arc::new(CallGate::new());
    fake.state().log_gate = Some(Arc::clone(&gate));
    fake.state().log = Ok(job_log(&check(1041, 41), 7));
    model.update(cx, |m, cx| m.view_log(job(1041), cx));
    assert!(gate.wait_until_arrived());
    model.update(cx, |m, cx| m.close_log(7, cx));
    gate.open();
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(50));
    cx.run_until_parked();
    assert_eq!(phase(&model, cx), None, "a stopped read shows nothing");
    let gate = Arc::new(CallGate::new());
    fake.state().log_gate = Some(Arc::clone(&gate));
    model.update(cx, |m, cx| m.view_log(job(1041), cx));
    assert!(gate.wait_until_arrived());
    model.update(cx, |m, cx| m.close(7, cx));
    model.update(cx, |m, cx| m.open(7, cx));
    gate.open();
    settle(&model, cx);
    assert_eq!(
        phase(&model, cx),
        None,
        "the reopened tab shows no old read"
    );
}

#[gpui::test]
async fn a_moved_head_or_another_link_drops_the_log(cx: &mut TestAppContext) {
    let shown = detail(7);
    let (fake, model) = harness(cx, &shown);
    open(&model, cx).await;
    fake.state().log = Ok(job_log(&check(1041, 41), 7));
    model.update(cx, |m, cx| m.view_log(job(1041), cx));
    settle(&model, cx);
    assert!(matches!(phase(&model, cx), Some(LogPhase::Shown(_))));
    model.update(cx, PullRequestsModel::refresh_detail);
    settle(&model, cx);
    assert!(
        matches!(phase(&model, cx), Some(LogPhase::Shown(_))),
        "the same head keeps it"
    );
    fake.state().details.insert(7, Ok(detail(8)));
    model.update(cx, PullRequestsModel::refresh_detail);
    settle(&model, cx);
    assert_eq!(phase(&model, cx), None, "another head drops it");

    fake.state().details.insert(7, Ok(detail(7)));
    model.update(cx, PullRequestsModel::refresh_detail);
    settle(&model, cx);
    let gate = Arc::new(CallGate::new());
    fake.state().log_gate = Some(Arc::clone(&gate));
    model.update(cx, |m, cx| m.view_log(job(1041), cx));
    assert!(gate.wait_until_arrived());
    let mut other = rc_git_kit::fake::linked();
    other.target = rc_git_kit::pull_requests::RepositoryTarget::on_host(
        &GitHubHost::github_com(),
        "https://github.com/octo/other",
    )
    .unwrap();
    fake.state().link = RepositoryLink::Linked(other);
    fake.state().log_gate = None;
    model.update(cx, PullRequestsModel::relink).await;
    settle(&model, cx);
    model.update(cx, |m, cx| m.open(7, cx));
    gate.open();
    settle(&model, cx);
    assert_eq!(
        phase(&model, cx),
        None,
        "the new link's #7 shows no old log"
    );
}

const TOKEN: &str = "ghp_live_log_token_0123";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const SIGNATURE: &str = "bGl2ZS1zaWduYXR1cmUtMDEyMzQ1Njc4OQ";

#[test]
fn the_live_service_reads_a_matched_log_and_never_sends_the_token_to_storage() {
    let server = GitHubServer::start();
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
    let RepositoryLink::Linked(linked) = service.link(&fixture.path().to_string_lossy()) else {
        panic!("linked");
    };
    let storage = StorageServer::start(StorageBehaviour::Log {
        body: b"\x1b[32mok\x1b[0m\n".to_vec(),
        ranges: true,
    });
    server.script_job(
        "octo/rc-git",
        5,
        "200 OK",
        &format!(
            r#"{{"id":5,"run_id":41,"head_sha":"{HEAD}","name":"test","status":"completed",
            "conclusion":"failure"}}"#
        ),
    );
    server.script_job_logs(
        "octo/rc-git",
        5,
        LogsAnswer::Redirect {
            status: "302 Found",
            location: format!("{}/job-5.txt?sig={SIGNATURE}", storage.origin()),
        },
    );
    let check = LogCheck {
        name: "test".to_owned(),
        run: RunId::new(41).unwrap(),
        job: job(5),
    };
    let ask = LogAsk {
        head: rc_git_core::Oid::from_hex(HEAD).unwrap(),
        check: &check,
        on_matched: &|| {},
    };
    let log = service.read_log(&linked, ask, &Cancel::new()).unwrap();
    assert_eq!((log.text.as_str(), log.cut), ("ok", LogCut::Whole));
    let downloads = storage.recorded();
    assert_eq!(downloads.len(), 1);
    assert_eq!(downloads[0].header("authorization"), None);
    assert!(
        downloads[0]
            .headers
            .iter()
            .all(|(_, value)| !value.contains(TOKEN))
    );
    let moved = LogAsk {
        head: oid(9),
        ..ask
    };
    let refused = service
        .read_log(&linked, moved, &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(&refused, LogStop::NotMatched(why) if why.contains("ran on commit")),
        "{refused:?}"
    );
    assert_eq!(storage.requests(), 1, "an unmatched job downloads nothing");
    assert_eq!(server.log_requests("octo/rc-git", 5), 1);
}

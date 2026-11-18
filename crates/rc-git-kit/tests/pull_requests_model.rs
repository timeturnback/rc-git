//! `PullRequestsModel` over the fake service, and the live service over the testkit's fake
//! GitHub host. Reads run on real threads, so races are ordered with `CallGate`s; polls ride
//! the test executor's clock.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_kit::fake::{
    CallGate, FakePullRequests, linked, oid, page, pull_request, pull_request_detail,
};
use rc_git_kit::github::accounts::{GitHubAccount, GitHubAccounts, SignInMethod};
use rc_git_kit::github::{ApiToken, GitHubHost, TokenSet};
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore, MemoryApiTokenStore};
use rc_git_kit::platform::settings::{MemorySettings, SettingsStore};
use rc_git_kit::pull_requests::link::RepositoryLink;
use rc_git_kit::pull_requests::live::LivePullRequests;
use rc_git_kit::pull_requests::model::PullRequestsModel;
use rc_git_kit::pull_requests::polling::{DETAIL_INTERVAL, LIST_INTERVAL};
use rc_git_kit::pull_requests::state::{DetailState, Freshness};
use rc_git_kit::pull_requests::{
    Cancel, Check, CheckList, CheckProgress, CheckStatus, PullRequestService, ReadError,
};
use rc_git_testkit::Fixture;
use rc_git_testkit::github::{FakeToken, GitHubServer};

const REPO: &str = "/work/rc-git";

struct Harness {
    fake: FakePullRequests,
    model: Entity<PullRequestsModel>,
    now: Arc<AtomicU64>,
}

fn harness(cx: &mut TestAppContext, fake: FakePullRequests) -> Harness {
    cx.executor().allow_parking();
    let now = Arc::new(AtomicU64::new(1_000));
    let clock_now = Arc::clone(&now);
    let service: Arc<dyn PullRequestService> = Arc::new(fake.clone());
    let model = cx.new(|_| {
        PullRequestsModel::new(service, Arc::new(move || clock_now.load(Ordering::SeqCst)))
    });
    Harness { fake, model, now }
}

impl Harness {
    fn settle(&self, cx: &mut TestAppContext) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let busy = self.model.read_with(cx, |m, _| {
                m.link().is_none()
                    || m.list().is_loading()
                    || m.tabs().iter().any(DetailState::is_loading)
            });
            if !busy {
                return;
            }
            assert!(Instant::now() < deadline, "the reads never ended");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn list_calls(&self) -> usize {
        self.fake.state().list_calls.len()
    }

    /// Moves both clocks on by `by`.
    fn advance(&self, cx: &mut TestAppContext, by: Duration) {
        self.now.fetch_add(by.as_secs(), Ordering::SeqCst);
        cx.executor().advance_clock(by);
        self.settle(cx);
    }
}

fn listed(fake: &FakePullRequests) {
    fake.state().pages.insert(
        None,
        Ok(page(vec![pull_request(7, "Add the thing", oid(7))], None)),
    );
}

#[gpui::test]
async fn attaching_reads_the_link_and_one_page(cx: &mut TestAppContext) {
    let h = harness(cx, FakePullRequests::new());
    listed(&h.fake);
    h.model
        .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
        .await;
    h.settle(cx);
    h.model.read_with(cx, |m, _| {
        assert_eq!(m.list().rows()[0].title, "Add the thing");
        assert_eq!(m.list().freshness(), &Freshness::Current { at: 1_000 });
    });
    let link_calls = h.fake.state().link_calls;
    assert_eq!((link_calls, h.list_calls()), (1, 1));
}

#[gpui::test]
async fn a_section_needing_a_sign_in_reads_nothing(cx: &mut TestAppContext) {
    let fake = FakePullRequests::with_link(RepositoryLink::SignIn {
        host: GitHubHost::github_com(),
    });
    let h = harness(cx, fake);
    h.model
        .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
        .await;
    h.advance(cx, LIST_INTERVAL * 3);
    assert_eq!(h.list_calls(), 0);
    assert!(!h.model.read_with(cx, |m, _| m.list_poll_armed()));
}

#[gpui::test]
async fn refreshes_during_a_slow_read_cost_one_more_read(cx: &mut TestAppContext) {
    let h = harness(cx, FakePullRequests::new());
    listed(&h.fake);
    h.model
        .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
        .await;
    h.settle(cx);
    let gate = Arc::new(CallGate::new());
    h.fake.state().list_gate = Some(Arc::clone(&gate));
    h.model.update(cx, PullRequestsModel::refresh_list);
    assert!(gate.wait_until_arrived());
    for _ in 0..20 {
        h.model.update(cx, PullRequestsModel::refresh_list);
    }
    gate.open();
    h.settle(cx);
    assert_eq!(
        h.list_calls(),
        3,
        "the first read, the slow one, and one for the burst"
    );
}

#[gpui::test]
async fn the_list_polls_each_minute_only_while_shown_in_the_active_window(cx: &mut TestAppContext) {
    let h = harness(cx, FakePullRequests::new());
    listed(&h.fake);
    h.model
        .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
        .await;
    h.settle(cx);
    h.advance(cx, LIST_INTERVAL);
    assert_eq!(h.list_calls(), 2);
    h.model.update(cx, |m, cx| m.set_window_active(false, cx));
    h.advance(cx, LIST_INTERVAL * 5);
    assert_eq!(h.list_calls(), 2, "an inactive window polls nothing");
    h.model.update(cx, |m, cx| m.set_window_active(true, cx));
    h.settle(cx);
    assert_eq!(
        h.list_calls(),
        3,
        "back in front, an overdue list reads once"
    );
    h.model.update(cx, |m, cx| m.set_section_visible(false, cx));
    h.advance(cx, LIST_INTERVAL * 5);
    assert_eq!(h.list_calls(), 3, "a hidden section polls nothing");
}

#[gpui::test]
async fn a_rate_limit_holds_the_poll_until_its_reset(cx: &mut TestAppContext) {
    let h = harness(cx, FakePullRequests::new());
    h.fake.state().pages.insert(
        None,
        Err(ReadError::RateLimited {
            reset: Some(1_000 + 600),
        }),
    );
    h.model
        .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
        .await;
    h.settle(cx);
    h.advance(cx, LIST_INTERVAL * 9);
    assert_eq!(h.list_calls(), 1, "nothing before the reset");
    h.advance(cx, LIST_INTERVAL * 2);
    assert_eq!(h.list_calls(), 2);
}

#[gpui::test]
async fn a_relink_while_a_page_reads_drops_the_old_answer(cx: &mut TestAppContext) {
    let h = harness(cx, FakePullRequests::new());
    listed(&h.fake);
    let gate = Arc::new(CallGate::new());
    h.fake.state().list_gate = Some(Arc::clone(&gate));
    let attach = h.model.update(cx, |m, cx| m.attach(REPO.to_owned(), cx));
    attach.await;
    assert!(gate.wait_until_arrived());
    h.fake.state().link = RepositoryLink::SignIn {
        host: GitHubHost::github_com(),
    };
    h.model.update(cx, PullRequestsModel::relink).await;
    gate.open();
    h.settle(cx);
    h.model.read_with(cx, |m, _| {
        assert!(matches!(m.link(), Some(RepositoryLink::SignIn { .. })));
        assert!(
            m.list().rows().is_empty(),
            "the old repository's rows never landed"
        );
    });
}

#[gpui::test]
async fn a_link_read_overtaken_by_a_newer_one_is_dropped(cx: &mut TestAppContext) {
    let h = harness(cx, FakePullRequests::new());
    listed(&h.fake);
    let (first, second) = (Arc::new(CallGate::new()), Arc::new(CallGate::new()));
    h.fake
        .state()
        .link_gates
        .extend([Arc::clone(&first), Arc::clone(&second)]);
    let attach = h.model.update(cx, |m, cx| m.attach(REPO.to_owned(), cx));
    assert!(
        first.wait_until_arrived(),
        "the first read took the linked answer"
    );
    h.fake.state().link = RepositoryLink::SignIn {
        host: GitHubHost::github_com(),
    };
    let relink = h.model.update(cx, PullRequestsModel::relink);
    assert!(second.wait_until_arrived());
    second.open();
    relink.await;
    first.open();
    attach.await;
    h.settle(cx);
    let link = h.model.read_with(cx, |m, _| m.link().cloned());
    assert!(
        matches!(link, Some(RepositoryLink::SignIn { .. })),
        "{link:?}"
    );
    assert_eq!(h.list_calls(), 0, "the overtaken link never read a page");
}

fn running() -> CheckList {
    CheckList {
        checks: vec![Check {
            name: "build".to_owned(),
            progress: CheckProgress::Run {
                status: CheckStatus::InProgress,
                conclusion: None,
            },
            description: None,
            required: Some(true),
            details: None,
            actions: None,
        }],
        total: 1,
    }
}

#[gpui::test]
async fn an_open_tab_polls_while_a_check_runs_and_stops_after(cx: &mut TestAppContext) {
    let h = harness(cx, FakePullRequests::new());
    listed(&h.fake);
    let detail = pull_request_detail(pull_request(7, "t", oid(7)), "body", running());
    h.fake.state().details.insert(7, Ok(detail.clone()));
    h.model
        .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
        .await;
    h.settle(cx);
    h.model.update(cx, |m, cx| m.open(7, cx));
    h.settle(cx);
    let stamp = h
        .model
        .read_with(cx, |m, _| m.selected_tab().unwrap().stamp());
    assert_eq!(stamp.unwrap().head_oid, oid(7));
    h.advance(cx, DETAIL_INTERVAL);
    assert_eq!(h.fake.state().detail_calls, [7, 7]);
    let done = pull_request_detail(pull_request(7, "t", oid(7)), "body", CheckList::default());
    h.fake.state().details.insert(7, Ok(done));
    h.advance(cx, DETAIL_INTERVAL);
    h.advance(cx, DETAIL_INTERVAL * 4);
    assert_eq!(
        h.fake.state().detail_calls,
        [7, 7, 7],
        "nothing runs, nothing polls"
    );
    h.model.update(cx, |m, cx| m.close(7, cx));
    assert!(
        h.model
            .read_with(cx, |m, _| m.tabs().is_empty() && !m.page_visible())
    );
}

#[gpui::test]
async fn choosing_an_account_saves_it_then_links_again(cx: &mut TestAppContext) {
    let key = AccountKey::new(GitHubHost::github_com(), "octocat").unwrap();
    let fake = FakePullRequests::with_link(RepositoryLink::ChooseAccount {
        host: GitHubHost::github_com(),
        accounts: vec![key.clone()],
    });
    let h = harness(cx, fake);
    h.model
        .update(cx, |m, cx| m.attach(REPO.to_owned(), cx))
        .await;
    h.settle(cx);
    h.fake.state().link = RepositoryLink::Linked(linked());
    listed(&h.fake);
    h.model
        .update(cx, |m, cx| m.choose_account(key.clone(), cx))
        .await;
    h.settle(cx);
    assert_eq!(h.fake.state().chosen, [(REPO.to_owned(), key.id())]);
    assert_eq!(h.list_calls(), 1);
}

const TOKEN: &str = "ghp_live_service_token_0123";

/// A repository whose `origin` is `octo/rc-git` on `server`, an account for `server` with
/// [`TOKEN`] stored, and the live service over both.
fn live(server: &GitHubServer) -> (Fixture, LivePullRequests, Arc<MemoryApiTokenStore>) {
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
    let ssh = Fixture::empty_dir();
    let service = LivePullRequests::new(settings, Arc::clone(&tokens) as Arc<dyn ApiTokenStore>)
        .with_ssh_dir(Some(ssh.path().to_path_buf()));
    (fixture, service, tokens)
}

#[test]
fn the_live_service_reads_with_the_account_token_from_the_account_host_only() {
    let server = GitHubServer::start();
    server.add_token(FakeToken {
        token: TOKEN.to_owned(),
        login: "octocat".to_owned(),
        scopes: None,
        expires: None,
    });
    let (fixture, service, tokens) = live(&server);
    let path = fixture.path().to_string_lossy().into_owned();
    let RepositoryLink::Linked(linked) = service.link(&path) else {
        panic!("linked");
    };
    let body = rc_git_guard::actions::github::testing::list_request_body(&linked.target, None);
    server.script_graphql(
        &body,
        r#"{"data":{"repository":{"pullRequests":{"pageInfo":{"hasNextPage":false},"nodes":[]}}}}"#,
    );
    let page = service.list(&linked, None, &Cancel::new()).unwrap();
    assert!(page.pull_requests.is_empty() && page.next.is_none());
    let recorded = server.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    tokens.delete_tokens(&linked.account.key).unwrap();
    let signed_out = service.list(&linked, None, &Cancel::new()).unwrap_err();
    assert!(
        matches!(signed_out, ReadError::SignedOut { .. }),
        "{signed_out:?}"
    );
    assert_eq!(server.requests(), 1, "a signed-out account sends nothing");
}

//! The pull request rules that need no GPUI: which account and repository a remote links to,
//! the list and tab state machines, the polling plan, the checks' order and summary, which
//! description links may open, and the words.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::collections::BTreeMap;
use std::time::Duration;

use rc_git_kit::fake::{oid, page, pull_request, pull_request_detail};
use rc_git_kit::github::accounts::{GitHubAccount, SignInMethod};
use rc_git_kit::github::{ClientId, GitHubHost};
use rc_git_kit::markdown::MarkdownDocument;
use rc_git_kit::platform::api_tokens::AccountKey;
use rc_git_kit::pull_requests::body::{BodyLink, body_link};
use rc_git_kit::pull_requests::checks::{CheckState, ordered, pending, summary};
use rc_git_kit::pull_requests::copy::{branches, freshness_problem, problem};
use rc_git_kit::pull_requests::link::{RepositoryLink, link_repository};
use rc_git_kit::pull_requests::polling::{
    DETAIL_INTERVAL, LIST_INTERVAL, PollInputs, PollPlan, detail_poll, is_due, list_poll,
};
use rc_git_kit::pull_requests::state::{DetailState, Freshness, ListState, MAX_PAGES};
use rc_git_kit::pull_requests::{
    Check, CheckConclusion, CheckList, CheckProgress, CheckStatus, PullRequestRemote, ReadError,
    StatusState,
};
use rc_git_testkit::Fixture;

const REPO: &str = "/work/rc-git";

fn account(host: &str, login: &str) -> GitHubAccount {
    GitHubAccount {
        key: AccountKey::new(GitHubHost::parse(host).unwrap(), login).unwrap(),
        method: SignInMethod::GitHubApp,
        client_id: Some(ClientId::parse("Iv23li.client").unwrap()),
        scopes: None,
        token_expires: None,
    }
}

/// The pull request remote of a scratch repository whose `origin` is `url`.
fn remote(url: &str) -> (Fixture, PullRequestRemote) {
    let fixture = Fixture::standard();
    fixture.git(&["remote", "add", "origin", url]);
    let remote = rc_git_guard::actions::github::pull_request_remote(fixture.path())
        .unwrap()
        .unwrap();
    (fixture, remote)
}

fn link(
    url: &str,
    accounts: &[GitHubAccount],
    choices: &BTreeMap<String, String>,
) -> RepositoryLink {
    let (_fixture, remote) = remote(url);
    let ssh = Fixture::empty_dir();
    link_repository(Some(&remote), REPO, accounts, choices, Some(ssh.path()))
}

#[test]
fn one_account_on_the_host_links_the_repository() {
    let accounts = [
        account("github.com", "octocat"),
        account("ghe.example.com", "octocat"),
    ];
    let RepositoryLink::Linked(linked) = link(
        "git@github.com:octo/rc-git.git",
        &accounts,
        &BTreeMap::new(),
    ) else {
        panic!("linked");
    };
    assert_eq!(linked.account, accounts[0]);
    assert_eq!(
        (linked.target.owner(), linked.target.name()),
        ("octo", "rc-git")
    );
    assert_eq!(linked.remote, "origin");
}

#[test]
fn several_accounts_ask_until_one_is_chosen_and_never_fall_through() {
    let accounts = [
        account("github.com", "mona"),
        account("github.com", "octocat"),
    ];
    let url = "https://github.com/octo/rc-git";
    assert_eq!(
        link(url, &accounts, &BTreeMap::new()),
        RepositoryLink::ChooseAccount {
            host: GitHubHost::github_com(),
            accounts: vec![accounts[0].key.clone(), accounts[1].key.clone()],
        }
    );
    let chosen = BTreeMap::from([(REPO.to_owned(), accounts[1].key.id())]);
    let RepositoryLink::Linked(linked) = link(url, &accounts, &chosen) else {
        panic!("linked");
    };
    assert_eq!(linked.account, accounts[1]);
    let elsewhere = BTreeMap::from([("/other".to_owned(), accounts[1].key.id())]);
    assert!(matches!(
        link(url, &accounts, &elsewhere),
        RepositoryLink::ChooseAccount { .. }
    ));
}

#[test]
fn no_account_offers_the_sign_in_on_github_com_and_hides_elsewhere() {
    assert_eq!(
        link("https://github.com/octo/rc-git", &[], &BTreeMap::new()),
        RepositoryLink::SignIn {
            host: GitHubHost::github_com()
        }
    );
    for url in [
        "https://gitlab.example/octo/rc-git",
        "https://user:pw@github.com/octo/rc-git",
        "https://github.com.evil.example/octo/rc-git",
    ] {
        let accounts = [account("github.com", "octocat")];
        assert_eq!(
            link(url, &accounts, &BTreeMap::new()),
            RepositoryLink::Hidden,
            "{url}"
        );
    }
    assert_eq!(
        link_repository(None, REPO, &[], &BTreeMap::new(), None),
        RepositoryLink::Hidden
    );
}

#[test]
fn an_ssh_alias_links_through_the_hostname_the_user_mapped_it_to() {
    let (_fixture, aliased) = remote("git@github-work:octo/rc-git.git");
    let accounts = [account("github.com", "octocat")];
    let mapped = Fixture::empty_dir();
    mapped.write("config", b"Host github-work\n  HostName github.com\n");
    let linked = link_repository(
        Some(&aliased),
        REPO,
        &accounts,
        &BTreeMap::new(),
        Some(mapped.path()),
    );
    let RepositoryLink::Linked(linked) = linked else {
        panic!("{linked:?}");
    };
    assert_eq!(linked.target.host(), &GitHubHost::github_com());
    let evil = Fixture::empty_dir();
    evil.write(
        "config",
        b"Host github-work\n  HostName github.com.evil.example\n",
    );
    let refused = link_repository(
        Some(&aliased),
        REPO,
        &accounts,
        &BTreeMap::new(),
        Some(evil.path()),
    );
    assert_eq!(refused, RepositoryLink::Hidden);
}

fn rows(state: &ListState) -> Vec<u64> {
    state.rows().iter().map(|row| row.number).collect()
}

#[test]
fn a_burst_of_refreshes_while_one_reads_queues_exactly_one_more() {
    let mut list = ListState::new();
    let first = list.refresh().unwrap();
    for _ in 0..10 {
        assert_eq!(list.refresh(), None);
    }
    let answer = page(vec![pull_request(1, "a", oid(1))], None);
    let queued = list
        .finish(first.generation, Ok(answer.clone()), 100)
        .unwrap();
    assert_eq!(queued.after, None);
    assert_eq!(list.finish(queued.generation, Ok(answer), 101), None);
    assert!(!list.is_loading());
}

#[test]
fn an_answer_for_an_earlier_generation_is_dropped() {
    let mut list = ListState::new();
    let old = list.refresh().unwrap();
    list.reset();
    let current = list.refresh().unwrap();
    let stale = page(vec![pull_request(9, "old repository", oid(9))], None);
    assert_eq!(list.finish(old.generation, Ok(stale), 100), None);
    assert!(list.rows().is_empty() && list.is_loading());
    let fresh = page(vec![pull_request(1, "new", oid(1))], None);
    list.finish(current.generation, Ok(fresh), 100);
    assert_eq!(rows(&list), [1]);

    let mut tab = DetailState::new(4);
    let early = tab.refresh().unwrap();
    assert_eq!(tab.refresh(), None, "queued behind the running read");
    let detail = pull_request_detail(pull_request(4, "t", oid(4)), "", CheckList::default());
    let next = tab
        .finish(early.generation, Ok(detail.clone()), 100)
        .unwrap();
    assert_eq!(tab.finish(early.generation, Ok(detail.clone()), 100), None);
    tab.finish(next.generation, Ok(detail), 101);
    assert_eq!(tab.freshness(), &Freshness::Current { at: 101 });
}

#[test]
fn pages_append_once_each_and_stop_at_the_bound() {
    let mut list = ListState::new();
    let load = list.refresh().unwrap();
    list.finish(
        load.generation,
        Ok(page(vec![pull_request(3, "c", oid(3))], Some("c1"))),
        1,
    );
    for index in 1..MAX_PAGES {
        assert!(list.has_more(), "page {index}");
        let more = list.load_more().unwrap();
        assert_eq!(
            more.after.as_ref().map(|c| c.as_str().to_owned()),
            Some(format!("c{index}"))
        );
        let next = format!("c{}", index + 1);
        let rows = vec![
            pull_request(3, "dup", oid(3)),
            pull_request(100 + index as u64, "x", oid(4)),
        ];
        list.finish(more.generation, Ok(page(rows, Some(&next))), 2);
    }
    assert_eq!(list.pages(), MAX_PAGES);
    assert!(!list.has_more() && list.load_more().is_none());
    assert_eq!(
        list.rows().len(),
        MAX_PAGES,
        "the duplicate of #3 was dropped each time"
    );
    let refresh = list.refresh().unwrap();
    list.finish(
        refresh.generation,
        Ok(page(vec![pull_request(7, "only", oid(7))], None)),
        3,
    );
    assert_eq!(rows(&list), [7]);
}

#[test]
fn a_failed_refresh_keeps_the_rows_but_never_as_current() {
    let mut list = ListState::new();
    let load = list.refresh().unwrap();
    list.finish(
        load.generation,
        Ok(page(vec![pull_request(1, "a", oid(1))], None)),
        1,
    );
    let again = list.refresh().unwrap();
    let error = ReadError::Failed("connection refused".to_owned());
    list.finish(again.generation, Err(error.clone()), 2);
    assert_eq!(rows(&list), [1]);
    assert_eq!(list.freshness(), &Freshness::Failed(error));
    assert_eq!(
        freshness_problem(list.freshness(), 2).as_deref(),
        Some("Couldn't check: connection refused")
    );

    let mut tab = DetailState::new(1);
    let read = tab.refresh().unwrap();
    let detail = pull_request_detail(pull_request(1, "a", oid(1)), "", CheckList::default());
    tab.finish(read.generation, Ok(detail), 1);
    assert_eq!(tab.stamp().unwrap().head_oid, oid(1));
    let failed = tab.refresh().unwrap();
    tab.finish(failed.generation, Err(ReadError::Failed("x".to_owned())), 2);
    assert!(tab.detail().is_some());
    assert_eq!(tab.stamp(), None, "a stale screen is not what the user saw");
}

fn inputs(window_active: bool, visible: bool) -> PollInputs {
    PollInputs {
        window_active,
        visible,
        now: 1_000,
    }
}

#[test]
fn only_what_shows_in_the_active_window_polls() {
    let current = Freshness::Current { at: 990 };
    assert_eq!(
        list_poll(inputs(true, true), &current),
        PollPlan::After(LIST_INTERVAL)
    );
    assert_eq!(list_poll(inputs(false, true), &current), PollPlan::Stop);
    assert_eq!(list_poll(inputs(true, false), &current), PollPlan::Stop);
    assert_eq!(
        detail_poll(inputs(true, true), &current, true),
        PollPlan::After(DETAIL_INTERVAL)
    );
    assert_eq!(
        detail_poll(inputs(true, true), &current, false),
        PollPlan::Stop
    );
    assert_eq!(
        detail_poll(inputs(false, true), &current, true),
        PollPlan::Stop
    );
}

#[test]
fn a_rate_limit_pauses_until_its_reset_and_a_sign_out_stops_polling() {
    let limited = Freshness::Failed(ReadError::RateLimited { reset: Some(1_600) });
    assert_eq!(
        list_poll(inputs(true, true), &limited),
        PollPlan::After(Duration::from_secs(601))
    );
    assert_eq!(
        detail_poll(inputs(true, true), &limited, true),
        PollPlan::After(Duration::from_secs(601))
    );
    assert!(!is_due(&limited, LIST_INTERVAL, 1_000) && is_due(&limited, LIST_INTERVAL, 1_600));
    let signed_out = Freshness::Failed(ReadError::SignedOut {
        account: "octocat@github.com".to_owned(),
    });
    assert_eq!(list_poll(inputs(true, true), &signed_out), PollPlan::Stop);
    assert!(!is_due(&signed_out, LIST_INTERVAL, 5_000));
    let failed = Freshness::Failed(ReadError::Failed("down".to_owned()));
    assert_eq!(
        list_poll(inputs(true, true), &failed),
        PollPlan::After(LIST_INTERVAL)
    );
    assert!(is_due(
        &Freshness::Current { at: 940 },
        LIST_INTERVAL,
        1_000
    ));
    assert!(!is_due(
        &Freshness::Current { at: 941 },
        LIST_INTERVAL,
        1_000
    ));
}

fn run(
    name: &str,
    status: CheckStatus,
    conclusion: Option<CheckConclusion>,
    required: bool,
) -> Check {
    Check {
        name: name.to_owned(),
        progress: CheckProgress::Run { status, conclusion },
        description: None,
        required: Some(required),
        details: None,
        actions: None,
    }
}

#[test]
fn checks_list_required_first_then_failing_and_sum_up() {
    let checks = vec![
        run(
            "lint",
            CheckStatus::Completed,
            Some(CheckConclusion::Success),
            false,
        ),
        run("deploy", CheckStatus::InProgress, None, false),
        run(
            "unit",
            CheckStatus::Completed,
            Some(CheckConclusion::Success),
            true,
        ),
        run(
            "e2e",
            CheckStatus::Completed,
            Some(CheckConclusion::TimedOut),
            false,
        ),
        run(
            "build",
            CheckStatus::Completed,
            Some(CheckConclusion::Failure),
            true,
        ),
        Check {
            name: "ci/legacy".to_owned(),
            progress: CheckProgress::Status(StatusState::Success),
            description: None,
            required: None,
            details: None,
            actions: None,
        },
    ];
    let names: Vec<&str> = ordered(&checks).iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["build", "unit", "e2e", "deploy", "ci/legacy", "lint"]
    );
    assert!(pending(&checks));
    let list = CheckList { checks, total: 8 };
    assert_eq!(
        summary(&list),
        "2 failing, 1 pending, 3 passing; 2 required; 2 more not shown."
    );
    assert_eq!(summary(&CheckList::default()), "No checks.");
    assert!(CheckState::Failing < CheckState::Pending);
}

#[test]
fn only_plain_web_links_in_a_description_may_open_and_they_name_their_host() {
    assert_eq!(
        body_link("https://GitHub.com/o/r/pull/1"),
        Some(BodyLink {
            url: "https://GitHub.com/o/r/pull/1".to_owned(),
            host: "github.com".to_owned()
        })
    );
    assert_eq!(
        body_link("http://ci.example:8080/x").unwrap().host,
        "ci.example:8080"
    );
    for refused in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "mailto:a@b.example",
        "vscode://open",
        "/o/r/pull/1",
        "#heading",
        "//evil.example/x",
        "https://github.com@evil.example/x",
        "https://evil.example\u{202e}/x",
        "https://github.com/login\u{202e}lmth.exe",
        "https://github.com/a b",
        "https://gïthub.com/x",
        "https:// evil.example",
        "",
    ] {
        assert_eq!(body_link(refused), None, "{refused:?}");
    }
    let tooltip = body_link("https://github.com.evil.example/login")
        .unwrap()
        .tooltip();
    assert!(
        tooltip.starts_with("Opens github.com.evil.example in your browser"),
        "{tooltip}"
    );
}

#[test]
fn a_description_image_is_a_linked_name_and_loads_nothing() {
    let body =
        "![logo](https://tracker.example/pixel.png)\n\n<img src=\"https://tracker.example/x.png\">";
    let document = MarkdownDocument::parse(body);
    let runs: Vec<_> = document
        .blocks
        .iter()
        .flat_map(|block| block.runs.iter())
        .collect();
    let image = runs.iter().find(|run| run.is_image).unwrap();
    assert_eq!(image.text, "logo");
    let source = &image.link.as_ref().unwrap().destination;
    assert_eq!(body_link(source).unwrap().host, "tracker.example");
    assert!(
        runs.iter().all(|run| !run.text.contains("<img")),
        "a raw image tag is not drawn: {runs:?}"
    );
}

#[test]
fn problems_say_couldnt_check_with_the_cause_and_what_to_do() {
    assert_eq!(
        problem(
            &ReadError::RateLimited {
                reset: Some(1_000 + 125)
            },
            1_000
        ),
        "Couldn't check: the host's rate limit is spent. RCGit checks again in about 3 minutes."
    );
    let signed_out = problem(
        &ReadError::SignedOut {
            account: "octocat@github.com".to_owned(),
        },
        0,
    );
    assert!(signed_out.contains("octocat@github.com") && signed_out.contains("Settings"));
    let mut fork = pull_request(1, "a", oid(1));
    fork.cross_repository = true;
    fork.head_owner = Some("someone".to_owned());
    assert_eq!(branches(&fork), "someone:feature → main");
}

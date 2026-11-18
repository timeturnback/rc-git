//! The GitHub client against the testkit's fake GitHub host on loopback. Under `cfg(test)` a
//! loopback host is reached over plain `http://`; every other rule is the shipped one.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rc_git_testkit::github::{
    DEVICE_CODE, DeviceScript, EchoForm, FakeToken, GitHubServer, Grant, PollAnswer, USER_CODE,
};

use super::api::{API_BODY_LIMIT, Session};
use super::host::{PLAIN_HTTP, PlainHttp};
use super::*;
use crate::actions::remote::Cancel;

const TOKEN: &str = "ghp_fake_token_never_printed_0123";
const CLIENT: &str = "Iv23li.fake-client";
const QUERY: &str = "query($owner: String!) { repositoryOwner(login: $owner) { login } }";

/// Device flows hold a process-wide slot, so the tests that start one run one at a time.
static DEVICE_FLOWS: Mutex<()> = Mutex::new(());

fn one_flow_at_a_time() -> MutexGuard<'static, ()> {
    DEVICE_FLOWS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn host_of(server: &GitHubServer) -> GitHubHost {
    GitHubHost::parse(&server.host()).unwrap()
}

fn token() -> ApiToken {
    ApiToken::new(TOKEN.to_owned()).unwrap()
}

fn client() -> ClientId {
    ClientId::parse(CLIENT).unwrap()
}

/// A server that knows [`TOKEN`] as `octocat`.
fn server_with_token() -> GitHubServer {
    let server = GitHubServer::start();
    server.add_token(FakeToken {
        token: TOKEN.to_owned(),
        login: "octocat".to_owned(),
        scopes: Some("repo, read:org".to_owned()),
        expires: Some("2026-12-01 10:00:00 UTC".to_owned()),
    });
    server
}

fn assert_no_secret(error: &GitHubError, secrets: &[&str]) {
    let printed = format!("{error} | {error:?} | {error:#?}");
    for secret in secrets {
        assert!(!printed.contains(secret), "{printed}");
    }
}

#[test]
fn hosts_parse_to_one_lowercase_spelling() {
    assert_eq!(
        GitHubHost::parse(" GitHub.COM ").unwrap(),
        GitHubHost::github_com()
    );
    let ghe = GitHubHost::parse("GHE.example.com:443").unwrap();
    assert_eq!((ghe.name(), ghe.port()), ("ghe.example.com", None));
    let ported = GitHubHost::parse("ghe.example.com:8443").unwrap();
    assert_eq!(ported.to_string(), "ghe.example.com:8443");
    assert!(!ported.is_github_com());
}

#[test]
fn hosts_refuse_schemes_paths_userinfo_ports_and_bad_labels() {
    for bad in [
        "",
        "https://github.com",
        "github.com/x",
        "user@github.com",
        "github.com.",
        ".github.com",
        "a..b",
        "-a.example",
        "a-.example",
        "[::1]",
        "github.com:0",
        "github.com:65536",
        "github.com:8443",
        "api.github.com",
        "gïthub.com",
        "git hub.com",
        "github.com:",
        "a:1:2",
    ] {
        let error = GitHubHost::parse(bad).unwrap_err();
        assert!(
            matches!(error, GitHubError::InvalidHost { .. }),
            "{bad}: {error}"
        );
    }
}

#[test]
fn api_urls_come_from_the_account_host() {
    let com = GitHubHost::github_com();
    assert_eq!(
        com.rest_url(&["user"], "").unwrap().as_str(),
        "https://api.github.com/user"
    );
    assert_eq!(com.graphql_url().as_str(), "https://api.github.com/graphql");
    assert_eq!(com.verification_url(), "https://github.com/login/device");
    let ghe = GitHubHost::parse("ghe.example.com:8443").unwrap();
    assert_eq!(
        ghe.rest_url(&["repos", "o", "r"], "per_page=30")
            .unwrap()
            .as_str(),
        "https://ghe.example.com:8443/api/v3/repos/o/r?per_page=30"
    );
    assert_eq!(
        ghe.graphql_url().as_str(),
        "https://ghe.example.com:8443/api/graphql"
    );
    assert_eq!(
        ghe.verification_url(),
        "https://ghe.example.com:8443/login/device"
    );
    for bad in ["..", "a/b", "", "a?b"] {
        assert!(ghe.rest_url(&["repos", bad], "").is_err(), "{bad}");
    }
}

#[test]
fn a_shipped_build_reaches_even_a_loopback_host_over_https() {
    let loopback = GitHubHost::parse("127.0.0.1:4010").unwrap();
    assert_eq!(
        loopback.origin(PlainHttp::Refused),
        "https://127.0.0.1:4010"
    );
    assert_eq!(
        loopback.origin(PlainHttp::Loopback),
        "http://127.0.0.1:4010"
    );
    let ghe = GitHubHost::parse("ghe.example.com").unwrap();
    assert_eq!(ghe.origin(PlainHttp::Loopback), "https://ghe.example.com");
    assert_eq!(
        PLAIN_HTTP,
        PlainHttp::Loopback,
        "unit tests run with the allowance on"
    );
}

#[test]
fn a_remote_on_the_account_host_names_its_repository() {
    let com = GitHubHost::github_com();
    for remote in [
        "https://github.com/octo/rc-git.git",
        "https://github.com/octo/rc-git",
        "https://GitHub.com/octo/rc-git.git",
        "https://github.com:443/octo/rc-git.git",
        "git@github.com:octo/rc-git.git",
        "ssh://git@github.com/octo/rc-git.git",
        "ssh://git@github.com:22/octo/rc-git",
    ] {
        let target = RepositoryTarget::on_host(&com, remote).unwrap();
        assert_eq!(
            (target.owner(), target.name()),
            ("octo", "rc-git"),
            "{remote}"
        );
        assert_eq!(remote_host(remote).unwrap(), com, "{remote}");
    }
    let ported = GitHubHost::parse("ghe.example.com:8443").unwrap();
    assert!(RepositoryTarget::on_host(&ported, "https://ghe.example.com:8443/o/r").is_ok());
    assert!(RepositoryTarget::on_host(&ported, "https://ghe.example.com/o/r").is_err());
}

#[test]
fn hostile_remotes_name_no_repository_on_the_account_host() {
    let com = GitHubHost::github_com();
    for remote in [
        "https://github.com.evil.example/o/r.git",
        "https://evilgithub.com/o/r.git",
        "https://sub.github.com/o/r.git",
        "https://github.co/o/r.git",
        "https://evil.example/github.com/o/r.git",
        "https://github.com@evil.example/o/r.git",
        "https://user:hunter2@github.com/o/r.git",
        "https://token@github.com/o/r.git",
        "https://github.com:8443/o/r.git",
        "ssh://git@github.com:2222/o/r.git",
        "ssh://alice@github.com/o/r.git",
        "alice@github.com:o/r.git",
        "git://github.com/o/r.git",
        "file:///github.com/o/r.git",
        "http://github.com/o/r.git",
        "https://github.com/o/r/extra",
        "https://github.com/o",
        "https://github.com/../r",
        "https://github.com/o/..",
        "https://github%2ecom/o/r",
        "https://github.com./o/r",
        "https://xn--github-xyz.com/o/r",
        "not a url at all",
    ] {
        let error = RepositoryTarget::on_host(&com, remote).unwrap_err();
        assert!(
            matches!(error, GitHubError::ForeignRemote { .. }),
            "{remote}: {error}"
        );
        assert_no_secret(&error, &["hunter2"]);
    }
}

#[test]
fn an_oversized_owner_name_or_remote_url_names_no_repository() {
    let com = GitHubHost::github_com();
    let longest = format!(
        "https://github.com/{}/{}.git",
        "o".repeat(39),
        "r".repeat(100)
    );
    let target = RepositoryTarget::on_host(&com, &longest).unwrap();
    assert_eq!((target.owner().len(), target.name().len()), (39, 100));
    for (remote, unparsed) in [
        (
            format!("https://github.com/{}/r.git", "o".repeat(40)),
            false,
        ),
        (
            format!("https://github.com/o/{}.git", "r".repeat(101)),
            false,
        ),
        (
            format!("https://github.com/o/r.git?{}", "x".repeat(1024)),
            true,
        ),
        (
            format!("https://user:hunter2@github.com/o/{}", "r".repeat(1024)),
            true,
        ),
    ] {
        let error = RepositoryTarget::on_host(&com, &remote).unwrap_err();
        let GitHubError::ForeignRemote { reason, .. } = &error else {
            panic!("{remote}: {error}");
        };
        assert_eq!(
            reason.contains("longer than 1024 bytes"),
            unparsed,
            "an oversized URL is refused before it is parsed: {error}"
        );
        assert!(error.to_string().len() < 600, "{error}");
        assert_no_secret(&error, &["hunter2"]);
    }
}

/// What a pull request call does with an untrusted remote: resolve it on the account's host,
/// then ask the API about it.
fn ask_about(host: &GitHubHost, remote: &str) -> GitHubResult<String> {
    let target = RepositoryTarget::on_host(host, remote)?;
    let token = token();
    let reply =
        Session::new(host, &token).get_repo("read the repository", &target, &[], &Cancel::new())?;
    Ok(reply.value["full_name"]
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

#[test]
fn a_hostile_remote_gets_no_request_at_all() {
    let server = server_with_token();
    let host = host_of(&server);
    let port = host.port().unwrap();
    let other_port = if port == 65_535 { port - 1 } else { port + 1 };
    for remote in [
        format!("http://127.0.0.1.evil.example:{port}/o/r.git"),
        format!("http://evil.example/127.0.0.1:{port}/o/r.git"),
        format!("http://127.0.0.1:{other_port}/o/r.git"),
        "http://127.0.0.1/o/r.git".to_owned(),
        format!("http://user:pw@127.0.0.1:{port}/o/r.git"),
        format!("http://localhost:{port}/o/r.git"),
        format!("http://127.0.0.1:{port}/o/../r.git"),
    ] {
        let error = ask_about(&host, &remote).unwrap_err();
        assert!(
            matches!(error, GitHubError::ForeignRemote { .. }),
            "{remote}: {error}"
        );
    }
    assert_eq!(server.requests(), 0, "{:?}", server.recorded());

    let named = ask_about(&host, &format!("http://127.0.0.1:{port}/octo/rc-git.git")).unwrap();
    assert_eq!(named, "octo/rc-git");
    let recorded = server.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].target, "/api/v3/repos/octo/rc-git");
}

#[test]
fn a_target_on_another_host_is_refused_before_any_request() {
    let server = server_with_token();
    let host = host_of(&server);
    let elsewhere = GitHubHost::github_com();
    let target = RepositoryTarget::on_host(&elsewhere, "https://github.com/o/r").unwrap();
    let token = token();
    let error = Session::new(&host, &token)
        .get_repo("read the repository", &target, &[], &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(error, GitHubError::ForeignRemote { .. }),
        "{error}"
    );
    assert_eq!(server.requests(), 0);
}

#[test]
fn a_redirect_target_receives_no_request_and_no_token() {
    let origin = server_with_token();
    let target = server_with_token();
    origin.redirect_to(&format!("{}/api/v3/user", target.origin()));
    let host = host_of(&origin);
    let token = token();
    let error = Session::new(&host, &token)
        .get("follow", &["redirect"], "", &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(error, GitHubError::Redirected { status: 302, .. }),
        "{error}"
    );
    assert_eq!(target.requests(), 0, "the redirect target was contacted");
    assert_eq!(origin.requests(), 1);
    assert_no_secret(&error, &[TOKEN]);
}

#[test]
fn an_oversized_body_is_an_error_not_a_truncation() {
    let server = server_with_token();
    let host = host_of(&server);
    let token = token();
    let session = Session::new(&host, &token);
    server.set_oversized_bytes(API_BODY_LIMIT + 1);
    for endpoint in ["oversized", "oversized-unsized"] {
        let error = session
            .get("read", &[endpoint], "", &Cancel::new())
            .unwrap_err();
        assert!(
            matches!(error, GitHubError::TooLarge { limit, .. } if limit == API_BODY_LIMIT),
            "{endpoint}: {error}"
        );
    }
    server.set_oversized_bytes(API_BODY_LIMIT);
    for endpoint in ["oversized", "oversized-unsized"] {
        let error = session
            .get("read", &[endpoint], "", &Cancel::new())
            .unwrap_err();
        assert!(
            matches!(error, GitHubError::Malformed { .. }),
            "{endpoint}: {error}"
        );
    }
}

#[test]
fn the_token_never_appears_in_an_error() {
    let server = server_with_token();
    let host = host_of(&server);
    let token = token();
    let session = Session::new(&host, &token);
    let echoed = session
        .get("read", &["echo-error"], "", &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(&echoed, GitHubError::Status { status: 422, message, .. }
            if message.contains("<redacted>")),
        "{echoed}"
    );
    assert_no_secret(&echoed, &[TOKEN]);

    let wrong = ApiToken::new("ghp_wrong_token_never_printed".to_owned()).unwrap();
    let refused = validate_token(&host, &wrong, &Cancel::new()).unwrap_err();
    assert!(
        matches!(refused, GitHubError::Unauthorized { .. }),
        "{refused}"
    );
    assert_no_secret(&refused, &["ghp_wrong_token_never_printed"]);

    drop(server);
    let unreachable = validate_token(&host, &token, &Cancel::new()).unwrap_err();
    assert!(
        matches!(unreachable, GitHubError::Transport { .. }),
        "{unreachable}"
    );
    assert_no_secret(&unreachable, &[TOKEN]);
}

#[test]
fn a_spent_rate_limit_names_its_reset() {
    let server = server_with_token();
    let host = host_of(&server);
    let token = token();
    let error = Session::new(&host, &token)
        .get("read", &["rate-limited"], "", &Cancel::new())
        .unwrap_err();
    assert!(
        matches!(
            error,
            GitHubError::RateLimited {
                reset: Some(1_700_000_000),
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn a_cancelled_call_sends_nothing_and_a_stalled_one_stops_within_a_poll() {
    let server = server_with_token();
    let host = host_of(&server);
    let token = token();
    let cancelled = Cancel::new();
    cancelled.signal();
    let error = validate_token(&host, &token, &cancelled).unwrap_err();
    assert!(matches!(error, GitHubError::Cancelled { .. }), "{error}");
    assert_eq!(server.requests(), 0);

    let cancel = Cancel::new();
    let signal = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        signal.signal();
    });
    let started = Instant::now();
    let error = Session::new(&host, &token)
        .get("read", &["stall"], "", &cancel)
        .unwrap_err();
    canceller.join().unwrap();
    assert!(matches!(error, GitHubError::Cancelled { .. }), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_silent_server_ends_at_the_idle_budget() {
    let server = server_with_token();
    let host = host_of(&server);
    let token = token();
    let cancel = Cancel::new().with_idle_timeout(Duration::from_secs(1));
    let started = Instant::now();
    let error = Session::new(&host, &token)
        .get("read", &["stall"], "", &cancel)
        .unwrap_err();
    assert!(matches!(error, GitHubError::Transport { .. }), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn validate_token_reports_login_scopes_and_expiry_from_the_token_host() {
    let server = server_with_token();
    let identity = validate_token(&host_of(&server), &token(), &Cancel::new()).unwrap();
    assert_eq!(
        identity,
        TokenIdentity {
            login: "octocat".to_owned(),
            scopes: Some("repo, read:org".to_owned()),
            expires: Some("2026-12-01 10:00:00 UTC".to_owned()),
        }
    );
    let recorded = server.recorded();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].target, "/api/v3/user");
    assert_eq!(
        recorded[0].header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(recorded[0].header("user-agent"), Some("rc-git"));
}

#[test]
fn a_token_without_scope_headers_reports_none() {
    let server = GitHubServer::start();
    server.add_token(FakeToken {
        token: TOKEN.to_owned(),
        login: "app-user".to_owned(),
        scopes: None,
        expires: None,
    });
    let identity = validate_token(&host_of(&server), &token(), &Cancel::new()).unwrap();
    assert_eq!((identity.scopes, identity.expires), (None, None));
}

#[test]
fn graphql_sends_the_static_query_with_values_only_in_variables() {
    let server = server_with_token();
    server.echo_graphql();
    let host = host_of(&server);
    let token = token();
    let variables = serde_json::json!({ "owner": "o\"} } mutation { x" });
    let reply = Session::new(&host, &token)
        .graphql("read", QUERY, &variables, &Cancel::new())
        .unwrap();
    assert!(reply.errors.is_empty());
    let data = reply.data.unwrap();
    assert_eq!(data["echo"]["query"], QUERY);
    assert_eq!(data["echo"]["variables"], variables);
    let recorded = server.recorded();
    let sent = &recorded[0];
    assert_eq!(
        (sent.method.as_str(), sent.target.as_str()),
        ("POST", "/api/graphql")
    );
    assert_eq!(
        sent.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(sent.header("content-type"), Some("application/json"));
    assert_eq!(sent.header("x-github-api-version"), Some("2022-11-28"));
    assert_eq!(sent.header("accept"), Some("application/vnd.github+json"));
}

fn script(polls: Vec<PollAnswer>) -> DeviceScript {
    DeviceScript {
        client_id: CLIENT.to_owned(),
        scope: None,
        verification_uri: None,
        interval: 1,
        expires_in: 900,
        polls: polls.into(),
    }
}

fn grant() -> Grant {
    Grant {
        access: "ghu_granted_access".to_owned(),
        expires_in: Some(28_800),
        refresh: Some("ghr_granted_refresh".to_owned()),
    }
}

#[test]
fn a_device_flow_signs_in_after_pending_and_slow_down() {
    let _one = one_flow_at_a_time();
    let server = GitHubServer::start();
    let slow = PollAnswer::SlowDown { interval: 2 };
    server.script_device_flow(script(vec![
        PollAnswer::Pending,
        slow,
        PollAnswer::Grant(grant()),
    ]));
    let host = host_of(&server);
    let cancel = Cancel::new();
    let flow = start_device_flow(&host, &client(), DeviceMethod::GitHubApp, &cancel).unwrap();
    assert_eq!(flow.user_code(), USER_CODE);
    assert_eq!(
        flow.verification_url(),
        format!("{}/login/device", server.origin())
    );
    assert_eq!(flow.interval(), Duration::from_secs(1));

    let started = Instant::now();
    assert_eq!(flow.poll(&cancel).unwrap(), DevicePoll::Pending);
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the first poll waits the interval"
    );
    let pending_at = Instant::now();
    let slowed = flow.poll(&cancel).unwrap();
    assert!(
        pending_at.elapsed() >= Duration::from_secs(1),
        "each poll waits the interval"
    );
    assert_eq!(
        slowed,
        DevicePoll::SlowedDown {
            interval: Duration::from_secs(2)
        }
    );
    assert_eq!(
        flow.interval(),
        Duration::from_secs(2),
        "every later poll waits longer"
    );
    let before = Instant::now();
    let DevicePoll::Granted(tokens) = flow.poll(&cancel).unwrap() else {
        panic!("the third poll grants");
    };
    assert!(
        before.elapsed() >= Duration::from_secs(2),
        "{:?}",
        before.elapsed()
    );
    assert_eq!(tokens.access.expose_secret(), "ghu_granted_access");
    assert_eq!(
        tokens.refresh.as_ref().map(RefreshToken::expose_secret),
        Some("ghr_granted_refresh")
    );
    let now = unix_now();
    assert!(
        tokens
            .access_expires_at
            .is_some_and(|at| at > now && at <= now + 28_800)
    );

    let recorded = server.recorded();
    assert_eq!(recorded[0].body, format!("client_id={CLIENT}").into_bytes());
    let poll = String::from_utf8(recorded[1].body.clone()).unwrap();
    assert!(
        poll.contains(&format!("device_code={DEVICE_CODE}")),
        "{poll}"
    );
    assert!(!poll.contains("client_secret"), "{poll}");
    assert_eq!(server.requests_to("/login/oauth/access_token"), 3);
}

#[test]
fn an_oauth_app_asks_for_the_repo_scope() {
    let _one = one_flow_at_a_time();
    let server = GitHubServer::start();
    let mut oauth = script(vec![PollAnswer::Grant(Grant {
        access: "gho_lasting".to_owned(),
        expires_in: None,
        refresh: None,
    })]);
    oauth.scope = Some("repo".to_owned());
    server.script_device_flow(oauth);
    let cancel = Cancel::new();
    let flow = start_device_flow(
        &host_of(&server),
        &client(),
        DeviceMethod::OAuthApp,
        &cancel,
    )
    .unwrap();
    let DevicePoll::Granted(tokens) = flow.poll(&cancel).unwrap() else {
        panic!("the poll grants");
    };
    assert_eq!(
        tokens,
        TokenSet::lasting(ApiToken::new("gho_lasting".to_owned()).unwrap())
    );
}

#[test]
fn a_declined_or_expired_code_ends_the_flow() {
    let _one = one_flow_at_a_time();
    let cancel = Cancel::new();
    for (answer, expected_denied) in [(PollAnswer::Denied, true), (PollAnswer::Expired, false)] {
        let server = GitHubServer::start();
        server.script_device_flow(script(vec![answer]));
        let flow = start_device_flow(
            &host_of(&server),
            &client(),
            DeviceMethod::GitHubApp,
            &cancel,
        )
        .unwrap();
        let error = flow.poll(&cancel).unwrap_err();
        if expected_denied {
            assert!(matches!(error, GitHubError::AccessDenied { .. }), "{error}");
        } else {
            assert!(
                matches!(error, GitHubError::DeviceCodeExpired { .. }),
                "{error}"
            );
        }
    }
}

/// Asserts `error` names none of `secrets` and, when `scrubbed`, shows `<redacted>` where the
/// host echoed one.
fn assert_echo_scrubbed(error: &GitHubError, secrets: &[&str], scrubbed: bool) {
    assert_no_secret(error, secrets);
    if scrubbed {
        assert!(error.to_string().contains("<redacted>"), "{error}");
    }
}

#[test]
fn a_device_code_the_host_echoes_back_never_reaches_an_error() {
    let _one = one_flow_at_a_time();
    for into in [EchoForm::Description, EchoForm::Code, EchoForm::Body] {
        let server = GitHubServer::start();
        server.script_device_flow(script(vec![PollAnswer::Grant(grant())]));
        let cancel = Cancel::new();
        let flow = start_device_flow(
            &host_of(&server),
            &client(),
            DeviceMethod::GitHubApp,
            &cancel,
        )
        .unwrap();
        server.echo_form(into);
        let error = flow.poll(&cancel).unwrap_err();
        match into {
            EchoForm::Description => assert!(
                matches!(&error, GitHubError::OAuth { code, .. } if code == "echoed_form"),
                "{error}"
            ),
            EchoForm::Code => assert!(
                matches!(&error, GitHubError::OAuth { code, .. }
                    if code == "unrecognized error code"),
                "{error}"
            ),
            EchoForm::Body => assert!(
                matches!(error, GitHubError::Status { status: 400, .. }),
                "{error}"
            ),
        }
        assert_echo_scrubbed(&error, &[DEVICE_CODE], into != EchoForm::Code);
    }
}

#[test]
fn a_refresh_token_the_host_echoes_back_never_reaches_an_error() {
    let refresh = RefreshToken::new("ghr_echoed_refresh".to_owned()).unwrap();
    for into in [EchoForm::Description, EchoForm::Code, EchoForm::Body] {
        let server = GitHubServer::start();
        server.echo_form(into);
        let error =
            refresh_user_token(&host_of(&server), &client(), &refresh, &Cancel::new()).unwrap_err();
        match into {
            EchoForm::Description => assert!(
                matches!(&error, GitHubError::RefreshRejected { code, .. }
                    if code == "echoed_form"),
                "{error}"
            ),
            EchoForm::Code => assert!(
                matches!(&error, GitHubError::RefreshRejected { code, .. }
                    if code == "unrecognized error code"),
                "{error}"
            ),
            EchoForm::Body => assert!(
                matches!(error, GitHubError::Status { status: 400, .. }),
                "{error}"
            ),
        }
        assert_echo_scrubbed(&error, &["ghr_echoed_refresh"], into == EchoForm::Body);
    }
}

#[test]
fn a_foreign_verification_page_is_refused_before_any_poll() {
    let _one = one_flow_at_a_time();
    for page in [
        "https://evil.example/login/device",
        "https://github.com/login/device",
        "http://127.0.0.1:1/login/device",
        "",
    ] {
        let server = GitHubServer::start();
        let mut foreign = script(vec![PollAnswer::Grant(grant())]);
        foreign.verification_uri = Some(page.to_owned());
        server.script_device_flow(foreign);
        let error = start_device_flow(
            &host_of(&server),
            &client(),
            DeviceMethod::GitHubApp,
            &Cancel::new(),
        )
        .unwrap_err();
        assert!(
            matches!(error, GitHubError::ForeignVerificationUri { .. }),
            "{page}: {error}"
        );
        assert_eq!(server.requests_to("/login/oauth/access_token"), 0);
    }
}

#[test]
fn only_one_device_flow_runs_at_a_time() {
    let _one = one_flow_at_a_time();
    let server = GitHubServer::start();
    server.script_device_flow(script(Vec::new()));
    let host = host_of(&server);
    let cancel = Cancel::new();
    let first = start_device_flow(&host, &client(), DeviceMethod::GitHubApp, &cancel).unwrap();
    let second = start_device_flow(&host, &client(), DeviceMethod::GitHubApp, &cancel).unwrap_err();
    assert!(
        matches!(second, GitHubError::FlowInProgress { .. }),
        "{second}"
    );
    assert_eq!(
        server.requests_to("/login/device/code"),
        1,
        "the refused start sent nothing"
    );
    drop(first);
    assert!(start_device_flow(&host, &client(), DeviceMethod::GitHubApp, &cancel).is_ok());
}

#[test]
fn a_cancel_stops_the_wait_between_polls_without_a_request() {
    let _one = one_flow_at_a_time();
    let server = GitHubServer::start();
    let mut slow = script(Vec::new());
    slow.interval = 60;
    server.script_device_flow(slow);
    let cancel = Cancel::new();
    let flow = start_device_flow(
        &host_of(&server),
        &client(),
        DeviceMethod::GitHubApp,
        &cancel,
    )
    .unwrap();
    let signal = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        signal.signal();
    });
    let started = Instant::now();
    let error = flow.poll(&cancel).unwrap_err();
    canceller.join().unwrap();
    assert!(matches!(error, GitHubError::Cancelled { .. }), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(server.requests_to("/login/oauth/access_token"), 0);
}

#[test]
fn a_code_past_its_expiry_ends_polling_without_a_request() {
    let _one = one_flow_at_a_time();
    let server = GitHubServer::start();
    let mut short = script(vec![PollAnswer::Grant(grant())]);
    short.expires_in = 1;
    short.interval = 3;
    server.script_device_flow(short);
    let cancel = Cancel::new();
    let flow = start_device_flow(
        &host_of(&server),
        &client(),
        DeviceMethod::GitHubApp,
        &cancel,
    )
    .unwrap();
    let error = flow.poll(&cancel).unwrap_err();
    assert!(
        matches!(error, GitHubError::DeviceCodeExpired { .. }),
        "{error}"
    );
    assert_eq!(server.requests_to("/login/oauth/access_token"), 0);
}

#[test]
fn an_unknown_client_id_is_an_oauth_error() {
    let _one = one_flow_at_a_time();
    let server = GitHubServer::start();
    server.script_device_flow(script(Vec::new()));
    let other = ClientId::parse("Iv23li.someone-else").unwrap();
    let error = start_device_flow(
        &host_of(&server),
        &other,
        DeviceMethod::GitHubApp,
        &Cancel::new(),
    )
    .unwrap_err();
    assert!(
        matches!(&error, GitHubError::OAuth { code, .. } if code == "incorrect_client_credentials"),
        "{error}"
    );
}

#[test]
fn a_refresh_token_buys_new_tokens_exactly_once() {
    let server = GitHubServer::start();
    server.add_refresh(
        "ghr_first",
        CLIENT,
        Grant {
            access: "ghu_second".to_owned(),
            expires_in: Some(28_800),
            refresh: Some("ghr_second".to_owned()),
        },
    );
    let host = host_of(&server);
    let first = RefreshToken::new("ghr_first".to_owned()).unwrap();
    let renewed = refresh_user_token(&host, &client(), &first, &Cancel::new()).unwrap();
    assert_eq!(renewed.access.expose_secret(), "ghu_second");
    assert_eq!(
        renewed.refresh.as_ref().map(RefreshToken::expose_secret),
        Some("ghr_second")
    );
    assert!(renewed.refresh_expires_at.is_some());

    let again = refresh_user_token(&host, &client(), &first, &Cancel::new()).unwrap_err();
    assert!(
        matches!(&again, GitHubError::RefreshRejected { code, .. } if code == "bad_refresh_token"),
        "{again}"
    );
    let body = String::from_utf8(server.recorded()[0].body.clone()).unwrap();
    assert_eq!(
        body,
        format!("client_id={CLIENT}&grant_type=refresh_token&refresh_token=ghr_first")
    );
}

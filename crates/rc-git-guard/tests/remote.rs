#![expect(
    clippy::expect_used,
    reason = "test helpers outside #[test] functions may panic"
)]
//! Fetch, pull, push, and upstream status against real loopback servers: smart HTTP
//! (`git http-backend` behind the test kit's listener) and the test kit's russh SSH server. The
//! git CLI builds the fixtures; the guard is the only code under test that touches the network.

use std::path::{Path, PathBuf};

use rc_git_guard::Policy;
use rc_git_guard::actions::{
    ActionError, Cancel, Credentials, RemoteAuth, fetch_upstream, push_upstream, upstream_status,
};

/// A flag that is already set, for the tests that check a cancelled operation never
/// reaches the network.
fn cancelled() -> Cancel {
    let cancel = Cancel::new();
    cancel.signal();
    cancel
}

/// `RemoteAuth` carrying only credentials, as most tests need.
fn auth(creds: &Credentials) -> RemoteAuth<'_> {
    RemoteAuth {
        creds: Some(creds),
        ..RemoteAuth::default()
    }
}
use rc_git_testkit::{Fixture, SshAgent};

/// An origin repository, a bare copy served over HTTP from a sibling directory
/// (never inside the origin's working tree), and a local clone of it.
struct Remoted {
    origin: Fixture,
    bare: Fixture,
    bare_path: PathBuf,
    server: rc_git_testkit::HttpGitServer,
    clone: Fixture,
}

fn remoted() -> Remoted {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::HttpGitServer::serve(&bare_path);
    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", &server.url(), "."]);
    Remoted {
        origin,
        bare,
        bare_path,
        server,
        clone,
    }
}

/// `git rev-parse <rev>` in the served bare repository held by `bare`.
fn bare_rev(bare: &Fixture, rev: &str) -> String {
    bare.git_output(&["--git-dir", "remote.git", "rev-parse", rev])
}

/// Adds one commit on `origin` and pushes it into the served bare repository at
/// `bare_path`, past what the clone has.
fn advance_origin(origin: &Fixture, bare_path: &Path) {
    origin.write("news.txt", b"news\n");
    origin.commit("News on origin", "2024-03-01T00:00:00+0000");
    origin.push_main_to(bare_path);
}

#[test]
fn upstream_status_reports_no_upstream_without_a_remote() {
    let fixture = Fixture::standard();
    let status = upstream_status(fixture.path()).expect("status");
    assert!(!status.has_upstream);
    assert!(!status.published);
    assert_eq!(status.branch, "main");
}

#[test]
fn upstream_status_reads_the_clone_configuration_and_counts() {
    let remoted = remoted();
    let status = upstream_status(remoted.clone.path()).expect("status");
    assert!(status.has_upstream);
    assert_eq!(status.branch, "main");
    assert_eq!(status.remote, "origin");
    assert_eq!(status.upstream_ref, "refs/remotes/origin/main");
    assert!(status.published);
    assert!(
        status.url.starts_with("http://127.0.0.1:"),
        "{}",
        status.url
    );
    assert!(status.host.starts_with("127.0.0.1:"), "{}", status.host);
    assert_eq!((status.ahead, status.behind), (0, 0));

    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");
    let status = upstream_status(remoted.clone.path()).expect("status");
    assert_eq!((status.ahead, status.behind), (1, 0));
}

#[test]
fn upstream_status_reports_a_never_pushed_branch_as_unpublished() {
    let remoted = remoted();
    remoted.clone.git(&["checkout", "-b", "topic"]);
    remoted.clone.write("topic.txt", b"topic\n");
    remoted
        .clone
        .commit("Topic work", "2024-03-02T00:00:00+0000");
    let status = upstream_status(remoted.clone.path()).expect("status");
    assert!(status.has_upstream, "origin is the fallback upstream");
    assert!(
        !status.published,
        "refs/remotes/origin/topic does not exist"
    );
    assert_eq!((status.ahead, status.behind), (0, 0));
}

#[test]
fn upstream_status_reports_the_ssh_transport_from_the_parsed_scheme() {
    let fixture = Fixture::standard();
    assert!(!upstream_status(fixture.path()).expect("status").uses_ssh);
    fixture.git(&["remote", "add", "origin", "https://host/x"]);
    fixture.git(&["config", "branch.main.remote", "origin"]);
    fixture.git(&["config", "branch.main.merge", "refs/heads/main"]);
    for (url, uses_ssh) in [
        ("git@host:x/y.git", true),
        ("host:x/y.git", true),
        ("ssh://git@host/x", true),
        ("https://host/x", false),
    ] {
        fixture.git(&["remote", "set-url", "origin", url]);
        let status = upstream_status(fixture.path()).expect("status");
        assert_eq!(status.uses_ssh, uses_ssh, "{url}");
    }

    // The display URL drops the user, so an scp-like origin loses its `@`: the flag, not
    // the string, is what tells a caller the transport.
    fixture.git(&["remote", "set-url", "origin", "git@example.com:x/y.git"]);
    let status = upstream_status(fixture.path()).expect("status");
    assert!(status.uses_ssh);
    assert_eq!(status.url, "example.com:x/y.git");
    assert_eq!(status.host, "example.com");
}

#[test]
fn upstream_status_strips_userinfo_from_the_url() {
    let fixture = Fixture::standard();
    fixture.git(&[
        "remote",
        "add",
        "origin",
        "https://user:tok@example.com/x.git",
    ]);
    let status = upstream_status(fixture.path()).expect("status");
    assert!(status.has_upstream);
    assert_eq!(status.url, "https://example.com/x.git");
    assert!(!status.url.contains("tok"), "{}", status.url);
    assert!(!status.url.contains("user@"), "{}", status.url);
    assert_eq!(status.host, "example.com");
}

/// Drops `branch.main.remote` and `branch.main.merge`, the state of a branch made with
/// `git init` and pushed without `-u`: `origin` exists, but nothing names it the upstream.
fn untrack(remoted: &Remoted) {
    remoted.clone.git(&["branch", "--unset-upstream", "main"]);
}

#[test]
fn upstream_status_counts_against_origin_when_the_branch_names_no_upstream() {
    let remoted = remoted();
    untrack(&remoted);
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");
    advance_origin(&remoted.origin, &remoted.bare_path);
    fetch_clone(&remoted);

    let status = upstream_status(remoted.clone.path()).expect("status");
    assert!(status.has_upstream);
    assert_eq!(status.remote, "origin");
    assert_eq!(status.upstream_ref, "refs/remotes/origin/main");
    assert_eq!((status.ahead, status.behind), (1, 1));
}

#[test]
fn fetch_updates_the_tracking_ref_and_reports_it() {
    let remoted = remoted();
    advance_origin(&remoted.origin, &remoted.bare_path);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("fetch");
    assert_eq!(outcome.remote, "origin");
    let served_tip = bare_rev(&remoted.bare, "main");
    let moved = outcome
        .updated_refs
        .iter()
        .find(|update| update.name == "refs/remotes/origin/main")
        .expect("origin/main updated");
    assert_eq!(moved.target.to_string(), served_tip);
    assert_eq!(
        remoted.clone.rev_parse("refs/remotes/origin/main"),
        served_tip
    );

    let status = upstream_status(remoted.clone.path()).expect("status");
    assert_eq!((status.ahead, status.behind), (0, 1));
}

#[test]
fn fetch_on_a_detached_head_fetches_origin() {
    let remoted = remoted();
    remoted.clone.git(&["checkout", "-q", "--detach"]);
    advance_origin(&remoted.origin, &remoted.bare_path);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("fetch on a detached HEAD");
    assert_eq!(outcome.remote, "origin");
    assert_eq!(
        remoted.clone.rev_parse("refs/remotes/origin/main"),
        bare_rev(&remoted.bare, "main")
    );
}

#[test]
fn fetch_on_a_detached_head_without_origin_reports_no_upstream() {
    let remoted = remoted();
    remoted.clone.git(&["checkout", "-q", "--detach"]);
    remoted
        .clone
        .git(&["remote", "rename", "origin", "upstream"]);

    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("no origin");
    let ActionError::NoUpstream { branch, .. } = &err else {
        panic!("expected NoUpstream, got {err}");
    };
    assert_eq!(branch, "HEAD");
}

#[test]
fn fetch_ignores_the_repository_http_settings() {
    let remoted = remoted();
    advance_origin(&remoted.origin, &remoted.bare_path);
    remoted
        .clone
        .git(&["config", "http.proxy", "http://127.0.0.1:9"]);
    remoted
        .clone
        .git(&["config", "remote.origin.proxy", "http://127.0.0.1:9"]);

    fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("fetch ignores the proxy the repository names");
    assert_eq!(
        remoted.clone.rev_parse("refs/remotes/origin/main"),
        bare_rev(&remoted.bare, "main")
    );
}

#[test]
fn fetch_ignores_repository_http_and_credential_config() {
    let remoted = remoted();
    advance_origin(&remoted.origin, &remoted.bare_path);
    let marker = remoted.clone.git_dir().join("helper-ran");
    remoted
        .clone
        .git(&["config", "http.proxy", "http://u@127.0.0.1:1"]);
    let helper = format!("!touch '{}'; :", marker.display());
    remoted.clone.git(&["config", "credential.helper", &helper]);

    fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("fetch ignores the repository's proxy and credential helper");
    assert!(
        !marker.exists(),
        "the repository's credential helper ran: {}",
        marker.display()
    );
}

/// Set in the child process `fetch_from_loopback_bypasses_the_environment_proxy` starts.
const DEAD_PROXY_CHILD: &str = "RC_GIT_TEST_DEAD_PROXY_CHILD";

#[test]
fn fetch_from_loopback_bypasses_the_environment_proxy() {
    if std::env::var_os(DEAD_PROXY_CHILD).is_none() {
        let dead = std::ffi::OsStr::new("http://127.0.0.1:9");
        rc_git_testkit::run_test_with_env(
            "fetch_from_loopback_bypasses_the_environment_proxy",
            &[
                (DEAD_PROXY_CHILD, std::ffi::OsStr::new("1")),
                ("http_proxy", dead),
                ("HTTPS_PROXY", dead),
                ("ALL_PROXY", dead),
            ],
        );
        return;
    }
    let remoted = remoted();
    advance_origin(&remoted.origin, &remoted.bare_path);
    fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("a loopback fetch ignores the proxy the environment names");
    assert_eq!(
        remoted.clone.rev_parse("refs/remotes/origin/main"),
        bare_rev(&remoted.bare, "main")
    );
}

#[test]
fn fetch_with_nothing_new_reports_no_updates() {
    let remoted = remoted();
    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("fetch");
    assert_eq!(outcome.updated_refs.len(), 0, "{:?}", outcome.updated_refs);
}

#[test]
fn fetch_reports_a_new_annotated_tag() {
    let remoted = remoted();
    remoted.origin.write("news.txt", b"news\n");
    remoted
        .origin
        .commit("News on origin", "2024-03-01T00:00:00+0000");
    remoted.origin.git(&["tag", "-a", "-m", "r", "v2"]);
    let bare = remoted.bare_path.to_string_lossy();
    remoted
        .origin
        .git(&["push", "--quiet", bare.as_ref(), "main", "v2"]);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("fetch");
    let tag = outcome
        .updated_refs
        .iter()
        .find(|update| update.name == "refs/tags/v2")
        .expect("refs/tags/v2 reported");
    assert_eq!(tag.target.to_string(), bare_rev(&remoted.bare, "v2"));
    let kind = remoted
        .clone
        .git_output(&["cat-file", "-t", &tag.target.to_string()]);
    assert_eq!(kind, "tag");
}

#[test]
fn fetch_against_an_auth_server_requires_matching_credentials() {
    let origin = Fixture::standard();
    let (_bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::HttpGitServer::serve_with_auth(&bare_path, "user", "sekret");
    let clone = Fixture::basic_auth_clone(&server, "user", "sekret");

    let anonymous = fetch_upstream(
        clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    );
    assert!(
        matches!(anonymous, Err(ActionError::AuthRequired { .. })),
        "{anonymous:?}"
    );

    let wrong = fetch_upstream(
        clone.path(),
        &Policy::default(),
        auth(&Credentials {
            username: "user".to_owned(),
            token: "wrong".to_owned(),
        }),
        &Cancel::new(),
    );
    assert!(
        matches!(wrong, Err(ActionError::AuthRequired { .. })),
        "{wrong:?}"
    );

    advance_origin(&origin, &bare_path);
    let outcome = fetch_upstream(
        clone.path(),
        &Policy::default(),
        auth(&Credentials {
            username: "user".to_owned(),
            token: "sekret".to_owned(),
        }),
        &Cancel::new(),
    )
    .expect("authenticated fetch");
    assert_ne!(outcome.updated_refs.len(), 0);
}

#[test]
fn credentials_issued_for_another_host_are_never_sent() {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::HttpGitServer::serve_with_auth(&bare_path, "user", "sekret");
    let clone = Fixture::basic_auth_clone(&server, "user", "sekret");
    clone.write("local.txt", b"local\n");
    clone.commit("Local work", "2024-03-02T00:00:00+0000");
    let creds = Credentials {
        username: "user".to_owned(),
        token: "sekret".to_owned(),
    };
    let elsewhere = RemoteAuth {
        host: Some("elsewhere.example"),
        ..auth(&creds)
    };

    let fetched = fetch_upstream(clone.path(), &Policy::default(), elsewhere, &Cancel::new());
    assert!(
        matches!(fetched, Err(ActionError::AuthRequired { .. })),
        "{fetched:?}"
    );
    let pushed = push_upstream(clone.path(), &Policy::default(), elsewhere, &Cancel::new());
    assert!(
        matches!(pushed, Err(ActionError::AuthRequired { .. })),
        "{pushed:?}"
    );
    assert_ne!(bare_rev(&bare, "main"), clone.rev_parse("HEAD"));

    let status = upstream_status(clone.path()).expect("status");
    let own_host = RemoteAuth {
        host: Some(&status.host),
        ..auth(&creds)
    };
    fetch_upstream(clone.path(), &Policy::default(), own_host, &Cancel::new())
        .expect("fetch with credentials for the remote's own host");
    let outcome = push_upstream(clone.path(), &Policy::default(), own_host, &Cancel::new())
        .expect("push with credentials for the remote's own host");
    assert_eq!(outcome.pushed_commits, 1);
}

/// Sets the mode of `path`, for the tests that make a directory unwritable.
fn chmod(path: &Path, mode: u32) {
    #[expect(
        clippy::disallowed_methods,
        reason = "test setup changes a directory's mode"
    )]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }
}

#[test]
fn fetch_into_an_unwritable_pack_directory_is_a_git_error() {
    let remoted = remoted();
    advance_origin(&remoted.origin, &remoted.bare_path);
    let pack_dir = remoted.clone.path().join(".git/objects/pack");
    chmod(&pack_dir, 0o555);

    let result = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    );
    chmod(&pack_dir, 0o755);
    assert!(matches!(result, Err(ActionError::Git { .. })), "{result:?}");
}

/// Fetch and push from `fixture` against whatever its `origin` names.
fn fetch_and_push(fixture: &Fixture) -> [(&'static str, Result<(), ActionError>); 2] {
    let policy = Policy::default();
    let cancel = Cancel::new();
    [
        (
            "fetch",
            fetch_upstream(fixture.path(), &policy, RemoteAuth::default(), &cancel).map(drop),
        ),
        (
            "push",
            push_upstream(fixture.path(), &policy, RemoteAuth::default(), &cancel).map(drop),
        ),
    ]
}

#[test]
fn unsupported_remote_schemes_are_refused() {
    let fixture = Fixture::standard();
    fixture.git(&["remote", "add", "origin", "git://example.com/x.git"]);
    let empty = Fixture::empty_dir();
    let plain_path = empty.path().join("x.git");
    let plain_path = plain_path.to_str().expect("utf-8 temp path");
    for (url, expected) in [
        ("git://example.com/x.git", "git"),
        ("http://example.com/x.git", "http"),
        ("file:///x.git", "file"),
        (plain_path, "file"),
    ] {
        fixture.git(&["remote", "set-url", "origin", url]);
        for (op, result) in fetch_and_push(&fixture) {
            let err = result.expect_err(url);
            assert!(
                matches!(
                    &err,
                    ActionError::UnsupportedRemote { op: got, name, scheme }
                        if *got == op && name == "origin" && scheme == expected
                ),
                "{op} {url}: {err}"
            );
        }
    }
}

#[test]
fn loopback_http_hosts_are_not_refused_as_unsupported() {
    let fixture = Fixture::standard();
    fixture.git(&["remote", "add", "origin", "http://localhost:1/x.git"]);
    for url in ["http://localhost:1/x.git", "http://[::1]:1/x.git"] {
        fixture.git(&["remote", "set-url", "origin", url]);
        for (op, result) in fetch_and_push(&fixture) {
            let err = result.expect_err("nothing listens on port 1");
            assert!(
                !matches!(err, ActionError::UnsupportedRemote { .. }),
                "{op} {url}: {err}"
            );
        }
    }
}

#[test]
fn fetch_without_a_remote_reports_no_upstream() {
    let fixture = Fixture::standard();
    let err = fetch_upstream(
        fixture.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("no remote");
    assert!(matches!(err, ActionError::NoUpstream { .. }), "{err}");
}

#[test]
fn a_preset_cancel_flag_stops_the_fetch_before_it_connects() {
    let remoted = remoted();
    let before = remoted.server.requests();
    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &cancelled(),
    )
    .expect_err("cancelled");
    assert!(
        matches!(err, ActionError::Cancelled { op: "fetch" }),
        "{err}"
    );
    assert_eq!(
        remoted.server.requests(),
        before,
        "a cancelled op must not reach the server"
    );
}

/// Which remote operation a stalled-server test drives.
#[derive(Clone, Copy)]
enum Transfer {
    Fetch,
    Push,
}

/// A clone whose `origin` points at a server that reads each request and never answers,
/// with `transfer` already running against it on its own thread under `cancel`. The
/// receiver yields the result, so a call that never ends fails at `recv_timeout` instead
/// of hanging the suite. Keep the returned server alive until the result arrives.
fn transfer_against_a_silent_peer(
    transfer: Transfer,
    cancel: Cancel,
) -> (
    Remoted,
    rc_git_testkit::HttpGitServer,
    std::sync::mpsc::Receiver<Result<(), ActionError>>,
) {
    let remoted = remoted();
    let stalling = rc_git_testkit::HttpGitServer::serve_stalling(&remoted.bare_path);
    remoted
        .clone
        .git(&["remote", "set-url", "origin", &stalling.url()]);
    let repo = remoted.clone.path().to_path_buf();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let policy = Policy::default();
        let auth = RemoteAuth::default();
        let result = match transfer {
            Transfer::Fetch => fetch_upstream(&repo, &policy, auth, &cancel).map(drop),
            Transfer::Push => push_upstream(&repo, &policy, auth, &cancel).map(drop),
        };
        drop(done.send(result));
    });
    (remoted, stalling, finished)
}

/// A cancel raised while curl waits on a server that never answers ends the call within
/// the 5 s allowed here, though the idle budget left in place is two minutes.
fn assert_cancel_ends_a_stalled_handshake(transfer: Transfer, op: &str) {
    let cancel = Cancel::new();
    let signal = cancel.clone();
    let (_remoted, _stalling, finished) = transfer_against_a_silent_peer(transfer, cancel);
    std::thread::sleep(std::time::Duration::from_secs(1));
    signal.signal();
    let err = finished
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the call ends within 5 s of the cancel")
        .expect_err("cancelled");
    assert!(
        matches!(err, ActionError::Cancelled { op: got } if got == op),
        "{err}"
    );
}

/// The idle budget ends a read curl is blocked in when nobody cancels. This fails if the
/// budget stops reaching the backend, whether through a gix type change, a call site
/// that drops `http_options`, or a switch to a backend that ignores it.
fn assert_idle_budget_ends_a_silent_peer(transfer: Transfer, op: &str) {
    let cancel = Cancel::new().with_idle_timeout(std::time::Duration::from_secs(2));
    let (_remoted, _stalling, finished) = transfer_against_a_silent_peer(transfer, cancel);
    let err = finished
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the call ends within 10 s of a 2 s idle budget")
        .expect_err("a silent server fails the call");
    assert!(
        matches!(err, ActionError::Git { op: got, .. } if got == op),
        "{err}"
    );
}

#[test]
fn a_cancel_signalled_during_an_http_handshake_ends_the_fetch() {
    assert_cancel_ends_a_stalled_handshake(Transfer::Fetch, "fetch");
}

#[test]
fn a_cancel_signalled_during_an_http_handshake_ends_the_push() {
    assert_cancel_ends_a_stalled_handshake(Transfer::Push, "push");
}

#[test]
fn an_https_fetch_from_a_silent_peer_ends_within_the_idle_budget() {
    assert_idle_budget_ends_a_silent_peer(Transfer::Fetch, "fetch");
}

#[test]
fn an_https_push_to_a_silent_peer_ends_within_the_idle_budget() {
    assert_idle_budget_ends_a_silent_peer(Transfer::Push, "push");
}

/// Cancels a fetch stalled on a silent peer under `first`, then points `origin` back at the
/// live server and moves the served `main` one commit past the clone, so a second fetch has
/// something to write. Keep the returned stalling server alive while the abandoned thread
/// should keep waiting on it.
fn cancel_a_stalled_fetch_then_advance(first: Cancel) -> (Remoted, rc_git_testkit::HttpGitServer) {
    let signal = first.clone();
    let (remoted, stalling, finished) = transfer_against_a_silent_peer(Transfer::Fetch, first);
    std::thread::sleep(std::time::Duration::from_secs(1));
    signal.signal();
    let err = finished
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the cancel lands within 5 s")
        .expect_err("cancelled");
    assert!(
        matches!(err, ActionError::Cancelled { op: "fetch" }),
        "{err}"
    );
    remoted
        .clone
        .git(&["remote", "set-url", "origin", &remoted.server.url()]);
    advance_origin(&remoted.origin, &remoted.bare_path);
    (remoted, stalling)
}

#[test]
fn a_fetch_started_right_after_a_cancelled_one_waits_for_it_and_succeeds() {
    let first = Cancel::new().with_idle_timeout(std::time::Duration::from_secs(2));
    let (remoted, _stalling) = cancel_a_stalled_fetch_then_advance(first);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("the second fetch succeeds once the abandoned one has ended");
    let served_tip = bare_rev(&remoted.bare, "main");
    assert!(
        outcome
            .updated_refs
            .iter()
            .any(|update| update.name == "refs/remotes/origin/main"),
        "{outcome:?}"
    );
    assert_eq!(
        remoted.clone.rev_parse("refs/remotes/origin/main"),
        served_tip
    );
}

#[test]
fn a_fetch_refuses_while_a_cancelled_one_of_the_same_repository_still_runs() {
    let (remoted, stalling) = cancel_a_stalled_fetch_then_advance(Cancel::new());
    let before = remoted.server.requests();

    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new().with_connect_timeout(std::time::Duration::from_secs(1)),
    )
    .expect_err("the abandoned fetch still holds the repository");
    assert!(
        matches!(&err, ActionError::Git { op: "fetch", .. } if err.to_string().contains("still running")),
        "{err}"
    );
    assert_eq!(
        remoted.server.requests(),
        before,
        "a refused fetch must not reach the server"
    );
    drop(stalling);
}

// --- fast-forward pull ---

use rc_git_guard::actions::pull_fast_forward;

/// Fetches the clone so its tracking ref is current.
fn fetch_clone(remoted: &Remoted) {
    fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("fetch");
}

#[test]
fn pull_fast_forwards_files_index_and_ref() {
    let remoted = remoted();
    remoted.origin.write("news.txt", b"news\n");
    remoted
        .origin
        .write("README.md", b"# fixture\n\nrewritten\n");
    remoted.origin.git(&["rm", "-q", "alpha.txt"]);
    remoted
        .origin
        .commit("Rework on origin", "2024-03-01T00:00:00+0000");
    remoted.origin.push_main_to(&remoted.bare_path);
    fetch_clone(&remoted);

    let outcome = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect("pull");
    assert_eq!(outcome.branch, "main");
    assert_ne!(outcome.old, outcome.new);
    assert!(!outcome.already_up_to_date);
    assert_eq!(outcome.new.to_hex(), bare_rev(&remoted.bare, "main"));
    assert_eq!(outcome.files_changed, 3);
    assert_eq!(
        remoted.clone.rev_parse("main"),
        bare_rev(&remoted.bare, "main")
    );
    assert_eq!(
        std::fs::read(remoted.clone.path().join("news.txt")).expect("news.txt"),
        b"news\n"
    );
    assert_eq!(
        std::fs::read(remoted.clone.path().join("README.md")).expect("README.md"),
        b"# fixture\n\nrewritten\n"
    );
    assert!(!remoted.clone.path().join("alpha.txt").exists());
    assert_eq!(
        remoted.clone.git_output(&["status", "--porcelain"]),
        "",
        "index must agree with the new tree"
    );
}

/// Commits `origin`'s current working tree, pushes it into the served bare repository, fetches
/// the clone, and fast-forwards it; the clone then matches the served `main` and is clean.
fn pull_origin_change(remoted: &Remoted, title: &str) {
    remoted.origin.commit(title, "2024-03-01T00:00:00+0000");
    remoted.origin.push_main_to(&remoted.bare_path);
    fetch_clone(remoted);

    let outcome = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect("pull");
    assert_eq!(outcome.files_changed, 1);
    assert_eq!(
        remoted.clone.rev_parse("main"),
        bare_rev(&remoted.bare, "main")
    );
    assert_eq!(
        remoted.clone.git_output(&["status", "--porcelain"]),
        "",
        "index must agree with the new tree"
    );
}

#[test]
fn pull_fast_forwards_a_file_into_a_symlink() {
    let remoted = remoted();
    remoted.origin.remove("alpha.txt");
    remoted.origin.symlink("README.md", "alpha.txt");
    pull_origin_change(&remoted, "Replace alpha with a link");

    assert_eq!(
        std::fs::read_link(remoted.clone.path().join("alpha.txt")).expect("alpha.txt link"),
        Path::new("README.md")
    );
}

#[test]
fn pull_fast_forwards_a_symlink_into_a_file() {
    let remoted = remoted();
    remoted.origin.remove("link");
    remoted.origin.write("link", b"plain file\n");
    pull_origin_change(&remoted, "Replace link with a file");

    let link = remoted.clone.path().join("link");
    assert!(
        std::fs::symlink_metadata(&link)
            .expect("link metadata")
            .is_file()
    );
    assert_eq!(std::fs::read(&link).expect("link"), b"plain file\n");
}

#[test]
fn pull_reaches_heads_reflog_so_head_at_one_is_the_pre_pull_commit() {
    let remoted = remoted();
    let before = remoted.clone.rev_parse("HEAD");
    advance_origin(&remoted.origin, &remoted.bare_path);
    fetch_clone(&remoted);

    pull_fast_forward(remoted.clone.path(), &Policy::default()).expect("pull");
    assert_eq!(
        remoted
            .clone
            .git_output(&["log", "-g", "-1", "--format=%gs", "HEAD"]),
        "rc-git: pull --ff-only"
    );
    assert_eq!(
        remoted.clone.rev_parse("HEAD@{0}"),
        bare_rev(&remoted.bare, "main")
    );
    assert_eq!(
        remoted.clone.rev_parse("HEAD@{1}"),
        before,
        "`git reset --hard HEAD@{{1}}` must undo exactly this pull"
    );
}

#[test]
fn pull_fast_forwards_from_origin_when_the_branch_names_no_upstream() {
    let remoted = remoted();
    untrack(&remoted);
    advance_origin(&remoted.origin, &remoted.bare_path);
    fetch_clone(&remoted);

    let outcome = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect("pull");
    assert_eq!(outcome.files_changed, 1);
    assert_eq!(
        remoted.clone.rev_parse("main"),
        bare_rev(&remoted.bare, "main")
    );
}

#[test]
fn pull_with_nothing_new_is_a_no_op() {
    let remoted = remoted();
    let outcome = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect("pull");
    assert_eq!(outcome.old, outcome.new);
    assert!(outcome.already_up_to_date);
    assert_eq!(outcome.files_changed, 0);
}

#[test]
fn pull_when_only_local_moved_is_a_no_op() {
    let remoted = remoted();
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");
    let before = remoted.clone.rev_parse("main");
    let outcome = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect("pull");
    assert_eq!(outcome.old, outcome.new);
    assert!(outcome.already_up_to_date);
    assert_eq!(remoted.clone.rev_parse("main"), before);
}

/// Checks out a new local branch `topic` with one commit the remote does not have.
fn commit_on_new_topic(remoted: &Remoted) {
    remoted.clone.git(&["checkout", "-q", "-b", "topic"]);
    remoted.clone.write("topic.txt", b"topic\n");
    remoted
        .clone
        .commit("Topic work", "2024-03-02T00:00:00+0000");
}

#[test]
fn pull_on_a_never_pushed_branch_is_a_no_op() {
    let remoted = remoted();
    commit_on_new_topic(&remoted);
    let before = remoted.clone.rev_parse("topic");

    let outcome = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect("pull");
    assert!(outcome.unpublished);
    assert!(outcome.already_up_to_date);
    assert_eq!(outcome.old, outcome.new);
    assert_eq!(outcome.files_changed, 0);
    assert_eq!(remoted.clone.rev_parse("topic"), before);
}

#[test]
fn pull_refuses_when_the_upstream_branch_is_gone() {
    let remoted = remoted();
    commit_on_new_topic(&remoted);
    remoted
        .clone
        .git(&["config", "branch.topic.remote", "origin"]);
    remoted
        .clone
        .git(&["config", "branch.topic.merge", "refs/heads/topic"]);
    let before = remoted.clone.rev_parse("topic");

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("gone");
    let ActionError::UpstreamGone {
        op,
        branch,
        upstream_ref,
    } = &err
    else {
        panic!("expected UpstreamGone, got {err:?}");
    };
    assert_eq!(
        (*op, branch.as_str(), upstream_ref.as_str()),
        ("pull", "topic", "refs/remotes/origin/topic")
    );
    assert!(err.to_string().contains("no longer exists"), "{err}");
    assert_eq!(remoted.clone.rev_parse("topic"), before);
}

#[test]
fn pull_refuses_when_both_sides_moved() {
    let remoted = remoted();
    advance_origin(&remoted.origin, &remoted.bare_path);
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");
    fetch_clone(&remoted);

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("diverged");
    assert!(
        matches!(
            err,
            ActionError::Diverged {
                ahead: 1,
                behind: 1,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn pull_refuses_dirty_overlap_and_touches_nothing() {
    let remoted = remoted();
    remoted
        .origin
        .write("README.md", b"# fixture\n\nrewritten\n");
    remoted
        .origin
        .commit("Rewrite README", "2024-03-01T00:00:00+0000");
    remoted.origin.push_main_to(&remoted.bare_path);
    fetch_clone(&remoted);
    remoted
        .clone
        .write("README.md", b"# fixture\n\nlocal edit\n");
    let before_head = remoted.clone.rev_parse("main");

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("dirty");
    assert!(
        matches!(&err, ActionError::DirtyPath { op: "pull", path } if path == "README.md"),
        "{err}"
    );
    assert!(err.to_string().starts_with("pull: 'README.md'"), "{err}");
    assert_eq!(remoted.clone.rev_parse("main"), before_head);
    assert_eq!(
        std::fs::read(remoted.clone.path().join("README.md")).expect("README.md"),
        b"# fixture\n\nlocal edit\n",
        "the local edit must survive"
    );
}

#[test]
fn pull_refuses_a_fetched_tree_path_under_dot_git() {
    for path in [".git/hooks/pre-commit", ".GIT/hooks/pre-commit"] {
        let remoted = remoted();
        let evil = remoted
            .origin
            .commit_raw_path(path, b"#!/bin/sh\necho hooked\n", "100755");
        remoted
            .origin
            .git(&["update-ref", "refs/heads/main", &evil]);
        remoted.origin.push_main_to(&remoted.bare_path);
        fetch_clone(&remoted);
        let before_head = remoted.clone.rev_parse("main");

        let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err(path);
        assert!(
            matches!(&err, ActionError::Git { op: "pull", name, .. } if name == path),
            "{path}: {err}"
        );
        assert!(
            !remoted.clone.git_dir().join("hooks/pre-commit").exists(),
            "{path} planted a hook"
        );
        assert_eq!(remoted.clone.rev_parse("main"), before_head, "{path}");
        assert_eq!(
            remoted.clone.git_output(&["status", "--porcelain"]),
            "",
            "{path}"
        );
    }
}

#[test]
fn pull_refuses_a_fetched_symlink_that_case_folds_onto_a_directory() {
    let remoted = remoted();
    let outside = Fixture::empty_dir();
    let target = outside.path().to_str().expect("utf-8 temp path").as_bytes();
    let evil = remoted
        .origin
        .commit_raw_entries(&[("D", target, "120000"), ("d/payload", b"pwned\n", "100644")]);
    remoted
        .origin
        .git(&["update-ref", "refs/heads/main", &evil]);
    remoted.origin.push_main_to(&remoted.bare_path);
    fetch_clone(&remoted);
    let before_head = remoted.clone.rev_parse("main");

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("evil");
    assert!(
        matches!(&err, ActionError::Git { op: "pull", name, .. } if name == "d/payload"),
        "{err}"
    );
    assert!(
        err.to_string()
            .contains("which would leave the working tree"),
        "{err}"
    );
    assert!(
        std::fs::read_dir(outside.path())
            .expect("list outside")
            .next()
            .is_none(),
        "the pull wrote outside the tree"
    );
    assert_eq!(remoted.clone.rev_parse("main"), before_head);
    assert_eq!(remoted.clone.git_output(&["status", "--porcelain"]), "");
}

#[test]
fn pull_refuses_an_untracked_file_in_the_way() {
    let remoted = remoted();
    advance_origin(&remoted.origin, &remoted.bare_path); // adds news.txt upstream
    fetch_clone(&remoted);
    remoted.clone.write("news.txt", b"mine, not yours\n");

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("blocked");
    assert!(
        matches!(&err, ActionError::DirtyPath { op: "pull", path } if path == "news.txt"),
        "{err}"
    );
    assert_eq!(
        std::fs::read(remoted.clone.path().join("news.txt")).expect("news.txt"),
        b"mine, not yours\n"
    );
}

#[test]
fn pull_refuses_a_staged_change_on_a_touched_path() {
    let remoted = remoted();
    remoted
        .origin
        .write("README.md", b"# fixture\n\nrewritten\n");
    remoted
        .origin
        .commit("Rewrite README", "2024-03-01T00:00:00+0000");
    remoted.origin.push_main_to(&remoted.bare_path);
    fetch_clone(&remoted);
    remoted
        .clone
        .write("README.md", b"# fixture\n\nstaged edit\n");
    remoted.clone.git(&["add", "README.md"]);

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("staged");
    assert!(
        matches!(&err, ActionError::DirtyPath { path, .. } if path == "README.md"),
        "{err}"
    );
}

/// A clone one fetched commit behind its upstream, with `README.md` conflicted by a merge
/// that stopped, and `main` moved back to its tip so the pull would otherwise fast-forward.
fn conflicted_clone_behind_origin() -> (Remoted, String) {
    let remoted = remoted();
    advance_origin(&remoted.origin, &remoted.bare_path); // adds news.txt upstream
    fetch_clone(&remoted);
    let tip = remoted.clone.rev_parse("main");
    remoted.clone.conflicted_merge("README.md");
    remoted
        .clone
        .git(&["update-ref", "refs/heads/main", tip.as_str()]);
    (remoted, tip)
}

#[test]
fn pull_refuses_during_a_conflicted_merge_and_leaves_head() {
    let (remoted, tip) = conflicted_clone_behind_origin();

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("merging");
    assert!(matches!(&err, ActionError::Git { op: "pull", .. }), "{err}");
    assert!(
        err.to_string()
            .contains("a merge is in progress; finish or abort it in git"),
        "{err}"
    );
    assert_eq!(remoted.clone.rev_parse("main"), tip);
    assert!(!remoted.clone.path().join("news.txt").exists());
}

#[test]
fn pull_refuses_a_conflicted_index_on_a_path_it_does_not_touch() {
    let (remoted, tip) = conflicted_clone_behind_origin();
    remoted.clone.remove(".git/MERGE_HEAD");

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("conflict");
    assert!(
        matches!(&err, ActionError::Git { op: "pull", name, .. } if name == "README.md"),
        "{err}"
    );
    assert!(err.to_string().contains("unresolved conflicts"), "{err}");
    assert_eq!(remoted.clone.rev_parse("main"), tip);
    assert!(!remoted.clone.path().join("news.txt").exists());
}

#[test]
fn pull_refuses_a_detached_head() {
    let remoted = remoted();
    remoted.clone.git(&["checkout", "-q", "--detach"]);
    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("detached");
    assert!(matches!(err, ActionError::DetachedHead { .. }), "{err}");
}

#[test]
fn detached_head_is_refused_by_every_entry_point() {
    let remoted = remoted();
    remoted.clone.git(&["checkout", "-q", "--detach"]);
    let served = bare_rev(&remoted.bare, "main");

    let err = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("push on a detached HEAD");
    assert!(
        matches!(err, ActionError::DetachedHead { op: "push" }),
        "{err}"
    );
    assert_eq!(bare_rev(&remoted.bare, "main"), served);

    let err = upstream_status(remoted.clone.path()).expect_err("status on a detached HEAD");
    assert!(matches!(err, ActionError::DetachedHead { .. }), "{err}");
}

#[test]
fn pull_without_a_remote_reports_no_upstream() {
    let fixture = Fixture::standard();
    let before = fixture.rev_parse("main");
    let err = pull_fast_forward(fixture.path(), &Policy::default()).expect_err("no remote");
    assert!(
        matches!(&err, ActionError::NoUpstream { op: "pull", branch } if branch == "main"),
        "{err}"
    );
    assert_eq!(fixture.rev_parse("main"), before);
}

/// A repository on an unborn `main`, with an `origin` that is never contacted.
fn unborn_with_origin() -> Fixture {
    let fixture = Fixture::empty_dir();
    fixture.git(&["init", "-q", "-b", "main"]);
    fixture.git(&["remote", "add", "origin", "https://example.invalid/x.git"]);
    fixture
}

#[test]
fn pull_refuses_an_unborn_branch() {
    let fixture = unborn_with_origin();
    let err = pull_fast_forward(fixture.path(), &Policy::default()).expect_err("unborn");
    assert!(matches!(&err, ActionError::Git { op: "pull", .. }), "{err}");
    assert!(err.to_string().contains("no commits yet"), "{err}");
    assert_eq!(
        fixture.git_output(&["for-each-ref", "--format=%(refname)"]),
        "",
        "the refused pull must create no ref"
    );
}

#[test]
fn push_refuses_an_unborn_branch() {
    let fixture = unborn_with_origin();
    let err = push_upstream(
        fixture.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("unborn");
    assert!(matches!(&err, ActionError::Git { op: "push", .. }), "{err}");
    assert!(err.to_string().contains("no commits yet"), "{err}");
    assert_eq!(
        fixture.git_output(&["for-each-ref", "--format=%(refname)"]),
        "",
        "the refused push must create no ref"
    );
}

#[test]
fn push_without_a_remote_reports_no_upstream() {
    let fixture = Fixture::standard();
    let err = push_upstream(
        fixture.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("no remote");
    assert!(
        matches!(&err, ActionError::NoUpstream { op: "push", branch } if branch == "main"),
        "{err}"
    );
}

// --- push ---

#[test]
fn push_uploads_commits_and_updates_the_tracking_ref() {
    let remoted = remoted();
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");
    let tip = remoted.clone.rev_parse("HEAD");

    let outcome = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("push");
    assert_eq!(outcome.remote, "origin");
    assert_eq!(outcome.branch, "main");
    assert_eq!(outcome.pushed_commits, 1);
    assert!(!outcome.already_up_to_date);
    assert_eq!(outcome.new.to_string(), tip);
    assert_eq!(
        bare_rev(&remoted.bare, "main"),
        tip,
        "the served repository must have the commit"
    );
    assert_eq!(
        remoted.clone.rev_parse("refs/remotes/origin/main"),
        tip,
        "the tracking ref must be advanced"
    );
    let status = upstream_status(remoted.clone.path()).expect("status");
    assert_eq!((status.ahead, status.behind), (0, 0));
}

#[test]
fn push_reports_a_stuck_tracking_ref_beside_the_push_and_a_retry_repairs_it() {
    let remoted = remoted();
    let before = remoted.clone.rev_parse("refs/remotes/origin/main");
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");
    let tip = remoted.clone.rev_parse("HEAD");
    let lock = remoted
        .clone
        .path()
        .join(".git/refs/remotes/origin/main.lock");
    #[expect(
        clippy::disallowed_methods,
        reason = "test setup holds the tracking ref's lock, as a concurrent git would"
    )]
    std::fs::write(&lock, b"").expect("hold the tracking ref lock");

    let outcome = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("a push the server accepted is not a failure");
    assert_eq!(outcome.pushed_commits, 1);
    assert_eq!(bare_rev(&remoted.bare, "main"), tip, "the push must land");
    let message = outcome
        .tracking_ref_error
        .expect("the stuck tracking ref must be reported");
    assert!(message.contains("refs/remotes/origin/main"), "{message}");
    assert_eq!(remoted.clone.rev_parse("refs/remotes/origin/main"), before);

    #[expect(
        clippy::disallowed_methods,
        reason = "test setup releases the lock it took"
    )]
    std::fs::remove_file(&lock).expect("release the tracking ref lock");
    let retry = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("retry");
    assert_eq!(retry.pushed_commits, 0);
    assert!(retry.already_up_to_date);
    assert_eq!(retry.tracking_ref_error, None);
    assert_eq!(
        remoted.clone.rev_parse("refs/remotes/origin/main"),
        tip,
        "the retry must move the stale tracking ref"
    );
    let status = upstream_status(remoted.clone.path()).expect("status");
    assert_eq!((status.ahead, status.behind), (0, 0));
}

#[test]
fn push_with_nothing_new_is_a_no_op() {
    let remoted = remoted();
    let outcome = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("push");
    assert_eq!(outcome.pushed_commits, 0);
    assert_eq!(outcome.old, Some(outcome.new));
    assert!(outcome.already_up_to_date);
    assert_eq!(outcome.tracking_ref_error, None);
}

/// About 4 KiB of text that differs from `variant` to `variant` in one line, so git stores
/// each version as a delta against another.
fn four_kib_text(variant: usize) -> Vec<u8> {
    let mut lines = Vec::new();
    for line in 0..128 {
        if line == 64 {
            lines.push(format!("variant {variant:>22}\n"));
        } else {
            lines.push(format!("line {line:>25}\n"));
        }
    }
    lines.concat().into_bytes()
}

#[test]
fn push_after_repack_sends_a_self_contained_pack() {
    let remoted = remoted();
    for (variant, date) in [
        "2024-03-02T00:00:00+0000",
        "2024-03-03T00:00:00+0000",
        "2024-03-04T00:00:00+0000",
    ]
    .into_iter()
    .enumerate()
    {
        remoted.clone.write("big.txt", &four_kib_text(variant));
        remoted.clone.commit(&format!("Edit {variant}"), date);
    }
    remoted.clone.git(&["repack", "-adf", "--quiet"]);
    let tip = remoted.clone.rev_parse("HEAD");

    let outcome = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("push");
    assert_eq!(outcome.pushed_commits, 3);
    assert_eq!(bare_rev(&remoted.bare, "main"), tip);
    remoted
        .bare
        .git(&["--git-dir", "remote.git", "fsck", "--strict"]);
}

#[test]
fn push_to_an_empty_remote_uploads_the_whole_history() {
    let local = Fixture::standard();
    local.git(&["repack", "-adf", "--quiet"]);
    let bare = Fixture::empty_dir();
    bare.git(&["init", "--bare", "--quiet", "remote.git"]);
    bare.git(&[
        "--git-dir",
        "remote.git",
        "config",
        "http.receivepack",
        "true",
    ]);
    let server = rc_git_testkit::HttpGitServer::serve(&bare.path().join("remote.git"));
    local.git(&["remote", "add", "origin", &server.url()]);
    local.git(&["config", "branch.main.remote", "origin"]);
    local.git(&["config", "branch.main.merge", "refs/heads/main"]);

    let outcome = push_upstream(
        local.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("push");
    let history: usize = local
        .git_output(&["rev-list", "--count", "HEAD"])
        .parse()
        .expect("commit count");
    assert_eq!(outcome.pushed_commits, history);
    assert_eq!(outcome.old, None);
    assert_eq!(bare_rev(&bare, "main"), local.rev_parse("HEAD"));
    bare.git(&["--git-dir", "remote.git", "fsck", "--strict"]);
    assert_eq!(
        server.chunked_requests(),
        1,
        "the pack must stream as a chunked POST, not be buffered for Content-Length"
    );
}

#[test]
fn push_names_a_missing_local_object() {
    let remoted = remoted();
    remoted.clone.write("local.txt", b"never fetched\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");
    let blob = remoted.clone.rev_parse("HEAD:local.txt");
    let (dir, file) = blob.split_at(2);
    let loose = remoted
        .clone
        .path()
        .join(".git/objects")
        .join(dir)
        .join(file);
    #[expect(
        clippy::disallowed_methods,
        reason = "test setup drops a loose blob, as a partial clone never has it"
    )]
    std::fs::remove_file(&loose).expect("remove the loose blob");

    let err = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("push with a missing blob");
    assert!(
        matches!(&err, ActionError::Git { op: "push", name, .. } if *name == blob),
        "{err}"
    );
    assert!(err.to_string().contains("missing"), "{err}");
    assert_ne!(
        bare_rev(&remoted.bare, "main"),
        remoted.clone.rev_parse("HEAD"),
        "the served repository must not move"
    );
}

#[test]
fn push_creates_a_new_branch_from_an_existing_commit() {
    let remoted = remoted();
    remoted.clone.git(&["checkout", "-q", "-b", "topic"]);

    let outcome = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("push");
    assert_eq!(outcome.branch, "topic");
    assert_eq!(outcome.old, None);
    assert!(
        !outcome.already_up_to_date,
        "creating the remote branch is not a no-op"
    );
    assert_eq!(
        outcome.pushed_commits, 0,
        "every object already exists remotely"
    );
    assert_eq!(
        bare_rev(&remoted.bare, "topic"),
        remoted.clone.rev_parse("HEAD")
    );

    // `topic` has no `branch.topic.*` keys, so only the origin fallback names the tracking
    // ref; a missing one would also read as (0, 0), hence the explicit ref checks.
    assert_eq!(
        remoted.clone.rev_parse("refs/remotes/origin/topic"),
        remoted.clone.rev_parse("HEAD")
    );
    let status = upstream_status(remoted.clone.path()).expect("status");
    assert!(status.has_upstream);
    assert_eq!(status.upstream_ref, "refs/remotes/origin/topic");
    assert_eq!((status.ahead, status.behind), (0, 0));
}

#[test]
fn push_agrees_with_the_configured_upstream() {
    let remoted = remoted();
    remoted
        .clone
        .git(&["checkout", "-q", "-b", "topic", "origin/main"]);
    remoted.clone.write("topic.txt", b"topic\n");
    remoted
        .clone
        .commit("Topic work", "2024-03-02T00:00:00+0000");

    let err = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("a push to a differently named upstream is refused");
    let message = err.to_string();
    assert!(
        message.contains("upstream origin/main does not match branch topic"),
        "{message}"
    );
    let listed = remoted.bare.git_output(&[
        "--git-dir",
        "remote.git",
        "for-each-ref",
        "--format=%(refname)",
        "refs/heads/topic",
    ]);
    assert_eq!(listed, "", "no differently named remote branch is created");
    assert!(
        remoted
            .clone
            .git_output(&["for-each-ref", "refs/remotes/origin/topic"])
            .is_empty(),
        "no tracking ref is written for the refused push"
    );
    let status = upstream_status(remoted.clone.path()).expect("status");
    assert_eq!(status.upstream_ref, "refs/remotes/origin/main");
    assert_eq!((status.ahead, status.behind), (1, 0));
}

#[test]
fn push_sends_to_the_remote_pushurl() {
    let remoted = remoted();
    remoted.clone.git(&[
        "config",
        "remote.origin.url",
        "git://example.invalid/up.git",
    ]);
    remoted
        .clone
        .git(&["config", "remote.origin.pushurl", &remoted.server.url()]);
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");
    let tip = remoted.clone.rev_parse("HEAD");

    let outcome = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("push goes to the pushurl, not the unsupported fetch url");
    assert_eq!(outcome.remote, "origin");
    assert_eq!(outcome.pushed_commits, 1);
    assert_eq!(bare_rev(&remoted.bare, "main"), tip);
    assert_eq!(remoted.clone.rev_parse("refs/remotes/origin/main"), tip);
}

#[test]
fn push_from_a_single_branch_clone_writes_no_unmapped_tracking_ref() {
    let remoted = remoted();
    let clone = Fixture::empty_dir();
    clone.git(&[
        "clone",
        "--quiet",
        "--single-branch",
        "-b",
        "main",
        &remoted.server.url(),
        ".",
    ]);
    clone.git(&["checkout", "-q", "-b", "topic"]);
    clone.write("topic.txt", b"topic\n");
    clone.commit("Topic work", "2024-03-02T00:00:00+0000");
    let tip = clone.rev_parse("HEAD");

    let outcome = push_upstream(
        clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("push");
    assert_eq!(outcome.tracking_ref_error, None);
    assert_eq!(bare_rev(&remoted.bare, "topic"), tip);
    assert_eq!(
        clone.git_output(&["for-each-ref", "--format=%(refname)", "refs/remotes/"]),
        "refs/remotes/origin/main",
        "no fetch refspec maps topic, so git writes no tracking ref for it"
    );

    clone.git(&["update-ref", "refs/remotes/origin/topic", "main"]);
    let status = upstream_status(clone.path()).expect("status");
    assert!(status.has_upstream);
    assert_eq!(
        status.upstream_ref, "",
        "a ref no fetch refspec maps is never fetched, so status must not count against it"
    );
    assert_eq!((status.ahead, status.behind), (0, 0));
    let pulled = pull_fast_forward(clone.path(), &Policy::default()).expect("pull");
    assert!(pulled.unpublished);
    assert_eq!(clone.rev_parse("topic"), tip);
}

#[test]
fn push_to_a_url_upstream_writes_no_tracking_ref() {
    let remoted = remoted();
    let clone = &remoted.clone;
    clone.git(&["remote", "remove", "origin"]);
    clone.git(&["config", "branch.main.remote", &remoted.server.url()]);
    clone.git(&["config", "branch.main.merge", "refs/heads/main"]);
    clone.write("local.txt", b"local\n");
    clone.commit("Local work", "2024-03-02T00:00:00+0000");
    let tip = clone.rev_parse("HEAD");

    let outcome = push_upstream(
        clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("push to a URL upstream");
    assert_eq!(outcome.pushed_commits, 1);
    assert_eq!(outcome.tracking_ref_error, None);
    assert_eq!(bare_rev(&remoted.bare, "main"), tip);
    assert_eq!(
        clone.git_output(&["for-each-ref", "--format=%(refname)", "refs/remotes/"]),
        "",
        "a URL upstream has no remote to track under"
    );
}

#[test]
fn upstream_status_follows_a_remapping_fetch_refspec_without_branch_config() {
    let remoted = remoted();
    untrack(&remoted);
    remoted.clone.git(&[
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/upstream/*",
    ]);
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");
    fetch_clone(&remoted);

    let status = upstream_status(remoted.clone.path()).expect("status");
    assert_eq!(status.upstream_ref, "refs/remotes/upstream/main");
    assert_eq!((status.ahead, status.behind), (1, 0));
}

/// A second served bare copy of `remoted`'s origin, added to the clone as remote `fork`.
fn add_fork(remoted: &Remoted) -> (Fixture, rc_git_testkit::HttpGitServer) {
    let (fork, fork_path) = remoted.origin.bare_copy();
    let server = rc_git_testkit::HttpGitServer::serve(&fork_path);
    remoted.clone.git(&["remote", "add", "fork", &server.url()]);
    (fork, server)
}

/// Pushes the clone's checked-out branch, which must land on `fork` and leave origin and
/// its tracking ref alone, and moves `refs/remotes/fork/<branch>`.
fn assert_push_lands_on_the_fork(remoted: &Remoted, fork: &Fixture, branch: &str) {
    let origin_before = bare_rev(&remoted.bare, "main");
    let tracking_before = remoted.clone.rev_parse("refs/remotes/origin/main");
    let tip = remoted.clone.rev_parse("HEAD");

    let outcome = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("push to the fork");
    assert_eq!(outcome.remote, "fork");
    assert_eq!(outcome.branch, branch);
    assert_eq!(bare_rev(fork, branch), tip);
    assert_eq!(bare_rev(&remoted.bare, "main"), origin_before);
    assert_eq!(
        remoted
            .clone
            .rev_parse(&format!("refs/remotes/fork/{branch}")),
        tip
    );
    assert_eq!(
        remoted.clone.rev_parse("refs/remotes/origin/main"),
        tracking_before,
        "the fetch remote's tracking ref must not claim a commit it lacks"
    );
}

#[test]
fn push_follows_the_branch_push_remote() {
    let remoted = remoted();
    let (fork, _server) = add_fork(&remoted);
    remoted
        .clone
        .git(&["config", "branch.main.pushRemote", "fork"]);
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");

    assert_push_lands_on_the_fork(&remoted, &fork, "main");
}

#[test]
fn push_follows_push_default_even_when_merge_names_another_branch() {
    let remoted = remoted();
    let (fork, _server) = add_fork(&remoted);
    remoted.clone.git(&["config", "remote.pushDefault", "fork"]);
    remoted
        .clone
        .git(&["checkout", "-q", "-b", "topic", "origin/main"]);
    remoted.clone.write("topic.txt", b"topic\n");
    remoted
        .clone
        .commit("Topic work", "2024-03-02T00:00:00+0000");

    assert_push_lands_on_the_fork(&remoted, &fork, "topic");
}

#[test]
fn push_is_rejected_when_the_remote_moved() {
    let remoted = remoted();
    advance_origin(&remoted.origin, &remoted.bare_path);
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");

    let err = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("rejected");
    assert!(matches!(err, ActionError::PushRejected { .. }), "{err}");
    assert_ne!(
        bare_rev(&remoted.bare, "main"),
        remoted.clone.rev_parse("HEAD")
    );
}

#[test]
fn push_against_an_auth_server_requires_credentials() {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::HttpGitServer::serve_with_auth(&bare_path, "user", "sekret");
    let clone = Fixture::basic_auth_clone(&server, "user", "sekret");
    clone.write("local.txt", b"local\n");
    clone.commit("Local work", "2024-03-02T00:00:00+0000");

    let anonymous = push_upstream(
        clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    );
    assert!(
        matches!(anonymous, Err(ActionError::AuthRequired { .. })),
        "{anonymous:?}"
    );

    let outcome = push_upstream(
        clone.path(),
        &Policy::default(),
        auth(&Credentials {
            username: "user".to_owned(),
            token: "sekret".to_owned(),
        }),
        &Cancel::new(),
    )
    .expect("authenticated push");
    assert_eq!(outcome.pushed_commits, 1);
    assert_eq!(bare_rev(&bare, "main"), clone.rev_parse("HEAD"));
}

#[test]
fn push_surfaces_a_hook_rejection() {
    let remoted = remoted();
    remoted.bare.write(
        "remote.git/hooks/pre-receive",
        b"#!/bin/sh\necho refused by policy >&2\nexit 1\n",
    );
    #[expect(
        clippy::disallowed_methods,
        reason = "test setup marks the hook executable"
    )]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            remoted.bare.path().join("remote.git/hooks/pre-receive"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod hook");
    }
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");

    let err = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("hook rejects");
    let message = err.to_string();
    assert!(message.contains("by policy"), "{message}");
    assert_ne!(
        bare_rev(&remoted.bare, "main"),
        remoted.clone.rev_parse("HEAD")
    );
}

#[test]
fn a_preset_cancel_flag_stops_the_push_before_it_connects() {
    let remoted = remoted();
    let before = remoted.server.requests();
    let err = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &cancelled(),
    )
    .expect_err("cancelled");
    assert!(
        matches!(err, ActionError::Cancelled { op: "push" }),
        "{err}"
    );
    assert_eq!(
        remoted.server.requests(),
        before,
        "a cancelled op must not reach the server"
    );
}

#[test]
fn a_failed_push_never_names_the_remote_url_password() {
    // A URL written straight into `branch.main.remote` is the upstream's name in errors, and
    // plain http to a non-loopback host is refused by that name before any connection.
    let fixture = Fixture::standard();
    fixture.git(&[
        "config",
        "branch.main.remote",
        "http://user:secret@example.invalid/x.git",
    ]);
    let err = push_upstream(
        fixture.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("plain http to a remote host is refused");
    let message = err.to_string();
    assert!(
        message.contains("http://example.invalid/x.git"),
        "the refusal names the URL: {message}"
    );
    assert!(!message.contains("secret"), "{message}");
}

// ---- ssh ----------------------------------------------------------------------------

/// An origin repository, a bare copy served over loopback ssh, a local clone whose
/// `origin` points at the ssh URL, and a fixture `~/.ssh`-style directory.
struct SshRemoted {
    origin: Fixture,
    bare: Fixture,
    bare_path: PathBuf,
    server: rc_git_testkit::SshGitServer,
    clone: Fixture,
    ssh_home: Fixture,
}

impl SshRemoted {
    /// `RemoteAuth` pointing at the fixture ssh directory.
    fn auth(&self) -> RemoteAuth<'_> {
        RemoteAuth {
            ssh_dir: Some(self.ssh_home.path()),
            ..RemoteAuth::default()
        }
    }
}

/// Serves a bare copy over ssh; the clone is made from the local path (the git CLI
/// never speaks to the test server) and re-pointed at the ssh URL.
fn ssh_remoted() -> SshRemoted {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::SshGitServer::serve(&bare_path);
    let (clone, ssh_home) = Fixture::ssh_clone(&bare_path, &server);
    SshRemoted {
        origin,
        bare,
        bare_path,
        server,
        clone,
        ssh_home,
    }
}

#[test]
fn ssh_fetch_updates_the_tracking_ref() {
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    advance_origin(&remoted.origin, &remoted.bare_path);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect("ssh fetch");
    assert_eq!(outcome.remote, "origin");
    assert_eq!(outcome.updated_refs.len(), 1);
    assert_eq!(outcome.updated_refs[0].name, "refs/remotes/origin/main");
    let status = upstream_status(remoted.clone.path()).expect("status");
    assert_eq!((status.ahead, status.behind), (0, 1));
    drop(remoted.server);
}

#[test]
fn ssh_push_uploads_commits_and_updates_the_tracking_ref() {
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");

    let outcome = push_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect("ssh push");
    assert_eq!(outcome.pushed_commits, 1);
    assert_eq!(
        bare_rev(&remoted.bare, "main"),
        remoted.clone.rev_parse("HEAD")
    );
    let status = upstream_status(remoted.clone.path()).expect("status");
    assert_eq!((status.ahead, status.behind), (0, 0));
}

/// A cancel raised while the push waits on the server's report ends it as cancelled.
/// The server's `pre-receive` hook marks that the pack arrived and then holds the
/// report back, so the cancel lands inside the client's report read every time. Run on
/// its own thread so a cancel that never lands fails here instead of hanging the suite.
#[test]
fn ssh_push_cancelled_mid_transfer_ends_as_cancelled() {
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    let marker = remoted.bare.path().join("pack-received");
    let hook = format!("#!/bin/sh\n: > '{}'\nsleep 5\n", marker.display());
    remoted
        .bare
        .write("remote.git/hooks/pre-receive", hook.as_bytes());
    remoted.bare.set_mode("remote.git/hooks/pre-receive", 0o755);
    remoted.clone.write("local.txt", b"local\n");
    remoted
        .clone
        .commit("Local work", "2024-03-02T00:00:00+0000");

    let cancel = Cancel::new();
    let signal = cancel.clone();
    let repo = remoted.clone.path().to_path_buf();
    let ssh_dir = remoted.ssh_home.path().to_path_buf();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let auth = RemoteAuth {
            ssh_dir: Some(&ssh_dir),
            ..RemoteAuth::default()
        };
        drop(done.send(push_upstream(&repo, &Policy::default(), auth, &cancel)));
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the server never ran its pre-receive hook"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    signal.signal();

    let err = finished
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("the push ends within 2 s of the cancel")
        .expect_err("cancelled");
    assert!(
        matches!(err, ActionError::Cancelled { op: "push" }),
        "{err}"
    );
}

/// Runs `fetch_upstream` on its own thread, so a call that never returns fails the test
/// at `recv_timeout` instead of hanging the suite.
fn fetch_on_a_thread(
    remoted: &SshRemoted,
    cancel: Cancel,
) -> std::sync::mpsc::Receiver<Result<rc_git_guard::actions::FetchOutcome, ActionError>> {
    let repo = remoted.clone.path().to_path_buf();
    let ssh_dir = remoted.ssh_home.path().to_path_buf();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let auth = RemoteAuth {
            ssh_dir: Some(&ssh_dir),
            ..RemoteAuth::default()
        };
        drop(done.send(fetch_upstream(&repo, &Policy::default(), auth, &cancel)));
    });
    finished
}

/// A peer that accepts the TCP connection and never sends its ssh banner stalls the dial;
/// a cancel raised then still ends the fetch as cancelled.
#[test]
fn ssh_a_cancel_during_a_stalled_connect_returns_cancelled() {
    let remoted = ssh_remoted();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    let silent = std::thread::spawn(move || listener.accept().map(|(stream, _)| stream));
    remoted.clone.git(&[
        "remote",
        "set-url",
        "origin",
        &format!("ssh://git@127.0.0.1:{port}{}", remoted.bare_path.display()),
    ]);

    let cancel = Cancel::new();
    let signal = cancel.clone();
    let finished = fetch_on_a_thread(&remoted, cancel);
    std::thread::sleep(std::time::Duration::from_millis(300));
    signal.signal();

    let err = finished
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("the fetch ends within 2 s of the cancel")
        .expect_err("cancelled");
    assert!(
        matches!(err, ActionError::Cancelled { op: "fetch" }),
        "{err}"
    );
    drop(silent.join().expect("listener thread").expect("accepted"));
}

/// A server that authenticates and then never confirms the session channel counts
/// against the connect budget instead of hanging the fetch.
#[test]
fn ssh_a_server_that_never_confirms_the_channel_times_out() {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::SshGitServer::serve_never_confirming_channels(&bare_path);
    let (clone, ssh_home) = Fixture::ssh_clone(&bare_path, &server);
    server.write_known_hosts(ssh_home.path());
    let remoted = SshRemoted {
        origin,
        bare,
        bare_path,
        server,
        clone,
        ssh_home,
    };

    let cancel = Cancel::new().with_connect_timeout(std::time::Duration::from_millis(1500));
    let err = fetch_on_a_thread(&remoted, cancel)
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the fetch ends once the connect budget is spent")
        .expect_err("timed out");
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    assert!(
        err.to_string().contains("did not start the git service"),
        "{err}"
    );
}

/// A server that authenticates and then refuses the git command on stderr, as GitHub
/// does when the accepted key's account cannot see the repository, reports that text
/// rather than a bare end of stream.
#[test]
fn ssh_a_server_refusal_reports_its_stderr() {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    let server = rc_git_testkit::SshGitServer::serve_refusing_repositories(&bare_path);
    let (clone, ssh_home) = Fixture::ssh_clone(&bare_path, &server);
    server.write_known_hosts(ssh_home.path());
    let remoted = SshRemoted {
        origin,
        bare,
        bare_path,
        server,
        clone,
        ssh_home,
    };

    let err = fetch_on_a_thread(&remoted, Cancel::new())
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the fetch ends once the server refuses")
        .expect_err("refused");
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    assert!(
        err.to_string()
            .contains("the server said: ERROR: Repository not found. (exit status 1)"),
        "{err}"
    );
}

#[test]
fn ssh_unknown_host_reports_the_fingerprint_and_an_accepted_retry_succeeds() {
    // No known_hosts entry is written, so the host is unknown.
    let remoted = ssh_remoted();

    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect_err("unknown host refused");
    let ActionError::UnknownHostKey {
        key_type,
        fingerprint,
        ..
    } = &err
    else {
        panic!("expected UnknownHostKey, got {err}");
    };
    assert_eq!(key_type, "ssh-ed25519");
    assert_eq!(fingerprint, &remoted.server.host_fingerprint());

    let accepted = remoted.server.host_fingerprint();
    fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth {
            accepted_hostkey: Some(&accepted),
            ..remoted.auth()
        },
        &Cancel::new(),
    )
    .expect("accepted fingerprint fetch");
    assert!(
        !remoted.ssh_home.path().join("known_hosts").exists(),
        "an accepted fingerprint must not be written to known_hosts"
    );
}

#[test]
fn ssh_host_key_mismatch_is_fatal() {
    let remoted = ssh_remoted();
    remoted
        .server
        .write_mismatched_known_hosts(remoted.ssh_home.path());

    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect_err("mismatch refused");
    assert!(matches!(&err, ActionError::HostKeyMismatch { .. }), "{err}");
    assert!(err.to_string().contains("man-in-the-middle"), "{err}");

    let offered = remoted.server.host_fingerprint();
    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth {
            accepted_hostkey: Some(&offered),
            ..remoted.auth()
        },
        &Cancel::new(),
    )
    .expect_err("approving the offered key does not override known_hosts");
    assert!(matches!(&err, ActionError::HostKeyMismatch { .. }), "{err}");
}

/// A `known_hosts` line for the host whose key does not decode is skipped, as OpenSSH
/// skips it: the valid entry after it still pins the host.
#[test]
fn ssh_malformed_known_hosts_line_is_skipped() {
    let remoted = ssh_remoted();
    let line = format!("[127.0.0.1]:{} ssh-ed25519 %%%%\n", remoted.server.port());
    remoted.ssh_home.write("known_hosts", line.as_bytes());
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    advance_origin(&remoted.origin, &remoted.bare_path);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect("the valid line after the malformed one pins the host");
    assert_eq!(outcome.updated_refs.len(), 1);
}

/// A host pinned only in the file the ssh config names is known, as it is to OpenSSH:
/// no trust-on-first-use prompt, no approval needed.
#[test]
fn ssh_user_known_hosts_file() {
    let remoted = ssh_remoted();
    let pinned = Fixture::empty_dir();
    remoted.server.write_known_hosts(pinned.path());
    let pinned = std::fs::read(pinned.path().join("known_hosts")).expect("read known_hosts");
    remoted.ssh_home.write("other_hosts", &pinned);
    let other_hosts = remoted.ssh_home.path().join("other_hosts");
    remoted.ssh_home.write(
        "config",
        format!(
            "Host 127.0.0.1\n  UserKnownHostsFile {}\n",
            other_hosts.display()
        )
        .as_bytes(),
    );
    advance_origin(&remoted.origin, &remoted.bare_path);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect("a host pinned in UserKnownHostsFile is known");
    assert_eq!(outcome.updated_refs.len(), 1);
    assert!(!remoted.ssh_home.path().join("known_hosts").exists());
}

/// A stale same-algorithm line ahead of the correct one does not refuse the host:
/// OpenSSH accepts any line holding the exact key.
#[test]
fn ssh_stale_known_hosts_line() {
    let remoted = ssh_remoted();
    remoted
        .server
        .write_mismatched_known_hosts(remoted.ssh_home.path());
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    advance_origin(&remoted.origin, &remoted.bare_path);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect("the correct line wins over the stale one");
    assert_eq!(outcome.updated_refs.len(), 1);
}

/// A fingerprint that is not the server's real key, as an earlier approval.
const STALE_APPROVAL: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

#[test]
fn ssh_changed_key_for_an_approved_host_is_a_mismatch() {
    let remoted = ssh_remoted();

    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth {
            accepted_hostkey: Some(STALE_APPROVAL),
            ..remoted.auth()
        },
        &Cancel::new(),
    )
    .expect_err("a changed key for an approved host is refused");
    let ActionError::HostKeyMismatch {
        fingerprint,
        approved,
        ..
    } = &err
    else {
        panic!("expected HostKeyMismatch, got {err}");
    };
    assert_eq!(fingerprint, &remoted.server.host_fingerprint());
    assert_eq!(approved.as_deref(), Some(STALE_APPROVAL));
    assert!(err.to_string().contains(STALE_APPROVAL), "{err}");
}

#[test]
fn ssh_an_accepted_fingerprint_for_another_key_is_still_refused() {
    let remoted = ssh_remoted();
    remoted
        .server
        .write_mismatched_known_hosts(remoted.ssh_home.path());

    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth {
            accepted_hostkey: Some(STALE_APPROVAL),
            ..remoted.auth()
        },
        &Cancel::new(),
    )
    .expect_err("mismatch refused");
    assert!(matches!(&err, ActionError::HostKeyMismatch { .. }), "{err}");
}

#[test]
fn ssh_wrong_key_reports_auth_required() {
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    remoted
        .server
        .write_unauthorized_client_key(remoted.ssh_home.path());

    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect_err("wrong key refused");
    assert!(matches!(&err, ActionError::AuthRequired { .. }), "{err}");
}

#[test]
fn ssh_encrypted_key_requires_a_passphrase_and_opens_with_it() {
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    let passphrase = remoted
        .server
        .write_encrypted_client_key(remoted.ssh_home.path());

    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect_err("encrypted key without passphrase");
    assert!(
        matches!(&err, ActionError::PassphraseRequired { .. }),
        "{err}"
    );

    let creds = Credentials {
        username: String::new(),
        token: passphrase.to_owned(),
    };
    fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth {
            creds: Some(&creds),
            ..remoted.auth()
        },
        &Cancel::new(),
    )
    .expect("passphrase fetch");
}

#[test]
fn ssh_wrong_passphrase_reports_passphrase_required() {
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    remoted
        .server
        .write_encrypted_client_key(remoted.ssh_home.path());
    let creds = Credentials {
        username: String::new(),
        token: "not-the-passphrase".to_owned(),
    };

    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth {
            creds: Some(&creds),
            ..remoted.auth()
        },
        &Cancel::new(),
    )
    .expect_err("a wrong passphrase cannot open the key");
    let ActionError::PassphraseRequired { key, .. } = &err else {
        panic!("expected PassphraseRequired, got {err}");
    };
    let expected = remoted.ssh_home.path().join("id_ed25519");
    assert_eq!(key, &expected.display().to_string());
    assert!(err.to_string().contains("passphrase is wrong"), "{err}");
}

#[test]
fn ssh_unreadable_identity_names_the_file() {
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    remoted.ssh_home.write("id_ed25519", b"not a key");

    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect_err("a file that is not a key cannot authenticate");
    assert!(
        !matches!(
            &err,
            ActionError::PassphraseRequired { .. } | ActionError::AuthRequired { .. }
        ),
        "{err}"
    );
    let path = remoted.ssh_home.path().join("id_ed25519");
    let message = err.to_string();
    assert!(
        message.contains(&format!("'{}' (not a private key)", path.display())),
        "{message}"
    );
}

#[test]
fn scp_like_urls_parse_as_the_ssh_scheme() {
    // The router keys on gix's URL parsing; this pins the scp-like form to `Ssh` so a
    // gix upgrade cannot silently reroute it.
    let url = gix::url::parse(gix::bstr::BStr::new("git@example.com:x/y.git")).expect("parse");
    assert_eq!(url.scheme, gix::url::Scheme::Ssh);
    assert_eq!(url.host(), Some("example.com"));
}

#[test]
fn ssh_config_alias_resolves_hostname_user_and_port() {
    // The multi-account pattern that produced "failed to lookup address information":
    // the URL names an alias; HostName, Port, and User come from the ssh config, and
    // known_hosts is keyed by the dialed name, not the alias.
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    remoted.ssh_home.write(
        "config",
        format!(
            "Host bitbucket.org-work\n\
             \tHostName 127.0.0.1\n\
             \tPort {}\n\
             \tUser rcgit-alias\n",
            remoted.server.port()
        )
        .as_bytes(),
    );
    remoted.clone.git(&[
        "remote",
        "set-url",
        "origin",
        &format!("ssh://bitbucket.org-work{}", remoted.bare_path.display()),
    ]);
    advance_origin(&remoted.origin, &remoted.bare_path);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect("alias fetch");
    assert_eq!(outcome.updated_refs.len(), 1);
    assert_eq!(
        remoted.server.last_user().as_deref(),
        Some("rcgit-alias"),
        "the connection authenticates as the ssh-config User"
    );
}

/// `HostKeyAlias` replaces the dialed address as the `known_hosts` key and is looked up
/// bare, as OpenSSH does: the only entry is under the alias, so the fetch needs no approval.
#[test]
fn ssh_host_key_alias() {
    let remoted = ssh_remoted();
    let pinned = Fixture::empty_dir();
    remoted.server.write_known_hosts(pinned.path());
    let pinned = std::fs::read_to_string(pinned.path().join("known_hosts")).expect("read");
    let key = pinned
        .split_once(' ')
        .map(|(_, key)| key)
        .expect("a known_hosts line has a key");
    remoted
        .ssh_home
        .write("known_hosts", format!("git.corp.example {key}").as_bytes());
    remoted.ssh_home.write(
        "config",
        format!(
            "Host gitbox\n\
             \tHostName 127.0.0.1\n\
             \tPort {}\n\
             \tHostKeyAlias Git.Corp.Example\n",
            remoted.server.port()
        )
        .as_bytes(),
    );
    remoted.clone.git(&[
        "remote",
        "set-url",
        "origin",
        &format!("ssh://git@gitbox{}", remoted.bare_path.display()),
    ]);
    advance_origin(&remoted.origin, &remoted.bare_path);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect("a host pinned under its HostKeyAlias is known");
    assert_eq!(outcome.updated_refs.len(), 1);
}

/// A private agent holding the server's unauthorized key, then its authorized one, and
/// the directories holding them, each as `id_ed25519`: authorized first, unauthorized second.
fn agent_with_both_keys(server: &rc_git_testkit::SshGitServer) -> (SshAgent, Fixture, Fixture) {
    let authorized = Fixture::empty_dir();
    let unauthorized = Fixture::empty_dir();
    server.write_client_key(authorized.path());
    server.write_unauthorized_client_key(unauthorized.path());
    let agent = SshAgent::start();
    agent.add(&unauthorized.path().join("id_ed25519"));
    agent.add(&authorized.path().join("id_ed25519"));
    (agent, authorized, unauthorized)
}

/// The field failure: the agent lists another account's key first. The agent key that
/// matches the configured identity file is offered first, and the file is not offered
/// again.
#[test]
fn ssh_agent_key_matching_the_identity_file_is_offered_first() {
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    let (agent, authorized, _unauthorized) = agent_with_both_keys(&remoted.server);
    remoted.ssh_home.write(
        "config",
        format!(
            "Host *\n  IdentityAgent \"{}\"\n  IdentityFile \"{}\"\n",
            agent.socket().display(),
            authorized.path().join("id_ed25519").display()
        )
        .as_bytes(),
    );
    advance_origin(&remoted.origin, &remoted.bare_path);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect("the agent key matching the identity file authenticates");
    assert_eq!(outcome.updated_refs.len(), 1);
    assert_eq!(
        remoted.server.offered_keys(),
        vec![remoted.server.client_key_fingerprint()],
        "only the agent key matching IdentityFile is offered"
    );
}

/// Only `<key>.pub` is on disk, with `IdentitiesOnly yes`: the public key selects the
/// agent's copy of the private key, and the agent's other key is never offered.
#[test]
fn ssh_agent_pub_identity_file_selects_the_agent_key() {
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    let (agent, _authorized, _unauthorized) = agent_with_both_keys(&remoted.server);
    let public = Fixture::empty_dir();
    remoted.server.write_client_public_key(public.path());
    remoted.ssh_home.write(
        "config",
        format!(
            "Host *\n  IdentityAgent \"{}\"\n  IdentityFile \"{}\"\n  IdentitiesOnly yes\n",
            agent.socket().display(),
            public.path().join("id_ed25519.pub").display()
        )
        .as_bytes(),
    );
    advance_origin(&remoted.origin, &remoted.bare_path);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect("the agent key the .pub file names authenticates");
    assert_eq!(outcome.updated_refs.len(), 1);
    assert_eq!(
        remoted.server.offered_keys(),
        vec![remoted.server.client_key_fingerprint()]
    );
}

// --- local and URL upstreams ---

/// A standard fixture checked out on `topic`, which tracks the local branch `main`
/// (`branch.topic.remote = .`), as `git checkout -b topic --track main` sets it up.
fn tracking_local_main() -> Fixture {
    let fixture = Fixture::standard();
    fixture.git(&["checkout", "-q", "-b", "topic", "--track", "main"]);
    assert_eq!(fixture.git_output(&["config", "branch.topic.remote"]), ".");
    fixture
}

#[test]
fn upstream_status_reports_a_local_upstream() {
    let fixture = tracking_local_main();
    fixture.write("local.txt", b"local\n");
    fixture.commit("Local work", "2024-03-02T00:00:00+0000");

    let status = upstream_status(fixture.path()).expect("status");
    assert!(status.has_upstream);
    assert_eq!(status.branch, "topic");
    assert_eq!(status.remote, ".");
    assert_eq!(status.upstream_ref, "refs/heads/main");
    assert_eq!(status.url, "");
    assert_eq!(status.host, "");
    assert!(!status.uses_ssh);
    assert_eq!((status.ahead, status.behind), (1, 0));
}

#[test]
fn fetch_with_a_local_upstream_is_a_no_op() {
    let fixture = tracking_local_main();
    let refs_before = fixture.git_output(&["for-each-ref"]);

    let outcome = fetch_upstream(
        fixture.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("fetch");
    assert_eq!(outcome.remote, ".");
    assert!(outcome.updated_refs.is_empty());
    assert_eq!(fixture.git_output(&["for-each-ref"]), refs_before);
}

#[test]
fn pull_fast_forwards_from_a_local_upstream() {
    let fixture = tracking_local_main();
    fixture.git(&["checkout", "-q", "main"]);
    fixture.write("news.txt", b"news\n");
    fixture.commit("News on main", "2024-03-02T00:00:00+0000");
    fixture.git(&["checkout", "-q", "topic"]);
    let before = fixture.rev_parse("topic");

    let outcome = pull_fast_forward(fixture.path(), &Policy::default()).expect("pull");
    assert_eq!(outcome.branch, "topic");
    assert!(!outcome.already_up_to_date);
    assert!(!outcome.unpublished);
    assert_eq!(outcome.old.to_hex(), before);
    assert_eq!(outcome.new.to_hex(), fixture.rev_parse("main"));
    assert_eq!(outcome.files_changed, 1);
    assert_eq!(fixture.rev_parse("topic"), fixture.rev_parse("main"));
    assert_eq!(
        std::fs::read(fixture.path().join("news.txt")).expect("news.txt"),
        b"news\n"
    );
    assert_eq!(fixture.git_output(&["status", "--porcelain"]), "");
}

#[test]
fn push_refuses_a_local_upstream() {
    let fixture = tracking_local_main();
    fixture.write("local.txt", b"local\n");
    fixture.commit("Local work", "2024-03-02T00:00:00+0000");
    let main_before = fixture.rev_parse("main");

    let err = push_upstream(
        fixture.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("push into a local branch");
    assert!(matches!(err, ActionError::Git { .. }), "{err}");
    let message = err.to_string();
    assert!(message.contains("refs/heads/main"), "{message}");
    assert!(
        message.contains("does not push into a local branch"),
        "{message}"
    );
    assert_eq!(fixture.rev_parse("main"), main_before);
}

/// A standard fixture whose `main` names a URL, not a remote, in `branch.main.remote`.
fn tracking_a_url() -> Fixture {
    let fixture = Fixture::standard();
    fixture.git(&[
        "config",
        "branch.main.remote",
        "https://user:secret@example.invalid/x.git",
    ]);
    fixture.git(&["config", "branch.main.merge", "refs/heads/main"]);
    fixture
}

#[test]
fn upstream_status_reports_a_url_upstream() {
    let fixture = tracking_a_url();
    let status = upstream_status(fixture.path()).expect("status");
    assert!(status.has_upstream);
    assert_eq!(status.remote, "https://example.invalid/x.git");
    assert_eq!(status.url, "https://example.invalid/x.git");
    assert_eq!(status.host, "example.invalid");
    assert_eq!(status.upstream_ref, "");
    assert_eq!((status.ahead, status.behind), (0, 0));
}

#[test]
fn fetch_and_pull_refuse_a_url_upstream_by_name() {
    let fixture = tracking_a_url();
    let err = fetch_upstream(
        fixture.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("fetch from a bare URL");
    let message = err.to_string();
    assert!(matches!(err, ActionError::Git { .. }), "{message}");
    assert!(message.contains("named remote"), "{message}");
    assert!(message.contains("git remote add"), "{message}");
    assert!(!message.contains("secret"), "{message}");

    let err = pull_fast_forward(fixture.path(), &Policy::default()).expect_err("pull");
    let message = err.to_string();
    assert!(matches!(err, ActionError::Git { .. }), "{message}");
    assert!(
        message.starts_with("pull 'https://example.invalid/x.git': "),
        "{message}"
    );
    assert!(message.contains("named remote"), "{message}");
    assert!(message.contains("git remote add"), "{message}");
    assert!(!message.contains("secret"), "{message}");
}

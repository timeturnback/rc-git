//! Fetch and upstream-status against a real smart-HTTP server (`git http-backend`
//! behind the test kit's loopback listener). The git CLI builds the fixtures; the
//! guard is the only code under test that touches the network.

use std::sync::atomic::AtomicBool;

use rc_git_guard::Policy;
use rc_git_guard::actions::{ActionError, Credentials, fetch_upstream, upstream_status};
use rc_git_testkit::Fixture;

/// An origin repository, a bare copy served over HTTP, and a local clone of it.
struct Remoted {
    origin: Fixture,
    server: rc_git_testkit::HttpGitServer,
    clone: Fixture,
}

fn remoted() -> Remoted {
    let origin = Fixture::standard();
    origin.git(&["clone", "--bare", "--quiet", ".", "remote.git"]);
    let server = rc_git_testkit::HttpGitServer::serve(&origin.path().join("remote.git"));
    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", &server.url(), "."]);
    Remoted {
        origin,
        server,
        clone,
    }
}

/// Adds one commit to the served bare repository, past what the clone has.
fn advance_origin(remoted: &Remoted) {
    remoted.origin.write("news.txt", b"news\n");
    remoted
        .origin
        .commit("News on origin", "2024-03-01T00:00:00+0000");
    remoted
        .origin
        .git(&["push", "--quiet", "remote.git", "main"]);
}

#[test]
fn upstream_status_reports_no_upstream_without_a_remote() {
    let fixture = Fixture::standard();
    let status = upstream_status(fixture.path()).expect("status");
    assert!(!status.has_upstream);
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
fn fetch_updates_the_tracking_ref_and_reports_it() {
    let remoted = remoted();
    advance_origin(&remoted);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        None,
        &AtomicBool::new(false),
    )
    .expect("fetch");
    assert_eq!(outcome.remote, "origin");
    let served_tip = remoted
        .origin
        .git_output(&["--git-dir", "remote.git", "rev-parse", "main"]);
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
fn fetch_with_nothing_new_reports_no_updates() {
    let remoted = remoted();
    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        None,
        &AtomicBool::new(false),
    )
    .expect("fetch");
    assert!(
        outcome.updated_refs.is_empty(),
        "{:?}",
        outcome.updated_refs
    );
}

#[test]
fn fetch_against_an_auth_server_requires_matching_credentials() {
    let origin = Fixture::standard();
    origin.git(&["clone", "--bare", "--quiet", ".", "remote.git"]);
    let server = rc_git_testkit::HttpGitServer::serve_with_auth(
        &origin.path().join("remote.git"),
        "user",
        "sekret",
    );
    let clone = Fixture::empty_dir();
    let with_creds = server.url().replace("http://", "http://user:sekret@");
    clone.git(&[
        "-c",
        "credential.helper=",
        "clone",
        "--quiet",
        &with_creds,
        ".",
    ]);
    // The cloned config keeps the userinfo URL; point it at the bare URL so the
    // guard must supply credentials itself.
    clone.git(&["remote", "set-url", "origin", &server.url()]);

    let anonymous = fetch_upstream(
        clone.path(),
        &Policy::default(),
        None,
        &AtomicBool::new(false),
    );
    assert!(
        matches!(anonymous, Err(ActionError::AuthRequired { .. })),
        "{anonymous:?}"
    );

    let wrong = fetch_upstream(
        clone.path(),
        &Policy::default(),
        Some(&Credentials {
            username: "user".to_owned(),
            token: "wrong".to_owned(),
        }),
        &AtomicBool::new(false),
    );
    assert!(
        matches!(wrong, Err(ActionError::AuthRequired { .. })),
        "{wrong:?}"
    );

    origin.write("news.txt", b"news\n");
    origin.commit("News on origin", "2024-03-01T00:00:00+0000");
    origin.git(&["push", "--quiet", "remote.git", "main"]);
    let outcome = fetch_upstream(
        clone.path(),
        &Policy::default(),
        Some(&Credentials {
            username: "user".to_owned(),
            token: "sekret".to_owned(),
        }),
        &AtomicBool::new(false),
    )
    .expect("authenticated fetch");
    assert!(!outcome.updated_refs.is_empty());
}

#[test]
fn non_https_remotes_are_refused_by_scheme() {
    let fixture = Fixture::standard();
    fixture.git(&["remote", "add", "origin", "ssh://git@example.com/x.git"]);
    let err = fetch_upstream(
        fixture.path(),
        &Policy::default(),
        None,
        &AtomicBool::new(false),
    )
    .expect_err("ssh refused");
    assert!(
        matches!(&err, ActionError::UnsupportedRemote { scheme, .. } if scheme == "ssh"),
        "{err}"
    );

    fixture.git(&["remote", "set-url", "origin", "http://example.com/x.git"]);
    let err = fetch_upstream(
        fixture.path(),
        &Policy::default(),
        None,
        &AtomicBool::new(false),
    )
    .expect_err("non-loopback http refused");
    assert!(
        matches!(&err, ActionError::UnsupportedRemote { scheme, .. } if scheme == "http"),
        "{err}"
    );
}

#[test]
fn fetch_without_a_remote_reports_no_upstream() {
    let fixture = Fixture::standard();
    let err = fetch_upstream(
        fixture.path(),
        &Policy::default(),
        None,
        &AtomicBool::new(false),
    )
    .expect_err("no remote");
    assert!(matches!(err, ActionError::NoUpstream { .. }), "{err}");
}

#[test]
fn a_preset_cancel_flag_stops_the_fetch_before_it_connects() {
    let remoted = remoted();
    let err = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        None,
        &AtomicBool::new(true),
    )
    .expect_err("cancelled");
    assert!(
        matches!(err, ActionError::Cancelled { op: "fetch" }),
        "{err}"
    );
    drop(remoted.server);
}

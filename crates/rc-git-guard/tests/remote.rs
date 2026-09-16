#![expect(
    clippy::expect_used,
    reason = "test helpers outside #[test] functions may panic"
)]
//! Fetch and upstream-status against a real smart-HTTP server (`git http-backend`
//! behind the test kit's loopback listener). The git CLI builds the fixtures; the
//! guard is the only code under test that touches the network.

use std::sync::atomic::AtomicBool;

use rc_git_guard::Policy;
use rc_git_guard::actions::{ActionError, Credentials, fetch_upstream, upstream_status};
use rc_git_testkit::Fixture;

/// An origin repository, a bare copy served over HTTP from a sibling directory
/// (never inside the origin's working tree), and a local clone of it.
struct Remoted {
    origin: Fixture,
    bare: Fixture,
    server: rc_git_testkit::HttpGitServer,
    clone: Fixture,
}

/// Bare-clones `origin` into its own temp dir and returns it with the serving path.
fn bare_copy(origin: &Fixture) -> (Fixture, std::path::PathBuf) {
    let bare = Fixture::empty_dir();
    let path = bare.path().join("remote.git");
    origin.git(&["clone", "--bare", "--quiet", ".", &path.to_string_lossy()]);
    (bare, path)
}

fn remoted() -> Remoted {
    let origin = Fixture::standard();
    let (bare, bare_path) = bare_copy(&origin);
    let server = rc_git_testkit::HttpGitServer::serve(&bare_path);
    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", &server.url(), "."]);
    Remoted {
        origin,
        bare,
        server,
        clone,
    }
}

/// `git rev-parse main` in the served bare repository.
fn bare_tip(remoted: &Remoted) -> String {
    remoted
        .bare
        .git_output(&["--git-dir", "remote.git", "rev-parse", "main"])
}

/// Adds one commit to the served bare repository, past what the clone has.
fn advance_origin(remoted: &Remoted) {
    remoted.origin.write("news.txt", b"news\n");
    remoted
        .origin
        .commit("News on origin", "2024-03-01T00:00:00+0000");
    push_origin(remoted);
}

/// Pushes the origin's `main` into the served bare repository.
fn push_origin(remoted: &Remoted) {
    let target = remoted.bare.path().join("remote.git");
    remoted
        .origin
        .git(&["push", "--quiet", &target.to_string_lossy(), "main"]);
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
    let served_tip = bare_tip(&remoted);
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
    let (bare, bare_path) = bare_copy(&origin);
    let server = rc_git_testkit::HttpGitServer::serve_with_auth(&bare_path, "user", "sekret");
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
    origin.git(&[
        "push",
        "--quiet",
        &bare.path().join("remote.git").to_string_lossy(),
        "main",
    ]);
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

// --- fast-forward pull ---

use rc_git_guard::actions::pull_fast_forward;

/// Fetches the clone so its tracking ref is current.
fn fetch_clone(remoted: &Remoted) {
    fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        None,
        &AtomicBool::new(false),
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
    push_origin(&remoted);
    fetch_clone(&remoted);

    let outcome = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect("pull");
    assert_eq!(outcome.branch, "main");
    assert_ne!(outcome.old, outcome.new);
    assert_eq!(outcome.files_changed, 3);
    assert_eq!(remoted.clone.rev_parse("main"), bare_tip(&remoted));
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

#[test]
fn pull_with_nothing_new_is_a_no_op() {
    let remoted = remoted();
    let outcome = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect("pull");
    assert_eq!(outcome.old, outcome.new);
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
    assert_eq!(remoted.clone.rev_parse("main"), before);
}

#[test]
fn pull_refuses_when_both_sides_moved() {
    let remoted = remoted();
    advance_origin(&remoted);
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
    push_origin(&remoted);
    fetch_clone(&remoted);
    remoted
        .clone
        .write("README.md", b"# fixture\n\nlocal edit\n");
    let before_head = remoted.clone.rev_parse("main");

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("dirty");
    assert!(
        matches!(&err, ActionError::DirtyPath { path } if path == "README.md"),
        "{err}"
    );
    assert_eq!(remoted.clone.rev_parse("main"), before_head);
    assert_eq!(
        std::fs::read(remoted.clone.path().join("README.md")).expect("README.md"),
        b"# fixture\n\nlocal edit\n",
        "the local edit must survive"
    );
}

#[test]
fn pull_refuses_an_untracked_file_in_the_way() {
    let remoted = remoted();
    advance_origin(&remoted); // adds news.txt upstream
    fetch_clone(&remoted);
    remoted.clone.write("news.txt", b"mine, not yours\n");

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("blocked");
    assert!(
        matches!(&err, ActionError::DirtyPath { path } if path == "news.txt"),
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
    push_origin(&remoted);
    fetch_clone(&remoted);
    remoted
        .clone
        .write("README.md", b"# fixture\n\nstaged edit\n");
    remoted.clone.git(&["add", "README.md"]);

    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("staged");
    assert!(
        matches!(&err, ActionError::DirtyPath { path } if path == "README.md"),
        "{err}"
    );
}

#[test]
fn pull_refuses_a_detached_head() {
    let remoted = remoted();
    remoted.clone.git(&["checkout", "-q", "--detach"]);
    let err = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect_err("detached");
    assert!(matches!(err, ActionError::DetachedHead { .. }), "{err}");
}

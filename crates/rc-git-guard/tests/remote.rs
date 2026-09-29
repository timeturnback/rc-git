#![expect(
    clippy::expect_used,
    reason = "test helpers outside #[test] functions may panic"
)]
//! Fetch and upstream-status against a real smart-HTTP server (`git http-backend`
//! behind the test kit's loopback listener). The git CLI builds the fixtures; the
//! guard is the only code under test that touches the network.

use rc_git_guard::Policy;
use rc_git_guard::actions::{
    ActionError, Cancel, Credentials, RemoteAuth, fetch_upstream, upstream_status,
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
    bare.git(&[
        "--git-dir",
        "remote.git",
        "config",
        "http.receivepack",
        "true",
    ]);
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
    advance_origin(&remoted);
    fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect("fetch");

    let status = upstream_status(remoted.clone.path()).expect("status");
    assert!(status.has_upstream);
    assert_eq!(status.remote, "origin");
    assert_eq!(status.upstream_ref, "refs/remotes/origin/main");
    assert_eq!((status.ahead, status.behind), (1, 1));
}

#[test]
fn fetch_updates_the_tracking_ref_and_reports_it() {
    let remoted = remoted();
    advance_origin(&remoted);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
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
        RemoteAuth::default(),
        &Cancel::new(),
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
        auth(&Credentials {
            username: "user".to_owned(),
            token: "sekret".to_owned(),
        }),
        &Cancel::new(),
    )
    .expect("authenticated fetch");
    assert!(!outcome.updated_refs.is_empty());
}

#[test]
fn unsupported_remote_schemes_are_refused() {
    let fixture = Fixture::standard();
    fixture.git(&["remote", "add", "origin", "git://example.com/x.git"]);
    let err = fetch_upstream(
        fixture.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
    )
    .expect_err("git protocol refused");
    assert!(
        matches!(&err, ActionError::UnsupportedRemote { scheme, .. } if scheme == "git"),
        "{err}"
    );

    fixture.git(&["remote", "set-url", "origin", "http://example.com/x.git"]);
    let err = fetch_upstream(
        fixture.path(),
        &Policy::default(),
        RemoteAuth::default(),
        &Cancel::new(),
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
        RemoteAuth::default(),
        &Cancel::new(),
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
        RemoteAuth::default(),
        &cancelled(),
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
fn pull_fast_forwards_from_origin_when_the_branch_names_no_upstream() {
    let remoted = remoted();
    untrack(&remoted);
    advance_origin(&remoted);
    fetch_clone(&remoted);

    let outcome = pull_fast_forward(remoted.clone.path(), &Policy::default()).expect("pull");
    assert_eq!(outcome.files_changed, 1);
    assert_eq!(remoted.clone.rev_parse("main"), bare_tip(&remoted));
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

// --- push ---

use rc_git_guard::actions::push_upstream;

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
    assert_eq!(outcome.new.to_string(), tip);
    assert_eq!(
        bare_tip(&remoted),
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
    assert_eq!(
        outcome.pushed_commits, 0,
        "every object already exists remotely"
    );
    assert_eq!(
        remoted
            .bare
            .git_output(&["--git-dir", "remote.git", "rev-parse", "topic"]),
        remoted.clone.rev_parse("HEAD")
    );
}

#[test]
fn push_is_rejected_when_the_remote_moved() {
    let remoted = remoted();
    advance_origin(&remoted);
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
    assert_ne!(bare_tip(&remoted), remoted.clone.rev_parse("HEAD"));
}

#[test]
fn push_against_an_auth_server_requires_credentials() {
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
    clone.git(&["remote", "set-url", "origin", &server.url()]);
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
    assert_eq!(
        bare.git_output(&["--git-dir", "remote.git", "rev-parse", "main"]),
        clone.rev_parse("HEAD")
    );
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
    assert!(message.contains("refused"), "{message}");
    assert_ne!(bare_tip(&remoted), remoted.clone.rev_parse("HEAD"));
}

#[test]
fn a_preset_cancel_flag_stops_the_push_before_it_connects() {
    let remoted = remoted();
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
}

// ---- ssh ----------------------------------------------------------------------------

/// An origin repository, a bare copy served over loopback ssh, a local clone whose
/// `origin` points at the ssh URL, and a fixture `~/.ssh`-style directory.
struct SshRemoted {
    origin: Fixture,
    bare: Fixture,
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
    let (bare, bare_path) = bare_copy(&origin);
    let server = rc_git_testkit::SshGitServer::serve(&bare_path);
    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", &bare_path.to_string_lossy(), "."]);
    clone.git(&["remote", "set-url", "origin", &server.url()]);
    let ssh_home = Fixture::empty_dir();
    server.write_client_key(ssh_home.path());
    SshRemoted {
        origin,
        bare,
        server,
        clone,
        ssh_home,
    }
}

/// Adds one commit to the ssh-served bare repository, past what the clone has.
fn advance_ssh_origin(remoted: &SshRemoted) {
    remoted.origin.write("news.txt", b"news\n");
    remoted
        .origin
        .commit("News on origin", "2024-03-01T00:00:00+0000");
    let target = remoted.bare.path().join("remote.git");
    remoted
        .origin
        .git(&["push", "--quiet", &target.to_string_lossy(), "main"]);
}

#[test]
fn ssh_fetch_updates_the_tracking_ref() {
    let remoted = ssh_remoted();
    remoted.server.write_known_hosts(remoted.ssh_home.path());
    advance_ssh_origin(&remoted);

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
    let tip = remoted
        .bare
        .git_output(&["--git-dir", "remote.git", "rev-parse", "main"]);
    assert_eq!(tip, remoted.clone.rev_parse("HEAD"));
    let status = upstream_status(remoted.clone.path()).expect("status");
    assert_eq!((status.ahead, status.behind), (0, 0));
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
             \tUser git\n",
            remoted.server.port()
        )
        .as_bytes(),
    );
    remoted.clone.git(&[
        "remote",
        "set-url",
        "origin",
        &format!(
            "ssh://bitbucket.org-work{}",
            remoted.bare.path().join("remote.git").display()
        ),
    ]);
    advance_ssh_origin(&remoted);

    let outcome = fetch_upstream(
        remoted.clone.path(),
        &Policy::default(),
        remoted.auth(),
        &Cancel::new(),
    )
    .expect("alias fetch");
    assert_eq!(outcome.updated_refs.len(), 1);
}

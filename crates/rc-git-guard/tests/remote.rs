#![expect(
    clippy::expect_used,
    reason = "test helpers outside #[test] functions may panic"
)]
//! Fetch and upstream-status against a real smart-HTTP server (`git http-backend`
//! behind the test kit's loopback listener). The git CLI builds the fixtures; the
//! guard is the only code under test that touches the network.

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
use rc_git_testkit::Fixture;

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
    assert!(message.contains("refused"), "{message}");
    assert_ne!(
        bare_rev(&remoted.bare, "main"),
        remoted.clone.rev_parse("HEAD")
    );
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
}

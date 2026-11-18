#![expect(
    clippy::expect_used,
    reason = "test helpers outside #[test] functions may panic"
)]
//! The pull request head fetch against a real loopback smart-HTTP server whose bare repository
//! holds `refs/pull/<n>/head` refs, as GitHub's do: one from a branch of the repository, one
//! from a fork that no branch holds, and a head force-pushed over another. The git CLI builds
//! the fixtures; the guard is the only code under test that touches the network.

use std::path::PathBuf;

use rc_git_core::{Oid, RefKind, Repository};
use rc_git_guard::Policy;
use rc_git_guard::actions::{
    ActionError, Cancel, Credentials, PullHeadFetch, PullHeadRequest, RemoteAuth,
    fetch_pull_request_head, pull_head_ref,
};
use rc_git_testkit::{Fixture, HttpGitServer};

/// An origin, its bare copy served over HTTP (from a sibling directory), a clone of it made
/// before any pull request existed, and the bare copy's pull request heads: `refs/pull/7/head`
/// is branch `pr-seven` of the origin, tagged `pr-tag`; `refs/pull/8/head` is a commit made in
/// a fork, which no branch of the served repository holds.
struct Served {
    origin: Fixture,
    bare: Fixture,
    bare_path: PathBuf,
    server: HttpGitServer,
    clone: Fixture,
}

impl Served {
    fn bare_rev(&self, rev: &str) -> String {
        self.bare
            .git_output(&["--git-dir", "remote.git", "rev-parse", rev])
    }

    fn head(&self, number: u64) -> Oid {
        Oid::from_hex(&self.bare_rev(&format!("refs/pull/{number}/head"))).expect("an id")
    }

    /// Every ref of the clone with its target, one per line.
    fn clone_refs(&self) -> String {
        self.clone
            .git_output(&["for-each-ref", "--format=%(refname) %(objectname)"])
    }

    fn has_object(&self, oid: Oid) -> bool {
        Repository::open(self.clone.path())
            .expect("open the clone")
            .has_object(oid)
    }

    fn bare_target(&self) -> String {
        self.bare_path.to_string_lossy().into_owned()
    }
}

fn served_with(server: impl FnOnce(&std::path::Path) -> HttpGitServer) -> Served {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    let server = server(&bare_path);
    let clone = Fixture::empty_dir();
    clone.git(&[
        "-c",
        "credential.helper=",
        "clone",
        "--quiet",
        &bare_path.to_string_lossy(),
        ".",
    ]);
    clone.git(&["remote", "set-url", "origin", &server.url()]);
    let served = Served {
        origin,
        bare,
        bare_path,
        server,
        clone,
    };
    add_pull_requests(&served);
    served
}

fn served() -> Served {
    served_with(HttpGitServer::serve)
}

fn add_pull_requests(served: &Served) {
    let (origin, bare) = (&served.origin, served.bare_target());
    origin.git(&["checkout", "-q", "-b", "pr-seven"]);
    origin.write("feature.txt", b"feature\n");
    origin.commit("Feature work", "2024-03-03T00:00:00+0000");
    origin.git(&["tag", "pr-tag"]);
    origin.git(&[
        "push",
        "--quiet",
        &bare,
        "pr-seven:refs/pull/7/head",
        "pr-tag",
    ]);
    origin.git(&["checkout", "-q", "main"]);

    let fork = Fixture::empty_dir();
    fork.git(&["clone", "--quiet", &bare, "."]);
    fork.write("fork.txt", b"fork\n");
    fork.commit("Fork work", "2024-03-04T00:00:00+0000");
    fork.git(&["push", "--quiet", &bare, "HEAD:refs/pull/8/head"]);
}

fn fetch(served: &Served, number: u64, expected: Oid) -> Result<PullHeadFetch, ActionError> {
    fetch_pull_request_head(
        served.clone.path(),
        &Policy::default(),
        PullHeadRequest { number, expected },
        RemoteAuth::default(),
        &Cancel::new(),
    )
}

#[test]
fn a_pull_request_head_lands_in_its_private_ref_and_nothing_else_moves() {
    let served = served();
    let head = served.head(7);
    let before = served.clone_refs();
    assert!(!served.has_object(head));

    let outcome = fetch(&served, 7, head).expect("fetch pull request 7");
    assert_eq!(
        outcome,
        PullHeadFetch::Fetched {
            remote: "origin".to_owned(),
            ref_name: "refs/rc-git/pr/7/head".to_owned(),
            updated: true,
        }
    );
    assert_eq!(
        served.clone.rev_parse("refs/rc-git/pr/7/head"),
        head.to_hex()
    );
    assert!(served.has_object(head));
    let expected = format!("{before}\nrefs/rc-git/pr/7/head {head}");
    let mut expected: Vec<&str> = expected.lines().collect();
    expected.sort_unstable();
    assert_eq!(
        served.clone_refs().lines().collect::<Vec<_>>(),
        expected,
        "only the private ref is new: no tag, no other pull request, no tracking ref"
    );
    assert!(!served.clone.git_dir().join("FETCH_HEAD").exists());
    assert!(
        !served.has_object(served.head(8)),
        "pull request 8 stays on the server"
    );

    let again = fetch(&served, 7, head).expect("fetch again");
    assert!(
        matches!(again, PullHeadFetch::Fetched { updated: false, .. }),
        "{again:?}"
    );
}

#[test]
fn a_fork_head_no_branch_holds_is_fetched_by_its_number() {
    let served = served();
    let head = served.head(8);
    let outcome = fetch(&served, 8, head).expect("fetch pull request 8");
    assert!(
        matches!(&outcome, PullHeadFetch::Fetched { ref_name, updated: true, .. }
            if *ref_name == pull_head_ref(8)),
        "{outcome:?}"
    );
    assert_eq!(served.clone.rev_parse(&pull_head_ref(8)), head.to_hex());
}

#[test]
fn a_force_pushed_head_moves_the_private_ref_to_the_new_commit() {
    let served = served();
    fetch(&served, 7, served.head(7)).expect("first fetch");
    let origin = &served.origin;
    origin.git(&["checkout", "-q", "-b", "rewritten", "main"]);
    origin.write("feature.txt", b"rewritten\n");
    origin.commit("Feature work, rewritten", "2024-03-05T00:00:00+0000");
    origin.git(&[
        "push",
        "--quiet",
        "--force",
        &served.bare_target(),
        "rewritten:refs/pull/7/head",
    ]);
    let rewritten = served.head(7);
    assert_ne!(
        served.clone.rev_parse(&pull_head_ref(7)),
        rewritten.to_hex()
    );

    let outcome = fetch(&served, 7, rewritten).expect("fetch the rewritten head");
    assert!(
        matches!(outcome, PullHeadFetch::Fetched { updated: true, .. }),
        "{outcome:?}"
    );
    assert_eq!(
        served.clone.rev_parse(&pull_head_ref(7)),
        rewritten.to_hex()
    );
}

#[test]
fn a_head_that_moved_since_it_was_read_writes_nothing() {
    let served = served();
    let head = served.head(7);
    let seen = Oid::from_hex(&served.bare_rev("main")).expect("an id");
    let before = served.clone_refs();

    let outcome = fetch(&served, 7, seen).expect("the fetch answers");
    assert_eq!(
        outcome,
        PullHeadFetch::Moved {
            remote: "origin".to_owned(),
            found: head,
        }
    );
    assert_eq!(served.clone_refs(), before, "no ref written");
    assert!(!served.has_object(head), "no object received");
}

#[test]
fn a_pull_request_the_remote_lacks_is_named_and_writes_nothing() {
    let served = served();
    let before = served.clone_refs();
    let err = fetch(&served, 9, served.head(7)).expect_err("no pull request 9");
    let message = err.to_string();
    assert!(
        matches!(
            err,
            ActionError::Git {
                op: "fetch pull request",
                ..
            }
        ),
        "{message}"
    );
    assert!(message.contains("refs/pull/9/head"), "{message}");
    assert_eq!(served.clone_refs(), before);
}

#[test]
fn number_zero_and_a_preset_cancel_never_reach_the_server() {
    let served = served();
    let head = served.head(7);
    let before = served.server.requests();
    let err = fetch(&served, 0, head).expect_err("0 is no pull request");
    assert!(matches!(err, ActionError::Git { .. }), "{err}");

    let cancel = Cancel::new();
    cancel.signal();
    let err = fetch_pull_request_head(
        served.clone.path(),
        &Policy::default(),
        PullHeadRequest {
            number: 7,
            expected: head,
        },
        RemoteAuth::default(),
        &cancel,
    )
    .expect_err("cancelled");
    assert!(
        matches!(
            err,
            ActionError::Cancelled {
                op: "fetch pull request"
            }
        ),
        "{err}"
    );
    assert_eq!(served.server.requests(), before);
}

#[test]
fn the_private_ref_never_lists_as_a_branch_or_a_tag() {
    let served = served();
    fetch(&served, 7, served.head(7)).expect("fetch");
    let listing = Repository::open(served.clone.path())
        .expect("open")
        .refs()
        .expect("list refs");
    let private: Vec<_> = listing
        .refs
        .iter()
        .filter(|entry| entry.full_name.starts_with("refs/rc-git/pr/"))
        .collect();
    assert_eq!(private.len(), 1, "{listing:?}");
    assert_eq!(private[0].kind, RefKind::Other);
    for entry in &listing.refs {
        let is_branch_or_tag = matches!(
            entry.kind,
            RefKind::LocalBranch | RefKind::RemoteBranch | RefKind::Tag
        );
        assert!(
            !(is_branch_or_tag && entry.short_name.contains("pr/7")),
            "{entry:?}"
        );
    }
}

#[test]
fn git_transport_credentials_answer_only_the_remote_host() {
    let served = served_with(|bare| HttpGitServer::serve_with_auth(bare, "user", "sekret"));
    let head = served.head(7);
    let creds = Credentials {
        username: "user".to_owned(),
        token: "sekret".to_owned(),
    };
    let attempt = |auth: RemoteAuth<'_>| {
        fetch_pull_request_head(
            served.clone.path(),
            &Policy::default(),
            PullHeadRequest {
                number: 7,
                expected: head,
            },
            auth,
            &Cancel::new(),
        )
    };
    let anonymous = attempt(RemoteAuth::default());
    assert!(
        matches!(anonymous, Err(ActionError::AuthRequired { .. })),
        "{anonymous:?}"
    );
    let elsewhere = attempt(RemoteAuth {
        creds: Some(&creds),
        host: Some("elsewhere.example"),
        ..RemoteAuth::default()
    });
    assert!(
        matches!(elsewhere, Err(ActionError::AuthRequired { .. })),
        "{elsewhere:?}"
    );
    assert!(!served.has_object(head));
    let own = attempt(RemoteAuth {
        creds: Some(&creds),
        ..RemoteAuth::default()
    });
    assert!(matches!(own, Ok(PullHeadFetch::Fetched { .. })), "{own:?}");
}

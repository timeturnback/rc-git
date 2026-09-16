//! The HTTP server proves itself against the real git CLI: clone from it, push to it.

use rc_git_testkit::{Fixture, HttpGitServer};

#[test]
fn a_served_repository_clones_and_accepts_a_push() {
    let origin = Fixture::standard();
    origin.git(&["clone", "--bare", "--quiet", ".", "remote.git"]);
    origin.git(&[
        "--git-dir",
        "remote.git",
        "config",
        "http.receivepack",
        "true",
    ]);
    let bare = origin.path().join("remote.git");
    let server = HttpGitServer::serve(&bare);

    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", &server.url(), "."]);
    assert_eq!(clone.rev_parse("HEAD"), origin.rev_parse("HEAD"));

    clone.write("pushed.txt", b"pushed\n");
    clone.commit("Pushed over HTTP", "2024-02-01T00:00:00+0000");
    clone.git(&["-c", "http.receivepack=true", "push", "--quiet"]);

    let served_tip = origin.git_output(&["--git-dir", "remote.git", "rev-parse", "main"]);
    assert_eq!(clone.rev_parse("HEAD"), served_tip);
}

#[test]
fn missing_or_wrong_credentials_are_rejected_and_correct_ones_accepted() {
    let origin = Fixture::standard();
    let server = origin.serve_http_with_auth("user", "sekret");

    let denied = Fixture::empty_dir();
    let output = denied.git_expect_failure(&[
        "-c",
        "credential.helper=",
        "clone",
        "--quiet",
        &server.url(),
        ".",
    ]);
    // A 401 makes git ask for credentials; with prompts disabled that surfaces as
    // "could not read Username".
    assert!(
        output.contains("401")
            || output.contains("Authentication")
            || output.contains("could not read Username"),
        "expected an auth failure, got: {output}"
    );

    let allowed = Fixture::empty_dir();
    let with_creds = server.url().replace("http://", "http://user:sekret@");
    allowed.git(&[
        "-c",
        "credential.helper=",
        "clone",
        "--quiet",
        &with_creds,
        ".",
    ]);
    assert_eq!(allowed.rev_parse("HEAD"), origin.rev_parse("HEAD"));
}

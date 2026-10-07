//! The HTTP server proves itself against the real git CLI: clone from it, push to it.

use rc_git_testkit::{Fixture, HttpGitServer};

#[test]
fn a_served_repository_clones_and_accepts_a_push() {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    let server = HttpGitServer::serve(&bare_path);

    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", &server.url(), "."]);
    assert_eq!(clone.rev_parse("HEAD"), origin.rev_parse("HEAD"));

    clone.write("pushed.txt", b"pushed\n");
    clone.commit("Pushed over HTTP", "2024-02-01T00:00:00+0000");
    clone.git(&["-c", "http.receivepack=true", "push", "--quiet"]);

    let served_tip = bare.git_output(&["--git-dir", "remote.git", "rev-parse", "main"]);
    assert_eq!(clone.rev_parse("HEAD"), served_tip);
}

#[test]
fn missing_or_wrong_credentials_are_rejected_and_correct_ones_accepted() {
    let origin = Fixture::standard();
    let (_bare, bare_path) = origin.bare_copy();
    let server = HttpGitServer::serve_with_auth(&bare_path, "user", "sekret");

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

#[test]
fn a_fixture_git_that_needs_a_credential_fails_instead_of_prompting() {
    let origin = Fixture::standard();
    let (_bare, bare_path) = origin.bare_copy();
    let server = HttpGitServer::serve_with_auth(&bare_path, "user", "sekret");
    let clone = Fixture::empty_dir();
    let url = server.url();

    let panic = std::panic::catch_unwind(|| {
        clone.git(&["-c", "credential.helper=", "clone", "--quiet", &url, "."]);
    })
    .expect_err("a clone without credentials must fail");
    let message = panic
        .downcast_ref::<String>()
        .expect("the fixture panics with a formatted message");
    assert!(
        message.contains("terminal prompts disabled"),
        "git must refuse to prompt, got: {message}"
    );
}

#[test]
fn a_clone_whose_negotiation_is_gzipped_succeeds() {
    let origin = Fixture::standard();
    for n in 0..30 {
        origin.write("many.txt", format!("{n}\n").as_bytes());
        origin.commit(&format!("Branch {n}"), "2024-03-01T00:00:00+0000");
        origin.git(&["branch", &format!("many-{n}")]);
    }
    let (_bare, bare_path) = origin.bare_copy();
    let server = HttpGitServer::serve(&bare_path);

    // Thirty wants push the upload-pack request past 1 KiB, which git sends gzipped.
    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", &server.url(), "."]);
    assert_eq!(
        clone.rev_parse("origin/many-29"),
        origin.rev_parse("many-29")
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test speaks raw HTTP to its own loopback server"
)]
fn a_git_protocol_header_selects_protocol_v2() {
    use std::io::{Read, Write};

    let origin = Fixture::standard();
    let (_bare, bare_path) = origin.bare_copy();
    let server = HttpGitServer::serve(&bare_path);
    let url = server.url();
    let (authority, mount) = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split_once('/'))
        .expect("server url has an authority and a mount");

    let mut stream = std::net::TcpStream::connect(authority).expect("connect to the server");
    let request = format!(
        "GET /{mount}/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: {authority}\r\nGit-Protocol: version=2\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).expect("send request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");

    assert!(
        response.contains("version 2"),
        "expected a protocol v2 advertisement, got: {response}"
    );
}

#[test]
fn a_refused_push_names_the_backend_reason() {
    let origin = Fixture::standard();
    let (bare, bare_path) = origin.bare_copy();
    bare.git(&[
        "--git-dir",
        "remote.git",
        "config",
        "http.receivepack",
        "false",
    ]);
    let server = HttpGitServer::serve(&bare_path);

    let clone = Fixture::empty_dir();
    clone.git(&["clone", "--quiet", &server.url(), "."]);
    clone.write("pushed.txt", b"pushed\n");
    clone.commit("Refused over HTTP", "2024-02-01T00:00:00+0000");
    let output = clone.git_expect_failure(&["push", "--quiet"]);

    assert!(output.contains("Service not enabled"), "{output}");
}

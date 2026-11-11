//! Ported from `MarkdownLinkTargetTests.swift`: what a link destination means once it is read
//! against the file that shows it.

#![expect(
    clippy::unnecessary_wraps,
    reason = "the expectation helpers return what `resolve` returns, so each assertion compares like with like"
)]

use rc_git_kit::markdown::MarkdownLinkTarget;

fn resolve(destination: &str) -> Option<MarkdownLinkTarget> {
    MarkdownLinkTarget::resolve(destination, Some("docs/guide/index.md"))
}

fn web(url: &str) -> Option<MarkdownLinkTarget> {
    Some(MarkdownLinkTarget::Web(url.to_owned()))
}

fn other(url: &str) -> Option<MarkdownLinkTarget> {
    Some(MarkdownLinkTarget::OtherScheme(url.to_owned()))
}

fn path(path: &str, fragment: Option<&str>) -> Option<MarkdownLinkTarget> {
    Some(MarkdownLinkTarget::RepositoryPath {
        path: path.to_owned(),
        fragment: fragment.map(str::to_owned),
    })
}

fn fragment(fragment: &str) -> Option<MarkdownLinkTarget> {
    Some(MarkdownLinkTarget::Fragment(fragment.to_owned()))
}

#[test]
fn web_schemes_go_to_the_browser() {
    for url in [
        "https://example.com",
        "http://example.com",
        "HTTPS://example.com",
        "mailto:a@b.com",
    ] {
        assert_eq!(resolve(url), web(url), "{url}");
    }
    assert_eq!(resolve("//example.com/x"), web("https://example.com/x"));
}

#[test]
fn other_schemes_are_not_web_links() {
    for url in [
        "smb://h/share",
        "ssh://h",
        "vnc://h",
        "x-man-page://ls",
        "vscode://x",
        "applescript://com.apple.scripteditor?action=new",
        "my-app+v2:open",
    ] {
        assert_eq!(resolve(url), other(url), "{url}");
    }
}

#[test]
fn a_scheme_follows_the_common_mark_ascii_rule() {
    assert_eq!(
        resolve("x-apple.systempreferences:foo"),
        other("x-apple.systempreferences:foo")
    );
    assert_eq!(resolve("é:x"), path("docs/guide/é:x", None));
    assert_eq!(resolve("éa:x"), path("docs/guide/éa:x", None));
    assert_eq!(resolve("1ab:x"), path("docs/guide/1ab:x", None));
    assert_eq!(resolve("C:x"), path("docs/guide/C:x", None));
}

#[test]
fn a_local_file_url_is_refused() {
    assert_eq!(resolve("file:///etc/passwd"), None);
    assert_eq!(resolve("FILE:///etc/passwd"), None);
}

#[test]
fn a_bare_fragment_stays_in_the_open_file() {
    assert_eq!(resolve("#invariants"), fragment("invariants"));
}

#[test]
fn an_encoded_fragment_is_decoded() {
    assert_eq!(
        resolve("#ti%E1%BA%BFng-vi%E1%BB%87t"),
        fragment("tiếng-việt")
    );
    assert_eq!(resolve("#c%C3%A0i-%C4%91%E1%BA%B7t"), fragment("cài-đặt"));
    assert_eq!(
        resolve("other.md#ti%E1%BA%BFng"),
        path("docs/guide/other.md", Some("tiếng"))
    );
    assert_eq!(resolve("../ffi.md#a%20b"), path("docs/ffi.md", Some("a b")));
    assert_eq!(resolve("?x#a%20b"), fragment("a b"));
}

#[test]
fn a_fragment_that_does_not_decode_stays_as_written() {
    assert_eq!(resolve("#100%"), fragment("100%"));
    assert_eq!(
        resolve("other.md#100%"),
        path("docs/guide/other.md", Some("100%"))
    );
    assert_eq!(resolve("#%zz"), fragment("%zz"));
    assert_eq!(resolve("#%E1%BA"), fragment("%E1%BA"));
}

#[test]
fn relative_paths_resolve_against_the_open_files_directory() {
    assert_eq!(resolve("other.md"), path("docs/guide/other.md", None));
    assert_eq!(resolve("./other.md"), path("docs/guide/other.md", None));
    assert_eq!(resolve("../ffi.md"), path("docs/ffi.md", None));
    assert_eq!(resolve("../../README.md"), path("README.md", None));
}

#[test]
fn a_leading_slash_means_the_repository_root() {
    assert_eq!(resolve("/CONCEPTS.md"), path("CONCEPTS.md", None));
}

#[test]
fn without_an_open_path_links_resolve_from_the_root() {
    assert_eq!(
        MarkdownLinkTarget::resolve("docs/x.md", None),
        path("docs/x.md", None)
    );
}

#[test]
fn fragments_and_queries_are_split_off() {
    assert_eq!(
        resolve("../ffi.md#errors"),
        path("docs/ffi.md", Some("errors"))
    );
    assert_eq!(resolve("other.md?raw=1"), path("docs/guide/other.md", None));
    assert_eq!(
        resolve("other.md?raw=1#errors"),
        path("docs/guide/other.md", Some("errors"))
    );
    assert_eq!(
        resolve("other.md?plain=1#errors"),
        path("docs/guide/other.md", Some("errors"))
    );
}

#[test]
fn a_query_only_destination_is_no_file() {
    for destination in ["?", "??", " ? ", "?a=b", "?raw=1", "?tab=1"] {
        assert_eq!(resolve(destination), None, "{destination:?}");
    }
    assert_eq!(resolve("?x#sec"), fragment("sec"));
}

#[test]
fn percent_escapes_are_decoded() {
    assert_eq!(resolve("../my%20file.md"), path("docs/my file.md", None));
}

#[test]
fn a_path_that_does_not_decode_stays_as_written() {
    assert_eq!(resolve("a%ZZb.md"), path("docs/guide/a%ZZb.md", None));
    assert_eq!(resolve("%E1%BA"), path("docs/guide/%E1%BA", None));
}

#[test]
fn climbing_above_the_repository_is_refused() {
    assert_eq!(resolve("../../../outside.md"), None);
    assert_eq!(MarkdownLinkTarget::resolve("", Some("README.md")), None);
    assert_eq!(resolve("/"), None);
}

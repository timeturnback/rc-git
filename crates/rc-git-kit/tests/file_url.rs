//! The path a `file://` URL names.

use rc_git_kit::file_url::path;

#[test]
fn a_file_url_names_its_decoded_path() {
    assert_eq!(
        path("file:///Users/me/repo/").as_deref(),
        Some("/Users/me/repo")
    );
    assert_eq!(
        path("file:///Users/me/my%20repo").as_deref(),
        Some("/Users/me/my repo")
    );
    assert_eq!(
        path("file://localhost/tmp/r%C3%A9po").as_deref(),
        Some("/tmp/répo")
    );
    assert_eq!(path("file:///").as_deref(), Some("/"));
}

#[test]
fn a_plain_path_passes_through() {
    assert_eq!(path("/tmp/repo").as_deref(), Some("/tmp/repo"));
}

#[test]
fn anything_else_names_no_path() {
    assert_eq!(path("https://example.com/repo"), None);
    assert_eq!(path("file://server/share"), None);
    assert_eq!(path("file:///bad%2"), None);
    assert_eq!(path("file:///bad%zz"), None);
    assert_eq!(path("file:///bad%ff"), None);
    assert_eq!(path("relative/path"), None);
}

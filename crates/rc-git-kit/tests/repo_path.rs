//! Port of `RepoPathTests.swift`.

use rc_git_kit::repo_path::flatten;

fn owned(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

#[test]
fn climbing_past_the_root_is_none() {
    assert_eq!(flatten(&[], ".."), None);
    assert_eq!(flatten(&owned(&["a"]), "../.."), None);
    assert_eq!(flatten(&owned(&["a"]), "../../b"), None);
}

#[test]
fn climbing_to_the_root_is_empty() {
    assert_eq!(flatten(&owned(&["a", "b"]), "../.."), Some(vec![]));
}

#[test]
fn a_dot_alone_is_the_base() {
    assert_eq!(flatten(&owned(&["a", "b"]), "."), Some(owned(&["a", "b"])));
}

#[test]
fn an_empty_relative_is_the_base() {
    assert_eq!(flatten(&owned(&["a", "b"]), ""), Some(owned(&["a", "b"])));
    assert_eq!(flatten(&[], ""), Some(vec![]));
}

#[test]
fn repeated_slashes_collapse() {
    assert_eq!(
        flatten(&owned(&["a"]), "b//c///d"),
        Some(owned(&["a", "b", "c", "d"]))
    );
    assert_eq!(flatten(&owned(&["a"]), ".//..//b/"), Some(owned(&["b"])));
}

#[test]
fn dots_mix_with_names() {
    assert_eq!(
        flatten(&owned(&["src", "lib"]), "./../util/./x.ts"),
        Some(owned(&["src", "util", "x.ts"]))
    );
}

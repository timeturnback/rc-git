//! Ported from `TreePlaceholderTests.swift` (the value cases; the `BrowserModel.attach`
//! cases move with the browser model).
#![expect(clippy::unwrap_used, reason = "the oid helper builds a fixed valid id")]

use rc_git_core::{Head, Oid};
use rc_git_kit::tree_placeholder::{NO_COMMITS, NO_REPOSITORY, NOTHING_TO_BROWSE, text};

fn oid() -> Oid {
    Oid::from_hex(&"11".repeat(20)).unwrap()
}

fn head(ref_name: Option<&str>, target: Option<Oid>) -> Head {
    Head {
        ref_name: ref_name.map(str::to_owned),
        target,
        exact_ref_name: true,
    }
}

#[test]
fn no_repository_asks_to_open_one() {
    let born = head(Some("refs/heads/main"), Some(oid()));
    assert_eq!(text(false, None), NO_REPOSITORY);
    assert_eq!(text(false, Some(&born)), NO_REPOSITORY);
    assert_eq!(
        text(false, Some(&head(Some("refs/heads/main"), None))),
        NO_REPOSITORY
    );
}

#[test]
fn unborn_head_has_no_commits() {
    let unborn = head(Some("refs/heads/main"), None);
    assert_eq!(text(true, Some(&unborn)), NO_COMMITS);
}

#[test]
fn otherwise_there_is_nothing_to_browse() {
    let detached = head(None, Some(oid()));
    assert_eq!(text(true, Some(&detached)), NOTHING_TO_BROWSE);
    assert_eq!(text(true, None), NOTHING_TO_BROWSE);
}

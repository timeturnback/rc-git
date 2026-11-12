//! Ported from `RecentRepositoriesTests.swift`. The fuzzy ranking itself belongs to the path
//! search; these tests drive `matching` with a stand-in scorer that rewards a match inside
//! the last path component, which is the behaviour the picker relies on.

use rc_git_kit::path_search;
use rc_git_kit::recent_repositories::{CAPACITY, display_path, empty_note, inserting, matching};

const HOME: &str = "/Users/me";

fn paths(list: &[&str]) -> Vec<String> {
    list.iter().map(|path| (*path).to_owned()).collect()
}

/// A case-insensitive subsequence match: 10 points when the last component alone holds it,
/// 1 when only the whole path does.
fn subsequence(candidate: &str, query: &str) -> Option<i64> {
    let holds = |text: &str| {
        let mut wanted = query.chars().flat_map(char::to_lowercase).peekable();
        for ch in text.chars().flat_map(char::to_lowercase) {
            if wanted.peek() == Some(&ch) {
                wanted.next();
            }
        }
        wanted.peek().is_none()
    };
    let name = candidate.rsplit('/').next().unwrap_or(candidate);
    if holds(name) {
        Some(10)
    } else if holds(candidate) {
        Some(1)
    } else {
        None
    }
}

#[test]
fn inserting_moves_to_front_and_deduplicates() {
    assert_eq!(
        inserting("/b", &paths(&["/a", "/b", "/c"])),
        paths(&["/b", "/a", "/c"])
    );
}

#[test]
fn inserting_caps_the_list() {
    let existing: Vec<String> = (0..CAPACITY).map(|index| format!("/repo{index}")).collect();
    let list = inserting("/new", &existing);
    assert_eq!(list.len(), CAPACITY);
    assert_eq!(list[0], "/new");
    assert!(!list.contains(&format!("/repo{}", CAPACITY - 1)));
    let again = inserting("/repo3", &list);
    assert_eq!(again.len(), CAPACITY, "a repeat does not shorten the list");
    assert_eq!(again[0], "/repo3");
}

#[test]
fn a_blank_query_lists_every_recent_in_recency_order() {
    let list = paths(&["/work/b", "/work/a", "/work/c"]);
    assert_eq!(matching(&list, "", HOME, subsequence), list);
    assert_eq!(matching(&list, " \t ", HOME, subsequence), list);
    assert!(matching(&[], "a", HOME, subsequence).is_empty());
}

#[test]
fn a_query_keeps_only_matches_and_ranks_by_score() {
    let list = paths(&["/rc/git-tools/web", "/src/notes", "/src/rc-git"]);
    assert_eq!(
        matching(&list, "notes", HOME, subsequence),
        paths(&["/src/notes"])
    );
    assert_eq!(
        matching(&list, "rcgit", HOME, subsequence),
        paths(&["/src/rc-git", "/rc/git-tools/web"]),
        "a higher score beats recency"
    );
    assert!(matching(&list, "zzz", HOME, subsequence).is_empty());
}

#[test]
fn equal_scores_keep_recency_order() {
    let list = paths(&["/a/app", "/b/app", "/c/app"]);
    assert_eq!(matching(&list, "app", HOME, subsequence), list);
    assert_eq!(matching(&list, " APP ", HOME, subsequence), list);
}

#[test]
fn a_decomposed_query_matches_a_precomposed_path() {
    let precomposed = "/src/caf\u{e9}".to_owned();
    let list = vec![precomposed.clone()];
    assert_eq!(
        matching(&list, "cafe\u{301}", HOME, subsequence),
        vec![precomposed]
    );
    let decomposed = vec!["/src/cafe\u{301}".to_owned()];
    assert_eq!(
        matching(&decomposed, "caf\u{e9}", HOME, subsequence),
        decomposed,
        "the path side is normalized too"
    );
}

#[test]
fn a_newline_is_part_of_the_query() {
    let list = paths(&["/src/app"]);
    assert!(
        matching(&list, "\n", HOME, subsequence).is_empty(),
        "only spaces and tabs are trimmed, as Foundation's .whitespaces does"
    );
}

#[test]
fn a_query_typed_as_displayed_matches_under_the_home_directory() {
    let inside = format!("{HOME}/code/app");
    let outside = "/opt/code/app".to_owned();
    assert_eq!(display_path(&inside, HOME), "~/code/app");
    assert_eq!(display_path(&outside, HOME), outside);
    assert_eq!(
        matching(&[outside, inside.clone()], "~/co", HOME, subsequence),
        vec![inside]
    );
}

#[test]
fn only_the_home_directory_itself_or_below_gets_a_tilde() {
    assert_eq!(display_path(HOME, HOME), "~");
    assert_eq!(display_path("/Users/meg/x", HOME), "/Users/meg/x");
    assert_eq!(display_path("/Users/me/x", "/Users/me/"), "~/x");
    assert_eq!(display_path("/x", ""), "/x");
}

/// The picker passes the path search's own scorer; these pin what that pairing ranks.
#[test]
fn the_real_scorer_ranks_a_name_match_over_a_directory_match() {
    let list = paths(&["/src/notes-rcgit-tools", "/rc/git/web", "/src/rcgit"]);
    let found = matching(&list, "rcgit", HOME, path_search::score);
    assert_eq!(
        found.first().map(String::as_str),
        Some("/src/rcgit"),
        "the whole match inside the last component wins: {found:?}"
    );
    assert!(found.contains(&"/rc/git/web".to_owned()));
    assert!(matching(&list, "zzz", HOME, path_search::score).is_empty());
}

#[test]
fn the_real_scorer_ignores_case_and_matches_the_displayed_home() {
    let inside = format!("{HOME}/Code/App");
    let list = vec!["/opt/other".to_owned(), inside.clone()];
    assert_eq!(
        matching(&list, "app", HOME, path_search::score),
        vec![inside.clone()]
    );
    assert_eq!(
        matching(&list, "~/code", HOME, path_search::score),
        vec![inside]
    );
    assert_eq!(
        matching(&list, "", HOME, path_search::score),
        list,
        "a blank query keeps recency order"
    );
}

#[test]
fn the_real_scorer_matches_a_decomposed_query() {
    let precomposed = "/src/caf\u{e9}".to_owned();
    assert_eq!(
        matching(
            std::slice::from_ref(&precomposed),
            "cafe\u{301}",
            HOME,
            path_search::score
        ),
        vec![precomposed]
    );
}

#[test]
fn the_empty_note_says_whether_any_recent_exists() {
    assert_eq!(empty_note(false), "No recent repositories");
    assert_eq!(empty_note(true), "No recent repository matches");
}

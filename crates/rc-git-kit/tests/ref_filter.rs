//! The ref list's sections, filter and Return pick. Port of `RefFilterTests.swift`.
#![expect(clippy::unwrap_used, reason = "the fixed target id is valid hex")]

use rc_git_core::{BrokenRef, Oid, RefEntry, RefKind, RefListing};
use rc_git_kit::ref_filter::{RefList, RefSection, query, working_tree_matches};

fn entry(short_name: &str, kind: RefKind) -> RefEntry {
    let namespace = match kind {
        RefKind::LocalBranch => "refs/heads/",
        RefKind::RemoteBranch => "refs/remotes/",
        RefKind::Tag => "refs/tags/",
        RefKind::Other => "refs/",
    };
    RefEntry {
        full_name: format!("{namespace}{short_name}"),
        short_name: short_name.to_owned(),
        kind,
        target: Oid::from_hex("1a2b3c4d5e6f708192a3b4c5d6e7f80910a1b2c3").unwrap(),
        exact_name: true,
    }
}

fn branch(short_name: &str) -> RefEntry {
    entry(short_name, RefKind::LocalBranch)
}

fn broken(full_name: &str, error: &str) -> BrokenRef {
    BrokenRef {
        full_name: full_name.to_owned(),
        error: error.to_owned(),
    }
}

fn list(refs: Vec<RefEntry>) -> RefList {
    RefList::new(RefListing {
        refs,
        broken: Vec::new(),
    })
}

fn names(sections: &[RefSection]) -> Vec<Vec<String>> {
    sections
        .iter()
        .map(|section| {
            section
                .refs
                .iter()
                .map(|entry| entry.short_name.clone())
                .collect()
        })
        .collect()
}

fn full_names(broken: &[BrokenRef]) -> Vec<&str> {
    broken
        .iter()
        .map(|entry| entry.full_name.as_str())
        .collect()
}

fn mixed() -> RefList {
    list(vec![
        entry("other-thing", RefKind::Other),
        entry("v1.0", RefKind::Tag),
        entry("origin/main", RefKind::RemoteBranch),
        branch("main"),
        branch("feature"),
    ])
}

fn best(list: &RefList, filter: &str) -> Option<String> {
    list.best_match(filter).map(|entry| entry.short_name)
}

#[test]
fn sections_come_in_fixed_order() {
    let sections = mixed().sections("");
    let titles: Vec<&str> = sections.iter().map(|section| section.title).collect();
    assert_eq!(titles, ["Branches", "Remotes", "Tags", "Other"]);
    let kinds: Vec<RefKind> = sections.iter().map(|section| section.kind).collect();
    assert_eq!(
        kinds,
        [
            RefKind::LocalBranch,
            RefKind::RemoteBranch,
            RefKind::Tag,
            RefKind::Other
        ]
    );
    assert_eq!(
        names(&sections),
        [
            vec!["feature", "main"],
            vec!["origin/main"],
            vec!["v1.0"],
            vec!["other-thing"]
        ]
    );
}

#[test]
fn empty_sections_are_dropped() {
    let sections = mixed().sections("main");
    let titles: Vec<&str> = sections.iter().map(|section| section.title).collect();
    assert_eq!(titles, ["Branches", "Remotes"]);
    assert_eq!(names(&sections), [vec!["main"], vec!["origin/main"]]);
    assert!(list(Vec::new()).sections("").is_empty());
}

#[test]
fn matching_ignores_case() {
    let refs = list(vec![
        branch("Main"),
        branch("feature/MAIN-fix"),
        branch("dev"),
    ]);
    assert_eq!(
        names(&refs.sections("MAIN")),
        [vec!["feature/MAIN-fix", "Main"]]
    );
    assert_eq!(
        names(&refs.sections("main")),
        [vec!["feature/MAIN-fix", "Main"]]
    );
    assert_eq!(best(&refs, "MAIN").as_deref(), Some("Main"));
}

#[test]
fn refs_sort_in_natural_number_order() {
    let refs = list(vec![
        entry("v10", RefKind::Tag),
        entry("v2", RefKind::Tag),
        entry("v1", RefKind::Tag),
    ]);
    assert_eq!(names(&refs.sections("")), [vec!["v1", "v2", "v10"]]);
}

#[test]
fn changed_refs_are_sorted_again() {
    let mut refs = list(vec![entry("v2", RefKind::Tag)]);
    let mut grown = refs.listing().refs.clone();
    grown.push(entry("v10", RefKind::Tag));
    grown.push(entry("v1", RefKind::Tag));
    grown.push(branch("main"));
    refs.set_refs(grown);
    assert_eq!(
        names(&refs.sections("")),
        [vec!["main"], vec!["v1", "v2", "v10"]]
    );
    assert_eq!(names(&refs.sections("v1")), [vec!["v1", "v10"]]);
    refs.set_broken(vec![
        broken("refs/tags/v10", "missing object"),
        broken("refs/tags/v9", "missing object"),
    ]);
    assert_eq!(
        full_names(&refs.unreadable("")),
        ["refs/tags/v9", "refs/tags/v10"]
    );
}

#[test]
fn lists_with_the_same_refs_are_equal() {
    let built = list(vec![branch("main"), entry("v1", RefKind::Tag)]);
    let mut grown = list(vec![branch("main")]);
    grown.set_refs(vec![branch("main"), entry("v1", RefKind::Tag)]);
    assert_eq!(built, grown);
    assert_ne!(built, list(vec![branch("main")]));
}

#[test]
fn a_whitespace_filter_filters_nothing() {
    let refs = mixed();
    assert_eq!(names(&refs.sections("  \t ")), names(&refs.sections("")));
    assert_eq!(query(" \n"), None);
    assert_eq!(query(" Main ").as_deref(), Some("main"));
    assert_eq!(refs.best_match("   "), None);
}

#[test]
fn no_match_gives_none() {
    let refs = mixed();
    assert_eq!(refs.best_match("zzz"), None);
    assert!(refs.sections("zzz").is_empty());
}

#[test]
fn an_exact_name_beats_earlier_substring_matches() {
    let refs = list(vec![
        branch("chore/maintenance"),
        branch("domain-model"),
        branch("feature/main-fix"),
        branch("main"),
    ]);
    assert_eq!(best(&refs, "main").as_deref(), Some("main"));
    // The displayed order stays natural; only the pick moves.
    assert_eq!(
        names(&refs.sections("main")),
        [vec![
            "chore/maintenance",
            "domain-model",
            "feature/main-fix",
            "main"
        ]]
    );
}

#[test]
fn a_prefix_beats_a_later_component_and_a_substring() {
    let refs = list(vec![
        branch("chore/maintenance"),
        branch("domain-model"),
        branch("feature/main-fix"),
        branch("main"),
    ]);
    assert_eq!(best(&refs, "ma").as_deref(), Some("main"));
}

#[test]
fn a_last_component_match_beats_a_substring() {
    let refs = list(vec![
        branch("domain-model"),
        entry("origin/main", RefKind::RemoteBranch),
    ]);
    assert_eq!(best(&refs, "main").as_deref(), Some("origin/main"));
}

#[test]
fn the_local_branch_wins_a_tie_with_its_remote() {
    let full = mixed().best_match("main").map(|entry| entry.full_name);
    assert_eq!(full.as_deref(), Some("refs/heads/main"));
    let prefix_tie = list(vec![
        entry("origin/main", RefKind::RemoteBranch),
        entry("main-a", RefKind::Tag),
        branch("main-b"),
    ]);
    let full = prefix_tie.best_match("main").map(|entry| entry.full_name);
    assert_eq!(full.as_deref(), Some("refs/heads/main-b"));
}

#[test]
fn a_tie_in_one_section_goes_to_natural_order() {
    let refs = list(vec![branch("main-10"), branch("main-2")]);
    assert_eq!(best(&refs, "main").as_deref(), Some("main-2"));
}

#[test]
fn broken_refs_are_unreadable_in_natural_order_and_never_picked() {
    let refs = RefList::new(RefListing {
        refs: vec![branch("main")],
        broken: vec![
            broken("refs/tags/v10", "missing object"),
            broken("refs/remotes/origin/HEAD", "dangling symref"),
            broken("refs/tags/v2", "missing object"),
        ],
    });
    assert_eq!(
        full_names(&refs.unreadable(" ")),
        ["refs/remotes/origin/HEAD", "refs/tags/v2", "refs/tags/v10"]
    );
    assert_eq!(
        full_names(&refs.unreadable("ORIGIN")),
        ["refs/remotes/origin/HEAD"]
    );
    assert!(refs.unreadable("nothing").is_empty());
    assert_eq!(names(&refs.sections("")), [vec!["main"]]);
    assert_eq!(refs.best_match("v2"), None);
}

#[test]
fn no_broken_refs_leaves_the_unreadable_section_out() {
    assert!(mixed().unreadable("").is_empty());
}

#[test]
fn the_working_tree_row_follows_the_filter() {
    assert!(working_tree_matches(""));
    assert!(working_tree_matches("  \t "));
    assert!(working_tree_matches("Work"));
    assert!(working_tree_matches(" tree "));
    assert!(working_tree_matches("working tree"));
    assert!(!working_tree_matches("zzzz"));
    assert!(!working_tree_matches("mian"));
    assert!(!working_tree_matches("working trees"));
}

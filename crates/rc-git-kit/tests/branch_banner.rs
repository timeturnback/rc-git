//! What the sidebar banner names in Files and Changes. Port of `BranchBannerTests.swift`.
#![expect(clippy::unwrap_used, reason = "the fixed ids are valid hex")]

use rc_git_core::{CommitSummary, Head, Oid, RefEntry, RefKind};
use rc_git_kit::branch_banner::{BranchBanner, STALE_REFS_DETAIL, Symbol};

fn tip() -> Oid {
    Oid::from_hex("1a2b3c4d5e6f708192a3b4c5d6e7f80910a1b2c3").unwrap()
}

fn other() -> Oid {
    Oid::from_hex("ffeeddccbbaa99887766554433221100ffeeddcc").unwrap()
}

fn commit(title: &str) -> CommitSummary {
    CommitSummary {
        oid: other(),
        parents: Vec::new(),
        author_name: "A".to_owned(),
        author_email: "a@example.com".to_owned(),
        author_time: 0,
        author_offset: 0,
        committer_time: 0,
        title: title.to_owned(),
    }
}

fn head(ref_name: Option<&str>, target: Option<Oid>) -> Head {
    Head {
        ref_name: ref_name.map(str::to_owned),
        target,
        exact_ref_name: true,
    }
}

fn entry(full_name: &str, short_name: &str, kind: RefKind) -> RefEntry {
    RefEntry {
        full_name: full_name.to_owned(),
        short_name: short_name.to_owned(),
        kind,
        target: tip(),
        exact_name: true,
    }
}

// Files

#[test]
fn the_working_tree_names_the_branch_under_it() {
    let head = head(Some("refs/heads/feat/banner"), Some(tip()));
    let banner = BranchBanner::browsing(None, None, true, Some(&head));
    assert_eq!(
        banner,
        BranchBanner::new(Symbol::WorkingTree, "Working Tree", Some("on feat/banner"))
    );
}

#[test]
fn a_detached_working_tree_names_its_commit() {
    let head = head(None, Some(tip()));
    let banner = BranchBanner::browsing(None, None, true, Some(&head));
    assert_eq!(banner.detail.as_deref(), Some("detached at 1a2b3c4"));
}

#[test]
fn the_working_tree_wins_over_a_stale_selection() {
    let main = entry("refs/heads/main", "main", RefKind::LocalBranch);
    let old = commit("Old");
    let banner = BranchBanner::browsing(Some(&main), Some(&old), true, None);
    assert_eq!(banner.symbol, Symbol::WorkingTree);
    assert_eq!(banner.detail, None);
}

#[test]
fn a_working_tree_on_an_unborn_detached_head_has_no_detail() {
    let head = head(None, None);
    let banner = BranchBanner::browsing(None, None, true, Some(&head));
    assert_eq!(banner.detail, None);
}

#[test]
fn a_browsed_commit_names_its_id_and_title() {
    let fix = commit("Fix the parser");
    let banner = BranchBanner::browsing(None, Some(&fix), false, None);
    assert_eq!(
        banner,
        BranchBanner::new(Symbol::Commit, "ffeeddc", Some("Fix the parser"))
    );
}

#[test]
fn a_browsed_commit_wins_over_a_selected_ref() {
    let main = entry("refs/heads/main", "main", RefKind::LocalBranch);
    let fix = commit("Fix");
    let banner = BranchBanner::browsing(Some(&main), Some(&fix), false, None);
    assert_eq!(banner.symbol, Symbol::Commit);
}

#[test]
fn a_selected_ref_names_itself_with_its_kind() {
    let tag = entry("refs/tags/v1.0", "v1.0", RefKind::Tag);
    let banner = BranchBanner::browsing(Some(&tag), None, false, None);
    assert_eq!(
        banner,
        BranchBanner::new(Symbol::Ref(RefKind::Tag), "v1.0", None)
    );
}

#[test]
fn nothing_browsed_is_the_nothing_banner() {
    let banner = BranchBanner::browsing(None, None, false, None);
    assert_eq!(banner, BranchBanner::nothing());
}

// Changes

#[test]
fn changes_name_the_checked_out_branch() {
    let banner = BranchBanner::checked_out(Some(&head(Some("refs/heads/main"), Some(tip()))));
    assert_eq!(
        banner,
        BranchBanner::new(Symbol::Ref(RefKind::LocalBranch), "main", None)
    );
}

#[test]
fn an_unborn_branch_says_it_has_no_commits() {
    let banner = BranchBanner::checked_out(Some(&head(Some("refs/heads/main"), None)));
    assert_eq!(banner.title, "main");
    assert_eq!(banner.detail.as_deref(), Some("No commits yet"));
}

#[test]
fn a_detached_head_names_its_commit() {
    let banner = BranchBanner::checked_out(Some(&head(None, Some(tip()))));
    assert_eq!(
        banner,
        BranchBanner::new(Symbol::Detached, "Detached HEAD", Some("1a2b3c4"))
    );
}

#[test]
fn no_head_is_the_nothing_banner() {
    assert_eq!(BranchBanner::checked_out(None), BranchBanner::nothing());
    assert_eq!(
        BranchBanner::checked_out(Some(&head(None, None))),
        BranchBanner::nothing()
    );
}

#[test]
fn a_ref_name_outside_the_branch_namespace_stays_whole() {
    let banner = BranchBanner::checked_out(Some(&head(Some("refs/other/x"), Some(tip()))));
    assert_eq!(banner.title, "refs/other/x");
}

// Summary

#[test]
fn the_summary_joins_title_and_detail() {
    assert_eq!(
        BranchBanner::new(Symbol::Commit, "ffeeddc", Some("Fix")).summary(),
        "ffeeddc \u{2014} Fix"
    );
    assert_eq!(
        BranchBanner::new(Symbol::Ref(RefKind::LocalBranch), "main", None).summary(),
        "main"
    );
    assert_eq!(BranchBanner::nothing().summary(), "");
}

// Stale refs

#[test]
fn stale_refs_replace_the_detail_and_keep_the_name() {
    let banner = BranchBanner::new(Symbol::WorkingTree, "Working Tree", Some("on main"));
    assert_eq!(banner.marking_refs_stale(false), banner);
    assert_eq!(
        banner.marking_refs_stale(true),
        BranchBanner::new(Symbol::WorkingTree, "Working Tree", Some(STALE_REFS_DETAIL))
    );
    assert_eq!(
        BranchBanner::new(Symbol::Ref(RefKind::LocalBranch), "main", None)
            .marking_refs_stale(true)
            .summary(),
        "main \u{2014} Refs could not be re-read"
    );
}

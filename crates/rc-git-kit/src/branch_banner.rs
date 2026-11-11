//! Port of `BranchBanner.swift`: what the sidebar banner names in the modes that show it
//! instead of the ref list. Files names what the tree browses: the working tree and the branch
//! under it, one commit picked from the history, or a ref. Changes names the branch `HEAD` is
//! on, because the change list always compares the working tree with `HEAD`, whatever Files
//! browses.

use rc_git_core::{CommitSummary, Head, Oid, RefEntry, RefKind};

/// Which icon stands beside the title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Symbol {
    /// A ref, by its namespace.
    Ref(RefKind),
    /// One commit.
    Commit,
    /// The files on disk.
    WorkingTree,
    /// `HEAD` on no branch.
    Detached,
    /// Nothing browsed yet.
    Nothing,
}

/// The banner's icon, title and quieter second line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchBanner {
    /// Which icon stands beside the title.
    pub symbol: Symbol,
    /// The banner's main line.
    pub title: String,
    /// A quieter second line: the branch under the working tree, or a commit's message.
    pub detail: Option<String>,
}

/// The detail a banner carries while the refs it names could not be re-read.
pub const STALE_REFS_DETAIL: &str = "Refs could not be re-read";

/// Hex digits a short commit id shows.
const SHORT_ID_LENGTH: usize = 7;

impl BranchBanner {
    /// A banner from its three parts.
    #[must_use]
    pub fn new(symbol: Symbol, title: impl Into<String>, detail: Option<&str>) -> Self {
        Self {
            symbol,
            title: title.into(),
            detail: detail.map(str::to_owned),
        }
    }

    /// Nothing is browsed yet: the repository is still loading, or it has no refs.
    #[must_use]
    pub fn nothing() -> Self {
        Self::new(Symbol::Nothing, "No branch", None)
    }

    /// The banner as one line, for the window subtitle. Empty when nothing is browsed.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.symbol == Symbol::Nothing {
            return String::new();
        }
        match &self.detail {
            Some(detail) => format!("{} \u{2014} {detail}", self.title),
            None => self.title.clone(),
        }
    }

    /// This banner, with its detail replaced by [`STALE_REFS_DETAIL`] when `stale`: the last
    /// listing stays on screen but no longer reads as current.
    #[must_use]
    pub fn marking_refs_stale(&self, stale: bool) -> Self {
        if !stale {
            return self.clone();
        }
        Self::new(self.symbol, self.title.clone(), Some(STALE_REFS_DETAIL))
    }

    /// What Files browses, in the order the model holds it: the working tree, then a commit
    /// from the history, then the selected ref.
    #[must_use]
    pub fn browsing(
        selected_ref: Option<&RefEntry>,
        browsed_commit: Option<&CommitSummary>,
        browsing_working_tree: bool,
        head: Option<&Head>,
    ) -> Self {
        if browsing_working_tree {
            return Self {
                symbol: Symbol::WorkingTree,
                title: "Working Tree".to_owned(),
                detail: under(head),
            };
        }
        if let Some(commit) = browsed_commit {
            return Self::new(Symbol::Commit, short_id(&commit.oid), Some(&commit.title));
        }
        if let Some(entry) = selected_ref {
            return Self::new(Symbol::Ref(entry.kind), entry.short_name.clone(), None);
        }
        Self::nothing()
    }

    /// The branch `HEAD` is on, for Changes.
    #[must_use]
    pub fn checked_out(head: Option<&Head>) -> Self {
        let Some(head) = head else {
            return Self::nothing();
        };
        if let Some(name) = &head.ref_name {
            let unborn = head.target.is_none().then_some("No commits yet");
            return Self::new(Symbol::Ref(RefKind::LocalBranch), branch_name(name), unborn);
        }
        if let Some(target) = &head.target {
            return Self::new(Symbol::Detached, "Detached HEAD", Some(&short_id(target)));
        }
        Self::nothing()
    }
}

/// "on main", or "detached at 1a2b3c4", for the line under the working tree.
fn under(head: Option<&Head>) -> Option<String> {
    let head = head?;
    if let Some(name) = &head.ref_name {
        return Some(format!("on {}", branch_name(name)));
    }
    head.target
        .as_ref()
        .map(|target| format!("detached at {}", short_id(target)))
}

/// `HEAD` names its branch in full; the banner shows it the way the ref list does.
fn branch_name(ref_name: &str) -> &str {
    ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name)
}

fn short_id(oid: &Oid) -> String {
    let mut hex = oid.to_hex();
    hex.truncate(SHORT_ID_LENGTH);
    hex
}

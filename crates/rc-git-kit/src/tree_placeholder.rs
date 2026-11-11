//! Port of `TreePlaceholder.swift`: what the file tree says while it has no root to show,
//! decided from what the model knows about the repository, so a window that has one open
//! never sends the user to open one.

use rc_git_core::Head;

/// No repository is attached to the window.
pub const NO_REPOSITORY: &str = "Open a repository (⌘O)";
/// `HEAD` is on an unborn branch (`git init`, no commits) and no ref gave a tree either.
pub const NO_COMMITS: &str = "No commits yet";
/// A repository is open but nothing in it is browsed: its refs could not be read, or `HEAD`
/// has a commit while no ref names one.
pub const NOTHING_TO_BROWSE: &str = "Nothing to browse";

/// The placeholder for a tree with no root, given whether a repository is attached and the
/// `HEAD` last read from it (`None` until a listing loads).
#[must_use]
pub fn text(attached: bool, head: Option<&Head>) -> &'static str {
    if !attached {
        return NO_REPOSITORY;
    }
    match head {
        Some(head) if head.target.is_none() => NO_COMMITS,
        Some(_) | None => NOTHING_TO_BROWSE,
    }
}

//! The ref list's context menu (`RefsViewController.menuNeedsUpdate`) and the name sheet its
//! create items open: which items a ref offers, their titles, and what the sheet says. The
//! actions themselves run through the guard plans in [`crate::repository_actions`].

use rc_git_core::{RefEntry, RefKind};

/// A mutating request from the ref list's context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefMenuAction {
    /// Create a branch at the ref's commit.
    NewBranch,
    /// Create a lightweight tag at the ref's commit.
    NewTag,
    /// Delete the local branch.
    DeleteBranch,
    /// Delete the tag.
    DeleteTag,
}

/// What the menu shows for one ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefMenu {
    /// The ref's name is not valid UTF-8: its U+FFFD spelling could name another ref, so
    /// nothing that takes the name is offered, only this disabled note.
    NotExact(&'static str),
    /// The items, in order; `true` before an item puts a separator above it.
    Items(Vec<(bool, RefMenuAction, String)>),
}

/// The note shown in place of the items for a ref whose name is not exact.
pub const NOT_EXACT_NOTE: &str = "Name Is Not Valid UTF-8";

/// The menu for `entry`: New Branch and New Tag at it, then Delete for a local branch or a
/// tag. Remote branches and other refs are never deleted from here.
#[must_use]
pub fn ref_menu(entry: &RefEntry) -> RefMenu {
    if !entry.exact_name {
        return RefMenu::NotExact(NOT_EXACT_NOTE);
    }
    let name = &entry.short_name;
    let mut items = vec![
        (
            false,
            RefMenuAction::NewBranch,
            format!("New Branch from \u{201c}{name}\u{201d}\u{2026}"),
        ),
        (
            false,
            RefMenuAction::NewTag,
            format!("New Tag at \u{201c}{name}\u{201d}\u{2026}"),
        ),
    ];
    match entry.kind {
        RefKind::LocalBranch => items.push((
            true,
            RefMenuAction::DeleteBranch,
            format!("Delete Branch \u{201c}{name}\u{201d}\u{2026}"),
        )),
        RefKind::Tag => items.push((
            true,
            RefMenuAction::DeleteTag,
            format!("Delete Tag \u{201c}{name}\u{201d}\u{2026}"),
        )),
        RefKind::RemoteBranch | RefKind::Other => {}
    }
    RefMenu::Items(items)
}

/// The verb that titles the action's alerts and challenge.
#[must_use]
pub fn verb(action: RefMenuAction) -> &'static str {
    match action {
        RefMenuAction::NewBranch => "Create Branch",
        RefMenuAction::NewTag => "Create Tag",
        RefMenuAction::DeleteBranch => "Delete Branch",
        RefMenuAction::DeleteTag => "Delete Tag",
    }
}

/// The name sheet's title and text for a create at `entry`, `None` for a delete.
#[must_use]
pub fn name_prompt(action: RefMenuAction, entry: &RefEntry) -> Option<(&'static str, String)> {
    let short = entry.target.to_hex().chars().take(7).collect::<String>();
    let at = format!("\u{201c}{}\u{201d} ({short})", entry.short_name);
    match action {
        RefMenuAction::NewBranch => Some(("New Branch", format!("Create a branch at {at}."))),
        RefMenuAction::NewTag => Some(("New Tag", format!("Create a tag at {at}."))),
        RefMenuAction::DeleteBranch | RefMenuAction::DeleteTag => None,
    }
}

/// What the name sheet says when it closes with no name, so nothing is dropped silently.
pub const NO_NAME: &str = "No name was entered, so nothing was created.";

#[cfg(test)]
mod tests {
    use rc_git_core::Oid;

    use super::*;

    fn entry(kind: RefKind, exact: bool) -> RefEntry {
        RefEntry {
            full_name: "refs/heads/x".into(),
            short_name: "x".into(),
            kind,
            target: Oid::from_bytes(&[0xab; 20]).unwrap(),
            exact_name: exact,
        }
    }

    fn actions(menu: &RefMenu) -> Vec<RefMenuAction> {
        match menu {
            RefMenu::Items(items) => items.iter().map(|(_, action, _)| *action).collect(),
            RefMenu::NotExact(_) => Vec::new(),
        }
    }

    #[test]
    fn only_local_branches_and_tags_offer_a_delete() {
        assert_eq!(
            actions(&ref_menu(&entry(RefKind::LocalBranch, true))),
            vec![
                RefMenuAction::NewBranch,
                RefMenuAction::NewTag,
                RefMenuAction::DeleteBranch
            ]
        );
        assert_eq!(
            actions(&ref_menu(&entry(RefKind::Tag, true))),
            vec![
                RefMenuAction::NewBranch,
                RefMenuAction::NewTag,
                RefMenuAction::DeleteTag
            ]
        );
        assert_eq!(
            actions(&ref_menu(&entry(RefKind::RemoteBranch, true))),
            vec![RefMenuAction::NewBranch, RefMenuAction::NewTag]
        );
    }

    #[test]
    fn an_inexact_name_offers_nothing() {
        assert_eq!(
            ref_menu(&entry(RefKind::LocalBranch, false)),
            RefMenu::NotExact(NOT_EXACT_NOTE)
        );
    }

    #[test]
    fn the_titles_name_the_ref() {
        let RefMenu::Items(items) = ref_menu(&entry(RefKind::LocalBranch, true)) else {
            panic!("exact names have items");
        };
        assert_eq!(items[0].2, "New Branch from \u{201c}x\u{201d}\u{2026}");
        assert!(items[2].0, "a separator sits above Delete");
        assert_eq!(
            name_prompt(RefMenuAction::NewTag, &entry(RefKind::Tag, true)),
            Some((
                "New Tag",
                "Create a tag at \u{201c}x\u{201d} (abababa).".to_owned()
            ))
        );
        assert_eq!(
            name_prompt(RefMenuAction::DeleteTag, &entry(RefKind::Tag, true)),
            None
        );
        assert_eq!(verb(RefMenuAction::DeleteBranch), "Delete Branch");
    }
}

//! Where a repository opens (`AppDelegate.openTab`): the tab already showing it, an empty tab,
//! or a new one. Every arrival — argv, the system's open callback, the Open panel, a folder
//! drop, Open Recent, a launch restore — asks these two functions, so one repository never
//! gets two tabs.

/// What tells one tab apart from another, as the caller holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabIdentity<'a> {
    /// The path the tab shows, `None` for an empty tab.
    pub path: Option<&'a str>,
    /// The repository's tab key ([`crate::canonical_path::tab_key`]), `None` for an empty tab.
    pub key: Option<&'a str>,
}

/// Where an opened repository goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// The tab at this index already shows it; bring that tab forward.
    Existing(usize),
    /// The empty tab at this index takes it.
    Reuse(usize),
    /// No tab can take it; open a new one.
    New,
}

/// The tab already showing canonical `path`, checked before the repository is opened so the
/// same path as an open tab costs no open.
#[must_use]
pub fn by_path(tabs: &[TabIdentity<'_>], path: &str) -> Option<usize> {
    tabs.iter().position(|tab| tab.path == Some(path))
}

/// Where the repository whose tab key is `key` goes once opened: a tab with the same key (the
/// working directory and its `.git`, any spelling of either), else the first empty tab, else
/// a new one.
#[must_use]
pub fn route(tabs: &[TabIdentity<'_>], key: &str) -> Route {
    if let Some(index) = tabs.iter().position(|tab| tab.key == Some(key)) {
        return Route::Existing(index);
    }
    match tabs.iter().position(|tab| tab.path.is_none()) {
        Some(index) => Route::Reuse(index),
        None => Route::New,
    }
}

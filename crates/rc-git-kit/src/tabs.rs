//! Port of `OpenTabs.swift` and `ClosedTabs.swift`: the repositories open in tabs, saved so a
//! relaunch reopens them, and the in-memory stack ⇧⌘T reopens from.

use std::collections::HashSet;
use std::path::Path;

/// Where the open-tab list lives between launches. The app's implementation is backed by
/// `UserDefaults` under [`PATHS_KEY`] and [`SELECTED_KEY`]; tests use [`MemoryTabStore`].
pub trait TabStore {
    /// The saved paths in tab order; empty when nothing is saved or the stored value has
    /// another type (another build under the same bundle id may have written it).
    fn paths(&self) -> Vec<String>;
    /// The saved selection as stored, unchecked; 0 when absent or of another type.
    fn selected(&self) -> i64;
    /// Replaces both values.
    fn write(&mut self, paths: &[String], selected: usize);
    /// Forces pending writes out to disk; `false` when that failed.
    fn flush(&mut self) -> bool;
}

/// The key the saved paths live under.
pub const PATHS_KEY: &str = "rc-git.openTabs";
/// The key the saved selection lives under.
pub const SELECTED_KEY: &str = "rc-git.openTabs.selected";

/// A [`TabStore`] held in memory, for tests and for a run with no persistent store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryTabStore {
    /// The saved paths.
    pub paths: Vec<String>,
    /// The saved selection, as a store would hold it.
    pub selected: i64,
    /// What the next [`TabStore::flush`] reports.
    pub flush_fails: bool,
    /// How many times [`TabStore::flush`] ran.
    pub flushes: usize,
}

impl TabStore for MemoryTabStore {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn selected(&self) -> i64 {
        self.selected
    }

    fn write(&mut self, paths: &[String], selected: usize) {
        self.paths = paths.to_vec();
        self.selected = i64::try_from(selected).unwrap_or(0);
    }

    fn flush(&mut self) -> bool {
        self.flushes += 1;
        !self.flush_fails
    }
}

/// What a hand-off to a relaunching build managed, for the caller to log: the new build reads
/// the tabs within milliseconds, while this process is still alive, so an unflushed write
/// can leave it reading an older list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandOff {
    /// The tabs were recorded and flushed; `count` live tabs.
    Flushed {
        /// How many tabs were live.
        count: usize,
    },
    /// The tabs were recorded but the flush failed; the new build may read older tabs.
    NotFlushed {
        /// How many tabs were live.
        count: usize,
    },
}

/// The repositories open in tabs, in tab order plus which tab was selected.
#[derive(Debug, Clone, Default)]
pub struct OpenTabs<S> {
    store: S,
}

impl<S: TabStore> OpenTabs<S> {
    /// The tab list kept in `store`.
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// The store, for a caller that needs to inspect it.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// The saved paths in tab order.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        self.store.paths()
    }

    /// The saved selection, kept inside the saved list.
    #[must_use]
    pub fn selected_index(&self) -> usize {
        clamped_selection(self.store.selected(), self.paths().len())
    }

    /// Replaces the list. Only the restore at launch writes this way, to drop a repository
    /// that no longer opens (see [`restored_paths`]); everything else records through
    /// [`OpenTabs::record`].
    pub fn save(&mut self, paths: &[String], selected_index: usize) {
        let selected = clamped_selection(i64::try_from(selected_index).unwrap_or(-1), paths.len());
        self.store.write(paths, selected);
    }

    /// Records the tabs that are open now without dropping the ones that are not, except
    /// `dropping`: the repository of a tab closed on its own with ⌘W. ⇧⌘W on a whole window
    /// and ⌘Q drop nothing, so a relaunch reopens every repository they closed.
    pub fn record(&mut self, live: &[String], selected_index: usize, dropping: Option<&str>) {
        let mut saved = self.paths();
        if let Some(dropping) = dropping {
            saved.retain(|path| path != dropping);
        }
        self.save(&merged(live, &saved), selected_index);
    }

    /// Records for a build that is about to replace this one, and forces the write out.
    pub fn hand_off(&mut self, live: &[String], selected_index: usize) -> HandOff {
        self.record(live, selected_index, None);
        let count = live.len();
        if self.store.flush() {
            HandOff::Flushed { count }
        } else {
            HandOff::NotFlushed { count }
        }
    }
}

/// The tabs on screen followed by the saved ones that are not open any more, each path once.
/// Live order comes first, which is what records a drag reorder; a closed tab keeps its place
/// after them, so the selected index still counts from the live tabs.
#[must_use]
pub fn merged(live: &[String], saved: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::with_capacity(live.len() + saved.len());
    for path in live.iter().chain(saved) {
        if !result.contains(path) {
            result.push(path.clone());
        }
    }
    result
}

/// The list to save after a launch restore: the tabs that opened, then each saved path that
/// failed to open while it was unreachable (see [`mount_folder`]) — an external disk or a
/// network share not mounted yet. Such a path is kept for the next launch. A failed path
/// whose mount folder exists is dropped: the volume is there and the repository is not, so
/// retrying it on every launch would fail the same way.
#[must_use]
pub fn restored_paths(
    opened: &[String],
    failed: &[String],
    directory_exists: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut unreachable = Vec::new();
    for path in failed {
        if !directory_exists(&mount_folder(path)) {
            unreachable.push(path.clone());
        }
    }
    merged(opened, &unreachable)
}

const VOLUMES_FOLDER: &str = "/Volumes/";

/// The folder whose absence means `path` is unreachable rather than gone: the path itself
/// when it is a volume root (`/Volumes/Ext`), since `/Volumes` stays put while the disk is
/// unmounted; otherwise the folder that contains it, with a trailing slash.
#[must_use]
pub fn mount_folder(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let parent = match trimmed.rfind('/') {
        Some(slash) => &trimmed[..=slash],
        None => "./",
    };
    if parent == VOLUMES_FOLDER {
        path.to_owned()
    } else {
        parent.to_owned()
    }
}

/// Whether a folder exists at `path`, following symlinks.
#[must_use]
pub fn directory_exists(path: &str) -> bool {
    Path::new(path).is_dir()
}

/// The selection after a restore that skipped repositories which no longer open: the index
/// in `opened` of the path that was selected in `saved`, or the first tab when that path is
/// the one that failed to open.
///
/// `launch_document` is the repository whose opening launched the app (Finder, the Dock,
/// `open -a`). It is what the user asked for, so it stays selected over the saved choice.
#[must_use]
pub fn restored_selection(
    saved: &[String],
    selected_index: i64,
    opened: &[String],
    launch_document: Option<&str>,
) -> usize {
    if let Some(launch) = launch_document
        && let Some(index) = opened.iter().position(|path| path == launch)
    {
        return index;
    }
    let Some(path) = saved.get(clamped_selection(selected_index, saved.len())) else {
        return 0;
    };
    opened.iter().position(|open| open == path).unwrap_or(0)
}

/// Keeps a selection inside a list of `count`: an index that no longer exists (the repository
/// failed to reopen, the list shrank) falls back to the first tab.
#[must_use]
pub fn clamped_selection(index: i64, count: usize) -> usize {
    match usize::try_from(index) {
        Ok(index) if index < count => index,
        Ok(_) | Err(_) => 0,
    }
}

/// The repositories whose tabs were closed this run, most recent last, for ⇧⌘T. In memory
/// only: tabs closed by ⇧⌘W or ⌘Q stay in the saved tab list for the next launch, and a tab
/// closed with ⌘W was dropped from it on purpose, so ⇧⌘T cannot bring it back after a
/// relaunch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClosedTabs {
    paths: Vec<String>,
}

impl ClosedTabs {
    /// How many closed tabs are remembered.
    pub const CAPACITY: usize = 20;

    /// An empty stack.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The closed repositories, oldest first.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// Records a closed tab. A repository closed again moves to the top instead of stacking
    /// twice; the oldest entry falls off past [`Self::CAPACITY`].
    pub fn push(&mut self, path: &str) {
        self.paths.retain(|existing| existing != path);
        self.paths.push(path.to_owned());
        if self.paths.len() > Self::CAPACITY {
            let excess = self.paths.len() - Self::CAPACITY;
            self.paths.drain(..excess);
        }
    }

    /// Whether [`ClosedTabs::pop`] has anything to hand back.
    #[must_use]
    pub fn can_reopen(&self, open: &HashSet<String>) -> bool {
        self.paths.iter().any(|path| !open.contains(path))
    }

    /// The most recently closed repository that is not open now. Entries above it that were
    /// reopened some other way (Open Recent, a drop) are discarded on the way down.
    pub fn pop(&mut self, open: &HashSet<String>) -> Option<String> {
        while let Some(path) = self.paths.pop() {
            if !open.contains(&path) {
                return Some(path);
            }
        }
        None
    }
}

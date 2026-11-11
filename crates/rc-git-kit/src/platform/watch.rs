//! Port of the pure halves of `RefWatcher.swift` and `WorktreeWatcher.swift`: which
//! filesystem events mean a repository's refs or its change list may be stale, which
//! directories a stream is rooted at and which it excludes, plus the watcher traits. The
//! `FSEvents` streams live in `rc-git-mac`; tests drive [`ManualWatchers`].
//!
//! `FSEvents` reports canonical realpaths (`/private/tmp/...`, symlinks resolved), so every
//! directory is resolved with [`canonical_path::resolve`] once, when its filter is built, and
//! that spelling is both watched and filtered against.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::canonical_path;

/// How long the ref watcher's stream coalesces events after the first (`latency: 0.2`).
pub const REF_LATENCY: Duration = Duration::from_millis(200);
/// How long the worktree watcher's stream coalesces events after the first (`latency: 0.5`).
pub const WORKTREE_LATENCY: Duration = Duration::from_millis(500);

/// What a watcher calls when a relevant event arrives. Fires on the watcher's own thread or
/// queue; hop to the main thread before touching a model.
pub type OnChange = Arc<dyn Fn() + Send + Sync>;

/// A watcher's stream could not be created or started, so no change will be reported.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{call} failed for {path}: changes on disk will not refresh this window")]
pub struct WatcherStartError {
    /// The call that failed, e.g. `FSEventStreamCreate`.
    pub call: &'static str,
    /// The directory the watcher was asked to observe.
    pub path: String,
}

/// `path` without one trailing slash.
fn normalized(dir: &str) -> &str {
    dir.strip_suffix('/').unwrap_or(dir)
}

fn resolved(dir: &str) -> String {
    canonical_path::resolve(Path::new(dir))
        .to_string_lossy()
        .into_owned()
}

/// `path` relative to `root` without leading or trailing slashes, or `None` when `path` is
/// not `root` or inside it. Matches on a component boundary, so `.gitrefs` is not inside
/// `.git`.
fn relative<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    let root = normalized(root);
    let rest = path.strip_prefix(root)?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    Some(rest.trim_start_matches('/').trim_end_matches('/'))
}

/// Whether `name` is a git lock file. git spells the suffix in lower case only.
fn is_lock(name: &str) -> bool {
    name.as_bytes().ends_with(b".lock")
}

fn is_refs(relative: &str) -> bool {
    relative == "refs" || relative.starts_with("refs/")
}

/// Whether an event at `path` inside a repository's git directories can change its refs,
/// `HEAD`, or the upstream status (`RefWatchFilter.isRelevant`).
///
/// `git_dir` is this worktree's own git directory (`HEAD`, per-worktree refs); `common_dir`
/// is the one every worktree shares (branches, tags, remotes, stash, `packed-refs`,
/// `config`). Objects, the index, logs, lock files and other worktrees' `HEAD`s are dropped.
#[must_use]
pub fn is_ref_relevant(path: &str, git_dir: &str, common_dir: &str) -> bool {
    if let Some(own) = relative(path, git_dir)
        && !is_lock(own)
        && (own == "HEAD" || is_refs(own))
    {
        return true;
    }
    let Some(shared) = relative(path, common_dir) else {
        return false;
    };
    if is_lock(shared) {
        return false;
    }
    shared.is_empty() || shared == "packed-refs" || shared == "config" || is_refs(shared)
}

/// The repository's own exclude file, shared by every worktree.
#[must_use]
pub fn exclude_file(common_dir: &str) -> String {
    format!("{}/info/exclude", normalized(common_dir))
}

/// Whether some proper ancestor of the repository-relative `relative` is in `roots`. Checks
/// each ancestor, so the cost follows the path's depth, not the set's size.
fn is_below(relative: &str, roots: &HashSet<String>) -> bool {
    if roots.is_empty() {
        return false;
    }
    for (slash, _) in relative.match_indices('/') {
        if roots.contains(&relative[..slash]) {
            return true;
        }
    }
    false
}

/// The directories the latest status walk left unopened: pruned as ignored
/// (`StatusReport.ignoredDirs`) and holding their own repository (`nestedRepos`), both
/// repository-relative.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrunedRoots {
    /// Directories pruned as ignored.
    pub ignored: HashSet<String>,
    /// Untracked directories holding their own repository.
    pub nested: HashSet<String>,
}

/// The three directories a worktree watcher works from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDirs {
    /// The working directory.
    pub work_dir: String,
    /// This worktree's git directory.
    pub git_dir: String,
    /// The git directory every worktree shares (`git_dir` outside a linked worktree).
    pub common_dir: String,
}

/// Whether an event at `path` can change the outcome of `worktree_status`
/// (`WorktreeWatchFilter.isRelevant`).
///
/// Everything inside the git directory is dropped except the common dir's `info/exclude`,
/// which counts even when it lies outside the working directory (a linked worktree). Events
/// strictly below a pruned ignored directory are dropped; the directory's own path stays
/// relevant, since a file replacing it may not be ignored. Events below a nested repository
/// are dropped except at its `.git`, whose removal turns its files into untracked ones.
#[must_use]
pub fn is_worktree_relevant(path: &str, dirs: &WorktreeDirs, pruned: &PrunedRoots) -> bool {
    if path == exclude_file(&dirs.common_dir) {
        return true;
    }
    let root = normalized(&dirs.work_dir);
    let Some(rest) = path.strip_prefix(root) else {
        return false;
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        return false;
    }
    if relative(path, &dirs.git_dir).is_some() {
        return false;
    }
    let relative = rest.strip_prefix('/').unwrap_or(rest);
    if is_below(relative, &pruned.ignored) {
        return false;
    }
    if !is_below(relative, &pruned.nested) {
        return true;
    }
    relative
        .strip_suffix("/.git")
        .is_some_and(|repo| pruned.nested.contains(repo))
}

/// The `.git` subdirectories a worktree stream should never report: object, ref and reflog
/// churn. Not the whole `.git`, whose `info/exclude` stays relevant, and none when `.git`
/// lies outside the working directory (a linked worktree).
#[must_use]
pub fn excluded_paths(work_dir: &str, git_dir: &str) -> Vec<String> {
    let work = normalized(work_dir);
    let git = normalized(git_dir);
    if !git.starts_with(&format!("{work}/")) {
        return Vec::new();
    }
    let mut paths = Vec::new();
    for sub in ["objects", "refs", "logs"] {
        paths.push(format!("{git}/{sub}"));
    }
    paths
}

/// The directories a worktree stream is rooted at: the working directory, plus the common
/// dir's `info` folder when it lies outside it.
#[must_use]
pub fn watched_paths(work_dir: &str, common_dir: &str) -> Vec<String> {
    let work = normalized(work_dir);
    let info = format!("{}/info", normalized(common_dir));
    if info.starts_with(&format!("{work}/")) {
        return vec![work.to_owned()];
    }
    vec![work.to_owned(), info]
}

/// The ref watcher's filter over resolved directories. One stream rooted at the common dir
/// covers both: a linked worktree's own git directory is always `<common>/worktrees/<name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefFilter {
    git_dir: String,
    common_dir: String,
}

impl RefFilter {
    /// Resolves both directories to the spelling `FSEvents` reports.
    #[must_use]
    pub fn new(git_dir: &str, common_dir: &str) -> Self {
        Self {
            git_dir: resolved(git_dir),
            common_dir: resolved(common_dir),
        }
    }

    /// The directory the stream is rooted at.
    #[must_use]
    pub fn watched_path(&self) -> &str {
        &self.common_dir
    }

    /// Whether any of `paths` is relevant.
    #[must_use]
    pub fn any_relevant(&self, paths: &[String]) -> bool {
        paths
            .iter()
            .any(|path| is_ref_relevant(path, &self.git_dir, &self.common_dir))
    }
}

/// The worktree watcher's filter over resolved directories, with the pruned roots of the
/// latest scan behind a lock so a model can replace them from any thread while events arrive.
#[derive(Debug)]
pub struct WorktreeFilter {
    dirs: WorktreeDirs,
    pruned: Mutex<PrunedRoots>,
}

impl WorktreeFilter {
    /// Resolves every directory to the spelling `FSEvents` reports; resolving only the
    /// working directory would let every `.git` write past the exclusion.
    #[must_use]
    pub fn new(dirs: &WorktreeDirs) -> Self {
        Self {
            dirs: WorktreeDirs {
                work_dir: resolved(&dirs.work_dir),
                git_dir: resolved(&dirs.git_dir),
                common_dir: resolved(&dirs.common_dir),
            },
            pruned: Mutex::new(PrunedRoots::default()),
        }
    }

    /// The resolved directories.
    #[must_use]
    pub fn dirs(&self) -> &WorktreeDirs {
        &self.dirs
    }

    /// The directories the stream is rooted at.
    #[must_use]
    pub fn watched_paths(&self) -> Vec<String> {
        watched_paths(&self.dirs.work_dir, &self.dirs.common_dir)
    }

    /// The directories the stream excludes.
    #[must_use]
    pub fn excluded_paths(&self) -> Vec<String> {
        excluded_paths(&self.dirs.work_dir, &self.dirs.git_dir)
    }

    /// Replaces the directories whose contents no longer count as a change.
    pub fn set_pruned_dirs(&self, ignored: &[String], nested: &[String]) {
        let roots = PrunedRoots {
            ignored: ignored.iter().cloned().collect(),
            nested: nested.iter().cloned().collect(),
        };
        *self.pruned.lock().unwrap_or_else(PoisonError::into_inner) = roots;
    }

    /// Whether any of `paths` is relevant under the latest pruned roots.
    #[must_use]
    pub fn any_relevant(&self, paths: &[String]) -> bool {
        let pruned = self
            .pruned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        paths
            .iter()
            .any(|path| is_worktree_relevant(path, &self.dirs, &pruned))
    }
}

/// A running or stopped watcher. Dropping it stops it.
pub trait Watcher: Send + Sync {
    /// Starts delivering events. Later calls do nothing while a stream is running; after a
    /// failure, `start` may be called again.
    ///
    /// # Errors
    /// [`WatcherStartError`] naming the call that failed and the watched path.
    fn start(&self) -> Result<(), WatcherStartError>;

    /// Stops delivering events.
    fn stop(&self);

    /// Replaces the pruned directories (`StatusReport.ignoredDirs` and `nestedRepos` of the
    /// latest scan). Only a worktree watcher filters on them; a ref watcher ignores this.
    fn set_pruned_dirs(&self, ignored: &[String], nested: &[String]);
}

/// Makes watchers, so a model can be handed live `FSEvents` watchers or [`ManualWatchers`].
pub trait WatcherFactory: Send + Sync {
    /// A watcher reporting when the refs under `git_dir` and `common_dir` may have changed.
    fn ref_watcher(&self, git_dir: &str, common_dir: &str, on_change: OnChange)
    -> Box<dyn Watcher>;

    /// A watcher reporting when the change list of `dirs` may be stale.
    fn worktree_watcher(&self, dirs: &WorktreeDirs, on_change: OnChange) -> Box<dyn Watcher>;
}

enum ManualFilter {
    Refs(RefFilter),
    Worktree(WorktreeFilter),
}

struct ManualState {
    filter: ManualFilter,
    on_change: OnChange,
    running: Mutex<bool>,
}

impl ManualState {
    fn emit(&self, paths: &[String]) -> bool {
        if !*self.running.lock().unwrap_or_else(PoisonError::into_inner) {
            return false;
        }
        let relevant = match &self.filter {
            ManualFilter::Refs(filter) => filter.any_relevant(paths),
            ManualFilter::Worktree(filter) => filter.any_relevant(paths),
        };
        if relevant {
            (self.on_change)();
        }
        relevant
    }
}

/// A watcher whose events a test emits by hand, through the same filter the live watcher
/// uses.
pub struct ManualWatcher {
    state: Arc<ManualState>,
}

impl Watcher for ManualWatcher {
    fn start(&self) -> Result<(), WatcherStartError> {
        *self
            .state
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        Ok(())
    }

    fn stop(&self) {
        *self
            .state
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = false;
    }

    fn set_pruned_dirs(&self, ignored: &[String], nested: &[String]) {
        if let ManualFilter::Worktree(filter) = &self.state.filter {
            filter.set_pruned_dirs(ignored, nested);
        }
    }
}

impl Drop for ManualWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A [`WatcherFactory`] whose watchers fire only when a test calls [`ManualWatchers::emit`].
#[derive(Clone, Default)]
pub struct ManualWatchers {
    created: Arc<Mutex<Vec<Arc<ManualState>>>>,
}

impl fmt::Debug for ManualWatchers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManualWatchers")
            .field("created", &self.count())
            .finish()
    }
}

impl ManualWatchers {
    /// A factory that has made nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many watchers were made.
    #[must_use]
    pub fn count(&self) -> usize {
        self.created
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Delivers `paths` to every started watcher, as one `FSEvents` callback would; returns
    /// how many of them found an event relevant and called back.
    pub fn emit(&self, paths: &[&str]) -> usize {
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        let created = self
            .created
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut fired = 0;
        for state in created {
            if state.emit(&paths) {
                fired += 1;
            }
        }
        fired
    }

    fn make(&self, filter: ManualFilter, on_change: OnChange) -> Box<dyn Watcher> {
        let state = Arc::new(ManualState {
            filter,
            on_change,
            running: Mutex::new(false),
        });
        self.created
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(state.clone());
        Box::new(ManualWatcher { state })
    }
}

impl WatcherFactory for ManualWatchers {
    fn ref_watcher(
        &self,
        git_dir: &str,
        common_dir: &str,
        on_change: OnChange,
    ) -> Box<dyn Watcher> {
        self.make(
            ManualFilter::Refs(RefFilter::new(git_dir, common_dir)),
            on_change,
        )
    }

    fn worktree_watcher(&self, dirs: &WorktreeDirs, on_change: OnChange) -> Box<dyn Watcher> {
        self.make(ManualFilter::Worktree(WorktreeFilter::new(dirs)), on_change)
    }
}

#[cfg(test)]
mod tests;

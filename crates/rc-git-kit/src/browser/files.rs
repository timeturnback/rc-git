//! The file pane and the working-tree browse: opening a blob, a disk file or a path a link
//! names, "Load anyway", and the tree rows of one working-tree directory.

use std::borrow::Cow;
use std::sync::Arc;

use gpui::{Context, Task};
use rc_git_core::bstr::{BString, ByteSlice};
use rc_git_core::{
    BlobContent, DEFAULT_BLOB_LIMIT, EntryKind, Oid, TreeEntry, WorktreeListing, WorktreeStatus,
};

use super::{BrowserEvent, BrowserModel, OpenedFileSource, call, null_oid, with};
use crate::blocking::blocking;
use crate::hidden_files::is_hidden_name;
use crate::models::{BrowseRow, path_refusal};
use crate::path_search::PathIndexEntry;
use crate::repository::{ReadResult, RepositoryError, RepositoryReading};
use crate::text_document::{FileReveal, HARD_LIMIT, TextDocument};
use crate::tree_store::{Listing, TreeStore};

/// The file in the file pane: its name as stored, its blob, and what it is. A file read from
/// disk has no blob id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// The name as stored, which git does not require to be UTF-8.
    pub name: BString,
    /// The blob; `None` for a file read from disk.
    pub oid: Option<Oid>,
    /// What the entry is.
    pub kind: EntryKind,
}

impl FileEntry {
    /// The name for display; invalid UTF-8 becomes U+FFFD.
    #[must_use]
    pub fn name_lossy(&self) -> Cow<'_, str> {
        self.name.to_str_lossy()
    }

    /// Whether the name is valid UTF-8, so its display spelling names this entry alone.
    #[must_use]
    pub fn has_exact_name(&self) -> bool {
        self.name.to_str().is_ok()
    }
}

impl From<&TreeEntry> for FileEntry {
    fn from(entry: &TreeEntry) -> Self {
        Self {
            name: entry.name.clone(),
            oid: Some(entry.oid),
            kind: entry.kind,
        }
    }
}

impl From<&PathIndexEntry> for FileEntry {
    fn from(entry: &PathIndexEntry) -> Self {
        Self {
            name: entry.name().into(),
            oid: entry.oid,
            kind: entry.kind,
        }
    }
}

/// What the file pane shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// No file.
    Empty,
    /// The file is loading.
    Loading(FileEntry),
    /// A text file.
    Text(FileEntry, Arc<TextDocument>),
    /// A binary file of this many bytes.
    Binary(FileEntry, u64),
    /// A file over the limit it was read with.
    TooLarge {
        /// The file.
        entry: FileEntry,
        /// Its size in bytes.
        size: u64,
        /// The limit it is over.
        limit: u64,
    },
    /// The read failed; the message says why.
    Failed(FileEntry, String),
}

/// How [`BrowserModel::open_path`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenPathOutcome {
    /// The file is open.
    Opened,
    /// Nothing openable lives at the path.
    Missing,
    /// Reading the trees or directories on the way failed with this message.
    Failed(String),
}

/// A file to open from the working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeFileOpen {
    /// The file name for display.
    pub name: String,
    /// The repository-relative path read from disk.
    pub path: String,
    /// False when a component of `path` arrived with U+FFFD replacements; such a file is not
    /// read, since the read goes by path and could land on another file.
    pub path_is_exact: bool,
    /// The file's status against `HEAD` when the opener knows it; an untracked file cannot be
    /// blamed.
    pub status: Option<WorktreeStatus>,
    /// How much is decoded.
    pub limit: u64,
}

impl WorktreeFileOpen {
    /// An exact path of unknown status, read up to the default limit.
    #[must_use]
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            path_is_exact: true,
            status: None,
            limit: DEFAULT_BLOB_LIMIT,
        }
    }

    /// The same open with the file's known status.
    #[must_use]
    pub fn status(mut self, status: WorktreeStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// The same open of a path that carries U+FFFD replacements.
    #[must_use]
    pub fn inexact(mut self) -> Self {
        self.path_is_exact = false;
        self
    }

    /// The same open with another limit.
    #[must_use]
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self
    }
}

/// A blob to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobOpen {
    /// The entry to read.
    pub entry: FileEntry,
    /// The repository-relative location, kept for blame and history; `None` when unknown.
    pub path: Option<String>,
    /// False when a folder on the way arrived with U+FFFD replacements; the entry's own name
    /// counts too.
    pub path_is_exact: bool,
    /// How much is decoded; pass more to load a file the default limit rejected.
    pub limit: u64,
}

impl BlobOpen {
    /// An open of `entry` at no known path, read up to the default limit.
    #[must_use]
    pub fn new(entry: impl Into<FileEntry>) -> Self {
        Self {
            entry: entry.into(),
            path: None,
            path_is_exact: true,
            limit: DEFAULT_BLOB_LIMIT,
        }
    }

    /// The same open at `path`.
    #[must_use]
    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// The same open of a path that carries U+FFFD replacements.
    #[must_use]
    pub fn inexact(mut self) -> Self {
        self.path_is_exact = false;
        self
    }

    /// The same open with another limit.
    #[must_use]
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self
    }
}

/// Maps read content to a pane state. Building a [`TextDocument`] walks the whole content, so
/// the opens call this off the UI thread.
#[must_use]
pub fn state_for(entry: FileEntry, content: BlobContent) -> FileState {
    match content {
        BlobContent::Text {
            data,
            line_starts,
            valid_utf8,
        } => FileState::Text(
            entry,
            Arc::new(TextDocument::new(data, &line_starts, valid_utf8)),
        ),
        BlobContent::Binary { size } => FileState::Binary(entry, size),
        BlobContent::TooLarge { size, limit } => FileState::TooLarge { entry, size, limit },
    }
}

/// Merges a working-tree listing into display order: directories first, then names ignoring
/// case, deleted files in their sorted place. Each name is lowercased once, not once per
/// comparison. A disk entry the compared tree lacks carries git's null id, which nothing reads.
#[must_use]
pub fn rows(listing: &WorktreeListing) -> Vec<BrowseRow> {
    let Some(unhashed) = null_oid() else {
        return Vec::new();
    };
    let mut keyed: Vec<(bool, String, BrowseRow)> =
        Vec::with_capacity(listing.entries.len() + listing.missing.len());
    for entry in &listing.entries {
        let row = BrowseRow {
            entry: TreeEntry {
                name: entry.name.clone(),
                oid: entry.oid.unwrap_or(unhashed),
                kind: entry.kind,
            },
            status: Some(entry.status),
            missing: false,
        };
        let key = entry.name.to_str_lossy().to_lowercase();
        keyed.push((entry.kind == EntryKind::Tree, key, row));
    }
    for entry in &listing.missing {
        let row = BrowseRow {
            entry: entry.clone(),
            status: None,
            missing: true,
        };
        let key = entry.name.to_str_lossy().to_lowercase();
        keyed.push((entry.kind == EntryKind::Tree, key, row));
    }
    keyed.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut sorted = Vec::with_capacity(keyed.len());
    for (_, _, row) in keyed {
        sorted.push(row);
    }
    sorted
}

/// The status of the file `path` names in the working directory, listing one directory per
/// component. `None` when a component is missing or the path names a directory.
pub(super) fn worktree_file_status(
    repository: &dyn RepositoryReading,
    root: Oid,
    path: &str,
) -> ReadResult<Option<WorktreeStatus>> {
    let components: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let mut dir = String::new();
    for (offset, component) in components.iter().enumerate() {
        let listing = repository.worktree_entries(Some(root), &dir)?;
        let Some(found) = listing
            .entries
            .iter()
            .find(|entry| entry.name == component.as_bytes())
        else {
            return Ok(None);
        };
        let is_dir = found.kind == EntryKind::Tree;
        if offset + 1 == components.len() {
            return Ok((!is_dir).then_some(found.status));
        }
        if !is_dir {
            return Ok(None);
        }
        if !dir.is_empty() {
            dir.push('/');
        }
        dir.push_str(component);
    }
    Ok(None)
}

/// The blob at `path` below `root`, reading one tree object per component through the shared
/// cache. `None` when a component is missing or the path names a directory.
fn blob_at(
    repository: &dyn RepositoryReading,
    store: &TreeStore,
    root: Oid,
    path: &str,
) -> ReadResult<Option<TreeEntry>> {
    let components: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let mut current = root;
    for (offset, component) in components.iter().enumerate() {
        let entries = store.entries(current, |tree| repository.tree_entries(tree))?;
        let Some(found) = entries
            .iter()
            .find(|entry| entry.name == component.as_bytes())
        else {
            return Ok(None);
        };
        let is_dir = found.kind == EntryKind::Tree;
        if offset + 1 == components.len() {
            return Ok((!is_dir).then(|| found.clone()));
        }
        if !is_dir {
            return Ok(None);
        }
        current = found.oid;
    }
    Ok(None)
}

impl BrowserModel {
    /// Shows or hides the entries [`crate::hidden_files`] names in the tree and path search.
    /// Nothing reloads: the tree keeps every row it read and asks [`Self::is_visible`] again.
    pub fn set_shows_hidden_files(&mut self, shows: bool, cx: &mut Context<Self>) {
        self.shows_hidden_files = shows;
        cx.notify();
    }

    /// Whether a tree row is on screen under the current hidden-files setting. A row's name
    /// is enough: a hidden folder's children are never shown, because the folder is not.
    #[must_use]
    pub fn is_visible(&self, row: &BrowseRow) -> bool {
        self.shows_hidden_files || !is_hidden_name(&row.entry.name)
    }

    /// One working-tree directory as display rows, in [`rows`] order. Not cached: disk is the
    /// one thing content addressing cannot cover.
    ///
    /// # Errors
    /// When no repository is open, or the directory cannot be listed; the message names it.
    pub fn worktree_rows(
        &self,
        dir: &str,
        cx: &mut Context<Self>,
    ) -> Task<ReadResult<Vec<BrowseRow>>> {
        let Some(repository) = self.repository.clone() else {
            return Task::ready(Err(RepositoryError::new(
                "list working tree: no repository is open",
            )));
        };
        let Some(root) = self.root_tree else {
            return Task::ready(Ok(Vec::new()));
        };
        let dir = dir.to_owned();
        blocking(cx.background_executor(), move || {
            repository
                .worktree_entries(Some(root), &dir)
                .map(|listing| rows(&listing))
        })
    }

    /// Children of a directory, from the cache when possible. Safe for any tree id, including
    /// one from a ref that is no longer selected.
    ///
    /// # Errors
    /// When no repository is open or the tree cannot be read; a failed read is not cached.
    pub fn children(&self, tree: Oid, cx: &mut Context<Self>) -> Task<ReadResult<Listing>> {
        if let Some(cached) = self.trees.cached(tree) {
            return Task::ready(Ok(cached));
        }
        let Some(repository) = self.repository.clone() else {
            return Task::ready(Err(RepositoryError::new(format!(
                "read tree {tree}: no repository is open"
            ))));
        };
        let store = Arc::clone(&self.trees);
        blocking(cx.background_executor(), move || {
            store.entries(tree, |tree| repository.tree_entries(tree))
        })
    }

    /// Opens a file straight from the working directory into the file pane.
    pub fn open_worktree_file(
        &mut self,
        open: WorktreeFileOpen,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        if self.repository.is_none() {
            return Task::ready(());
        }
        let WorktreeFileOpen {
            name,
            path,
            path_is_exact,
            status,
            limit,
        } = open;
        let entry = FileEntry {
            name: name.as_str().into(),
            oid: None,
            kind: EntryKind::Blob,
        };
        self.opened_file_source = Some(OpenedFileSource::Disk { name, status });
        self.opened_file_path = Some(path.clone());
        self.opened_path_is_exact = path_is_exact;
        self.opened_file_limit = limit;
        self.load_file(
            entry,
            move |repository| {
                if !path_is_exact {
                    return Err(RepositoryError::new(path_refusal("read", &path)));
                }
                repository.worktree_file(&path, limit)
            },
            cx,
        )
    }

    /// Opens a row of the file tree. A working-tree row still on disk reads from disk; every
    /// other row, a file deleted from disk included, reads its blob.
    pub fn open_tree_row(
        &mut self,
        row: &BrowseRow,
        path: &str,
        path_is_exact: bool,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        if self.browsing_working_tree && !row.missing {
            let mut open = WorktreeFileOpen::new(row.entry.name_lossy(), path);
            open.path_is_exact = path_is_exact;
            open.status = row.status;
            return self.open_worktree_file(open, cx);
        }
        let mut open = BlobOpen::new(&row.entry).at(path);
        open.path_is_exact = path_is_exact;
        self.open_file(open, cx)
    }

    /// Loads a blob into the file pane. A directory is ignored.
    pub fn open_file(&mut self, open: BlobOpen, cx: &mut Context<Self>) -> Task<()> {
        let BlobOpen {
            entry,
            path,
            path_is_exact,
            limit,
        } = open;
        if self.repository.is_none() || entry.kind == EntryKind::Tree {
            return Task::ready(());
        }
        self.opened_file_source = Some(OpenedFileSource::Blob(entry.clone()));
        self.opened_file_path = path;
        self.opened_path_is_exact = path_is_exact && entry.has_exact_name();
        self.opened_file_limit = limit;
        let oid = entry.oid;
        let name = entry.name_lossy().into_owned();
        self.load_file(
            entry,
            move |repository| match oid {
                Some(oid) => repository.read_blob(oid, limit),
                None => Err(RepositoryError::new(format!(
                    "read blob '{name}': it has no blob id"
                ))),
            },
            cx,
        )
    }

    /// The steps every open shares: a new generation, the loading state, a location report,
    /// then `read` off the UI thread, applied only while no newer open has started.
    fn load_file(
        &mut self,
        entry: FileEntry,
        read: impl FnOnce(&dyn RepositoryReading) -> ReadResult<BlobContent> + Send + 'static,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let Some(repository) = self.repository.clone() else {
            return Task::ready(());
        };
        let generation = self.bump_file_generation(cx);
        self.pending_reveal = self.reveal_for_next_load.take();
        self.file_state = FileState::Loading(entry.clone());
        Self::changed(BrowserEvent::FileChanged, cx);
        self.note_location(cx);
        let read_entry = entry.clone();
        let work = blocking(cx.background_executor(), move || {
            read(&*repository).map(|content| state_for(read_entry, content))
        });
        Self::run(cx, (), async move |this, cx| {
            let result = work.await;
            with(&this, cx, |model, cx| {
                if generation != model.generations.file {
                    return;
                }
                model.file_state = match result {
                    Ok(state) => state,
                    Err(error) => {
                        model.pending_reveal = None;
                        FileState::Failed(entry, error.message().to_owned())
                    }
                };
                Self::changed(BrowserEvent::FileChanged, cx);
            });
        })
    }

    /// Re-reads the file on screen with a larger limit, from where it was opened: the working
    /// directory for a file read from disk, its blob otherwise. The limit never exceeds
    /// [`HARD_LIMIT`], whatever the caller asks for.
    pub fn load_anyway(&mut self, requested: u64, cx: &mut Context<Self>) -> Task<()> {
        let limit = requested.min(HARD_LIMIT);
        match self.opened_file_source.clone() {
            None => Task::ready(()),
            Some(OpenedFileSource::Disk { name, status }) => {
                let Some(path) = self.opened_file_path.clone() else {
                    return Task::ready(());
                };
                let open = WorktreeFileOpen {
                    name,
                    path,
                    path_is_exact: self.opened_path_is_exact,
                    status,
                    limit,
                };
                self.open_worktree_file(open, cx)
            }
            Some(OpenedFileSource::Blob(entry)) => {
                let open = BlobOpen {
                    entry,
                    path: self.opened_file_path.clone(),
                    path_is_exact: self.opened_path_is_exact,
                    limit,
                };
                self.open_file(open, cx)
            }
        }
    }

    /// Opens the file at a repository-relative path, the way a markdown link asks for one.
    /// [`OpenPathOutcome::Missing`] when the path names nothing openable and
    /// [`OpenPathOutcome::Failed`] when a read on the way failed; either way the pane and the
    /// location stay on the previous file. `fragment` becomes the pending reveal only once the
    /// file load starts. Resolution reads one tree object, or one working-tree directory, per
    /// path component.
    pub fn open_path(
        &mut self,
        path: &str,
        fragment: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<OpenPathOutcome> {
        self.pending_reveal = None;
        let (Some(repository), Some(root)) = (self.repository.clone(), self.root_tree) else {
            return Task::ready(OpenPathOutcome::Missing);
        };
        if path.is_empty() {
            return Task::ready(OpenPathOutcome::Missing);
        }
        if self.browsing_working_tree {
            return Self::open_worktree_path(path, fragment, repository, root, cx);
        }
        let store = Arc::clone(&self.trees);
        let owned = path.to_owned();
        let found = blocking(cx.background_executor(), move || {
            blob_at(&*repository, &store, root, &owned)
        });
        let path = path.to_owned();
        Self::run(cx, OpenPathOutcome::Missing, async move |this, cx| {
            let entry = match found.await {
                Err(error) => return OpenPathOutcome::Failed(error.message().to_owned()),
                Ok(None) => return OpenPathOutcome::Missing,
                Ok(Some(entry)) => entry,
            };
            call(&this, cx, |model, cx| {
                model.reveal_for_next_load = fragment.map(FileReveal::Fragment);
                let open = model.open_file(BlobOpen::new(&entry).at(path), cx);
                model.reveal_for_next_load = None;
                open
            })
            .await;
            OpenPathOutcome::Opened
        })
    }

    /// [`Self::open_path`] over the working directory: confirms on disk that `path` names a
    /// file before the pane changes, so a dead link neither replaces the open file nor records
    /// a stop.
    fn open_worktree_path(
        path: &str,
        fragment: Option<String>,
        repository: Arc<dyn RepositoryReading>,
        root: Oid,
        cx: &mut Context<Self>,
    ) -> Task<OpenPathOutcome> {
        let owned = path.to_owned();
        let lookup = blocking(cx.background_executor(), move || {
            worktree_file_status(&*repository, root, &owned)
        });
        let path = path.to_owned();
        Self::run(cx, OpenPathOutcome::Missing, async move |this, cx| {
            let status = match lookup.await {
                Err(error) => return OpenPathOutcome::Failed(error.message().to_owned()),
                Ok(None) => return OpenPathOutcome::Missing,
                Ok(Some(status)) => status,
            };
            let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
            let open = WorktreeFileOpen::new(name, path).status(status);
            call(&this, cx, |model, cx| {
                model.reveal_for_next_load = fragment.map(FileReveal::Fragment);
                let task = model.open_worktree_file(open, cx);
                model.reveal_for_next_load = None;
                task
            })
            .await;
            let failed = with(&this, cx, |model, _| match &model.file_state {
                FileState::Failed(_, message) => Some(message.clone()),
                FileState::Empty
                | FileState::Loading(_)
                | FileState::Text(..)
                | FileState::Binary(..)
                | FileState::TooLarge { .. } => None,
            })
            .flatten();
            failed.map_or(OpenPathOutcome::Opened, OpenPathOutcome::Failed)
        })
    }

    /// Hands the pane the place to scroll to once the file it was asked for has painted, and
    /// forgets it, so a repaint of the same file does not jump back.
    pub fn take_pending_reveal(&mut self) -> Option<FileReveal> {
        self.pending_reveal.take()
    }

    /// Empties the file pane, for a location that had no file open.
    pub(super) fn close_file(&mut self, cx: &mut Context<Self>) {
        self.bump_file_generation(cx);
        self.file_state = FileState::Empty;
        self.pending_reveal = None;
        self.opened_file_path = None;
        self.opened_file_source = None;
        Self::changed(BrowserEvent::FileChanged, cx);
        self.note_location(cx);
    }
}

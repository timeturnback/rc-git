//! The working directory as a read-only view: files on disk with a status overlay against
//! `HEAD`. Nothing here writes; hashing a file to compare it with a blob reads it once.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::worktree::stack::state::ignore::Source;

use crate::blob::{BlobContent, is_binary, line_starts};
use crate::error::{BoxError, Error, Result, git, object};
use crate::oid::Oid;
use crate::repo::Repository;
use crate::tree::{EntryKind, TreeEntry, sort_for_display, tree_children};

/// How a file on disk relates to the same path in the compared tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorktreeStatus {
    /// Content and kind match the tree entry.
    Unchanged,
    /// The path exists in both but content or kind differs.
    Modified,
    /// The path exists only on disk.
    Untracked,
    /// The path exists only on disk and matches an ignore rule.
    Ignored,
    /// The path exists only in the compared tree; the file was deleted from disk.
    Deleted,
}

/// One entry of a working-tree directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// File name as stored on disk.
    pub name: BString,
    /// What the entry is on disk.
    pub kind: EntryKind,
    /// How it relates to the compared tree.
    pub status: WorktreeStatus,
    /// The blob at the same path in the compared tree, when there is one.
    pub oid: Option<Oid>,
    /// Size in bytes; 0 for directories.
    pub size: u64,
}

/// A working-tree directory: what is on disk, plus what the compared tree has that disk lost.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeListing {
    /// Entries on disk, ordered for display: directories first, then names.
    pub entries: Vec<WorktreeEntry>,
    /// Tree entries with no counterpart on disk: deleted files.
    pub missing: Vec<TreeEntry>,
}

/// One changed path found by [`Repository::worktree_status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// Repository-relative path with `/` separators.
    pub path: BString,
    /// The kind on disk; the tree's kind for [`WorktreeStatus::Deleted`].
    pub kind: EntryKind,
    /// Always `Modified`, `Untracked`, or `Deleted`; clean and ignored paths are not listed.
    pub status: WorktreeStatus,
    /// The blob at the same path in the compared tree; `None` for untracked files and for a
    /// file that replaced a directory.
    pub oid: Option<Oid>,
    /// Size on disk in bytes; 0 for deleted files.
    pub size: u64,
}

/// Every changed path in the working tree, with the walk's own cost accounting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusReport {
    /// Changed paths, sorted by path.
    pub entries: Vec<StatusEntry>,
    /// Entries visited before the walk finished or hit its cap.
    pub scanned: usize,
    /// Whether the walk stopped at `max_scan` before covering the whole tree.
    pub truncated: bool,
}

impl Repository {
    /// Lists one directory of the working tree, comparing each entry with the tree at the
    /// same path below `tree`. `dir` is repository-relative with `/` separators; empty for
    /// the root. Reads one directory and one tree object, plus one content read per file
    /// whose size matches its blob, so the cost stays proportional to the listing.
    ///
    /// # Errors
    /// [`Error::Worktree`] for a bare repository or an unreadable directory,
    /// [`Error::Object`] when the compared tree cannot be read.
    pub fn worktree_entries(&self, tree: Oid, dir: &str) -> Result<WorktreeListing> {
        let work_dir = self.require_work_dir("list working tree", dir)?;
        let tree_entries = self.tree_entries_at(tree, dir)?;
        let disk_dir = join_under(&work_dir, dir)
            .map_err(|source| worktree_error("list working tree", dir, source))?;
        let read = std::fs::read_dir(&disk_dir)
            .map_err(|source| worktree_error("list working tree", dir, Box::new(source)))?;

        let mut listing = WorktreeListing::default();
        let mut on_disk: Vec<(BString, EntryKind, u64, PathBuf)> = Vec::new();
        for entry in read {
            let entry = entry
                .map_err(|source| worktree_error("list working tree", dir, Box::new(source)))?;
            let name = BString::from(entry.file_name().as_encoded_bytes());
            if dir.is_empty() && name == ".git" {
                continue;
            }
            let meta = std::fs::symlink_metadata(entry.path()).map_err(|source| {
                worktree_error("stat working tree entry", dir, Box::new(source))
            })?;
            on_disk.push((name, disk_kind(&meta), meta.len(), entry.path()));
        }

        self.with(|repo| {
            let mut excludes = ExcludeCheck::new(repo);
            for (name, kind, size, path) in on_disk {
                let in_tree = tree_entries.iter().find(|entry| entry.name == name);
                let status = if let Some(tree_entry) = in_tree {
                    compare(repo, tree_entry, kind, size, &path, dir)?
                } else {
                    let rela = relative_path(dir, &name);
                    if excludes.is_ignored(&rela, kind == EntryKind::Tree) {
                        WorktreeStatus::Ignored
                    } else {
                        WorktreeStatus::Untracked
                    }
                };
                listing.entries.push(WorktreeEntry {
                    name,
                    kind,
                    status,
                    oid: in_tree
                        .and_then(|entry| (entry.kind != EntryKind::Tree).then_some(entry.oid)),
                    size,
                });
            }
            Ok(())
        })?;

        for tree_entry in &tree_entries {
            if !listing
                .entries
                .iter()
                .any(|entry| entry.name == tree_entry.name)
            {
                listing.missing.push(tree_entry.clone());
            }
        }
        sort_worktree(&mut listing.entries);
        sort_for_display(&mut listing.missing);
        Ok(listing)
    }

    /// Walks the whole working tree against `tree` and returns every changed path: modified,
    /// untracked, and deleted files, sorted by path. Clean and ignored paths are not listed,
    /// and an ignored directory is pruned without reading it, so `node_modules` never costs a
    /// `read_dir`. The walk stops after visiting `max_scan` entries and reports the
    /// truncation, which bounds both the disk work and how long the borrowed pool handle is
    /// held.
    ///
    /// # Errors
    /// [`Error::Worktree`] for a bare repository or an unreadable directory,
    /// [`Error::Object`] when a tree object on the walk cannot be read.
    pub fn worktree_status(&self, tree: Oid, max_scan: usize) -> Result<StatusReport> {
        let work_dir = self.require_work_dir("scan working tree", "")?;
        self.with(|repo| {
            let mut walk = StatusWalk {
                repo,
                excludes: ExcludeCheck::new(repo),
                report: StatusReport::default(),
                max_scan,
            };
            walk.walk_dir(Some(tree), &BString::from(""), &work_dir)?;
            let mut report = walk.report;
            report.entries.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(report)
        })
    }

    /// Reads one working-tree file for display, with the same size cap and binary probe as
    /// [`Repository::read_blob`]. A symlink yields its target as text.
    ///
    /// # Errors
    /// [`Error::Worktree`] for a bare repository, a missing file, or an unreadable one.
    pub fn worktree_file(&self, path: &str, limit: u64) -> Result<BlobContent> {
        let work_dir = self.require_work_dir("read working tree file", path)?;
        let full = join_under(&work_dir, path)
            .map_err(|source| worktree_error("read working tree file", path, source))?;
        let meta = std::fs::symlink_metadata(&full)
            .map_err(|source| worktree_error("read working tree file", path, Box::new(source)))?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&full)
                .map_err(|source| worktree_error("read symlink", path, Box::new(source)))?;
            let data = target.into_os_string().into_encoded_bytes();
            let starts = line_starts(&data);
            let valid_utf8 = std::str::from_utf8(&data).is_ok();
            return Ok(BlobContent::Text {
                data,
                line_starts: starts,
                valid_utf8,
            });
        }
        if meta.is_dir() {
            return Err(worktree_error(
                "read working tree file",
                path,
                "is a directory".into(),
            ));
        }
        let size = meta.len();
        if size > limit {
            return Ok(BlobContent::TooLarge { size, limit });
        }
        let data = std::fs::read(&full)
            .map_err(|source| worktree_error("read working tree file", path, Box::new(source)))?;
        if is_binary(&data) {
            return Ok(BlobContent::Binary { size });
        }
        let starts = line_starts(&data);
        let valid_utf8 = std::str::from_utf8(&data).is_ok();
        Ok(BlobContent::Text {
            data,
            line_starts: starts,
            valid_utf8,
        })
    }

    fn require_work_dir(&self, op: &'static str, path: &str) -> Result<PathBuf> {
        self.work_dir().map(PathBuf::from).ok_or_else(|| {
            worktree_error(
                op,
                path,
                "the repository is bare and has no working tree".into(),
            )
        })
    }

    /// The tree entries at `dir` below `tree`; empty when the directory is not in the tree.
    fn tree_entries_at(&self, tree: Oid, dir: &str) -> Result<Vec<TreeEntry>> {
        if dir.trim_matches('/').is_empty() {
            return self.tree_entries(tree);
        }
        match self.entry_at_path(tree, dir)? {
            Some(entry) if entry.kind == EntryKind::Tree => self.tree_entries(entry.oid),
            Some(_) | None => Ok(Vec::new()),
        }
    }
}

/// Whether a disk file differs from its tree entry. Sizes decide first; only equal sizes
/// hash the file, streaming, so memory stays constant.
fn compare(
    repo: &gix::Repository,
    tree_entry: &TreeEntry,
    disk_kind: EntryKind,
    disk_size: u64,
    disk_path: &std::path::Path,
    dir: &str,
) -> Result<WorktreeStatus> {
    if tree_entry.kind == EntryKind::Tree || disk_kind == EntryKind::Tree {
        return Ok(if tree_entry.kind == disk_kind {
            WorktreeStatus::Unchanged
        } else {
            WorktreeStatus::Modified
        });
    }
    if tree_entry.kind == EntryKind::Submodule {
        return Ok(WorktreeStatus::Unchanged);
    }
    if tree_entry.kind != disk_kind {
        return Ok(WorktreeStatus::Modified);
    }
    if disk_kind == EntryKind::Symlink {
        let target = std::fs::read_link(disk_path)
            .map_err(|source| worktree_error("read symlink", dir, Box::new(source)))?;
        let blob = repo
            .find_blob(*tree_entry.oid.as_gix())
            .map_err(object("read blob", tree_entry.oid))?;
        return Ok(if blob.data == target.as_os_str().as_encoded_bytes() {
            WorktreeStatus::Unchanged
        } else {
            WorktreeStatus::Modified
        });
    }
    let header = repo
        .find_header(*tree_entry.oid.as_gix())
        .map_err(object("read blob header", tree_entry.oid))?;
    if header.size() != disk_size {
        return Ok(WorktreeStatus::Modified);
    }
    let mut file = std::fs::File::open(disk_path)
        .map_err(|source| worktree_error("hash working tree file", dir, Box::new(source)))?;
    let hashed = gix::objs::compute_stream_hash(
        repo.object_hash(),
        gix::objs::Kind::Blob,
        &mut file,
        disk_size,
        &mut gix::progress::Discard,
        &AtomicBool::new(false),
    )
    .map_err(git("hash working tree file"))?;
    Ok(if Oid::from_gix(hashed) == tree_entry.oid {
        WorktreeStatus::Unchanged
    } else {
        WorktreeStatus::Modified
    })
}

/// Ignore-rule lookup, built lazily because most listings have no untracked entries.
struct ExcludeCheck<'repo> {
    repo: &'repo gix::Repository,
    state: ExcludeState<'repo>,
}

enum ExcludeState<'repo> {
    Unbuilt,
    Unavailable,
    Ready(Box<gix::AttributeStack<'repo>>),
}

impl<'repo> ExcludeCheck<'repo> {
    fn new(repo: &'repo gix::Repository) -> Self {
        Self {
            repo,
            state: ExcludeState::Unbuilt,
        }
    }

    /// Whether `rela_path` matches an ignore rule. Failing to build or query the stack means
    /// "not ignored": the entry then shows as untracked, which never hides anything.
    fn is_ignored(&mut self, rela_path: &BString, is_dir: bool) -> bool {
        if let ExcludeState::Unbuilt = self.state {
            self.state = match Self::build(self.repo) {
                Some(attributes) => ExcludeState::Ready(Box::new(attributes)),
                None => ExcludeState::Unavailable,
            };
        }
        let attributes = match &mut self.state {
            ExcludeState::Ready(attributes) => attributes,
            ExcludeState::Unbuilt | ExcludeState::Unavailable => return false,
        };
        let mode = if is_dir {
            gix::index::entry::Mode::DIR
        } else {
            gix::index::entry::Mode::FILE
        };
        attributes
            .at_entry(rela_path.as_bstr(), Some(mode))
            .is_ok_and(|platform| platform.is_excluded())
    }

    fn build(repo: &'repo gix::Repository) -> Option<gix::AttributeStack<'repo>> {
        let index = repo.index_or_empty().ok()?;
        repo.excludes(&index, None, Source::WorktreeThenIdMappingIfNotSkipped)
            .ok()
    }
}

/// Recursive full-tree status walk. Holds the borrowed handle for its whole run; the
/// caller's `max_scan` cap bounds that.
struct StatusWalk<'repo> {
    repo: &'repo gix::Repository,
    excludes: ExcludeCheck<'repo>,
    report: StatusReport,
    max_scan: usize,
}

impl StatusWalk<'_> {
    /// Counts one visited entry; `false` when the cap is hit and the walk must stop.
    fn visit(&mut self) -> bool {
        self.report.scanned += 1;
        if self.report.scanned > self.max_scan {
            self.report.truncated = true;
        }
        !self.report.truncated
    }

    fn walk_dir(
        &mut self,
        tree: Option<Oid>,
        dir: &BString,
        disk_dir: &std::path::Path,
    ) -> Result<()> {
        let tree_entries = match tree {
            Some(oid) => tree_children(self.repo, oid)?,
            None => Vec::new(),
        };
        let dir_str = dir.to_str_lossy().into_owned();
        let read = std::fs::read_dir(disk_dir)
            .map_err(|source| worktree_error("scan working tree", &dir_str, Box::new(source)))?;
        let mut on_disk: Vec<BString> = Vec::new();
        for entry in read {
            if self.report.truncated {
                return Ok(());
            }
            let entry = entry.map_err(|source| {
                worktree_error("scan working tree", &dir_str, Box::new(source))
            })?;
            let name = BString::from(entry.file_name().as_encoded_bytes());
            if dir.is_empty() && name == ".git" {
                continue;
            }
            if !self.visit() {
                return Ok(());
            }
            let meta = std::fs::symlink_metadata(entry.path()).map_err(|source| {
                worktree_error("stat working tree entry", &dir_str, Box::new(source))
            })?;
            let kind = disk_kind(&meta);
            let path = child_path(dir, &name);
            on_disk.push(name.clone());
            match tree_entries
                .iter()
                .find(|tree_entry| tree_entry.name == name)
            {
                Some(tree_entry)
                    if tree_entry.kind == EntryKind::Tree && kind == EntryKind::Tree =>
                {
                    self.walk_dir(Some(tree_entry.oid), &path, &entry.path())?;
                }
                Some(tree_entry) => {
                    let status = compare(
                        self.repo,
                        tree_entry,
                        kind,
                        meta.len(),
                        &entry.path(),
                        &dir_str,
                    )?;
                    if status == WorktreeStatus::Modified {
                        self.report.entries.push(StatusEntry {
                            path: path.clone(),
                            kind,
                            status,
                            oid: (tree_entry.kind != EntryKind::Tree).then_some(tree_entry.oid),
                            size: meta.len(),
                        });
                    }
                    if tree_entry.kind == EntryKind::Tree {
                        // A file now sits where a tracked directory was: everything below
                        // the old directory is deleted.
                        self.walk_deleted(tree_entry.oid, &path)?;
                    }
                }
                None => {
                    if self.excludes.is_ignored(&path, kind == EntryKind::Tree) {
                        continue;
                    }
                    if kind == EntryKind::Tree {
                        self.walk_dir(None, &path, &entry.path())?;
                    } else {
                        self.report.entries.push(StatusEntry {
                            path,
                            kind,
                            status: WorktreeStatus::Untracked,
                            oid: None,
                            size: meta.len(),
                        });
                    }
                }
            }
        }
        for tree_entry in &tree_entries {
            if self.report.truncated {
                return Ok(());
            }
            if on_disk.iter().any(|name| name == &tree_entry.name) {
                continue;
            }
            let path = child_path(dir, &tree_entry.name);
            match tree_entry.kind {
                EntryKind::Tree => self.walk_deleted(tree_entry.oid, &path)?,
                EntryKind::Submodule => {}
                EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink => {
                    if !self.visit() {
                        return Ok(());
                    }
                    self.emit_deleted(tree_entry, path);
                }
            }
        }
        Ok(())
    }

    /// Everything below a tree with no counterpart on disk is deleted; walking it reads
    /// tree objects only, never the disk.
    fn walk_deleted(&mut self, tree: Oid, dir: &BString) -> Result<()> {
        for tree_entry in tree_children(self.repo, tree)? {
            let path = child_path(dir, &tree_entry.name);
            match tree_entry.kind {
                EntryKind::Tree => self.walk_deleted(tree_entry.oid, &path)?,
                EntryKind::Submodule => {}
                EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink => {
                    if !self.visit() {
                        return Ok(());
                    }
                    self.emit_deleted(&tree_entry, path);
                }
            }
        }
        Ok(())
    }

    fn emit_deleted(&mut self, tree_entry: &TreeEntry, path: BString) {
        self.report.entries.push(StatusEntry {
            path,
            kind: tree_entry.kind,
            status: WorktreeStatus::Deleted,
            oid: Some(tree_entry.oid),
            size: 0,
        });
    }
}

/// What a disk entry is, from its `symlink_metadata`.
fn disk_kind(meta: &std::fs::Metadata) -> EntryKind {
    if meta.file_type().is_symlink() {
        return EntryKind::Symlink;
    }
    if meta.is_dir() {
        return EntryKind::Tree;
    }
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    if executable {
        EntryKind::ExecutableBlob
    } else {
        EntryKind::Blob
    }
}

/// `dir/name` with `/` separators; just `name` at the root.
fn child_path(dir: &BString, name: &BString) -> BString {
    let mut path = dir.clone();
    if !path.is_empty() {
        path.extend_from_slice(b"/");
    }
    path.extend_from_slice(name);
    path
}

fn relative_path(dir: &str, name: &BString) -> BString {
    let mut path = BString::from(dir.trim_matches('/'));
    if !path.is_empty() {
        path.extend_from_slice(b"/");
    }
    path.extend_from_slice(name);
    path
}

/// Joins a repository-relative path under `root`, refusing traversal outside it.
fn join_under(root: &std::path::Path, relative: &str) -> std::result::Result<PathBuf, BoxError> {
    let mut joined = root.to_path_buf();
    for segment in relative.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err("path may not leave the working tree".into());
        }
        joined.push(segment);
    }
    Ok(joined)
}

fn worktree_error(op: &'static str, path: &str, source: BoxError) -> Error {
    Error::Worktree {
        op,
        path: path.to_owned(),
        source,
    }
}

/// Orders entries like [`sort_for_display`]: directories first, then names.
fn sort_worktree(entries: &mut [WorktreeEntry]) {
    entries.sort_by_cached_key(|entry| {
        (
            entry.kind != EntryKind::Tree,
            entry.name.to_ascii_lowercase(),
        )
    });
}

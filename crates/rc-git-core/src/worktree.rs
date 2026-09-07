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
use crate::tree::{EntryKind, TreeEntry, sort_for_display};

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
            let kind = if meta.file_type().is_symlink() {
                EntryKind::Symlink
            } else if meta.is_dir() {
                EntryKind::Tree
            } else {
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
            };
            on_disk.push((name, kind, meta.len(), entry.path()));
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
            return Ok(BlobContent::Text {
                data,
                line_starts: starts,
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
        Ok(BlobContent::Text {
            data,
            line_starts: starts,
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

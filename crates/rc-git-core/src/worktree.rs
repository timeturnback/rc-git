//! The working directory as a read-only view: files on disk with a status overlay against
//! `HEAD`. Nothing here writes; hashing a file to compare it with a blob reads it once.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::index::entry::{Flags, Stage};
use gix::worktree::stack::state::ignore::Source;

use crate::blob::{BlobContent, clamp_read_limit, classify};
use crate::error::{BoxError, Error, Result, git, object};
use crate::oid::Oid;
use crate::repo::Repository;
use crate::tree::{EntryKind, TreeEntry, sort_for_display, tree_children};

/// One `lstat` of a working-tree entry, taken once by the walk and used for its kind, its size
/// and the stat cache alike. `gix_index`'s own metadata, not `std::fs`: it fills `ctime` the
/// way git does, so it compares with the index.
type DiskStat = gix::index::fs::Metadata;

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
    /// File name as git records it: the tree entry's name when the disk name matches one,
    /// which under `core.precomposeUnicode` or `core.ignoreCase` may be spelled differently on
    /// disk; otherwise the disk name, precomposed to NFC under `core.precomposeUnicode`.
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
    /// Lookups that failed without failing the listing, such as an unreadable index; each
    /// names what was read and why it failed. Ignore rules may then be incomplete.
    pub warnings: Vec<String>,
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

/// One file on disk found by [`Repository::worktree_files`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeFile {
    /// Repository-relative path with `/` separators.
    pub path: BString,
    /// What the file is on disk: a blob, an executable blob, or a symlink.
    pub kind: EntryKind,
}

/// A working-tree entry a walk skipped because it could not be read, such as a directory
/// without read permission. An entry that vanished mid-walk is not one: it is simply absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableEntry {
    /// Repository-relative path with `/` separators.
    pub path: BString,
    /// Which read failed and why, such as `read directory: Permission denied (os error 13)`.
    pub error: String,
}

/// Every file on disk, with the walk's own cost accounting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeFileList {
    /// Files sorted by path.
    pub files: Vec<WorktreeFile>,
    /// Entries visited before the walk finished or hit its cap.
    pub scanned: usize,
    /// Whether the walk stopped at `max_scan` before covering the whole tree.
    pub truncated: bool,
    /// Lookups that failed without failing the walk, as in [`WorktreeListing::warnings`].
    pub warnings: Vec<String>,
    /// Entries below the root that could not be read and were skipped, with their contents.
    pub unreadable: Vec<UnreadableEntry>,
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
    /// Lookups that failed without failing the walk, as in [`WorktreeListing::warnings`]: an
    /// unreadable index also means every tracked file was hashed.
    pub warnings: Vec<String>,
    /// Entries below the root that could not be read and were skipped, with their contents;
    /// none of them is listed as changed or deleted.
    pub unreadable: Vec<UnreadableEntry>,
    /// Untracked directories an ignore rule pruned without opening them, sorted by path. No
    /// write below one can change this report until the ignore rules or the tree change, so
    /// a watcher may drop events under them.
    pub ignored_dirs: Vec<BString>,
    /// Untracked directories the walk left unopened because they hold a repository of their
    /// own, sorted by path. Only a write to the `.git` directly below one (the repository
    /// appearing or going) can change this report, so a watcher may drop every other event
    /// under them.
    pub nested_repos: Vec<BString>,
}

impl Repository {
    /// Lists one directory of the working tree, comparing each entry with the tree at the same path
    /// below `tree`; with no tree (an unborn branch) every entry is untracked or ignored. `dir` is
    /// repository-relative with `/` separators; empty, `/` or `.` for the root, and `./` or doubled
    /// slashes name the same directory. Reads one directory and one tree object, plus one content
    /// read per file whose size matches its blob, so the cost stays proportional to the listing. A
    /// directory at or below a submodule lists every entry as unchanged with no tree entry: its
    /// files belong to another repository. `.git` is never listed.
    ///
    /// # Errors
    /// [`Error::Worktree`] for a bare repository, a `..` segment in `dir`, a symlink at `dir` or on
    /// the way to it, or an unreadable directory, [`Error::Object`] when the compared tree cannot
    /// be read.
    pub fn worktree_entries(&self, tree: Option<Oid>, dir: &str) -> Result<WorktreeListing> {
        let work_dir = self.require_work_dir("list working tree", dir)?;
        let dir = &normalize_dir(dir)
            .map_err(|source| worktree_error("list working tree", dir, source))?;
        let (tree_entries, in_submodule) = match tree.map(|tree| self.tree_at(tree, dir)) {
            None => (Vec::new(), false),
            Some(found) => match found? {
                TreeAt::Entries(entries) => (entries, false),
                TreeAt::Submodule => (Vec::new(), true),
            },
        };
        let disk_dir = join_under(&work_dir, dir)
            .and_then(|disk_dir| {
                refuse_symlink(&disk_dir, &[dir.as_str()])?;
                Ok(disk_dir)
            })
            .map_err(|source| worktree_error("list working tree", dir, source))?;

        let mut listing = WorktreeListing::default();
        self.with(|repo| {
            let mut context = WorktreeContext::new(repo, "list working tree")?;
            let on_disk = list_disk_dir(&disk_dir, dir, context.fs)?;
            let by_name = context.fs.index(&tree_entries);
            for DiskEntry {
                name,
                kind: disk,
                meta,
                path,
            } in on_disk
            {
                let size = meta.len();
                let (name, in_tree) = by_name.resolve(name);
                let rela = relative_path(dir, &name);
                let Some(disk) = disk else {
                    listing.entries.extend(special_entry(name, in_tree, size));
                    continue;
                };
                let kind = context.kind_of(disk, in_tree);
                let status = if in_submodule {
                    WorktreeStatus::Unchanged
                } else if let Some(tree_entry) = in_tree {
                    let disk = DiskFacts {
                        kind,
                        size,
                        meta: &meta,
                        path: &path,
                        rela: &rela,
                    };
                    match context.compare(tree_entry, &disk)? {
                        Ok(status) => status,
                        // Gone since the directory was read: the tree entry lists as missing.
                        Err(error) if error.is_vanished() => continue,
                        Err(error) => return Err(error.into_error(&rela)),
                    }
                } else if context.excludes.is_ignored(&rela, kind == EntryKind::Tree) {
                    WorktreeStatus::Ignored
                } else {
                    WorktreeStatus::Untracked
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
            listing.warnings = context.into_warnings();
            Ok(())
        })?;

        let on_disk_names: HashSet<&BString> =
            listing.entries.iter().map(|entry| &entry.name).collect();
        for tree_entry in &tree_entries {
            if !on_disk_names.contains(&tree_entry.name) {
                listing.missing.push(tree_entry.clone());
            }
        }
        sort_worktree(&mut listing.entries);
        sort_for_display(&mut listing.missing);
        Ok(listing)
    }

    /// Walks the whole working tree against `tree` and returns every changed path: modified,
    /// untracked, and deleted files, sorted by path. With no tree (an unborn branch) every
    /// file is untracked. Clean and ignored paths are not listed,
    /// and an ignored directory is pruned without reading it, so `node_modules` never costs a
    /// `read_dir`. `.git` is never entered at any depth, and an untracked directory that
    /// holds its own repository is skipped whole: its files belong to that repository. The
    /// walk stops after visiting `max_scan` entries and reports the truncation, which bounds
    /// both the disk work and how long the borrowed pool handle is held. An entry that vanishes
    /// mid-walk is absent, as git would see it; one below the root that cannot be read is
    /// skipped and listed in [`StatusReport::unreadable`].
    ///
    /// # Errors
    /// [`Error::Worktree`] for a bare repository or an unreadable working-tree root,
    /// [`Error::Object`] when a tree object on the walk cannot be read.
    pub fn worktree_status(&self, tree: Option<Oid>, max_scan: usize) -> Result<StatusReport> {
        let work_dir = self.require_work_dir("scan working tree", "")?;
        self.with(|repo| {
            let mut walk = StatusWalk {
                context: WorktreeContext::new(repo, "scan working tree")?,
                report: StatusReport::default(),
                budget: ScanBudget::new(max_scan),
            };
            walk.walk_dir(tree, &BString::from(""), &work_dir)?;
            let mut report = walk.report;
            report.scanned = walk.budget.scanned;
            report.truncated = walk.budget.truncated;
            report.entries.sort_by(|a, b| a.path.cmp(&b.path));
            report.ignored_dirs.sort();
            report.nested_repos.sort();
            report.warnings = walk.context.into_warnings();
            Ok(report)
        })
    }

    /// Walks the whole working tree and returns every file on disk, sorted by path: tracked,
    /// untracked, and ignored files alike, so a gitignored `.env` is found; `tree` is `None`
    /// on an unborn branch. An ignored directory that `tree` does not track is pruned without
    /// reading it, and neither a submodule, `.git`, nor an untracked directory holding its own
    /// repository is entered.
    /// The walk stops after visiting `max_scan` entries and reports the
    /// truncation, which bounds both the disk work and how long the borrowed pool handle is
    /// held. Nothing is hashed: this lists names, it does not compare content. An entry that
    /// vanishes mid-walk is absent; one below the root that cannot be read is skipped and
    /// listed in [`WorktreeFileList::unreadable`].
    ///
    /// # Errors
    /// [`Error::Worktree`] for a bare repository or an unreadable working-tree root,
    /// [`Error::Object`] when a tree object on the walk cannot be read.
    pub fn worktree_files(&self, tree: Option<Oid>, max_scan: usize) -> Result<WorktreeFileList> {
        let work_dir = self.require_work_dir("list working tree files", "")?;
        self.with(|repo| {
            let mut walk = FileWalk {
                repo,
                fs: FsRules::new(repo, "list working tree files")?,
                excludes: ExcludeCheck::new(repo),
                list: WorktreeFileList::default(),
                budget: ScanBudget::new(max_scan),
            };
            walk.walk_dir(tree, &BString::from(""), &work_dir)?;
            let mut list = walk.list;
            list.scanned = walk.budget.scanned;
            list.truncated = walk.budget.truncated;
            list.files.sort_by(|a, b| a.path.cmp(&b.path));
            list.warnings = walk.excludes.warnings;
            Ok(list)
        })
    }

    /// Reads one working-tree file for display, with the same size cap (clamped to
    /// [`crate::MAX_READ_LIMIT`]) and binary probe as [`Repository::read_blob`]. A symlink yields its target as text.
    ///
    /// # Errors
    /// [`Error::Worktree`] for a bare repository, a missing file, a directory, a FIFO, socket
    /// or device file, a path through a symlinked directory, or an unreadable file.
    pub fn worktree_file(&self, path: &str, limit: u64) -> Result<BlobContent> {
        let (full, meta) = self.stat_worktree_file(path)?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&full)
                .map_err(|source| worktree_error("read symlink", path, Box::new(source)))?;
            return Ok(classify(target.into_os_string().into_encoded_bytes()));
        }
        // A FIFO blocks `open` until a writer appears and a device never reaches EOF, so
        // anything but a regular file is refused before it is opened.
        if !meta.file_type().is_file() {
            return Err(worktree_error(
                "read working tree file",
                path,
                "is not a regular file".into(),
            ));
        }
        let limit = clamp_read_limit(limit);
        let size = meta.len();
        if size > limit {
            return Ok(BlobContent::TooLarge { size, limit });
        }
        let data = read_at_most(&full, limit)
            .map_err(|source| worktree_error("read working tree file", path, Box::new(source)))?;
        let read = u64::try_from(data.len()).unwrap_or(u64::MAX);
        if read > limit {
            // The file grew after the stat; the caller's limit still holds.
            return Ok(BlobContent::TooLarge {
                size: size.max(read),
                limit,
            });
        }
        Ok(classify(data))
    }

    /// Size of a working-tree file from its metadata, without reading it; a symlink's size is
    /// its target's length.
    pub(crate) fn worktree_file_size(&self, path: &str) -> Result<u64> {
        Ok(self.stat_worktree_file(path)?.1.len())
    }

    /// The file's full path and `symlink_metadata`; a directory is an error.
    fn stat_worktree_file(&self, path: &str) -> Result<(PathBuf, std::fs::Metadata)> {
        let work_dir = self.require_work_dir("read working tree file", path)?;
        let full = join_under(&work_dir, path)
            .map_err(|source| worktree_error("read working tree file", path, source))?;
        let meta = std::fs::symlink_metadata(&full)
            .map_err(|source| worktree_error("read working tree file", path, Box::new(source)))?;
        if meta.is_dir() {
            return Err(worktree_error(
                "read working tree file",
                path,
                "is a directory".into(),
            ));
        }
        Ok((full, meta))
    }

    pub(crate) fn require_work_dir(&self, op: &'static str, path: &str) -> Result<PathBuf> {
        self.work_dir().map(PathBuf::from).ok_or_else(|| {
            worktree_error(
                op,
                path,
                "the repository is bare and has no working tree".into(),
            )
        })
    }

    /// What `tree` holds at `dir`, resolved one segment at a time so the walk stops at a
    /// submodule instead of descending through its commit id, which lives in the submodule's
    /// own object store. `dir` is already normalized, so the root is the empty string.
    fn tree_at(&self, tree: Oid, dir: &str) -> Result<TreeAt> {
        self.with(|repo| {
            let mut entries = tree_children(repo, tree)?;
            for segment in dir.split('/').filter(|segment| !segment.is_empty()) {
                let Some(entry) = entries.iter().find(|entry| entry.name == segment) else {
                    return Ok(TreeAt::Entries(Vec::new()));
                };
                match entry.kind {
                    EntryKind::Tree => entries = tree_children(repo, entry.oid)?,
                    EntryKind::Submodule => return Ok(TreeAt::Submodule),
                    EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink => {
                        return Ok(TreeAt::Entries(Vec::new()));
                    }
                }
            }
            Ok(TreeAt::Entries(entries))
        })
    }
}

/// One directory entry as its one `lstat` saw it.
struct DiskEntry {
    name: BString,
    /// `None` for a FIFO, socket or device file.
    kind: Option<EntryKind>,
    meta: DiskStat,
    path: PathBuf,
}

/// One directory's entries with their `lstat`, named as `fs` spells them. An entry
/// that vanished between the listing and its stat is left out, as git would never have seen it.
fn list_disk_dir(disk_dir: &Path, dir: &str, fs: FsRules) -> Result<Vec<DiskEntry>> {
    let read = std::fs::read_dir(disk_dir)
        .map_err(|source| worktree_error("list working tree", dir, Box::new(source)))?;
    let mut on_disk = Vec::new();
    for entry in read {
        let entry =
            entry.map_err(|source| worktree_error("list working tree", dir, Box::new(source)))?;
        let name = fs.disk_name(&entry.file_name());
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        let meta = match DiskStat::from_path_no_follow(&path) {
            Ok(meta) => meta,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                let rela = relative_path(dir, &name);
                return Err(DiskError::new("stat working tree entry", source).into_error(&rela));
            }
        };
        on_disk.push(DiskEntry {
            name,
            kind: disk_kind(&meta),
            meta,
            path,
        });
    }
    Ok(on_disk)
}

/// What the compared tree holds at a listed directory.
enum TreeAt {
    /// The directory's entries; empty when the tree has no directory there.
    Entries(Vec<TreeEntry>),
    /// The directory is a submodule or lies inside one: another repository's files, which
    /// list as unchanged with no tree entry.
    Submodule,
}

/// What the walk's one `lstat` said about the file being compared, and where to find it.
struct DiskFacts<'a> {
    kind: EntryKind,
    size: u64,
    /// That `lstat`, which the stat cache compares with the index instead of taking another.
    meta: &'a DiskStat,
    /// The file's path on disk.
    path: &'a Path,
    /// The repository-relative path with `/` separators, for the index lookup.
    rela: &'a BString,
}

/// The lookups a working-tree comparison needs, each built on first use: the ignore stack and
/// the index read as a stat cache. One context serves a whole walk, so an ignored directory
/// costs one stack build and the index is read once however many files are compared.
struct WorktreeContext<'repo> {
    repo: &'repo gix::Repository,
    fs: FsRules,
    excludes: ExcludeCheck<'repo>,
    stats: StatCache<'repo>,
}

impl<'repo> WorktreeContext<'repo> {
    fn new(repo: &'repo gix::Repository, op: &'static str) -> Result<Self> {
        Ok(Self {
            repo,
            fs: FsRules::new(repo, op)?,
            excludes: ExcludeCheck::new(repo),
            stats: StatCache::new(repo),
        })
    }

    /// What the ignore stack and the stat cache could not read, each failure once.
    fn into_warnings(self) -> Vec<String> {
        let mut warnings = self.excludes.warnings;
        for warning in self.stats.warnings {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        warnings
    }

    /// The kind git would record for a disk entry of `disk` kind tracked as `tracked`.
    fn kind_of(&self, disk: EntryKind, tracked: Option<&TreeEntry>) -> EntryKind {
        git_kind(
            disk,
            tracked.map(|entry| entry.kind),
            self.fs.executable_bit,
        )
    }

    /// Whether a disk file differs from its tree entry. Kind decides first, then the blob
    /// header's size, then the index's stat cache; only a file none of those settle is read.
    /// A failed read of the disk file is the inner `Err`, so the walk can tell a file that
    /// vanished from one it may not read; the outer `Err` is the repository failing.
    fn compare(
        &mut self,
        tree_entry: &TreeEntry,
        disk: &DiskFacts<'_>,
    ) -> Result<std::result::Result<WorktreeStatus, DiskError>> {
        if let Some(status) = kind_verdict(tree_entry, disk.kind) {
            return Ok(Ok(status));
        }
        if disk.kind == EntryKind::Symlink {
            return self.compare_symlink(tree_entry, disk.path);
        }
        self.compare_content(tree_entry, disk)
    }

    /// A symlink is its target. The blob header must name a blob of the target's length before
    /// the blob is decoded, so a tree entry pointing at a huge blob costs one header read.
    fn compare_symlink(
        &self,
        tree_entry: &TreeEntry,
        disk_path: &Path,
    ) -> Result<std::result::Result<WorktreeStatus, DiskError>> {
        let target = match std::fs::read_link(disk_path) {
            Ok(target) => target,
            Err(source) => return Ok(Err(DiskError::new("read symlink", source))),
        };
        let target = target.as_os_str().as_encoded_bytes();
        let header = self
            .repo
            .find_header(*tree_entry.oid.as_gix())
            .map_err(object("read blob header", tree_entry.oid))?;
        let same_length = u64::try_from(target.len()).is_ok_and(|len| len == header.size());
        if header.kind() != gix::objs::Kind::Blob || !same_length {
            return Ok(Ok(WorktreeStatus::Modified));
        }
        let blob = self
            .repo
            .find_blob(*tree_entry.oid.as_gix())
            .map_err(object("read blob", tree_entry.oid))?;
        Ok(Ok(if blob.data == target {
            WorktreeStatus::Unchanged
        } else {
            WorktreeStatus::Modified
        }))
    }

    /// A regular file. The index's stat cache is asked first: git records the size it checked
    /// out, which differs from the blob's under an eol or LFS conversion, so a file the index
    /// vouches for is clean whatever its size. Otherwise a size that differs from the blob
    /// header settles it without reading the file, and an equal size is hashed, streaming, so
    /// memory stays constant.
    fn compare_content(
        &mut self,
        tree_entry: &TreeEntry,
        disk: &DiskFacts<'_>,
    ) -> Result<std::result::Result<WorktreeStatus, DiskError>> {
        if self
            .stats
            .proves_unchanged(disk.rela, tree_entry.oid, disk.meta)
        {
            return Ok(Ok(WorktreeStatus::Unchanged));
        }
        let header = self
            .repo
            .find_header(*tree_entry.oid.as_gix())
            .map_err(object("read blob header", tree_entry.oid))?;
        if header.size() != disk.size {
            return Ok(Ok(WorktreeStatus::Modified));
        }
        let mut file = match std::fs::File::open(disk.path) {
            Ok(file) => file,
            Err(source) => return Ok(Err(DiskError::new("open working tree file", source))),
        };
        // The walk's lstat is stale by now: a file rewritten in place since then is measured
        // again through the open handle, and one whose length moved is modified unhashed.
        let opened_size = match file.metadata() {
            Ok(meta) => meta.len(),
            Err(source) => return Ok(Err(DiskError::new("stat working tree file", source))),
        };
        if opened_size != header.size() {
            return Ok(Ok(WorktreeStatus::Modified));
        }
        let hashed = match gix::objs::compute_stream_hash(
            self.repo.object_hash(),
            gix::objs::Kind::Blob,
            &mut file,
            opened_size,
            &mut gix::progress::Discard,
            &AtomicBool::new(false),
        ) {
            Ok(hashed) => hashed,
            // Truncated while being read: it no longer holds the blob's bytes.
            Err(gix::hash::io::Error::Io(source))
                if source.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                return Ok(Ok(WorktreeStatus::Modified));
            }
            Err(gix::hash::io::Error::Io(source)) => {
                return Ok(Err(DiskError::new("hash working tree file", source)));
            }
            Err(source @ gix::hash::io::Error::Hasher(_)) => {
                return Err(worktree_error(
                    "hash working tree file",
                    &disk.rela.to_str_lossy(),
                    Box::new(source),
                ));
            }
        };
        Ok(Ok(if Oid::from_gix(hashed) == tree_entry.oid {
            WorktreeStatus::Unchanged
        } else {
            WorktreeStatus::Modified
        }))
    }
}

/// A read of the working tree that failed below the walk's root: which read, and why.
struct DiskError {
    op: &'static str,
    source: std::io::Error,
}

impl DiskError {
    fn new(op: &'static str, source: std::io::Error) -> Self {
        Self { op, source }
    }

    /// The entry is gone: deleted or renamed away between being listed and being read.
    fn is_vanished(&self) -> bool {
        self.source.kind() == std::io::ErrorKind::NotFound
    }

    fn into_unreadable(self, path: BString) -> UnreadableEntry {
        UnreadableEntry {
            path,
            error: format!("{}: {}", self.op, self.source),
        }
    }

    fn into_error(self, path: &BString) -> Error {
        worktree_error(self.op, &path.to_str_lossy(), Box::new(self.source))
    }
}

/// A directory below the root that could not be read is recorded and skipped; the root
/// itself failing fails the walk named by `walk_op`, since then there is nothing to show.
fn skip_unreadable_dir(
    walk_op: &'static str,
    dir: &BString,
    source: std::io::Error,
    unreadable: &mut Vec<UnreadableEntry>,
) -> Result<()> {
    if dir.is_empty() {
        return Err(worktree_error(walk_op, "", Box::new(source)));
    }
    unreadable.push(DiskError::new("read directory", source).into_unreadable(dir.clone()));
    Ok(())
}

/// How a listing shows a FIFO, socket or device file, judged without opening it: like git,
/// only where it replaced a tracked file, as modified. One where a tracked directory was is
/// left out, so the directory lists as missing.
fn special_entry(name: BString, tracked: Option<&TreeEntry>, size: u64) -> Option<WorktreeEntry> {
    let tree_entry = tracked?;
    let status = match tree_entry.kind {
        EntryKind::Tree => return None,
        EntryKind::Submodule => WorktreeStatus::Unchanged,
        EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink => {
            WorktreeStatus::Modified
        }
    };
    Some(WorktreeEntry {
        name,
        kind: tree_entry.kind,
        status,
        oid: Some(tree_entry.oid),
        size,
    })
}

/// The verdict kind alone settles: a directory on either side, a submodule, or a kind change.
/// `None` when both sides are the same kind of file and the content decides.
fn kind_verdict(tree_entry: &TreeEntry, disk_kind: EntryKind) -> Option<WorktreeStatus> {
    // A submodule is a directory on disk and a commit id in the tree; its contents belong to
    // another repository, so the kinds differing says nothing.
    if tree_entry.kind == EntryKind::Submodule {
        return Some(WorktreeStatus::Unchanged);
    }
    if tree_entry.kind == EntryKind::Tree || disk_kind == EntryKind::Tree {
        return Some(if tree_entry.kind == disk_kind {
            WorktreeStatus::Unchanged
        } else {
            WorktreeStatus::Modified
        });
    }
    if tree_entry.kind != disk_kind {
        return Some(WorktreeStatus::Modified);
    }
    None
}

/// The index read as a stat cache. Git records the `lstat` of every file it hashed when it
/// last wrote the index, so a file whose stat still matches has not been touched since and the
/// entry's object id still describes it — which turns a whole-file hash into one `lstat`.
///
/// Read-only in both directions: the index is never refreshed or written, and every case the
/// cache cannot settle answers "not known unchanged", so it can save a hash but never hide a
/// change. An index that cannot be read is reported in `warnings`.
struct StatCache<'repo> {
    repo: &'repo gix::Repository,
    state: StatCacheState,
    warnings: Vec<String>,
}

enum StatCacheState {
    Unbuilt,
    Unavailable,
    Ready(LoadedIndex),
}

/// The index, with the stat fields the repository's config says to compare.
struct LoadedIndex {
    index: gix::worktree::Index,
    options: gix::index::entry::stat::Options,
}

impl LoadedIndex {
    /// Whether this index proves the file at `rela_path` is still `oid`.
    fn proves_unchanged(&self, rela_path: &BString, oid: Oid, meta: &DiskStat) -> bool {
        let Some(entry) = self.index.entry_by_path(rela_path.as_bstr()) else {
            return false;
        };
        if entry.stage() != Stage::Unconflicted || entry.id != *oid.as_gix() {
            return false;
        }
        // These three say the recorded stat is not evidence: two ask git to skip the file, and
        // an intent-to-add entry holds the empty blob rather than the file's content.
        if entry
            .flags
            .intersects(Flags::ASSUME_VALID | Flags::SKIP_WORKTREE | Flags::INTENT_TO_ADD)
        {
            return false;
        }
        // A file written in the same second the index was is indistinguishable from one
        // written just after it, so its stat proves nothing.
        if entry.stat.is_racy(self.index.timestamp(), self.options) {
            return false;
        }
        let Ok(disk) = gix::index::entry::Stat::from_fs(meta) else {
            return false;
        };
        entry.stat.matches(&disk, self.options)
    }
}

impl<'repo> StatCache<'repo> {
    fn new(repo: &'repo gix::Repository) -> Self {
        Self {
            repo,
            state: StatCacheState::Unbuilt,
            warnings: Vec::new(),
        }
    }

    /// Whether the index proves the file at `rela_path` is still `oid`. A repository with no
    /// readable index answers `false` for every path, which only costs the hash it would have
    /// cost anyway.
    fn proves_unchanged(&mut self, rela_path: &BString, oid: Oid, meta: &DiskStat) -> bool {
        if let StatCacheState::Unbuilt = self.state {
            self.state = match self.build() {
                Some(loaded) => StatCacheState::Ready(loaded),
                None => StatCacheState::Unavailable,
            };
        }
        match &self.state {
            StatCacheState::Ready(loaded) => loaded.proves_unchanged(rela_path, oid, meta),
            StatCacheState::Unbuilt | StatCacheState::Unavailable => false,
        }
    }

    fn build(&mut self) -> Option<LoadedIndex> {
        let index = read_index(self.repo)
            .map_err(|warning| self.warnings.push(warning))
            .ok()?;
        let options = self
            .repo
            .stat_options()
            .map_err(|error| {
                self.warnings
                    .push(format!("read stat options for the index: {error}"));
            })
            .ok()?;
        Some(LoadedIndex { index, options })
    }
}

/// The largest ignore file read. git has no cap; this keeps a hostile or runaway file (a
/// symlink to `/dev/zero` reads as a character device and is refused before this) from
/// growing memory without bound.
const MAX_IGNORE_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Ignore-rule lookup, built lazily because most listings have no untracked entries. Every
/// failure lands in `warnings`, so a caller can say that ignore rules may be incomplete.
///
/// gix reads every ignore file by following symlinks with no file-type check or size cap, so a
/// FIFO would block the walk and a device would grow memory without bound. The global sources
/// (the user's global ignore file and `info/exclude`) are therefore read here, and each
/// directory's `.gitignore` is checked before gix reads it; a refused source counts as absent.
struct ExcludeCheck<'repo> {
    repo: &'repo gix::Repository,
    state: ExcludeState<'repo>,
    warnings: Vec<String>,
    /// Whether a failed query is already in `warnings`.
    query_failed: bool,
    /// Directories whose `.gitignore` was checked, and whether gix may read it.
    readable_dirs: HashMap<BString, bool>,
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
            warnings: Vec::new(),
            query_failed: false,
            readable_dirs: HashMap::new(),
        }
    }

    /// Whether `rela_path` matches an ignore rule. An unreadable index costs only the tracked
    /// `.gitignore` files missing from disk; those on disk and `info/exclude` still apply.
    /// Failing to build or query the stack means "not ignored" and records a warning, since
    /// the entry then shows as untracked when it may be ignored.
    fn is_ignored(&mut self, rela_path: &BString, is_dir: bool) -> bool {
        if let ExcludeState::Unbuilt = self.state {
            self.state = match self.build() {
                Some(attributes) => ExcludeState::Ready(Box::new(attributes)),
                None => ExcludeState::Unavailable,
            };
        }
        if let ExcludeState::Ready(_) = self.state
            && !self.ignore_files_readable(rela_path)
        {
            return false;
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
        match attributes.at_entry(rela_path.as_bstr(), Some(mode)) {
            Ok(platform) => platform.is_excluded(),
            Err(error) => {
                // One unreadable `.gitignore` fails every path below it; the first failure
                // already tells the caller the rules are incomplete.
                if !self.query_failed {
                    self.query_failed = true;
                    self.warnings
                        .push(format!("check ignore rules for '{rela_path}': {error}"));
                }
                false
            }
        }
    }

    fn build(&mut self) -> Option<gix::AttributeStack<'repo>> {
        let repo = self.repo;
        let loaded = read_index(repo);
        let empty;
        let index: &gix::index::State = match &loaded {
            Ok(index) => index,
            Err(warning) => {
                self.warnings.push(warning.clone());
                empty = gix::index::State::new(repo.object_hash());
                &empty
            }
        };
        let case = match repo.filesystem_options() {
            Ok(options) if options.ignore_case => gix::glob::pattern::Case::Fold,
            Ok(_) => gix::glob::pattern::Case::Sensitive,
            Err(error) => {
                self.warnings.push(format!("build ignore rules: {error}"));
                return None;
            }
        };
        let parse = gix::ignore::search::Ignore {
            support_precious: repo
                .config_snapshot()
                .boolean("gitoxide.parsePrecious")
                .unwrap_or(false),
        };
        let mut globals = gix::ignore::Search::default();
        let info_exclude = repo.common_dir().join("info").join("exclude");
        for source in crate::repo::global_excludes_file()
            .into_iter()
            .chain([info_exclude])
        {
            if let Some(bytes) = self.read_global_source(&source) {
                globals.add_patterns_buffer(&bytes, source, None, parse);
            }
        }
        let ignore = gix::worktree::stack::state::Ignore::new(
            gix::ignore::Search::default(),
            globals,
            None,
            Source::WorktreeThenIdMappingIfNotSkipped,
            parse,
        );
        let state = gix::worktree::stack::State::IgnoreStack(ignore);
        let id_mappings = state.id_mappings_from_index(index, index.path_backing(), case);
        let root = repo.workdir().unwrap_or(repo.git_dir());
        let stack = gix::worktree::Stack::new(root, state, case, Vec::new(), id_mappings);
        Some(gix::AttributeStack::new(stack, repo))
    }

    /// The bytes of a global ignore file, or `None` when it is absent or refused. A refusal or
    /// a failed read is recorded in `warnings`.
    fn read_global_source(&mut self, path: &Path) -> Option<Vec<u8>> {
        match read_ignore_file(path) {
            Ok(bytes) => bytes,
            Err(reason) => {
                self.warnings
                    .push(format!("skip ignore file '{}': {reason}", path.display()));
                None
            }
        }
    }

    /// Whether gix may read the `.gitignore` of every directory above `rela_path`, checking
    /// each directory once. A refused file makes every path below it "not ignored".
    fn ignore_files_readable(&mut self, rela_path: &BString) -> bool {
        let Some(work_dir) = self.repo.workdir() else {
            return true;
        };
        let mut dir = BString::from("");
        let mut components = rela_path.split_str("/").peekable();
        while let Some(component) = components.next() {
            if !self.dir_ignore_file_readable(work_dir, &dir) {
                return false;
            }
            if components.peek().is_none() {
                break;
            }
            if !dir.is_empty() {
                dir.push(b'/');
            }
            dir.extend_from_slice(component);
        }
        true
    }

    /// Whether gix may read `dir`'s `.gitignore`, checked once per directory; a refusal is
    /// recorded in `warnings`.
    fn dir_ignore_file_readable(&mut self, work_dir: &Path, dir: &BString) -> bool {
        if let Some(readable) = self.readable_dirs.get(dir) {
            return *readable;
        }
        let file = work_dir
            .join(gix::path::from_bstr(dir.as_bstr()))
            .join(".gitignore");
        let readable = match check_ignore_file(&file) {
            Ok(()) => true,
            Err(reason) => {
                self.warnings
                    .push(format!("skip ignore file '{}': {reason}", file.display()));
                false
            }
        };
        self.readable_dirs.insert(dir.clone(), readable);
        readable
    }
}

/// Whether the ignore file at `path` may be read: absent, or a regular file (through any
/// symlinks) of at most [`MAX_IGNORE_FILE_BYTES`]. `Err` names why it is refused.
fn check_ignore_file(path: &Path) -> std::result::Result<(), String> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) if is_absent(&error) => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if !meta.is_file() {
        return Err("not a regular file".to_owned());
    }
    if meta.len() > MAX_IGNORE_FILE_BYTES {
        return Err(format!("larger than {MAX_IGNORE_FILE_BYTES} bytes"));
    }
    Ok(())
}

/// The bytes of the ignore file at `path`, `None` when it is absent, or `Err` naming why it is
/// refused or unreadable. The read stops past the cap, so a file that grew after the check
/// cannot grow memory either.
fn read_ignore_file(path: &Path) -> std::result::Result<Option<Vec<u8>>, String> {
    use std::io::Read as _;
    check_ignore_file(path)?;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if is_absent(&error) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_IGNORE_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_IGNORE_FILE_BYTES {
        return Err(format!("larger than {MAX_IGNORE_FILE_BYTES} bytes"));
    }
    Ok(Some(bytes))
}

/// Whether an open or stat failed only because the file, or a directory on its path, is not
/// there, which git treats as no ignore file.
fn is_absent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// The on-disk index, empty when there is none, or a warning naming the index and why it
/// could not be read.
fn read_index(repo: &gix::Repository) -> std::result::Result<gix::worktree::Index, String> {
    let path = repo.index_path();
    if index_unparseable(repo) {
        return Err(format!(
            "read index '{}': too short to parse or not a regular file",
            path.display()
        ));
    }
    repo.index_or_empty()
        .map_err(|error| format!("read index '{}': {error}", path.display()))
}

/// Whether the index file exists but cannot hold even a header and its trailing hash, or is
/// not a regular file. gix-index 0.55 slices the trailer off before it checks the length, so
/// such a file panics (and aborts a release build) instead of returning an error; a FIFO
/// would block the open. A missing file, or a metadata read that fails, goes to gix as before.
fn index_unparseable(repo: &gix::Repository) -> bool {
    const HEADER_LEN: u64 = 12;
    let Ok(meta) = std::fs::metadata(repo.index_path()) else {
        return false;
    };
    let hash_len = u64::try_from(repo.object_hash().len_in_bytes()).unwrap_or(u64::MAX);
    !meta.is_file() || meta.len() < HEADER_LEN.saturating_add(hash_len)
}

/// Whether a listed entry was still on disk when the walk read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presence {
    OnDisk,
    /// Deleted or renamed away after its directory was read.
    Gone,
}

/// How many entries a walk may visit, and how many it has. One budget serves a whole walk, so
/// the cap bounds both the disk work and how long the borrowed pool handle is held.
struct ScanBudget {
    scanned: usize,
    max_scan: usize,
    /// Whether the walk hit `max_scan` and stopped.
    truncated: bool,
}

impl ScanBudget {
    fn new(max_scan: usize) -> Self {
        Self {
            scanned: 0,
            max_scan,
            truncated: false,
        }
    }

    /// Counts one visited entry; `false` when the cap is hit and the walk must stop.
    fn visit(&mut self) -> bool {
        if self.scanned >= self.max_scan {
            self.truncated = true;
            return false;
        }
        self.scanned += 1;
        true
    }
}

/// One directory's entries for a walk: each name spelled as git records it, with the entry's
/// path on disk. `.git` is never yielded, at any depth. A failed read of the next entry is the
/// item's `Err`, so the walk decides whether that fails it or skips the directory.
struct WalkEntries {
    read: std::fs::ReadDir,
    fs: FsRules,
}

impl WalkEntries {
    /// Opens `disk_dir`; `Ok(None)` when a directory below the root vanished since its parent
    /// was read.
    fn open(disk_dir: &Path, dir: &BString, fs: FsRules) -> std::io::Result<Option<Self>> {
        match std::fs::read_dir(disk_dir) {
            Ok(read) => Ok(Some(Self { read, fs })),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound && !dir.is_empty() => {
                Ok(None)
            }
            Err(source) => Err(source),
        }
    }
}

impl Iterator for WalkEntries {
    type Item = std::io::Result<(BString, PathBuf)>;

    fn next(&mut self) -> Option<Self::Item> {
        for entry in self.read.by_ref() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => return Some(Err(source)),
            };
            let name = self.fs.disk_name(&entry.file_name());
            if name != ".git" {
                return Some(Ok((name, entry.path())));
            }
        }
        None
    }
}

/// Recursive full-tree status walk. Holds the borrowed handle for its whole run; the
/// caller's `max_scan` cap bounds that.
struct StatusWalk<'repo> {
    context: WorktreeContext<'repo>,
    report: StatusReport,
    budget: ScanBudget,
}

impl StatusWalk<'_> {
    fn walk_dir(&mut self, tree: Option<Oid>, dir: &BString, disk_dir: &Path) -> Result<()> {
        let tree_entries = match tree {
            Some(oid) => tree_children(self.context.repo, oid)?,
            None => Vec::new(),
        };
        let by_name = self.context.fs.index(&tree_entries);
        let entries = match WalkEntries::open(disk_dir, dir, self.context.fs) {
            Ok(Some(entries)) => entries,
            // Gone since its parent was read: everything the tree has there is deleted.
            Ok(None) => return self.walk_missing(&tree_entries, &HashSet::new(), dir),
            Err(source) => return self.skip_unreadable_dir(dir, source),
        };
        let mut on_disk: HashSet<BString> = HashSet::new();
        for entry in entries {
            if self.budget.truncated {
                return Ok(());
            }
            let (name, disk_path) = match entry {
                Ok(entry) => entry,
                Err(source) => return self.skip_unreadable_dir(dir, source),
            };
            if !self.budget.visit() {
                return Ok(());
            }
            let (name, tracked) = by_name.resolve(name);
            let path = child_path(dir, &name);
            if self.walk_entry(tracked, path, &disk_path)? == Presence::OnDisk {
                on_disk.insert(name);
            }
        }
        self.walk_missing(&tree_entries, &on_disk, dir)
    }

    /// Records an unreadable directory below the root and skips it; the root fails the walk.
    fn skip_unreadable_dir(&mut self, dir: &BString, source: std::io::Error) -> Result<()> {
        skip_unreadable_dir(
            "scan working tree",
            dir,
            source,
            &mut self.report.unreadable,
        )
    }

    /// Reports one disk entry against its tree entry, descending into a directory. An entry
    /// that vanished since its directory was read is [`Presence::Gone`], so a tracked one is
    /// reported deleted; one that cannot be read is recorded and counts as present, so it is
    /// never reported deleted.
    fn walk_entry(
        &mut self,
        tracked: Option<&TreeEntry>,
        path: BString,
        disk_path: &Path,
    ) -> Result<Presence> {
        let meta = match DiskStat::from_path_no_follow(disk_path) {
            Ok(meta) => meta,
            Err(source) => return Ok(self.skip_entry(path, "stat working tree entry", source)),
        };
        let Some(disk) = disk_kind(&meta) else {
            self.special_file(tracked, path, meta.len())?;
            return Ok(Presence::OnDisk);
        };
        let kind = self.context.kind_of(disk, tracked);
        match tracked {
            Some(tree_entry) if tree_entry.kind == EntryKind::Tree && kind == EntryKind::Tree => {
                self.walk_dir(Some(tree_entry.oid), &path, disk_path)?;
            }
            Some(tree_entry)
                if kind == EntryKind::Tree && tree_entry.kind != EntryKind::Submodule =>
            {
                // A directory now sits where a tracked file was: the file is deleted and
                // everything in the directory is untracked, as git lists it.
                self.emit_deleted(tree_entry, path.clone());
                self.walk_untracked_dir(&path, disk_path)?;
            }
            Some(tree_entry) => {
                let disk = DiskFacts {
                    kind,
                    size: meta.len(),
                    meta: &meta,
                    path: disk_path,
                    rela: &path,
                };
                return self.walk_tracked_file(tree_entry, &disk);
            }
            None if kind == EntryKind::Tree => self.walk_untracked_dir(&path, disk_path)?,
            None => {
                if !self.context.excludes.is_ignored(&path, false) {
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
        Ok(Presence::OnDisk)
    }

    /// A tracked path whose disk kind is not a directory over a tracked directory.
    fn walk_tracked_file(
        &mut self,
        tree_entry: &TreeEntry,
        disk: &DiskFacts<'_>,
    ) -> Result<Presence> {
        let status = match self.context.compare(tree_entry, disk)? {
            Ok(status) => status,
            Err(error) => return Ok(self.skip_disk_error(disk.rela.clone(), error)),
        };
        if status == WorktreeStatus::Modified {
            self.report.entries.push(StatusEntry {
                path: disk.rela.clone(),
                kind: disk.kind,
                status,
                oid: (tree_entry.kind != EntryKind::Tree).then_some(tree_entry.oid),
                size: disk.size,
            });
        }
        if tree_entry.kind == EntryKind::Tree {
            // A file now sits where a tracked directory was: everything below the old
            // directory is deleted.
            self.walk_deleted(tree_entry.oid, disk.rela)?;
        }
        Ok(Presence::OnDisk)
    }

    fn skip_entry(&mut self, path: BString, op: &'static str, source: std::io::Error) -> Presence {
        self.skip_disk_error(path, DiskError::new(op, source))
    }

    /// A vanished entry is gone; any other failed read is recorded and the entry kept.
    fn skip_disk_error(&mut self, path: BString, error: DiskError) -> Presence {
        if error.is_vanished() {
            return Presence::Gone;
        }
        self.report.unreadable.push(error.into_unreadable(path));
        Presence::OnDisk
    }

    /// A directory the tree does not track: its files are untracked unless an ignore rule
    /// prunes it or it holds a repository of its own.
    fn walk_untracked_dir(&mut self, path: &BString, disk_path: &Path) -> Result<()> {
        if self.context.excludes.is_ignored(path, true) {
            self.report.ignored_dirs.push(path.clone());
            return Ok(());
        }
        if holds_nested_repository(disk_path) {
            self.report.nested_repos.push(path.clone());
            return Ok(());
        }
        self.walk_dir(None, path, disk_path)
    }

    /// The tree entries of `dir` with no counterpart on disk: deleted files and directories.
    fn walk_missing(
        &mut self,
        tree_entries: &[TreeEntry],
        on_disk: &HashSet<BString>,
        dir: &BString,
    ) -> Result<()> {
        for tree_entry in tree_entries {
            if self.budget.truncated {
                return Ok(());
            }
            if on_disk.contains(&tree_entry.name) {
                continue;
            }
            let path = child_path(dir, &tree_entry.name);
            match tree_entry.kind {
                EntryKind::Tree => self.walk_deleted(tree_entry.oid, &path)?,
                EntryKind::Submodule => {}
                EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink => {
                    if !self.budget.visit() {
                        return Ok(());
                    }
                    self.emit_deleted(tree_entry, path);
                }
            }
        }
        Ok(())
    }

    /// A FIFO, socket or device file, judged without opening it. Git lists one only when it
    /// replaced a tracked file, as modified; where a tracked directory was, the directory's
    /// files are deleted.
    fn special_file(
        &mut self,
        tracked: Option<&TreeEntry>,
        path: BString,
        size: u64,
    ) -> Result<()> {
        let Some(tree_entry) = tracked else {
            return Ok(());
        };
        match tree_entry.kind {
            EntryKind::Tree => self.walk_deleted(tree_entry.oid, &path),
            EntryKind::Submodule => Ok(()),
            EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink => {
                self.report.entries.push(StatusEntry {
                    path,
                    kind: tree_entry.kind,
                    status: WorktreeStatus::Modified,
                    oid: Some(tree_entry.oid),
                    size,
                });
                Ok(())
            }
        }
    }

    /// Everything below a tree with no counterpart on disk is deleted; walking it reads
    /// tree objects only, never the disk.
    fn walk_deleted(&mut self, tree: Oid, dir: &BString) -> Result<()> {
        for tree_entry in tree_children(self.context.repo, tree)? {
            if self.budget.truncated {
                return Ok(());
            }
            let path = child_path(dir, &tree_entry.name);
            match tree_entry.kind {
                EntryKind::Tree => self.walk_deleted(tree_entry.oid, &path)?,
                EntryKind::Submodule => {}
                EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink => {
                    if !self.budget.visit() {
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

/// Recursive walk listing every file on disk. Reads tree objects only to tell a tracked
/// directory, which is always entered, from an untracked one, which an ignore rule prunes.
struct FileWalk<'repo> {
    repo: &'repo gix::Repository,
    fs: FsRules,
    excludes: ExcludeCheck<'repo>,
    list: WorktreeFileList,
    budget: ScanBudget,
}

impl FileWalk<'_> {
    fn walk_dir(&mut self, tree: Option<Oid>, dir: &BString, disk_dir: &Path) -> Result<()> {
        let tree_entries = match tree {
            Some(oid) => tree_children(self.repo, oid)?,
            None => Vec::new(),
        };
        let by_name = self.fs.index(&tree_entries);
        let entries = match WalkEntries::open(disk_dir, dir, self.fs) {
            Ok(Some(entries)) => entries,
            // Gone since its parent was read: there is nothing on disk to list.
            Ok(None) => return Ok(()),
            Err(source) => return self.skip_unreadable_dir(dir, source),
        };
        for entry in entries {
            if self.budget.truncated {
                return Ok(());
            }
            let (name, disk_path) = match entry {
                Ok(entry) => entry,
                Err(source) => return self.skip_unreadable_dir(dir, source),
            };
            if !self.budget.visit() {
                return Ok(());
            }
            let (name, tracked) = by_name.resolve(name);
            self.walk_entry(tracked, child_path(dir, &name), &disk_path)?;
        }
        Ok(())
    }

    /// Records an unreadable directory below the root and skips it; the root fails the walk.
    fn skip_unreadable_dir(&mut self, dir: &BString, source: std::io::Error) -> Result<()> {
        skip_unreadable_dir(
            "list working tree files",
            dir,
            source,
            &mut self.list.unreadable,
        )
    }

    /// Lists one disk entry, descending into a directory. An entry that vanished since its
    /// directory was read is left out; one that cannot be stat'ed is recorded and skipped.
    fn walk_entry(
        &mut self,
        tracked: Option<&TreeEntry>,
        path: BString,
        disk_path: &Path,
    ) -> Result<()> {
        let meta = match DiskStat::from_path_no_follow(disk_path) {
            Ok(meta) => meta,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                let error = DiskError::new("stat working tree entry", source);
                self.list.unreadable.push(error.into_unreadable(path));
                return Ok(());
            }
        };
        let Some(disk) = disk_kind(&meta) else {
            return Ok(());
        };
        let kind = git_kind(
            disk,
            tracked.map(|entry| entry.kind),
            self.fs.executable_bit,
        );
        if kind != EntryKind::Tree {
            self.list.files.push(WorktreeFile { path, kind });
            return Ok(());
        }
        match tracked.map(|tree_entry| (tree_entry.kind, tree_entry.oid)) {
            Some((EntryKind::Tree, oid)) => self.walk_dir(Some(oid), &path, disk_path),
            Some((EntryKind::Submodule, _)) => Ok(()),
            Some((EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink, _)) | None => {
                if self.excludes.is_ignored(&path, true) || holds_nested_repository(disk_path) {
                    return Ok(());
                }
                self.walk_dir(None, &path, disk_path)
            }
        }
    }
}

/// Whether an untracked directory holds its own repository: a `.git` directory, file or
/// symlink of any kind. Its files belong to that repository, so a walk never enters it.
fn holds_nested_repository(disk_dir: &Path) -> bool {
    std::fs::symlink_metadata(disk_dir.join(".git")).is_ok()
}

/// What the repository's config says about its working tree's filesystem, read once per walk.
#[derive(Debug, Clone, Copy)]
struct FsRules {
    /// `core.fileMode`, which git turns off itself on a volume whose executable bit cannot be
    /// trusted (FAT, exFAT, many SMB mounts show every file as 0777).
    executable_bit: bool,
    /// `core.precomposeUnicode`: macOS hands back names in NFD, git records them in NFC.
    precompose: bool,
    /// `core.ignoreCase`: a disk name matches a tree entry that differs from it only in case.
    ignore_case: bool,
}

impl FsRules {
    fn new(repo: &gix::Repository, op: &'static str) -> Result<Self> {
        let options = repo.filesystem_options().map_err(git(op))?;
        Ok(Self {
            executable_bit: options.executable_bit,
            precompose: options.precompose_unicode,
            ignore_case: options.ignore_case,
        })
    }

    /// A name `read_dir` returned, spelled as git would record it. Only the name changes: the
    /// file is still opened through its `read_dir` path, which APFS resolves either way.
    fn disk_name(self, name: &OsStr) -> BString {
        let name = if self.precompose {
            gix::utils::str::precompose_os_string(Cow::Borrowed(name))
        } else {
            Cow::Borrowed(name)
        };
        BString::from(name.as_encoded_bytes())
    }

    /// Tree entries keyed by name, so matching a directory's files against the tree costs one
    /// lookup each rather than a scan of the whole tree listing per file.
    fn index(self, entries: &[TreeEntry]) -> TreeIndex<'_> {
        let mut exact = HashMap::with_capacity(entries.len());
        let mut folded = HashMap::new();
        for entry in entries {
            exact.insert(&entry.name, entry);
            if self.ignore_case {
                folded
                    .entry(entry.name.to_ascii_lowercase())
                    .or_insert(entry);
            }
        }
        TreeIndex { exact, folded }
    }
}

/// One tree's entries for matching disk names; `folded` is filled only under `core.ignoreCase`.
struct TreeIndex<'a> {
    exact: HashMap<&'a BString, &'a TreeEntry>,
    /// Entries keyed by ASCII-lowercased name, as git folds case.
    folded: HashMap<Vec<u8>, &'a TreeEntry>,
}

impl<'a> TreeIndex<'a> {
    /// The tree entry a disk name stands for, and the name to report: the entry's own name
    /// when there is one, so the path matches the tree and the index, else the disk name.
    fn resolve(&self, name: BString) -> (BString, Option<&'a TreeEntry>) {
        let tracked = match self.exact.get(&name) {
            Some(entry) => Some(*entry),
            None if self.folded.is_empty() => None,
            None => self.folded.get(&name.to_ascii_lowercase()).copied(),
        };
        match tracked {
            Some(entry) => (entry.name.clone(), Some(entry)),
            None => (name, None),
        }
    }
}

/// The kind git would record for a disk entry. With `core.fileMode` off the disk's executable
/// bit is ignored: a tracked file keeps the tree's kind and an untracked one is a plain blob.
fn git_kind(disk: EntryKind, tree: Option<EntryKind>, executable_bit: bool) -> EntryKind {
    let is_file = |kind| matches!(kind, EntryKind::Blob | EntryKind::ExecutableBlob);
    if executable_bit || !is_file(disk) {
        return disk;
    }
    match tree {
        Some(tree) if is_file(tree) => tree,
        Some(_) | None => EntryKind::Blob,
    }
}

/// What a disk entry is, from its `lstat`. Only the owner's executable bit counts, as in git:
/// a file executable by its group or others alone is still a plain blob. `None` for a FIFO,
/// socket or device file, which git cannot track and which is never opened: `open` on a FIFO
/// blocks until a writer appears.
fn disk_kind(meta: &DiskStat) -> Option<EntryKind> {
    if meta.is_symlink() {
        return Some(EntryKind::Symlink);
    }
    if meta.is_dir() {
        return Some(EntryKind::Tree);
    }
    if !meta.is_file() {
        return None;
    }
    Some(if meta.is_executable() {
        EntryKind::ExecutableBlob
    } else {
        EntryKind::Blob
    })
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

/// Spells a repository-relative directory one way: `/`, `./` and `.` segments dropped, so
/// the root is the empty string and `./src/` is `src`. A `..` segment is refused.
fn normalize_dir(dir: &str) -> std::result::Result<String, BoxError> {
    let mut segments = Vec::new();
    for segment in dir.split('/') {
        match segment {
            "" | "." => {}
            ".." => return Err("path may not contain a '..' segment".into()),
            _ => segments.push(segment),
        }
    }
    Ok(segments.join("/"))
}

/// Joins a repository-relative path under `root`, refusing traversal outside it: a `..`
/// segment, or a directory on the way to the last segment that is a symlink (git's "beyond a
/// symbolic link" rule). The last segment itself is not checked; callers lstat it.
fn join_under(root: &std::path::Path, relative: &str) -> std::result::Result<PathBuf, BoxError> {
    let segments: Vec<&str> = relative
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    if segments.contains(&"..") {
        return Err("path may not leave the working tree".into());
    }
    let mut joined = root.to_path_buf();
    for (index, segment) in segments.iter().enumerate() {
        joined.push(segment);
        if index + 1 < segments.len() {
            refuse_symlink(&joined, &segments[..=index])?;
        }
    }
    Ok(joined)
}

/// Refuses `full` when it is a symlink; a missing component is left to the caller's own read,
/// which reports it.
fn refuse_symlink(full: &std::path::Path, spelled: &[&str]) -> std::result::Result<(), BoxError> {
    match std::fs::symlink_metadata(full) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "crosses a symbolic link at '{}', which would leave the working tree",
            spelled.join("/")
        )
        .into()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Box::new(error)),
    }
}

/// Reads up to `limit + 1` bytes of `path`, so a caller can tell a file that grew past `limit`
/// from one that fits without ever holding more than one byte over the limit.
fn read_at_most(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut data)?;
    Ok(data)
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

#[cfg(test)]
mod tests {
    use super::*;
    use rc_git_testkit::Fixture;

    /// Runs `f` on a status walk over the standard fixture, with `HEAD`'s root tree entries.
    fn with_status_walk(f: impl FnOnce(&mut StatusWalk<'_>, &[TreeEntry], &Path)) {
        with_status_walk_in(&Fixture::standard(), f);
    }

    fn with_status_walk_in(fx: &Fixture, f: impl FnOnce(&mut StatusWalk<'_>, &[TreeEntry], &Path)) {
        let repo = Repository::open(fx.path()).unwrap();
        let tree = repo.resolve_tree("HEAD").unwrap();
        repo.with(|repo| {
            let mut walk = StatusWalk {
                context: WorktreeContext::new(repo, "scan working tree").unwrap(),
                report: StatusReport::default(),
                budget: ScanBudget::new(10_000),
            };
            let entries = tree_children(repo, tree).unwrap();
            f(&mut walk, &entries, fx.path());
        });
    }

    fn named<'a>(entries: &'a [TreeEntry], name: &str) -> &'a TreeEntry {
        entries.iter().find(|entry| entry.name == name).unwrap()
    }

    #[test]
    fn a_file_gone_before_its_stat_is_absent_not_an_error() {
        with_status_walk(|walk, entries, root| {
            let alpha = named(entries, "alpha.txt");
            let presence = walk
                .walk_entry(Some(alpha), "alpha.txt".into(), &root.join("vanished"))
                .unwrap();
            assert_eq!(presence, Presence::Gone);
            assert_eq!(walk.report.unreadable, []);
        });
    }

    #[test]
    fn a_file_gone_before_it_is_hashed_is_absent_not_an_error() {
        with_status_walk(|walk, entries, root| {
            let alpha = named(entries, "alpha.txt");
            let disk_path = root.join("vanished");
            let rela = BString::from("alpha.txt");
            // The root's stat, taken before the file vanished: it matches no index entry, so
            // the stat cache declines.
            let meta = DiskStat::from_path_no_follow(root).unwrap();
            let disk = DiskFacts {
                kind: EntryKind::Blob,
                // The blob's size, so only hashing the file can settle it.
                size: 6,
                meta: &meta,
                path: &disk_path,
                rela: &rela,
            };
            let presence = walk.walk_tracked_file(alpha, &disk).unwrap();
            assert_eq!(presence, Presence::Gone);
            assert_eq!(walk.report.entries, []);
            assert_eq!(walk.report.unreadable, []);
        });
    }

    /// The stat cache compares the index with the walk's own `lstat` and takes no second one:
    /// handed `alpha.txt`'s stat for a path that no longer exists, it still vouches for the
    /// file, where a second `lstat` of the path would fail and send it to be hashed.
    #[test]
    fn the_stat_cache_uses_the_walks_lstat_without_a_second_one() {
        let fx = Fixture::standard();
        // A second must pass so the index the `git add` writes is later than `alpha.txt`.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fx.write("Zeta.txt", b"omega\n");
        fx.git(&["add", "Zeta.txt"]);
        with_status_walk_in(&fx, |walk, entries, root| {
            let alpha = named(entries, "alpha.txt");
            let meta = DiskStat::from_path_no_follow(&root.join("alpha.txt")).unwrap();
            let disk_path = root.join("vanished");
            let rela = BString::from("alpha.txt");
            let disk = DiskFacts {
                kind: EntryKind::Blob,
                size: meta.len(),
                meta: &meta,
                path: &disk_path,
                rela: &rela,
            };
            let presence = walk.walk_tracked_file(alpha, &disk).unwrap();
            assert_eq!(presence, Presence::OnDisk);
            assert_eq!(walk.report.entries, []);
            assert_eq!(walk.report.unreadable, []);
        });
    }

    /// Walks `alpha.txt` (blob `alpha\n`, 6 bytes) as a file whose lstat said 6 bytes but which
    /// holds `contents` by the time it is opened: rewritten in place after the stat.
    fn walk_alpha_rewritten_to(contents: &[u8]) -> (Presence, StatusReport) {
        let fx = Fixture::standard();
        fx.write("rewritten.txt", contents);
        let mut outcome = None;
        with_status_walk_in(&fx, |walk, entries, root| {
            let alpha = named(entries, "alpha.txt");
            let disk_path = root.join("rewritten.txt");
            let rela = BString::from("alpha.txt");
            // Another inode than the one the index recorded, so the stat cache declines.
            let meta = DiskStat::from_path_no_follow(&disk_path).unwrap();
            let disk = DiskFacts {
                kind: EntryKind::Blob,
                size: 6,
                meta: &meta,
                path: &disk_path,
                rela: &rela,
            };
            let presence = walk.walk_tracked_file(alpha, &disk).unwrap();
            outcome = Some((presence, std::mem::take(&mut walk.report)));
        });
        outcome.unwrap()
    }

    fn modified_paths(report: &StatusReport) -> Vec<(String, WorktreeStatus)> {
        report
            .entries
            .iter()
            .map(|entry| (entry.path.to_string(), entry.status))
            .collect()
    }

    #[test]
    fn a_file_truncated_after_its_stat_is_modified_not_an_error() {
        let (presence, report) = walk_alpha_rewritten_to(b"al");
        assert_eq!(presence, Presence::OnDisk);
        assert_eq!(
            modified_paths(&report),
            [("alpha.txt".to_owned(), WorktreeStatus::Modified)]
        );
        assert_eq!(report.unreadable, []);
    }

    #[test]
    fn a_file_appended_to_after_its_stat_is_modified_not_its_prefix() {
        let (presence, report) = walk_alpha_rewritten_to(b"alpha\nmore\n");
        assert_eq!(presence, Presence::OnDisk);
        assert_eq!(
            modified_paths(&report),
            [("alpha.txt".to_owned(), WorktreeStatus::Modified)]
        );
        assert_eq!(report.unreadable, []);
    }

    #[test]
    fn a_tracked_directory_gone_before_it_is_read_lists_its_files_deleted() {
        with_status_walk(|walk, entries, root| {
            let src = named(entries, "src");
            walk.walk_dir(Some(src.oid), &"src".into(), &root.join("vanished"))
                .unwrap();
            let deleted: Vec<(String, WorktreeStatus)> = walk
                .report
                .entries
                .iter()
                .map(|entry| (entry.path.to_string(), entry.status))
                .collect();
            assert_eq!(
                deleted,
                [
                    ("src/lib.rs".to_owned(), WorktreeStatus::Deleted),
                    ("src/main.rs".to_owned(), WorktreeStatus::Deleted),
                ]
            );
            assert_eq!(walk.report.unreadable, []);
        });
    }

    #[test]
    fn a_directory_gone_before_it_is_listed_adds_no_files() {
        let fx = Fixture::standard();
        let repo = Repository::open(fx.path()).unwrap();
        repo.with(|repo| {
            let mut walk = FileWalk {
                repo,
                fs: FsRules::new(repo, "list working tree files").unwrap(),
                excludes: ExcludeCheck::new(repo),
                list: WorktreeFileList::default(),
                budget: ScanBudget::new(10_000),
            };
            walk.walk_dir(None, &"gone".into(), &fx.path().join("gone"))
                .unwrap();
            walk.walk_entry(None, "gone2".into(), &fx.path().join("gone2"))
                .unwrap();
            assert_eq!(walk.list.files, []);
            assert_eq!(walk.list.unreadable, []);
        });
    }
}

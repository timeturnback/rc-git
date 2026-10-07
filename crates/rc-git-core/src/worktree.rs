//! The working directory as a read-only view: files on disk with a status overlay against
//! `HEAD`. Nothing here writes; hashing a file to compare it with a blob reads it once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::index::entry::{Flags, Stage};
use gix::worktree::stack::state::ignore::Source;

use crate::blob::{BlobContent, classify};
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

/// One file on disk found by [`Repository::worktree_files`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeFile {
    /// Repository-relative path with `/` separators.
    pub path: BString,
    /// What the file is on disk: a blob, an executable blob, or a symlink.
    pub kind: EntryKind,
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
    /// same path below `tree`. `dir` is repository-relative with `/` separators; empty, `/`
    /// or `.` for the root, and `./` or doubled slashes name the same directory. Reads one directory and one tree object, plus one content read per file
    /// whose size matches its blob, so the cost stays proportional to the listing.
    /// A directory at or below a submodule lists every entry as unchanged with no tree
    /// entry: its files belong to another repository. `.git` is never listed.
    ///
    /// # Errors
    /// [`Error::Worktree`] for a bare repository, a `..` segment in `dir`, a symlink at `dir`
    /// or on the way to it, or an unreadable directory, [`Error::Object`] when the compared tree cannot be read.
    pub fn worktree_entries(&self, tree: Oid, dir: &str) -> Result<WorktreeListing> {
        let work_dir = self.require_work_dir("list working tree", dir)?;
        let dir = &normalize_dir(dir)
            .map_err(|source| worktree_error("list working tree", dir, source))?;
        let (tree_entries, in_submodule) = match self.tree_at(tree, dir)? {
            TreeAt::Entries(entries) => (entries, false),
            TreeAt::Submodule => (Vec::new(), true),
        };
        let disk_dir = join_under(&work_dir, dir)
            .and_then(|disk_dir| {
                refuse_symlink(&disk_dir, &[dir.as_str()])?;
                Ok(disk_dir)
            })
            .map_err(|source| worktree_error("list working tree", dir, source))?;
        let read = std::fs::read_dir(&disk_dir)
            .map_err(|source| worktree_error("list working tree", dir, Box::new(source)))?;

        let mut listing = WorktreeListing::default();
        let mut on_disk: Vec<(BString, Option<EntryKind>, u64, PathBuf)> = Vec::new();
        for entry in read {
            let entry = entry
                .map_err(|source| worktree_error("list working tree", dir, Box::new(source)))?;
            let name = BString::from(entry.file_name().as_encoded_bytes());
            if name == ".git" {
                continue;
            }
            let meta = std::fs::symlink_metadata(entry.path()).map_err(|source| {
                worktree_error("stat working tree entry", dir, Box::new(source))
            })?;
            on_disk.push((name, disk_kind(&meta), meta.len(), entry.path()));
        }

        let by_name = index_by_name(&tree_entries);
        self.with(|repo| {
            let mut context = WorktreeContext::new(repo, "list working tree")?;
            for (name, disk, size, path) in on_disk {
                let rela = relative_path(dir, &name);
                let in_tree = by_name.get(&name).copied();
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
                        path: &path,
                        rela: &rela,
                    };
                    context.compare(tree_entry, &disk, dir)?
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
                context: WorktreeContext::new(repo, "scan working tree")?,
                report: StatusReport::default(),
                max_scan,
            };
            walk.walk_dir(Some(tree), &BString::from(""), &work_dir)?;
            let mut report = walk.report;
            report.entries.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(report)
        })
    }

    /// Walks the whole working tree and returns every file on disk, sorted by path: tracked,
    /// untracked, and ignored files alike, so a gitignored `.env` is found. An ignored
    /// directory that `tree` does not track is pruned without reading it, and a submodule is
    /// not entered. The walk stops after visiting `max_scan` entries and reports the
    /// truncation, which bounds both the disk work and how long the borrowed pool handle is
    /// held. Nothing is hashed: this lists names, it does not compare content.
    ///
    /// # Errors
    /// [`Error::Worktree`] for a bare repository or an unreadable directory,
    /// [`Error::Object`] when a tree object on the walk cannot be read.
    pub fn worktree_files(&self, tree: Oid, max_scan: usize) -> Result<WorktreeFileList> {
        let work_dir = self.require_work_dir("list working tree files", "")?;
        self.with(|repo| {
            let mut walk = FileWalk {
                repo,
                executable_bit: executable_bit(repo, "list working tree files")?,
                excludes: ExcludeCheck::new(repo),
                list: WorktreeFileList::default(),
                max_scan,
            };
            walk.walk_dir(Some(tree), &BString::from(""), &work_dir)?;
            let mut list = walk.list;
            list.files.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(list)
        })
    }

    /// Reads one working-tree file for display, with the same size cap and binary probe as
    /// [`Repository::read_blob`]. A symlink yields its target as text.
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

    fn require_work_dir(&self, op: &'static str, path: &str) -> Result<PathBuf> {
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

/// What the compared tree holds at a listed directory.
enum TreeAt {
    /// The directory's entries; empty when the tree has no directory there.
    Entries(Vec<TreeEntry>),
    /// The directory is a submodule or lies inside one: another repository's files, which
    /// list as unchanged with no tree entry.
    Submodule,
}

/// What `symlink_metadata` said about the file being compared, and where to find it.
struct DiskFacts<'a> {
    kind: EntryKind,
    size: u64,
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
    /// `core.fileMode`: whether the disk's executable bit means anything in this repository.
    executable_bit: bool,
    excludes: ExcludeCheck<'repo>,
    stats: StatCache<'repo>,
}

impl<'repo> WorktreeContext<'repo> {
    fn new(repo: &'repo gix::Repository, op: &'static str) -> Result<Self> {
        Ok(Self {
            repo,
            executable_bit: executable_bit(repo, op)?,
            excludes: ExcludeCheck::new(repo),
            stats: StatCache::new(repo),
        })
    }

    /// The kind git would record for a disk entry of `disk` kind tracked as `tracked`.
    fn kind_of(&self, disk: EntryKind, tracked: Option<&TreeEntry>) -> EntryKind {
        git_kind(disk, tracked.map(|entry| entry.kind), self.executable_bit)
    }

    /// Whether a disk file differs from its tree entry. Kind decides first, then the blob
    /// header's size, then the index's stat cache; only a file none of those settle is read.
    fn compare(
        &mut self,
        tree_entry: &TreeEntry,
        disk: &DiskFacts<'_>,
        dir: &str,
    ) -> Result<WorktreeStatus> {
        if let Some(status) = kind_verdict(tree_entry, disk.kind) {
            return Ok(status);
        }
        if disk.kind == EntryKind::Symlink {
            return self.compare_symlink(tree_entry, disk.path, dir);
        }
        self.compare_content(tree_entry, disk, dir)
    }

    /// A symlink is its target. The blob header must name a blob of the target's length before
    /// the blob is decoded, so a tree entry pointing at a huge blob costs one header read.
    fn compare_symlink(
        &self,
        tree_entry: &TreeEntry,
        disk_path: &Path,
        dir: &str,
    ) -> Result<WorktreeStatus> {
        let target = std::fs::read_link(disk_path)
            .map_err(|source| worktree_error("read symlink", dir, Box::new(source)))?;
        let target = target.as_os_str().as_encoded_bytes();
        let header = self
            .repo
            .find_header(*tree_entry.oid.as_gix())
            .map_err(object("read blob header", tree_entry.oid))?;
        let same_length = u64::try_from(target.len()).is_ok_and(|len| len == header.size());
        if header.kind() != gix::objs::Kind::Blob || !same_length {
            return Ok(WorktreeStatus::Modified);
        }
        let blob = self
            .repo
            .find_blob(*tree_entry.oid.as_gix())
            .map_err(object("read blob", tree_entry.oid))?;
        Ok(if blob.data == target {
            WorktreeStatus::Unchanged
        } else {
            WorktreeStatus::Modified
        })
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
        dir: &str,
    ) -> Result<WorktreeStatus> {
        if self
            .stats
            .proves_unchanged(disk.rela, tree_entry.oid, disk.path)
        {
            return Ok(WorktreeStatus::Unchanged);
        }
        let header = self
            .repo
            .find_header(*tree_entry.oid.as_gix())
            .map_err(object("read blob header", tree_entry.oid))?;
        if header.size() != disk.size {
            return Ok(WorktreeStatus::Modified);
        }
        let mut file = std::fs::File::open(disk.path)
            .map_err(|source| worktree_error("hash working tree file", dir, Box::new(source)))?;
        let hashed = gix::objs::compute_stream_hash(
            self.repo.object_hash(),
            gix::objs::Kind::Blob,
            &mut file,
            disk.size,
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
/// change.
struct StatCache<'repo> {
    repo: &'repo gix::Repository,
    state: StatCacheState,
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
    fn proves_unchanged(&self, rela_path: &BString, oid: Oid, disk_path: &Path) -> bool {
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
        // `gix_index`'s own metadata, not `std::fs`: it fills `ctime` the way git does.
        let Ok(meta) = gix::index::fs::Metadata::from_path_no_follow(disk_path) else {
            return false;
        };
        let Ok(disk) = gix::index::entry::Stat::from_fs(&meta) else {
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
        }
    }

    /// Whether the index proves the file at `rela_path` is still `oid`. A repository with no
    /// readable index answers `false` for every path, which only costs the hash it would have
    /// cost anyway.
    fn proves_unchanged(&mut self, rela_path: &BString, oid: Oid, disk_path: &Path) -> bool {
        if let StatCacheState::Unbuilt = self.state {
            self.state = match Self::build(self.repo) {
                Some(loaded) => StatCacheState::Ready(loaded),
                None => StatCacheState::Unavailable,
            };
        }
        match &self.state {
            StatCacheState::Ready(loaded) => loaded.proves_unchanged(rela_path, oid, disk_path),
            StatCacheState::Unbuilt | StatCacheState::Unavailable => false,
        }
    }

    fn build(repo: &'repo gix::Repository) -> Option<LoadedIndex> {
        let index = repo.index().ok()?;
        let options = repo.stat_options().ok()?;
        Some(LoadedIndex { index, options })
    }
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
    context: WorktreeContext<'repo>,
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
            Some(oid) => tree_children(self.context.repo, oid)?,
            None => Vec::new(),
        };
        let by_name = index_by_name(&tree_entries);
        let dir_str = dir.to_str_lossy().into_owned();
        let read = std::fs::read_dir(disk_dir)
            .map_err(|source| worktree_error("scan working tree", &dir_str, Box::new(source)))?;
        let mut on_disk: HashSet<BString> = HashSet::new();
        for entry in read {
            if self.report.truncated {
                return Ok(());
            }
            let entry = entry.map_err(|source| {
                worktree_error("scan working tree", &dir_str, Box::new(source))
            })?;
            let name = BString::from(entry.file_name().as_encoded_bytes());
            if name == ".git" {
                continue;
            }
            if !self.visit() {
                return Ok(());
            }
            let meta = std::fs::symlink_metadata(entry.path()).map_err(|source| {
                worktree_error("stat working tree entry", &dir_str, Box::new(source))
            })?;
            let tracked = by_name.get(&name).copied();
            let path = child_path(dir, &name);
            on_disk.insert(name.clone());
            let Some(disk) = disk_kind(&meta) else {
                self.special_file(tracked, path, meta.len())?;
                continue;
            };
            let kind = self.context.kind_of(disk, tracked);
            match tracked {
                Some(tree_entry)
                    if tree_entry.kind == EntryKind::Tree && kind == EntryKind::Tree =>
                {
                    self.walk_dir(Some(tree_entry.oid), &path, &entry.path())?;
                }
                Some(tree_entry) => {
                    let disk_path = entry.path();
                    let disk = DiskFacts {
                        kind,
                        size: meta.len(),
                        path: &disk_path,
                        rela: &path,
                    };
                    let status = self.context.compare(tree_entry, &disk, &dir_str)?;
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
                    if self
                        .context
                        .excludes
                        .is_ignored(&path, kind == EntryKind::Tree)
                    {
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
        self.walk_missing(&tree_entries, &on_disk, dir)
    }

    /// The tree entries of `dir` with no counterpart on disk: deleted files and directories.
    fn walk_missing(
        &mut self,
        tree_entries: &[TreeEntry],
        on_disk: &HashSet<BString>,
        dir: &BString,
    ) -> Result<()> {
        for tree_entry in tree_entries {
            if self.report.truncated {
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
                    if !self.visit() {
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

/// Recursive walk listing every file on disk. Reads tree objects only to tell a tracked
/// directory, which is always entered, from an untracked one, which an ignore rule prunes.
struct FileWalk<'repo> {
    repo: &'repo gix::Repository,
    /// `core.fileMode`, as in [`WorktreeContext`].
    executable_bit: bool,
    excludes: ExcludeCheck<'repo>,
    list: WorktreeFileList,
    max_scan: usize,
}

impl FileWalk<'_> {
    /// Counts one visited entry; `false` when the cap is hit and the walk must stop.
    fn visit(&mut self) -> bool {
        self.list.scanned += 1;
        if self.list.scanned > self.max_scan {
            self.list.truncated = true;
        }
        !self.list.truncated
    }

    fn walk_dir(&mut self, tree: Option<Oid>, dir: &BString, disk_dir: &Path) -> Result<()> {
        let tree_entries = match tree {
            Some(oid) => tree_children(self.repo, oid)?,
            None => Vec::new(),
        };
        let by_name = index_by_name(&tree_entries);
        let dir_str = dir.to_str_lossy().into_owned();
        let read = std::fs::read_dir(disk_dir).map_err(|source| {
            worktree_error("list working tree files", &dir_str, Box::new(source))
        })?;
        for entry in read {
            if self.list.truncated {
                return Ok(());
            }
            let entry = entry.map_err(|source| {
                worktree_error("list working tree files", &dir_str, Box::new(source))
            })?;
            let name = BString::from(entry.file_name().as_encoded_bytes());
            if name == ".git" {
                continue;
            }
            if !self.visit() {
                return Ok(());
            }
            let meta = std::fs::symlink_metadata(entry.path()).map_err(|source| {
                worktree_error("stat working tree entry", &dir_str, Box::new(source))
            })?;
            let Some(disk) = disk_kind(&meta) else {
                continue;
            };
            let tracked = by_name.get(&name).copied();
            let kind = git_kind(disk, tracked.map(|entry| entry.kind), self.executable_bit);
            let path = child_path(dir, &name);
            if kind != EntryKind::Tree {
                self.list.files.push(WorktreeFile { path, kind });
                continue;
            }
            match tracked.map(|tree_entry| (tree_entry.kind, tree_entry.oid)) {
                Some((EntryKind::Tree, oid)) => self.walk_dir(Some(oid), &path, &entry.path())?,
                Some((EntryKind::Submodule, _)) => {}
                Some((EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink, _))
                | None => {
                    if !self.excludes.is_ignored(&path, true) {
                        self.walk_dir(None, &path, &entry.path())?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// The repository's `core.fileMode`, which git turns off itself on a volume whose executable
/// bit cannot be trusted (FAT, exFAT, many SMB mounts show every file as 0777).
fn executable_bit(repo: &gix::Repository, op: &'static str) -> Result<bool> {
    repo.filesystem_options()
        .map(|options| options.executable_bit)
        .map_err(git(op))
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

/// What a disk entry is, from its `symlink_metadata`. Only the owner's executable bit counts,
/// as in git: a file executable by its group or others alone is still a plain blob. `None` for
/// a FIFO, socket or device file, which git cannot track and which is never opened: `open` on
/// a FIFO blocks until a writer appears.
fn disk_kind(meta: &std::fs::Metadata) -> Option<EntryKind> {
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return Some(EntryKind::Symlink);
    }
    if file_type.is_dir() {
        return Some(EntryKind::Tree);
    }
    if !file_type.is_file() {
        return None;
    }
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o100 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    Some(if executable {
        EntryKind::ExecutableBlob
    } else {
        EntryKind::Blob
    })
}

/// Tree entries keyed by name, so matching a directory's files against the tree costs one
/// lookup each rather than a scan of the whole tree listing per file.
fn index_by_name(entries: &[TreeEntry]) -> HashMap<&BString, &TreeEntry> {
    entries.iter().map(|entry| (&entry.name, entry)).collect()
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

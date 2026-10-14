//! Differences between two trees and between two blobs.

use gix::bstr::BString;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, HunkHeader};
use gix::diff::blob::{Algorithm, Diff, InternedInput, UnifiedDiff};
use gix::object::tree::diff::{Action, Change};

use crate::blob::BlobContent;
use crate::error::{Result, git, object};
use crate::oid::Oid;
use crate::repo::Repository;
use crate::tree::EntryKind;

/// Default number of unchanged lines shown around each hunk, as in `git diff`.
pub const DEFAULT_DIFF_CONTEXT: u32 = 3;

/// What happened to one path between two trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    /// The path exists only in the new tree.
    Added,
    /// The path exists only in the old tree.
    Deleted,
    /// The path exists in both trees with different content or mode.
    Modified,
    /// The content moved from `source_path`; the old path is gone.
    Renamed,
}

/// One changed file between two trees. Directories are folded into their entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeChange {
    /// What happened.
    pub kind: ChangeKind,
    /// The path in the new tree, or in the old tree for a deletion.
    pub path: BString,
    /// Where the content came from, for renames.
    pub source_path: Option<BString>,
    /// The blob before the change. `None` for additions.
    pub old: Option<Oid>,
    /// The blob after the change. `None` for deletions.
    pub new: Option<Oid>,
    /// What the entry is after the change, or was before it for deletions.
    pub entry: EntryKind,
    /// What the entry was before the change. `None` for additions. It differs from `entry`
    /// when the path changed kind, such as a file replaced by a submodule.
    pub old_entry: Option<EntryKind>,
}

/// A line of a diff hunk, without its line terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// Whether the line is context, added, or removed.
    pub kind: DiffLineKind,
    /// The bytes of the line, its terminator stripped.
    pub text: BString,
    /// How the line ended in its blob, so a change in terminator alone stays visible.
    pub eol: LineEnd,
}

/// The terminator a diff line had before it was stripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineEnd {
    /// `\n`.
    Lf,
    /// `\r\n`.
    CrLf,
    /// None: the last line of a blob that does not end in a newline.
    None,
}

/// The role of one line in a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffLineKind {
    /// Unchanged, shown for context.
    Context,
    /// Present only in the new blob.
    Added,
    /// Present only in the old blob.
    Removed,
}

/// A run of changed lines with its surrounding context, as in unified diff output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    /// 1-based first line of the hunk in the old blob. When `old_lines` is 0 it is the line
    /// the empty range follows, 0 at the top of the blob, as in git's `@@` header.
    pub old_start: u32,
    /// Number of old-blob lines the hunk covers.
    pub old_lines: u32,
    /// 1-based first line of the hunk in the new blob. When `new_lines` is 0 it is the line
    /// the empty range follows, 0 at the top of the blob, as in git's `@@` header.
    pub new_start: u32,
    /// Number of new-blob lines the hunk covers.
    pub new_lines: u32,
    /// The lines, old and new interleaved the way `git diff` prints them.
    pub lines: Vec<DiffLine>,
}

/// What a blob diff produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobDiff {
    /// Both sides are text; `hunks` is empty when they are identical.
    Text {
        /// The changed regions.
        hunks: Vec<DiffHunk>,
    },
    /// One side contains a NUL byte near the start; no line diff is possible.
    Binary {
        /// Size of the old blob, 0 when absent.
        old_size: u64,
        /// Size of the new blob, 0 when absent.
        new_size: u64,
    },
    /// One side is larger than the caller's limit; nothing was decoded.
    TooLarge {
        /// Size of the larger side.
        size: u64,
        /// The limit that was exceeded.
        limit: u64,
    },
}

impl Repository {
    /// Lists every file that differs between two trees, renames detected, sorted by path.
    /// `from` and `to` may be any objects that peel to trees; `None` for `from` stands for the
    /// empty tree, which turns a root commit's diff into pure additions.
    ///
    /// Rename detection is pinned to gix's defaults (50% similarity, at most 1000 candidate
    /// pairs) whatever the repository's config says: copies are never reported, and
    /// `diff.renames=false` and `diff.renameLimit` are not honoured. A copy's source therefore
    /// keeps its own entry, and its destination is an addition.
    ///
    /// The cost scales with the difference between the trees, not with repository size:
    /// unchanged subtrees share an id and are skipped whole.
    ///
    /// # Errors
    /// [`Error::WrongKind`](crate::Error::WrongKind) when either id does not peel to a tree,
    /// [`Error::Object`](crate::Error::Object) when either id is missing, or when a tree below
    /// them cannot be read during the diff (then naming `to`).
    pub fn diff_trees(&self, from: Option<Oid>, to: Oid) -> Result<Vec<TreeChange>> {
        self.diff_trees_with(from, to, Some(gix::diff::Rewrites::default()))
    }

    /// Lists every file that differs between two trees with no rename detection, sorted by
    /// path: a moved file is a deletion plus an addition. For callers that write the change
    /// path by path and would discard a pairing, so no blob is loaded to measure similarity.
    ///
    /// # Errors
    /// The same as [`Repository::diff_trees`].
    pub fn diff_trees_without_renames(
        &self,
        from: Option<Oid>,
        to: Oid,
    ) -> Result<Vec<TreeChange>> {
        self.diff_trees_with(from, to, None)
    }

    fn diff_trees_with(
        &self,
        from: Option<Oid>,
        to: Oid,
        rewrites: Option<gix::diff::Rewrites>,
    ) -> Result<Vec<TreeChange>> {
        let from = from.map(|id| self.tree_of(id)).transpose()?;
        let to = self.tree_of(to)?;
        self.with(|repo| {
            let from_tree = match from {
                Some(id) => repo
                    .find_tree(*id.as_gix())
                    .map_err(object("read tree", id))?,
                None => repo.empty_tree(),
            };
            let to_tree = repo
                .find_tree(*to.as_gix())
                .map_err(object("read tree", to))?;
            let mut changes: Vec<TreeChange> = Vec::new();
            from_tree
                .changes()
                .map_err(object("prepare tree diff", to))?
                .options(|options| {
                    options.track_path().track_rewrites(rewrites);
                })
                .for_each_to_obtain_tree(&to_tree, |change| {
                    if let Some(mapped) = map_change(&change) {
                        changes.push(mapped);
                    }
                    Ok::<_, std::convert::Infallible>(Action::Continue(()))
                })
                .map_err(object("diff trees", to))?;
            changes.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(changes)
        })
    }

    /// Line diff between two blobs, either of which may be absent for an added or deleted
    /// file. Both sizes are read from the object headers and checked against `limit` before
    /// either side is decoded, and a binary old side settles the result without decoding the
    /// new one.
    ///
    /// A side of a [`TreeChange`] whose kind (`old_entry` for `old`, `entry` for `new`) is
    /// [`EntryKind::Submodule`] carries a gitlink id: a commit in the submodule's repository,
    /// not an object in this one. Callers pass `None` for that side instead of its id.
    ///
    /// # Errors
    /// [`Error::Object`](crate::Error::Object) when a given id is missing (a gitlink id
    /// included), [`Error::WrongKind`](crate::Error::WrongKind) when it is not a blob.
    pub fn diff_blobs(
        &self,
        old: Option<Oid>,
        new: Option<Oid>,
        limit: u64,
        context: u32,
    ) -> Result<BlobDiff> {
        let old_size = self.side_size(old)?;
        let new_size = self.side_size(new)?;
        if let Some(too_large) = too_large(old_size, new_size, limit) {
            return Ok(too_large);
        }
        let old_data = self.blob_for_diff(old, limit)?;
        if let DiffSide::Binary { size } = old_data {
            return Ok(BlobDiff::Binary {
                old_size: size,
                new_size,
            });
        }
        let new_data = self.blob_for_diff(new, limit)?;
        diff_sides(old_data, new_data, context)
    }

    /// Line diff between the blob at `old` and the working-tree file at `path`, both as raw
    /// bytes: gitattributes filters and eol conversion are not applied. `old` is absent for
    /// an untracked file, which makes the diff pure additions. The blob header and the file's
    /// metadata are checked against `limit` before either side is decoded.
    ///
    /// # Errors
    /// [`Error::Object`](crate::Error::Object) when `old` is missing,
    /// [`Error::WrongKind`](crate::Error::WrongKind) when it is not a blob,
    /// [`Error::Worktree`](crate::Error::Worktree) when the file cannot be read.
    pub fn diff_worktree_file(
        &self,
        old: Option<Oid>,
        path: &str,
        limit: u64,
        context: u32,
    ) -> Result<BlobDiff> {
        let old_size = self.side_size(old)?;
        let new_size = self.worktree_file_size(path)?;
        if let Some(too_large) = too_large(old_size, new_size, limit) {
            return Ok(too_large);
        }
        let old_data = self.blob_for_diff(old, limit)?;
        if let DiffSide::Binary { size } = old_data {
            return Ok(BlobDiff::Binary {
                old_size: size,
                new_size,
            });
        }
        let new_data = DiffSide::from(self.worktree_file(path, limit)?);
        diff_sides(old_data, new_data, context)
    }

    /// Header size of one diff side; an absent side is empty.
    fn side_size(&self, oid: Option<Oid>) -> Result<u64> {
        oid.map_or(Ok(0), |oid| self.blob_size(oid))
    }

    fn blob_for_diff(&self, oid: Option<Oid>, limit: u64) -> Result<DiffSide> {
        match oid {
            None => Ok(DiffSide::Text(Vec::new())),
            Some(oid) => self.read_blob(oid, limit).map(DiffSide::from),
        }
    }
}

/// The result when either side is over `limit`, naming the larger size.
fn too_large(old_size: u64, new_size: u64, limit: u64) -> Option<BlobDiff> {
    let size = old_size.max(new_size);
    (size > limit).then_some(BlobDiff::TooLarge { size, limit })
}

/// Runs the line diff once both sides are classified.
fn diff_sides(old: DiffSide, new: DiffSide, context: u32) -> Result<BlobDiff> {
    let (old_data, new_data) = match (old, new) {
        (DiffSide::Text(old), DiffSide::Text(new)) => (old, new),
        (DiffSide::TooLarge { size, limit }, _) | (_, DiffSide::TooLarge { size, limit }) => {
            return Ok(BlobDiff::TooLarge { size, limit });
        }
        (old, new) => {
            return Ok(BlobDiff::Binary {
                old_size: old.size(),
                new_size: new.size(),
            });
        }
    };
    let input = InternedInput::new(old_data.as_slice(), new_data.as_slice());
    let diff = Diff::compute(Algorithm::Histogram, &input);
    let unified = UnifiedDiff::new(
        &diff,
        &input,
        CollectHunks::default(),
        ContextSize::symmetrical(clamp_context(context, &input)),
    );
    let hunks = unified.consume().map_err(git("collect diff hunks"))?;
    Ok(BlobDiff::Text { hunks })
}

/// Caps `context` at the longer side's line count: more context adds nothing, and gix-diff's
/// unchecked `ctx_pos + ctx_size` and `2 * ctx_size` overflow on values near `u32::MAX`.
fn clamp_context(context: u32, input: &InternedInput<&[u8]>) -> u32 {
    let lines = input.before.len().max(input.after.len());
    let half_range = u32::try_from(i32::MAX).unwrap_or(u32::MAX);
    context
        .min(u32::try_from(lines).unwrap_or(u32::MAX))
        .min(half_range)
}

enum DiffSide {
    Text(Vec<u8>),
    Binary { size: u64 },
    TooLarge { size: u64, limit: u64 },
}

impl From<BlobContent> for DiffSide {
    fn from(content: BlobContent) -> Self {
        match content {
            BlobContent::Text { data, .. } => DiffSide::Text(data),
            BlobContent::Binary { size } => DiffSide::Binary { size },
            BlobContent::TooLarge { size, limit } => DiffSide::TooLarge { size, limit },
        }
    }
}

impl DiffSide {
    fn size(&self) -> u64 {
        match self {
            DiffSide::Text(data) => u64::try_from(data.len()).unwrap_or(u64::MAX),
            DiffSide::Binary { size } | DiffSide::TooLarge { size, .. } => *size,
        }
    }
}

#[derive(Default)]
struct CollectHunks {
    hunks: Vec<DiffHunk>,
}

impl ConsumeHunk for CollectHunks {
    type Out = Vec<DiffHunk>;

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(gix::diff::blob::unified_diff::DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        let mut collected = Vec::with_capacity(lines.len());
        for (kind, bytes) in lines {
            let kind = match kind {
                gix::diff::blob::unified_diff::DiffLineKind::Context => DiffLineKind::Context,
                gix::diff::blob::unified_diff::DiffLineKind::Add => DiffLineKind::Added,
                gix::diff::blob::unified_diff::DiffLineKind::Remove => DiffLineKind::Removed,
            };
            let (text, eol) = split_line_end(bytes);
            collected.push(DiffLine {
                kind,
                text: text.into(),
                eol,
            });
        }
        self.hunks.push(DiffHunk {
            old_start: unified_start(header.before_hunk_start, header.before_hunk_len),
            old_lines: header.before_hunk_len,
            new_start: unified_start(header.after_hunk_start, header.after_hunk_len),
            new_lines: header.after_hunk_len,
            lines: collected,
        });
        Ok(())
    }

    fn finish(self) -> Self::Out {
        self.hunks
    }
}

/// git's start line for one side of a hunk. gix numbers an empty side from the line after
/// the gap; unified diff numbers it from the line before, 0 at the top of the file.
fn unified_start(gix_start: u32, len: u32) -> u32 {
    if len == 0 {
        gix_start.saturating_sub(1)
    } else {
        gix_start
    }
}

/// Splits a line into its bytes and its terminator. A `\r` not followed by `\n` is content.
fn split_line_end(line: &[u8]) -> (&[u8], LineEnd) {
    if let Some(text) = line.strip_suffix(b"\r\n") {
        (text, LineEnd::CrLf)
    } else if let Some(text) = line.strip_suffix(b"\n") {
        (text, LineEnd::Lf)
    } else {
        (line, LineEnd::None)
    }
}

fn map_change(change: &Change<'_, '_, '_>) -> Option<TreeChange> {
    match *change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        } => (!entry_mode.is_tree()).then(|| TreeChange {
            kind: ChangeKind::Added,
            path: location.to_owned(),
            source_path: None,
            old: None,
            new: Some(Oid::from_gix(id.detach())),
            entry: EntryKind::from_gix(entry_mode.kind()),
            old_entry: None,
        }),
        Change::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => (!entry_mode.is_tree()).then(|| TreeChange {
            kind: ChangeKind::Deleted,
            path: location.to_owned(),
            source_path: None,
            old: Some(Oid::from_gix(id.detach())),
            new: None,
            entry: EntryKind::from_gix(entry_mode.kind()),
            old_entry: Some(EntryKind::from_gix(entry_mode.kind())),
        }),
        Change::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            ..
        } => (!entry_mode.is_tree()).then(|| TreeChange {
            kind: ChangeKind::Modified,
            path: location.to_owned(),
            source_path: None,
            old: Some(Oid::from_gix(previous_id.detach())),
            new: Some(Oid::from_gix(id.detach())),
            entry: EntryKind::from_gix(entry_mode.kind()),
            old_entry: Some(EntryKind::from_gix(previous_entry_mode.kind())),
        }),
        Change::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            entry_mode,
            location,
            id,
            ..
        } => (!entry_mode.is_tree()).then(|| TreeChange {
            kind: ChangeKind::Renamed,
            path: location.to_owned(),
            source_path: Some(source_location.to_owned()),
            old: Some(Oid::from_gix(source_id.detach())),
            new: Some(Oid::from_gix(id.detach())),
            entry: EntryKind::from_gix(entry_mode.kind()),
            old_entry: Some(EntryKind::from_gix(source_entry_mode.kind())),
        }),
    }
}

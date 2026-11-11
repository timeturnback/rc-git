//! Port of `PathSearch.swift`: the flattened file index behind jump-to-file, and the fuzzy
//! scorer that ranks it.

use std::cmp::Ordering;
use std::sync::atomic::{self, AtomicBool};

use rc_git_core::bstr::ByteSlice;
use rc_git_core::{EntryKind, Oid, TreeEntry, WorktreeFileList};
use unicode_normalization::UnicodeNormalization;

use crate::hidden_files::{is_hidden_name, is_hidden_path};

/// One file in the flattened tree below a root. `path` is repository-relative with `/`
/// separators, decoded for display (invalid UTF-8 becomes U+FFFD).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathIndexEntry {
    /// Repository-relative path with `/` separators.
    pub path: String,
    /// The blob id; `None` for a file listed from disk, which has no id until it is hashed.
    pub oid: Option<Oid>,
    /// What the file is.
    pub kind: EntryKind,
    /// Whether the path is under [`crate::hidden_files`], so search can leave it out.
    pub hidden: bool,
    /// Whether every component of `path` is the stored name byte for byte, i.e. decoding
    /// replaced nothing.
    pub has_exact_path: bool,
    search_key: String,
}

impl PathIndexEntry {
    /// An entry for `path`; computes the search key once.
    #[must_use]
    pub fn new(
        path: String,
        oid: Option<Oid>,
        kind: EntryKind,
        hidden: bool,
        has_exact_path: bool,
    ) -> Self {
        let search_key = search_key(&path);
        Self {
            path,
            oid,
            kind,
            hidden,
            has_exact_path,
            search_key,
        }
    }

    /// `path` in precomposed (NFC) form, which search matches against, so a name stored
    /// decomposed on disk matches the precomposed letters a keyboard types. Equal to `path`
    /// when the path is ASCII.
    #[must_use]
    pub fn search_key(&self) -> &str {
        &self.search_key
    }

    /// The file name after the last `/`.
    #[must_use]
    pub fn name(&self) -> &str {
        match self.path.rfind('/') {
            Some(slash) => &self.path[slash + 1..],
            None => &self.path,
        }
    }
}

/// A path index walk or search stopped because its cancel flag was set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("path search cancelled")]
pub struct Cancelled;

/// Why [`PathIndex::build`] produced no index.
#[derive(Debug, thiserror::Error)]
pub enum BuildError<E: std::error::Error + 'static> {
    /// The cancel flag was set before a directory read.
    #[error("path index build cancelled")]
    Cancelled,
    /// Listing one directory failed.
    #[error("path index: list tree {tree}: {source}")]
    Read {
        /// The tree whose listing failed.
        tree: Oid,
        /// What the listing returned.
        source: E,
    },
}

/// One step of `build`'s walk: a directory still to read, or files ready to emit.
enum PendingWalk {
    Tree {
        oid: Oid,
        prefix: String,
        hidden: bool,
        exact: bool,
    },
    Files(Vec<PathIndexEntry>),
}

/// Every file below one root tree, in depth-first display order. Keyed by the root tree id,
/// so the index for a ref stays valid until the ref moves to a different tree. The working
/// tree's index is built from disk instead and keyed by the `HEAD` tree it was walked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathIndex {
    /// The tree the index was built from (or compared against, for a working tree).
    pub root: Oid,
    /// Every file, in display order.
    pub entries: Vec<PathIndexEntry>,
    /// True when the walk stopped at its limit; matches may be missing.
    pub truncated: bool,
}

impl PathIndex {
    /// Upper bound on indexed files, so one pathological repository cannot exhaust memory.
    pub const MAX_ENTRIES: usize = 200_000;
    /// Entries `search` scores between two cancellation checks.
    pub const CANCELLATION_STRIDE: usize = 4_096;
    /// The note to show while a search result is truncated. It says entries, not files: the
    /// working-tree walk counts directories too, in disk order, so the cut is not "the first N
    /// files".
    pub const TRUNCATION_NOTE: &str =
        "Stopped indexing at 200,000 entries; some files may not be found";

    /// Flattens the tree below `root`, one tree object per directory, depth-first, stopping at
    /// `limit` files. Each directory lists its subdirectories first, each in full and in the
    /// order `children` returns them, then its own files. Submodules are skipped. This is the
    /// one operation that walks a whole tree, so it runs off the UI thread and checks `cancel`
    /// before every directory read. `children` is the read seam: the live caller passes
    /// `Repository::tree_entries`.
    ///
    /// # Errors
    /// [`BuildError::Cancelled`] when `cancel` is set before a directory read;
    /// [`BuildError::Read`] with the failing tree when `children` fails.
    pub fn build<E, F>(
        root: Oid,
        limit: usize,
        cancel: &AtomicBool,
        mut children: F,
    ) -> Result<Self, BuildError<E>>
    where
        E: std::error::Error + 'static,
        F: FnMut(Oid) -> Result<Vec<TreeEntry>, E>,
    {
        let mut entries = Vec::new();
        let mut pending = vec![PendingWalk::Tree {
            oid: root,
            prefix: String::new(),
            hidden: false,
            exact: true,
        }];
        while let Some(next) = pending.pop() {
            match next {
                PendingWalk::Files(files) => {
                    for file in files {
                        if entries.len() >= limit {
                            return Ok(Self {
                                root,
                                entries,
                                truncated: true,
                            });
                        }
                        entries.push(file);
                    }
                }
                PendingWalk::Tree {
                    oid,
                    prefix,
                    hidden,
                    exact,
                } => {
                    if cancel.load(atomic::Ordering::Relaxed) {
                        return Err(BuildError::Cancelled);
                    }
                    let listed =
                        children(oid).map_err(|source| BuildError::Read { tree: oid, source })?;
                    pending.extend(read_directory(&listed, &prefix, hidden, exact));
                }
            }
        }
        Ok(Self {
            root,
            entries,
            truncated: false,
        })
    }

    /// Every file a working-tree walk found, compared against the tree `root`. Disk files
    /// carry no blob id, so their entries hold `None`.
    #[must_use]
    pub fn from_worktree_files(root: Oid, list: &WorktreeFileList) -> Self {
        let mut entries = Vec::with_capacity(list.files.len());
        for file in &list.files {
            let exact = file.path.to_str().is_ok();
            entries.push(PathIndexEntry::new(
                file.path.to_str_lossy().into_owned(),
                None,
                file.kind,
                is_hidden_path(&file.path),
                exact,
            ));
        }
        Self {
            root,
            entries,
            truncated: list.truncated,
        }
    }

    /// The best `limit` matches for `query`, highest score first, ties by shorter path (in
    /// UTF-8 bytes) then path order, so the order is stable. A blank query returns the first
    /// entries. Entries marked hidden are left out unless `include_hidden`. The query is
    /// trimmed of spaces and tabs and matched in precomposed (NFC) form against each entry's
    /// search key. The scan checks `cancel` every [`Self::CANCELLATION_STRIDE`] entries, so a
    /// superseded query stops within milliseconds.
    ///
    /// # Errors
    /// [`Cancelled`] when `cancel` is set during the scan.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        include_hidden: bool,
        cancel: &AtomicBool,
    ) -> Result<Vec<&PathIndexEntry>, Cancelled> {
        let query: String = query.trim_matches(is_horizontal_space).nfc().collect();
        if query.is_empty() {
            let mut first = Vec::new();
            for entry in &self.entries {
                if first.len() == limit {
                    break;
                }
                if include_hidden || !entry.hidden {
                    first.push(entry);
                }
            }
            return Ok(first);
        }
        let mut scored = self.matches(&query, include_hidden, cancel)?;
        scored.sort_by(ranks_before);
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, entry)| entry).collect())
    }

    /// Every entry `query` matches, with its score, in index order.
    fn matches(
        &self,
        query: &str,
        include_hidden: bool,
        cancel: &AtomicBool,
    ) -> Result<Vec<(i64, &PathIndexEntry)>, Cancelled> {
        let mut scored = Vec::new();
        for (position, entry) in self.entries.iter().enumerate() {
            if position % Self::CANCELLATION_STRIDE == 0 && cancel.load(atomic::Ordering::Relaxed) {
                return Err(Cancelled);
            }
            if !include_hidden && entry.hidden {
                continue;
            }
            if let Some(score) = score(&entry.search_key, query) {
                scored.push((score, entry));
            }
        }
        Ok(scored)
    }
}

/// The stack pushes for one directory: its files as one chunk, then its subdirectories in
/// reverse, so the first subdirectory pops next and the files pop after every subdirectory.
fn read_directory(
    children: &[TreeEntry],
    prefix: &str,
    parent_hidden: bool,
    parent_exact: bool,
) -> Vec<PendingWalk> {
    let mut files = Vec::new();
    let mut trees = Vec::new();
    for entry in children {
        let name = entry.name_lossy();
        let path = if prefix.is_empty() {
            name.into_owned()
        } else {
            format!("{prefix}/{name}")
        };
        let hidden = parent_hidden || is_hidden_name(&entry.name);
        let exact = parent_exact && entry.name.to_str().is_ok();
        match entry.kind {
            EntryKind::Tree => trees.push(PendingWalk::Tree {
                oid: entry.oid,
                prefix: path,
                hidden,
                exact,
            }),
            EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink => {
                files.push(PathIndexEntry::new(
                    path,
                    Some(entry.oid),
                    entry.kind,
                    hidden,
                    exact,
                ));
            }
            EntryKind::Submodule => {}
        }
    }
    let mut pushes = Vec::with_capacity(trees.len() + 1);
    pushes.push(PendingWalk::Files(files));
    pushes.extend(trees.into_iter().rev());
    pushes
}

/// Search order: higher score first, then the shorter path in bytes, then the path itself
/// (compared in NFC, as Swift's `String` compares, then byte for byte).
fn ranks_before(left: &(i64, &PathIndexEntry), right: &(i64, &PathIndexEntry)) -> Ordering {
    right
        .0
        .cmp(&left.0)
        .then_with(|| left.1.path.len().cmp(&right.1.path.len()))
        .then_with(|| left.1.search_key.cmp(&right.1.search_key))
        .then_with(|| left.1.path.cmp(&right.1.path))
}

/// Whether `ch` is what Foundation's `CharacterSet.whitespaces` holds: tab and the Unicode
/// space separators (category Zs), not line breaks.
fn is_horizontal_space(ch: char) -> bool {
    matches!(
        ch,
        '\t' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// Points for a match directly after the previous one.
const CONSECUTIVE_BONUS: i64 = 8;
/// Points for a match at a word start: after `/`, `.`, `_`, `-`, or at the beginning. Applied
/// only when the match does not continue a run, so a run scores as one word.
const BOUNDARY_BONUS: i64 = 10;
/// Points lost for every gap between two matched characters, however long.
const GAP_PENALTY: i64 = 3;
/// Extra points when the whole match sits inside the file name, not the directories.
const FILE_NAME_BONUS: i64 = 20;

/// Score of matching `query` as a subsequence of `candidate`, or `None` when it is not one.
///
/// Case-insensitive subsequence matching for jump-to-file: greedy left-to-right with bonuses
/// for runs and word starts and no backtracking, so scoring 200 000 paths stays cheap.
/// Comparison is ASCII-case-insensitive. An ASCII query compares bytes, since an ASCII byte
/// never occurs inside a multi-byte character; any other query compares whole Unicode scalars,
/// so one typed letter never matches bytes taken from different letters. Both strings are
/// expected in the same normalization form (see [`search_key`]). When the leftmost match
/// starts in a directory, the scan runs once more from the file name, so `Model/Model.swift`
/// still earns the file-name bonus for `model`.
#[must_use]
pub fn score(candidate: &str, query: &str) -> Option<i64> {
    if query.is_ascii() {
        return score_units(candidate.as_bytes(), query.as_bytes());
    }
    let path: Vec<u32> = candidate.chars().map(u32::from).collect();
    let needle: Vec<u32> = query.chars().map(u32::from).collect();
    score_units(&path, &needle)
}

/// `text` in the form [`score`] compares: as is when ASCII, else precomposed (NFC), so a typed
/// query and a stored path meet in one normalization.
#[must_use]
pub fn search_key(text: &str) -> String {
    if text.is_ascii() {
        text.to_owned()
    } else {
        text.nfc().collect()
    }
}

/// The scoring loop over code units: UTF-8 bytes or Unicode scalar values.
fn score_units<U: Copy + Into<u32>>(path: &[U], needle: &[U]) -> Option<i64> {
    if needle.is_empty() {
        return Some(0);
    }
    let name_start = path
        .iter()
        .rposition(|unit| (*unit).into() == u32::from(b'/'))
        .map_or(0, |slash| slash + 1);
    let (leftmost, first_match) = scan(path, needle, 0)?;
    let mut score = leftmost;
    if first_match >= name_start {
        score += FILE_NAME_BONUS;
    } else if let Some((in_name, _)) = scan(path, needle, name_start) {
        score = score.max(in_name + FILE_NAME_BONUS);
    }
    Some(score - to_points((path.len() - name_start) / 8))
}

/// One greedy left-to-right match of `needle` in `path` starting at `from`: its run and
/// boundary score and where it began, or `None` when `needle` is not a subsequence there.
fn scan<U: Copy + Into<u32>>(path: &[U], needle: &[U], from: usize) -> Option<(i64, usize)> {
    let mut score = 0;
    let mut needle_index = 0;
    let mut previous: Option<usize> = None;
    let mut first_match = None;
    for (index, unit) in path.iter().enumerate().skip(from) {
        if lower((*unit).into()) != lower(needle[needle_index].into()) {
            continue;
        }
        score += points(index, previous, path);
        first_match.get_or_insert(index);
        previous = Some(index);
        needle_index += 1;
        if needle_index == needle.len() {
            return first_match.map(|first| (score, first));
        }
    }
    None
}

/// What a match at `index` earns given the previous match: the run bonus when it follows
/// directly, else the gap penalty after an earlier match and the word-start bonus.
fn points<U: Copy + Into<u32>>(index: usize, previous: Option<usize>, path: &[U]) -> i64 {
    if previous.is_some_and(|previous| index == previous + 1) {
        return 1 + CONSECUTIVE_BONUS;
    }
    let mut points = 1;
    if previous.is_some() {
        points -= GAP_PENALTY;
    }
    if index == 0 || is_boundary(path[index - 1].into()) {
        points += BOUNDARY_BONUS;
    }
    points
}

fn lower(unit: u32) -> u32 {
    if (u32::from(b'A')..=u32::from(b'Z')).contains(&unit) {
        unit + 32
    } else {
        unit
    }
}

fn is_boundary(unit: u32) -> bool {
    unit == u32::from(b'/')
        || unit == u32::from(b'.')
        || unit == u32::from(b'_')
        || unit == u32::from(b'-')
}

/// A length as points; saturates, which no real path reaches.
fn to_points(length: usize) -> i64 {
    i64::try_from(length).unwrap_or(i64::MAX)
}

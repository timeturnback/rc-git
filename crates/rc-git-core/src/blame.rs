//! Line-by-line attribution of a file to the commits that introduced its lines.

use std::cell::Cell;
use std::collections::HashMap;

use gix::ObjectId;
use gix::objs::{Find, FindHeader};

use crate::error::{Error, ObjectKind, Result, git, object};
use crate::log::peel_to_commit;
use crate::oid::Oid;
use crate::repo::Repository;
use crate::tree::lookup_path;

/// What a blame produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blame {
    /// Every line attributed, as consecutive ranges ordered by line number.
    Ranges(Vec<BlameRange>),
    /// The file at the tip is larger than the caller's limit; no history was walked and
    /// nothing was decoded.
    TooLarge {
        /// Size of the file at the tip in bytes.
        size: u64,
        /// The limit that was exceeded.
        limit: u64,
    },
}

/// A run of consecutive lines introduced by one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameRange {
    /// The commit that introduced the lines.
    pub commit: Oid,
    /// 1-based first line of the run in the blamed file.
    pub start_line: u32,
    /// Number of lines in the run.
    pub line_count: u32,
    /// The walk stopped before finding where these lines came from — git's `^` marker. Either
    /// `commit` is the first commit older than the caller's `since` floor, or it is a shallow
    /// clone's boundary commit, whose parents the clone does not have; the lines are at least
    /// that old.
    pub boundary: bool,
}

impl Repository {
    /// Attributes every line of the file at `path` in the commit `tip` to the commit that
    /// introduced it, as consecutive ranges ordered by line number.
    ///
    /// This is the one read that walks history: its cost grows with the history between `tip`
    /// and the commit that introduced the oldest surviving line — up to the whole ancestry for
    /// a file with a line from the first commit — so call it on demand for the file on screen,
    /// never in bulk.
    ///
    /// `since` (seconds since the Unix epoch) is the one budget the walk takes: it never
    /// reads past a commit whose committer time is older, and the lines still unattributed
    /// there go to that commit as [`BlameRange::boundary`] ranges. `None` walks the whole
    /// ancestry. There is no cancel: gix-blame takes no interrupt flag, so once started the
    /// walk runs to the floor while holding a pool handle; a real cancel needs upstream support.
    ///
    /// In a shallow clone the walk stops at the boundary, like `git blame`: a boundary commit
    /// is judged like a root and keeps every line still unattributed when the walk reaches it,
    /// as [`BlameRange::boundary`] ranges.
    ///
    /// Renames are followed, as `git blame` does: when a commit adds `path`, its parent is
    /// searched for the file it was renamed from (default similarity, no copies), and the walk
    /// continues under that name. Only a commit that adds or deletes the path pays for that diff.
    ///
    /// An annotated tag `tip` is followed to its commit.
    ///
    /// The file at `tip` is sized from its object header first: over `limit`, the result is
    /// [`Blame::TooLarge`] and nothing is decoded. Only the tip is checked; the versions the
    /// walk reaches in history are decoded whole, however large.
    ///
    /// # Errors
    /// [`Error::WrongKind`] when `tip` is, or tags, a tree or blob,
    /// or when `path` is a directory or a submodule in it.
    /// [`Error::PathNotFound`] when `path` does not exist in `tip`.
    /// [`Error::Git`] when the shallow file, diff settings or commit-graph
    /// cannot be read. [`Error::Object`] when `tip` is missing or the
    /// walk fails; it names the object the walk could not find when there was one, `tip`
    /// otherwise, or the commit whose time could not be read to mark a boundary range.
    pub fn blame(&self, tip: Oid, path: &str, limit: u64, since: Option<i64>) -> Result<Blame> {
        self.with(|repo| {
            let commit = peel_to_commit(repo, tip)?;
            let tip = Oid::from_gix(commit.id);
            let tree = commit.tree_id().map_err(object("decode commit", tip))?;
            let blob = expect_file(repo, tip, Oid::from_gix(tree.detach()), path)?;
            let size = repo
                .find_header(*blob.as_gix())
                .map_err(object("read blob header", blob))?
                .size();
            if size > limit {
                return Ok(Blame::TooLarge { size, limit });
            }
            let odb = ShallowCut {
                inner: &repo.objects,
                boundary: repo.shallow_commits().map_err(git("read shallow file"))?,
                missing: Cell::new(None),
            };
            let cache = repo
                .commit_graph_if_enabled()
                .map_err(git("open commit-graph"))?;
            let mut resource_cache = repo
                .diff_resource_cache_for_tree_diff()
                .map_err(git("prepare blame diff"))?;
            let options = gix::blame::Options {
                diff_algorithm: repo.diff_algorithm().map_err(git("read diff algorithm"))?,
                ranges: gix::blame::BlameRanges::default(),
                since: since.map(|seconds| gix::date::Time::new(seconds, 0)),
                rewrites: Some(gix::diff::Rewrites::default()),
                debug_track_path: false,
            };
            let outcome = gix::blame::file(
                &odb,
                gix::blame::Start::Commit(*tip.as_gix()),
                cache,
                &mut resource_cache,
                path.into(),
                options,
            )
            .map_err(|source| {
                let culprit = odb.missing.get().map_or(tip, Oid::from_gix);
                object("blame file", culprit)(source)
            })?;
            to_ranges(repo, outcome.entries, since, &odb).map(Blame::Ranges)
        })
    }
}

/// The walk's entries as ranges ordered by line number. A range whose commit is a shallow
/// boundary in `odb` is a boundary, and so, with a floor, is one whose commit is older than it:
/// gix-blame only attributes lines to such a commit when it stops there, since it never diffs
/// one. Each distinct commit's time is read once.
fn to_ranges(
    repo: &gix::Repository,
    entries: Vec<gix::blame::BlameEntry>,
    since: Option<i64>,
    odb: &ShallowCut<'_>,
) -> Result<Vec<BlameRange>> {
    let mut older: HashMap<ObjectId, bool> = HashMap::new();
    let mut ranges: Vec<BlameRange> = Vec::with_capacity(entries.len());
    for entry in entries {
        let commit = Oid::from_gix(entry.commit_id);
        let boundary = odb.is_boundary(&entry.commit_id)
            || since
                .map(|floor| is_older(repo, &mut older, commit, floor))
                .transpose()?
                .unwrap_or(false);
        ranges.push(BlameRange {
            commit,
            start_line: entry.start_in_blamed_file + 1,
            line_count: entry.len.get(),
            boundary,
        });
    }
    ranges.sort_by_key(|range| range.start_line);
    Ok(ranges)
}

/// Whether `commit` was committed before `floor`, remembered in `known` so each is read once.
fn is_older(
    repo: &gix::Repository,
    known: &mut HashMap<ObjectId, bool>,
    commit: Oid,
    floor: i64,
) -> Result<bool> {
    if let Some(older) = known.get(commit.as_gix()) {
        return Ok(*older);
    }
    let time = repo
        .find_commit(*commit.as_gix())
        .map_err(object("read commit", commit))?
        .time()
        .map_err(object("decode committer", commit))?;
    let older = time.seconds < floor;
    known.insert(*commit.as_gix(), older);
    Ok(older)
}

/// The blob at `path` in the commit `tip` whose root tree is `tree`. Fails unless `path` names
/// a file, so a missing path or a directory is reported as such instead of as a failed walk.
fn expect_file(repo: &gix::Repository, tip: Oid, tree: Oid, path: &str) -> Result<Oid> {
    const OP: &str = "blame file";
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .map(str::as_bytes);
    let Some(entry) = lookup_path(repo, tree, segments)? else {
        return Err(Error::PathNotFound {
            op: OP,
            path: path.to_owned(),
            commit: tip,
        });
    };
    let actual = if entry.mode.is_tree() {
        ObjectKind::Tree
    } else if entry.mode.is_commit() {
        ObjectKind::Commit
    } else {
        return Ok(Oid::from_gix(entry.oid));
    };
    Err(Error::WrongKind {
        op: OP,
        oid: Oid::from_gix(entry.oid),
        expected: ObjectKind::Blob,
        actual,
    })
}

/// The object database as seen through the shallow file: a boundary commit reads without its
/// parent lines, so the blame walk treats it as a root instead of chasing parents the clone
/// does not have. Remembers the last object that was not found, so an error can name it.
struct ShallowCut<'r> {
    inner: &'r gix::OdbHandle,
    boundary: Option<gix::shallow::Commits>,
    missing: Cell<Option<ObjectId>>,
}

impl ShallowCut<'_> {
    fn is_boundary(&self, id: &gix::hash::oid) -> bool {
        self.boundary
            .as_ref()
            .is_some_and(|commits| commits.binary_search(&id.to_owned()).is_ok())
    }
}

impl Find for ShallowCut<'_> {
    fn try_find<'a>(
        &self,
        id: &gix::hash::oid,
        buffer: &'a mut Vec<u8>,
    ) -> std::result::Result<Option<gix::objs::Data<'a>>, gix::objs::find::Error> {
        if !self.is_boundary(id) {
            let found = self.inner.try_find(id, buffer)?;
            if found.is_none() {
                self.missing.set(Some(id.to_owned()));
            }
            return Ok(found);
        }
        let Some(data) = self.inner.try_find(id, buffer)? else {
            self.missing.set(Some(id.to_owned()));
            return Ok(None);
        };
        let (kind, object_hash) = (data.kind, data.object_hash);
        let cut = without_parents(data.data);
        *buffer = cut;
        Ok(Some(gix::objs::Data {
            kind,
            object_hash,
            data: buffer.as_slice(),
        }))
    }
}

impl FindHeader for ShallowCut<'_> {
    fn try_header(
        &self,
        id: &gix::hash::oid,
    ) -> std::result::Result<Option<gix::objs::Header>, gix::objs::find::Error> {
        self.inner.try_header(id)
    }
}

/// A raw commit with the `parent` lines of its header removed. The message is left alone.
fn without_parents(commit: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(commit.len());
    let mut in_header = true;
    for line in commit.split_inclusive(|byte| *byte == b'\n') {
        if in_header && line == b"\n" {
            in_header = false;
        }
        if !(in_header && line.starts_with(b"parent ")) {
            out.extend_from_slice(line);
        }
    }
    out
}

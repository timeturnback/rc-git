//! Line-by-line attribution of a file to the commits that introduced its lines.

use std::cell::Cell;

use gix::ObjectId;
use gix::objs::{Find, FindHeader};

use crate::error::{Error, ObjectKind, Result, git, object};
use crate::log::peel_to_commit;
use crate::oid::Oid;
use crate::repo::Repository;
use crate::tree::lookup_path;

/// A run of consecutive lines introduced by one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameRange {
    /// The commit that introduced the lines.
    pub commit: Oid,
    /// 1-based first line of the run in the blamed file.
    pub start_line: u32,
    /// Number of lines in the run.
    pub line_count: u32,
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
    /// In a shallow clone the walk stops at the boundary, like `git blame`: a boundary commit
    /// is judged like a root and keeps every line still unattributed when the walk reaches it.
    ///
    /// An annotated tag `tip` is followed to its commit.
    ///
    /// # Errors
    /// [`Error::WrongKind`](crate::Error::WrongKind) when `tip` is, or tags, a tree or blob,
    /// or when `path` is a directory or a submodule in it.
    /// [`Error::PathNotFound`](crate::Error::PathNotFound) when `path` does not exist in `tip`.
    /// [`Error::Git`](crate::Error::Git) when the shallow file, diff settings or commit-graph
    /// cannot be read. [`Error::Object`](crate::Error::Object) when `tip` is missing or the
    /// walk fails; it names the object the walk could not find when there was one, `tip`
    /// otherwise.
    pub fn blame(&self, tip: Oid, path: &str) -> Result<Vec<BlameRange>> {
        self.with(|repo| {
            let commit = peel_to_commit(repo, tip)?;
            let tip = Oid::from_gix(commit.id);
            let tree = commit.tree_id().map_err(object("decode commit", tip))?;
            expect_file(repo, tip, Oid::from_gix(tree.detach()), path)?;
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
                since: None,
                rewrites: None,
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
            let mut ranges: Vec<BlameRange> = Vec::with_capacity(outcome.entries.len());
            for entry in outcome.entries {
                ranges.push(BlameRange {
                    commit: Oid::from_gix(entry.commit_id),
                    start_line: entry.start_in_blamed_file + 1,
                    line_count: entry.len.get(),
                });
            }
            ranges.sort_by_key(|range| range.start_line);
            Ok(ranges)
        })
    }
}

/// Fails unless `path` names a file in the commit `tip` whose root tree is `tree`, so a
/// missing path or a directory is reported as such instead of as a failed blame walk.
fn expect_file(repo: &gix::Repository, tip: Oid, tree: Oid, path: &str) -> Result<()> {
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
        return Ok(());
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

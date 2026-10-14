//! Commit history.

use std::sync::Mutex;

use gix::bstr::ByteSlice;

use crate::error::{Error, ObjectKind, Result, object};
use crate::oid::Oid;
use crate::repo::{Repository, peel_tags};
use crate::walk::{self, Traversal};

/// The parts of a commit a list view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitSummary {
    /// The commit id.
    pub oid: Oid,
    /// Parent ids, first parent first.
    pub parents: Vec<Oid>,
    /// Author name, trimmed.
    pub author_name: String,
    /// Author email, trimmed.
    pub author_email: String,
    /// Author time, seconds since the Unix epoch.
    pub author_time: i64,
    /// Author timezone, seconds east of UTC.
    pub author_offset: i32,
    /// First paragraph of the message folded onto one line, as git's `%s`.
    pub title: String,
}

/// Everything a commit detail view shows beyond the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetail {
    /// The list-view parts of the commit.
    pub summary: CommitSummary,
    /// Message body after the title, trimmed. Empty when the message is only a title.
    pub body: String,
    /// Committer name, trimmed.
    pub committer_name: String,
    /// Committer email, trimmed.
    pub committer_email: String,
    /// Committer time, seconds since the Unix epoch.
    pub committer_time: i64,
    /// Committer timezone, seconds east of UTC.
    pub committer_offset: i32,
    /// The root tree of the commit.
    pub tree: Oid,
}

/// A page-at-a-time walk over the commits reachable from one tip, newest commit time first;
/// not topological — under clock skew a parent can precede one of its children. Each
/// [`next_page`](Self::next_page) resumes where the previous one stopped, so paging to depth
/// N costs N commits in total, not N per page. The cursor owns its walk state rather than a
/// pooled repository handle, and may be used from any thread; pages are taken one at a time.
/// Pass the cursor the [`Repository`] that made it.
pub struct LogCursor {
    traversal: Mutex<Traversal>,
}

impl std::fmt::Debug for LogCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogCursor").finish_non_exhaustive()
    }
}

impl LogCursor {
    /// The next at most `limit` commits; fewer only at the end of history.
    ///
    /// # Errors
    /// [`Error::Git`] when the walk cannot advance (a commit it needs is missing) or an earlier
    /// page of this cursor failed, [`Error::Object`] when a walked commit cannot be decoded.
    pub fn next_page(&self, repo: &Repository, limit: usize) -> Result<Vec<CommitSummary>> {
        let mut traversal = walk::lock(&self.traversal)?;
        traversal.ensure_usable()?;
        let result = repo.with(|repo| {
            let mut commits = Vec::with_capacity(limit.min(256));
            while commits.len() < limit {
                let Some(info) = traversal.next_commit()? else {
                    break;
                };
                let oid = Oid::from_gix(info.id);
                let commit = repo
                    .find_commit(info.id)
                    .map_err(object("read commit", oid))?;
                commits.push(summarize(&commit)?);
            }
            Ok(commits)
        });
        if result.is_err() {
            traversal.mark_broken();
        }
        result
    }

    /// Passes over the next `count` commits without decoding them, and answers how many it
    /// passed: fewer than `count` only at the end of history.
    ///
    /// # Errors
    /// [`Error::Git`] when the walk cannot advance or an earlier page of this cursor failed.
    pub fn skip(&self, count: usize) -> Result<usize> {
        let mut traversal = walk::lock(&self.traversal)?;
        traversal.ensure_usable()?;
        let mut skipped = 0;
        while skipped < count {
            match traversal.next_commit() {
                Ok(Some(_)) => skipped += 1,
                Ok(None) => break,
                Err(err) => {
                    traversal.mark_broken();
                    return Err(err);
                }
            }
        }
        Ok(skipped)
    }
}

impl Repository {
    /// A cursor over the commits reachable from `tip`, positioned before the newest. An
    /// annotated tag `tip` is followed to its commit.
    ///
    /// # Errors
    /// [`Error::WrongKind`] when `tip` is, or tags, a tree or blob, [`Error::Object`] when it
    /// is missing, [`Error::Git`] when the walk cannot start or the shallow file is unreadable.
    pub fn log_cursor(&self, tip: Oid) -> Result<LogCursor> {
        self.with(|repo| {
            Ok(LogCursor {
                traversal: Mutex::new(Traversal::start(repo, tip, None)?),
            })
        })
    }

    /// One commit by id. An annotated tag is followed to its commit.
    ///
    /// # Errors
    /// [`Error::Object`] when missing, [`Error::WrongKind`] when it is, or tags, a tree or blob.
    pub fn commit(&self, oid: Oid) -> Result<CommitSummary> {
        self.with(|repo| {
            let commit = peel_to_commit(repo, oid)?;
            summarize(&commit)
        })
    }

    /// One commit by id with its full message, committer, and root tree. An annotated tag is
    /// followed to its commit.
    ///
    /// # Errors
    /// [`Error::Object`] when missing or undecodable, [`Error::WrongKind`] when it is, or
    /// tags, a tree or blob.
    pub fn commit_details(&self, oid: Oid) -> Result<CommitDetail> {
        self.with(|repo| {
            let commit = peel_to_commit(repo, oid)?;
            let oid = Oid::from_gix(commit.id);
            let summary = summarize(&commit)?;
            let decoded = commit.decode().map_err(object("decode commit", oid))?;
            let committer = decoded
                .committer()
                .map_err(object("decode committer", oid))?;
            let (committer_time, committer_offset) = match committer.time() {
                Ok(time) => (time.seconds, time.offset),
                Err(_) => (committer.seconds(), 0),
            };
            let body = match decoded.message().body {
                Some(body) => body.to_str_lossy().trim().to_owned(),
                None => String::new(),
            };
            Ok(CommitDetail {
                summary,
                body,
                committer_name: committer.name.to_str_lossy().into_owned(),
                committer_email: committer.email.to_str_lossy().into_owned(),
                committer_time,
                committer_offset,
                tree: Oid::from_gix(decoded.tree()),
            })
        })
    }
}

/// Follows annotated tags from `oid` to a commit, reading headers so a tree or blob is
/// rejected without inflating it.
pub(crate) fn peel_to_commit(repo: &gix::Repository, oid: Oid) -> Result<gix::Commit<'_>> {
    let (id, kind) = peel_tags(repo, oid)?;
    if kind != gix::objs::Kind::Commit {
        return Err(Error::WrongKind {
            op: "read commit",
            oid: id,
            expected: ObjectKind::Commit,
            actual: ObjectKind::from_gix(kind),
        });
    }
    repo.find_commit(*id.as_gix())
        .map_err(object("read commit", id))
}

pub(crate) fn summarize(commit: &gix::Commit<'_>) -> Result<CommitSummary> {
    let oid = Oid::from_gix(commit.id);
    let decoded = commit.decode().map_err(object("decode commit", oid))?;
    let author = decoded.author().map_err(object("decode author", oid))?;
    let (author_time, author_offset) = match author.time() {
        Ok(time) => (time.seconds, time.offset),
        Err(_) => (author.seconds(), 0),
    };
    let mut parents = Vec::new();
    for parent in decoded.parents() {
        parents.push(Oid::from_gix(parent));
    }
    Ok(CommitSummary {
        oid,
        parents,
        author_name: author.name.to_str_lossy().into_owned(),
        author_email: author.email.to_str_lossy().into_owned(),
        author_time,
        author_offset,
        title: decoded.message().summary().to_str_lossy().into_owned(),
    })
}

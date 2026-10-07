//! Commit history.

use gix::bstr::ByteSlice;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

use crate::error::{Result, git, object};
use crate::oid::Oid;
use crate::repo::Repository;

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

impl Repository {
    /// Commits reachable from `tip`, newest commit time first, skipping the first `skip` and
    /// returning at most `limit`. The walk restarts from `tip` on every call, so a page deep in
    /// a long history costs the pages before it; keep `skip` small or cache pages.
    ///
    /// # Errors
    /// [`Error::Git`](crate::Error::Git) when the walk cannot start,
    /// [`Error::Object`](crate::Error::Object) when a commit on the way is unreadable.
    pub fn log(&self, tip: Oid, skip: usize, limit: usize) -> Result<Vec<CommitSummary>> {
        self.with(|repo| {
            let walk = repo
                .rev_walk([*tip.as_gix()])
                .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
                .all()
                .map_err(git("start commit walk"))?;
            let mut commits = Vec::with_capacity(limit.min(256));
            for info in walk.skip(skip).take(limit) {
                let info = info.map_err(git("walk commits"))?;
                let oid = Oid::from_gix(info.id);
                let commit = info.object().map_err(object("read commit", oid))?;
                commits.push(summarize(&commit)?);
            }
            Ok(commits)
        })
    }

    /// One commit by id.
    ///
    /// # Errors
    /// [`Error::Object`](crate::Error::Object) when missing or not a commit.
    pub fn commit(&self, oid: Oid) -> Result<CommitSummary> {
        self.with(|repo| {
            let commit = repo
                .find_commit(*oid.as_gix())
                .map_err(object("read commit", oid))?;
            summarize(&commit)
        })
    }

    /// One commit by id with its full message, committer, and root tree.
    ///
    /// # Errors
    /// [`Error::Object`](crate::Error::Object) when missing, not a commit, or undecodable.
    pub fn commit_details(&self, oid: Oid) -> Result<CommitDetail> {
        self.with(|repo| {
            let commit = repo
                .find_commit(*oid.as_gix())
                .map_err(object("read commit", oid))?;
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

//! Pack generation for push: enumerate the commits the remote lacks, expand them to
//! trees and blobs, then stream a self-contained (never thin) pack v2 into the transport.
//! Counting holds object ids only; the compressed entries go straight to the writer.

use std::io::Write;

use gix::ObjectId;
use gix_pack::data::output::count::PackLocation;

use crate::actions::{ActionError, ActionResult, git_err};

use super::{Cancel, bail_if_cancelled};

const OP: &str = "push";

/// Everything reachable from `tip` that is not reachable from `haves`, counted but not yet
/// packed, plus how many commits that is. No objects at all (still a valid, empty pack)
/// when the remote already has every object, e.g. when a new branch points at an existing
/// commit.
pub(super) struct Counted {
    counts: Vec<gix_pack::data::output::Count>,
    /// Commits included.
    pub commits: usize,
    /// A handle that keeps packs mapped: counts store pack ids for later lookup.
    db: gix::odb::Handle,
    hash: gix::hash::Kind,
}

/// Enumerates the objects to send, before any request is opened.
///
/// # Errors
/// [`ActionError::Git`] when the walk or counting fails or an object to send is missing
/// locally (named by its id), [`ActionError::Cancelled`] when `cancel` was set during the
/// commit walk or counting.
pub(super) fn count_pack(
    repo: &gix::Repository,
    tip: ObjectId,
    haves: &[ObjectId],
    cancel: &Cancel,
) -> ActionResult<Counted> {
    let walk = repo
        .rev_walk([tip])
        .with_hidden(haves.iter().copied())
        .all()
        .map_err(|source| ActionError::git(OP, tip.to_string(), source))?;
    let mut commits: Vec<ObjectId> = Vec::new();
    for info in walk {
        bail_if_cancelled(cancel, OP)?;
        let info = info.map_err(|source| ActionError::git(OP, tip.to_string(), source))?;
        commits.push(info.id);
    }
    // `repo.objects` is a memory-Proxy wrapper without a pack-level Find impl; the
    // cache underneath implements it. Pack generation stores pack ids for later
    // lookup, so the handle must keep packs mapped.
    let mut db = (*repo.objects).clone();
    db.prevent_pack_unload();
    let commit_count = commits.len();
    let hash = repo.object_hash();
    if commit_count == 0 {
        return Ok(Counted {
            counts: Vec::new(),
            commits: 0,
            db,
            hash,
        });
    }
    let input = Box::new(commits.into_iter().map(Ok))
        as Box<
            dyn Iterator<
                    Item = Result<ObjectId, Box<dyn std::error::Error + Send + Sync + 'static>>,
                > + Send,
        >;
    let (counts, _outcome) = gix_pack::data::output::count::objects(
        db.clone(),
        input,
        &gix::progress::Discard,
        cancel.flag(),
        gix_pack::data::output::count::objects::Options {
            thread_limit: Some(1),
            input_object_expansion:
                gix_pack::data::output::count::objects::ObjectExpansion::TreeAdditionsComparedToAncestor,
            ..Default::default()
        },
    )
    .map_err(|source| {
        // gix-pack ends the count with `Error::Interrupted` once the flag is set.
        if cancel.is_cancelled() {
            return ActionError::Cancelled { op: OP };
        }
        ActionError::git(OP, "object enumeration", source)
    })?;
    refuse_missing_objects(repo, &counts)?;
    Ok(Counted {
        counts,
        commits: commit_count,
        db,
        hash,
    })
}

/// Writes the counted objects to `out` as a complete pack, header and trailing hash
/// included, checking `cancel` between entries. A failure part-way leaves `out` holding a
/// truncated pack, which the server's index-pack rejects.
///
/// # Errors
/// [`ActionError::Git`] when an entry cannot be produced or `out` fails,
/// [`ActionError::Cancelled`] when `cancel` was set while writing.
pub(super) fn write_pack(
    counted: Counted,
    out: &mut dyn Write,
    cancel: &Cancel,
) -> ActionResult<()> {
    let Counted {
        counts, db, hash, ..
    } = counted;
    if counts.is_empty() {
        return write_empty_pack(hash, out);
    }
    let entry_count = u32::try_from(counts.len())
        .map_err(|_| ActionError::git(OP, "pack", "more than u32::MAX objects in one push"))?;
    let entries = gix_pack::data::output::entry::iter_from_counts(
        counts,
        db,
        Box::new(gix::progress::Discard),
        gix_pack::data::output::entry::iter_from_counts::Options {
            allow_thin_pack: false,
            ..Default::default()
        },
    );
    let writer = gix_pack::data::output::bytes::FromEntriesIter::new(
        gix::features::parallel::InOrderIter::from(entries),
        out,
        entry_count,
        gix_pack::data::Version::V2,
        hash,
    );
    for written in writer {
        bail_if_cancelled(cancel, OP)?;
        written.map_err(git_err(OP, "pack"))?;
    }
    Ok(())
}

/// Fails on the first counted object the local database cannot produce. Counting records
/// blob ids from tree diffs without reading them, and the pack writer later drops an object
/// it cannot find after the header already counted it, so the server would get a pack with
/// fewer objects than it claims (a partial clone that never fetched those blobs). Only objects
/// not found in a pack are checked: those are loose or missing.
fn refuse_missing_objects(
    repo: &gix::Repository,
    counts: &[gix_pack::data::output::Count],
) -> ActionResult<()> {
    let missing = counts.iter().find(|count| {
        matches!(count.entry_pack_location, PackLocation::LookedUp(None))
            && !repo.has_object(count.id)
    });
    match missing {
        Some(count) => Err(ActionError::git(
            OP,
            count.id.to_string(),
            "this object is missing from the local object database (partial or shallow \
             clone); fetch it with git, then push again",
        )),
        None => Ok(()),
    }
}

/// A valid pack containing zero objects: header plus trailing hash.
fn write_empty_pack(hash: gix::hash::Kind, out: &mut dyn Write) -> ActionResult<()> {
    let empty = std::iter::empty::<Result<Vec<gix_pack::data::output::Entry>, EmptyError>>();
    let writer = gix_pack::data::output::bytes::FromEntriesIter::new(
        empty,
        out,
        0,
        gix_pack::data::Version::V2,
        hash,
    );
    for written in writer {
        written.map_err(git_err(OP, "pack"))?;
    }
    Ok(())
}

/// The impossible error of an empty entry stream.
#[derive(Debug, thiserror::Error)]
#[error("unreachable")]
struct EmptyError;

#[cfg(test)]
mod tests {
    use super::*;

    fn standard() -> (rc_git_testkit::Fixture, gix::Repository, ObjectId) {
        let fixture = rc_git_testkit::Fixture::standard();
        let repo = gix::open(fixture.path()).expect("open fixture");
        let tip = ObjectId::from_hex(fixture.rev_parse("HEAD").as_bytes()).expect("tip oid");
        (fixture, repo, tip)
    }

    #[test]
    fn count_pack_returns_cancelled_when_the_flag_is_set() {
        let (_fixture, repo, tip) = standard();
        let cancel = Cancel::new();
        cancel.signal();
        let result = count_pack(&repo, tip, &[], &cancel);
        assert!(
            matches!(result, Err(ActionError::Cancelled { op: "push" })),
            "expected Cancelled, got {:?}",
            result.err()
        );
    }

    #[test]
    fn write_pack_streams_every_counted_object_into_the_writer() {
        let (_fixture, repo, tip) = standard();
        let cancel = Cancel::new();
        let counted = count_pack(&repo, tip, &[], &cancel).expect("count");
        let objects = u32::try_from(counted.counts.len()).expect("object count");
        assert!(counted.commits > 0);
        let mut out = Vec::new();
        write_pack(counted, &mut out, &cancel).expect("write");
        assert_eq!(out.get(..4), Some(&b"PACK"[..]));
        assert_eq!(out.get(8..12), Some(&objects.to_be_bytes()[..]));
    }

    #[test]
    fn write_pack_writes_an_empty_pack_when_the_remote_has_everything() {
        let (_fixture, repo, tip) = standard();
        let cancel = Cancel::new();
        let counted = count_pack(&repo, tip, &[tip], &cancel).expect("count");
        assert_eq!(counted.commits, 0);
        let mut out = Vec::new();
        write_pack(counted, &mut out, &cancel).expect("write");
        assert_eq!(out.len(), 32, "12-byte header plus a SHA-1 trailer");
        assert_eq!(out.get(8..12), Some(&0_u32.to_be_bytes()[..]));
    }

    #[test]
    fn write_pack_returns_cancelled_when_the_flag_is_set_after_counting() {
        let (_fixture, repo, tip) = standard();
        let cancel = Cancel::new();
        let counted = count_pack(&repo, tip, &[], &cancel).expect("count");
        cancel.signal();
        let result = write_pack(counted, &mut std::io::sink(), &cancel);
        assert!(
            matches!(result, Err(ActionError::Cancelled { op: "push" })),
            "expected Cancelled, got {result:?}"
        );
    }

    #[test]
    fn write_pack_reports_a_failing_writer() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("connection reset"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (_fixture, repo, tip) = standard();
        let cancel = Cancel::new();
        let counted = count_pack(&repo, tip, &[], &cancel).expect("count");
        let result = write_pack(counted, &mut Broken, &cancel);
        assert!(
            matches!(result, Err(ActionError::Git { .. })),
            "expected Git, got {result:?}"
        );
    }
}

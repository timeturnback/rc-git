//! Pack generation for push: enumerate the commits the remote lacks, expand them to
//! trees and blobs, and write a self-contained (never thin) pack v2 into memory.

use gix::ObjectId;
use gix_pack::data::output::count::PackLocation;

use crate::actions::{ActionError, ActionResult, git_err};

use super::{Cancel, bail_if_cancelled};

const OP: &str = "push";

/// A pack holding everything reachable from `tip` that is not reachable from `haves`,
/// plus how many commits that is. The pack is empty (still valid) when the remote
/// already has every object, e.g. when a new branch points at an existing commit.
pub(super) struct BuiltPack {
    /// The complete pack bytes, header and trailing hash included.
    pub bytes: Vec<u8>,
    /// Commits included.
    pub commits: usize,
}

/// Enumerates and packs the objects to send.
///
/// # Errors
/// [`ActionError::Git`] when the walk or the pack writer fails or an object to send is
/// missing locally (named by its id), [`ActionError::Cancelled`]
/// when `cancel` was set during the commit walk, counting, or writing.
pub(super) fn build_pack(
    repo: &gix::Repository,
    tip: ObjectId,
    haves: &[ObjectId],
    cancel: &Cancel,
) -> ActionResult<BuiltPack> {
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
    let commit_count = commits.len();
    if commit_count == 0 {
        return Ok(BuiltPack {
            bytes: empty_pack(repo.object_hash())?,
            commits: 0,
        });
    }

    // `repo.objects` is a memory-Proxy wrapper without a pack-level Find impl; the
    // cache underneath implements it. Pack generation stores pack ids for later
    // lookup, so the handle must keep packs mapped.
    let mut db = (*repo.objects).clone();
    db.prevent_pack_unload();
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
    let mut bytes = Vec::new();
    let writer = gix_pack::data::output::bytes::FromEntriesIter::new(
        gix::features::parallel::InOrderIter::from(entries),
        &mut bytes,
        entry_count,
        gix_pack::data::Version::V2,
        repo.object_hash(),
    );
    for written in writer {
        bail_if_cancelled(cancel, OP)?;
        written.map_err(git_err(OP, "pack"))?;
    }
    Ok(BuiltPack {
        bytes,
        commits: commit_count,
    })
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
fn empty_pack(hash: gix::hash::Kind) -> ActionResult<Vec<u8>> {
    let empty = std::iter::empty::<Result<Vec<gix_pack::data::output::Entry>, EmptyError>>();
    let mut bytes = Vec::new();
    let writer = gix_pack::data::output::bytes::FromEntriesIter::new(
        empty,
        &mut bytes,
        0,
        gix_pack::data::Version::V2,
        hash,
    );
    for written in writer {
        written.map_err(git_err(OP, "pack"))?;
    }
    Ok(bytes)
}

/// The impossible error of an empty entry stream.
#[derive(Debug, thiserror::Error)]
#[error("unreachable")]
struct EmptyError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_pack_returns_cancelled_when_the_flag_is_set() {
        let fixture = rc_git_testkit::Fixture::standard();
        let repo = gix::open(fixture.path()).expect("open fixture");
        let tip = ObjectId::from_hex(fixture.rev_parse("HEAD").as_bytes()).expect("tip oid");
        let cancel = Cancel::new();
        cancel.signal();
        let result = build_pack(&repo, tip, &[], &cancel);
        assert!(
            matches!(result, Err(ActionError::Cancelled { op: "push" })),
            "expected Cancelled, got {:?}",
            result.err()
        );
    }
}

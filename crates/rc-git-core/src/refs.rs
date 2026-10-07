//! Branches, tags, remote branches, and `HEAD`.

use gix::bstr::{BStr, ByteSlice};
use gix::refs::file::ReferenceExt;
use gix::refs::{Category, FullNameRef};

use crate::error::{BoxError, Error, Result, cause_chain, git};
use crate::oid::Oid;
use crate::repo::Repository;

/// What a reference is, by namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    /// `refs/heads/*`
    LocalBranch,
    /// `refs/remotes/*`
    RemoteBranch,
    /// `refs/tags/*`
    Tag,
    /// Anything else, such as `refs/stash` or `refs/notes/*`.
    Other,
}

/// A reference and the object it finally points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    /// Full name, e.g. `refs/heads/main`.
    pub full_name: String,
    /// Name without the namespace, e.g. `main`, `origin/main`, `v1.0`.
    pub short_name: String,
    /// Namespace classification.
    pub kind: RefKind,
    /// The object after following symbolic refs and peeling annotated tags.
    pub target: Oid,
}

/// A reference whose target could not be read. Reported, not hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenRef {
    /// Full name of the reference.
    pub full_name: String,
    /// Why it could not be resolved.
    pub error: String,
}

/// Every reference in the repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefListing {
    /// References that resolved to an object.
    pub refs: Vec<RefEntry>,
    /// References that did not. An empty list is the normal case.
    pub broken: Vec<BrokenRef>,
}

/// Where `HEAD` points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The branch `HEAD` is on, e.g. `refs/heads/main`. `None` when detached.
    pub ref_name: Option<String>,
    /// The commit `HEAD` resolves to. `None` on an unborn branch.
    pub target: Option<Oid>,
}

impl Repository {
    /// Lists every reference, following symbolic refs and peeling annotated tags to the
    /// object they annotate.
    ///
    /// # Errors
    /// [`Error::Git`] when the reference store cannot be read or its directories cannot be
    /// walked. A single unreadable or unparsable reference lands in [`RefListing::broken`]
    /// instead of failing the whole listing.
    pub fn refs(&self) -> Result<RefListing> {
        self.with(|repo| {
            let platform = repo.references().map_err(git("open reference store"))?;
            let mut listing = RefListing::default();
            let mut collected = Vec::new();
            for reference in platform.all().map_err(git("list references"))? {
                match reference {
                    Ok(reference) => collected.push(reference.detach()),
                    Err(source) => listing.broken.push(unparsable_ref(
                        repo,
                        "walk the reference directories under .git/refs",
                        source,
                    )?),
                }
            }
            drop(platform);

            let packed = repo
                .refs
                .cached_packed_buffer()
                .map_err(git("read packed-refs"))?;
            let packed = packed.as_ref().map(|snapshot| &***snapshot);
            for mut reference in collected {
                let full_name = lossy(reference.name.as_bstr());
                let (kind, short_name) = classify(reference.name.as_ref());
                match reference.peel_to_id_packed(&repo.refs, &repo.objects, packed) {
                    Ok(id) => listing.refs.push(RefEntry {
                        full_name,
                        short_name,
                        kind,
                        target: Oid::from_gix(id),
                    }),
                    Err(error) => {
                        listing.broken.push(BrokenRef {
                            full_name,
                            error: cause_chain(&error),
                        });
                    }
                }
            }
            Ok(listing)
        })
    }

    /// Reads `HEAD`.
    ///
    /// # Errors
    /// [`Error::Git`] when `HEAD` is missing or malformed.
    pub fn head(&self) -> Result<Head> {
        self.with(|repo| {
            let head = repo.head().map_err(git("read HEAD"))?;
            let ref_name = head.referent_name().map(|name| lossy(name.as_bstr()));
            let target = head.id().map(|id| Oid::from_gix(id.detach()));
            Ok(Head { ref_name, target })
        })
    }

    /// The repository's mainline branch, without `refs/heads/`: the branch a remote's `HEAD`
    /// names when the clone recorded one, else whichever of `master` and `main` exists
    /// locally. `None` when nothing names one. The branch itself may not exist locally — the
    /// remote's `HEAD` is what the clone was told, so the caller reports a missing branch by
    /// name — but its remote-tracking branch must: a remote `HEAD` that names itself or a
    /// tracking branch that is gone is skipped.
    ///
    /// # Errors
    /// [`Error::Git`] when the reference store cannot be read or its directories cannot be
    /// walked. A remote reference that does not parse is skipped: it names no branch.
    pub fn default_branch(&self) -> Result<Option<String>> {
        self.with(|repo| {
            let platform = repo.references().map_err(git("open reference store"))?;
            let mut from_remote = None;
            for reference in platform
                .prefixed("refs/remotes/")
                .map_err(git("list remote branches"))?
            {
                let reference = match reference {
                    Ok(reference) => reference,
                    Err(source) => {
                        unparsable_ref(
                            repo,
                            "walk the remote branches under .git/refs/remotes",
                            source,
                        )?;
                        continue;
                    }
                };
                let Some(branch) = remote_head_branch(repo, &reference) else {
                    continue;
                };
                // `origin` wins when several remotes record a HEAD; any other remote answers
                // only when no origin does.
                let is_origin = reference
                    .name()
                    .as_bstr()
                    .starts_with(b"refs/remotes/origin/");
                if is_origin {
                    from_remote = Some(branch);
                    break;
                }
                from_remote = from_remote.or(Some(branch));
            }
            drop(platform);
            if let Some(branch) = from_remote {
                return Ok(Some(branch));
            }
            for candidate in ["master", "main"] {
                if repo
                    .find_reference(&format!("refs/heads/{candidate}"))
                    .is_ok()
                {
                    return Ok(Some(candidate.to_owned()));
                }
            }
            Ok(None)
        })
    }
}

/// The reference an iteration error is about, reported under its name relative to the git dir,
/// or `packed-refs:<line>` for a bad line in `packed-refs`. Both iterators have already moved
/// past the bad entry, so the caller can continue.
///
/// # Errors
/// [`Error::Git`] with `op` when the error is about the store rather than one reference, such
/// as a directory under `.git/refs` that cannot be walked.
fn unparsable_ref(repo: &gix::Repository, op: &'static str, source: BoxError) -> Result<BrokenRef> {
    use gix::refs::file::iter::loose_then_packed::Error as IterError;
    let full_name = match source.downcast_ref::<IterError>() {
        Some(IterError::ReferenceCreation { relative_path, .. }) => {
            relative_path.to_string_lossy().into_owned()
        }
        Some(IterError::ReadFileContents { path, .. }) => [repo.git_dir(), repo.common_dir()]
            .iter()
            .find_map(|base| path.strip_prefix(base).ok())
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned(),
        Some(IterError::PackedReference { line_number, .. }) => {
            format!("packed-refs:{line_number}")
        }
        Some(IterError::Traversal(_)) | None => return Err(Error::Git { op, source }),
    };
    Ok(BrokenRef {
        full_name,
        error: cause_chain(&*source),
    })
}

/// The branch `refs/remotes/<remote>/HEAD` points at, without its namespace. `None` for any
/// other reference, for a remote `HEAD` that is not symbolic, that names itself, or whose
/// tracking branch is gone (as after a remote renamed its default branch and a prune deleted
/// the old one): none of those names a branch the remote still has.
fn remote_head_branch(repo: &gix::Repository, reference: &gix::Reference<'_>) -> Option<String> {
    let full_name = reference.name().as_bstr();
    let remote_prefix = full_name.strip_suffix(b"/HEAD")?;
    let gix::refs::TargetRef::Symbolic(target) = reference.target() else {
        return None;
    };
    let branch = target
        .as_bstr()
        .strip_prefix(remote_prefix)?
        .strip_prefix(b"/")?;
    if branch.is_empty() || branch == b"HEAD" {
        return None;
    }
    // A target that cannot be read names no branch, the same as an unparsable remote ref.
    repo.find_reference(target).ok()?;
    Some(lossy(branch.as_bstr()))
}

/// The kind of a reference and its display name. Branches, remote branches and tags drop their
/// namespace; every other reference drops only a leading `refs/`, whether or not gix knows its
/// category, so `refs/stash` and `refs/notes/commits` read as `stash` and `notes/commits`.
fn classify(name: &FullNameRef) -> (RefKind, String) {
    let full = name.as_bstr();
    let other = || {
        let short = full.strip_prefix(b"refs/").unwrap_or(full);
        (RefKind::Other, lossy(short.as_bstr()))
    };
    let Some((category, short)) = name.category_and_short_name() else {
        return other();
    };
    match category {
        Category::LocalBranch => (RefKind::LocalBranch, lossy(short)),
        Category::RemoteBranch => (RefKind::RemoteBranch, lossy(short)),
        Category::Tag => (RefKind::Tag, lossy(short)),
        Category::Note
        | Category::PseudoRef
        | Category::MainPseudoRef
        | Category::MainRef
        | Category::LinkedPseudoRef { .. }
        | Category::LinkedRef { .. }
        | Category::Bisect
        | Category::Rewritten
        | Category::WorktreePrivate => other(),
    }
}

fn lossy(name: &BStr) -> String {
    name.to_str_lossy().into_owned()
}

//! Branches, tags, remote branches, and `HEAD`.

use gix::bstr::{BStr, ByteSlice};
use gix::refs::file::ReferenceExt;
use gix::refs::{Category, FullNameRef};

use crate::error::{Error, Result, git};
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
    /// [`Error::Git`] when the reference store cannot be read. A single unreadable reference
    /// lands in [`RefListing::broken`] instead of failing the whole listing.
    pub fn refs(&self) -> Result<RefListing> {
        self.with(|repo| {
            let platform = repo.references().map_err(git("open reference store"))?;
            let mut collected = Vec::new();
            for reference in platform.all().map_err(git("list references"))? {
                let reference = reference.map_err(|source| Error::Git {
                    op: "iterate references",
                    source,
                })?;
                collected.push(reference.detach());
            }
            drop(platform);

            let packed = repo
                .refs
                .cached_packed_buffer()
                .map_err(git("read packed-refs"))?;
            let packed = packed.as_ref().map(|snapshot| &***snapshot);
            let mut listing = RefListing::default();
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
                            error: error.to_string(),
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
    /// name.
    ///
    /// # Errors
    /// [`Error::Git`] when the reference store cannot be read.
    pub fn default_branch(&self) -> Result<Option<String>> {
        self.with(|repo| {
            let platform = repo.references().map_err(git("open reference store"))?;
            let mut from_remote = None;
            for reference in platform
                .prefixed("refs/remotes/")
                .map_err(git("list remote branches"))?
            {
                let reference = reference.map_err(|source| Error::Git {
                    op: "iterate remote branches",
                    source,
                })?;
                let Some(branch) = remote_head_branch(&reference) else {
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

/// The branch `refs/remotes/<remote>/HEAD` points at, without its namespace. `None` for any
/// other reference, and for a remote `HEAD` that is not symbolic.
fn remote_head_branch(reference: &gix::Reference<'_>) -> Option<String> {
    let full_name = reference.name().as_bstr();
    let remote_prefix = full_name.strip_suffix(b"/HEAD")?;
    let gix::refs::TargetRef::Symbolic(target) = reference.target() else {
        return None;
    };
    let target = target.as_bstr();
    let branch = target.strip_prefix(remote_prefix)?.strip_prefix(b"/")?;
    (!branch.is_empty()).then(|| lossy(branch.as_bstr()))
}

fn classify(name: &FullNameRef) -> (RefKind, String) {
    let Some((category, short)) = name.category_and_short_name() else {
        return (RefKind::Other, lossy(name.as_bstr()));
    };
    let kind = match category {
        Category::LocalBranch => RefKind::LocalBranch,
        Category::RemoteBranch => RefKind::RemoteBranch,
        Category::Tag => RefKind::Tag,
        Category::Note
        | Category::PseudoRef
        | Category::MainPseudoRef
        | Category::MainRef
        | Category::LinkedPseudoRef { .. }
        | Category::LinkedRef { .. }
        | Category::Bisect
        | Category::Rewritten
        | Category::WorktreePrivate => RefKind::Other,
    };
    (kind, lossy(short))
}

fn lossy(name: &BStr) -> String {
    name.to_str_lossy().into_owned()
}

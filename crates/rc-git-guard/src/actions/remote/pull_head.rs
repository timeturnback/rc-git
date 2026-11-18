//! Fetch one pull request's head (`refs/pull/<n>/head`) into rc-git's own ref for it,
//! `refs/rc-git/pr/<n>/head`, so its diff can be read from local objects. The refspec is built
//! from the number alone, never from text a server sent; the fetch asks for that one ref and no
//! tags, and writes nothing unless the remote advertises the head commit the caller saw. The
//! ref is force-updated, since a pull request's head is rewritten routinely. Nothing ever
//! checks it out: the private namespace is listed as "other" beside the backup refs, never as
//! a branch or a tag.

use std::path::Path;

use rc_git_core::Oid;

use super::fetch::{Received, Source, Wanted, transfer, updated_refs};
use super::github::repository::pull_request_upstream;
use super::{Cancel, RemoteAuth, Upstream, bail_if_cancelled, url_upstream_refused};
use crate::actions::{ActionError, ActionResult, authorize, git_err, open};
use crate::{Action, Policy};

const OP: &str = "fetch pull request";

/// Where rc-git keeps the pull request heads it fetched.
pub const PULL_HEAD_NAMESPACE: &str = "refs/rc-git/pr/";

/// rc-git's own ref for pull request `number`'s head: `refs/rc-git/pr/<number>/head`.
#[must_use]
pub fn pull_head_ref(number: u64) -> String {
    format!("{PULL_HEAD_NAMESPACE}{number}/head")
}

/// The one refspec a pull request head fetch asks for: the remote's `refs/pull/<number>/head`,
/// force-written to [`pull_head_ref`].
pub(super) fn refspec(number: u64) -> String {
    format!("+{}:{}", remote_ref(number), pull_head_ref(number))
}

/// The ref a GitHub repository keeps pull request `number`'s head under.
fn remote_ref(number: u64) -> String {
    format!("refs/pull/{number}/head")
}

/// Which pull request head to fetch, and the commit the caller saw as its head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullHeadRequest {
    /// The pull request's number, 1 or more.
    pub number: u64,
    /// The head commit the API reported for the pull request on screen.
    pub expected: Oid,
}

/// What a pull request head fetch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullHeadFetch {
    /// The head commit is in the object database and [`pull_head_ref`] points at it.
    Fetched {
        /// The remote fetched from.
        remote: String,
        /// The ref written, `refs/rc-git/pr/<number>/head`.
        ref_name: String,
        /// Whether the fetch created or moved the ref; false when it already pointed there.
        updated: bool,
    },
    /// The remote's head is another commit: the pull request moved after it was read. Nothing
    /// was received or written.
    Moved {
        /// The remote fetched from.
        remote: String,
        /// The head the remote advertised.
        found: Oid,
    },
}

/// Fetches pull request `request.number`'s head from the remote pull requests are read from
/// (`pull_request_remote`: the checked-out branch's remote, else `origin`) into
/// `refs/rc-git/pr/<number>/head`, when the remote's `refs/pull/<number>/head` is
/// `request.expected`; a different head answers [`PullHeadFetch::Moved`] before any object is
/// received. Credentials and cancel work as for [`super::fetch_upstream`]: `auth.creds` answer
/// only the remote's own host. No other ref is written, no tag fetched, and nothing is checked
/// out.
///
/// # Errors
/// [`ActionError::Git`] when the number is 0, when the repository has no remote with a URL or
/// names a bare URL, or when the remote advertises no `refs/pull/<number>/head`; otherwise as
/// [`super::fetch_upstream`], naming the operation `fetch pull request`.
pub fn fetch_pull_request_head(
    repo_path: &Path,
    policy: &Policy,
    request: PullHeadRequest,
    auth: RemoteAuth<'_>,
    cancel: &Cancel,
) -> ActionResult<PullHeadFetch> {
    let PullHeadRequest { number, expected } = request;
    if number == 0 {
        return Err(ActionError::git(OP, "0", "0 is not a pull request number"));
    }
    let action = Action::FetchPullRequest { number };
    let proof = authorize(policy, &action, OP, &remote_ref(number), None)?;
    bail_if_cancelled(cancel, OP)?;
    let mut repo = open(repo_path, OP, "repository")?;
    super::set_reflog_identity(&mut repo, OP)?;
    let (remote_name, url) = match pull_request_upstream(&repo, OP)? {
        Some(Upstream::Named { name, url }) => (name, url),
        Some(Upstream::Url { url }) => return Err(url_upstream_refused(OP, &url)),
        Some(Upstream::Local { .. }) | None => {
            return Err(ActionError::git(
                OP,
                "remote",
                "the repository has no remote to fetch the pull request from",
            ));
        }
    };
    let expected_id = gix_id(expected)?;
    let source = Source {
        remote_name: remote_name.clone(),
        url,
        wanted: Wanted::PullHead {
            number,
            expected: expected_id,
        },
        op: OP,
    };
    let outcome = match transfer(repo, proof, source, auth, cancel)? {
        Received::Moved { found } => {
            return Ok(PullHeadFetch::Moved {
                remote: remote_name,
                found: core_id(found)?,
            });
        }
        Received::Fetched(outcome) => outcome,
    };
    let ref_name = pull_head_ref(number);
    confirm_written(repo_path, &ref_name, expected_id)?;
    let updated = updated_refs(&outcome)
        .iter()
        .any(|update| update.name == ref_name);
    Ok(PullHeadFetch::Fetched {
        remote: remote_name,
        ref_name,
        updated,
    })
}

/// The object the remote advertises for pull request `number`'s head, read from the ref list
/// the negotiation began with.
///
/// # Errors
/// [`ActionError::Git`] when the remote advertises no `refs/pull/<number>/head`.
pub(super) fn advertised(
    ref_map: &gix::protocol::fetch::RefMap,
    number: u64,
    source: &Source,
) -> ActionResult<gix::ObjectId> {
    let wanted = remote_ref(number);
    for mapping in &ref_map.mappings {
        let named = mapping
            .remote
            .as_name()
            .is_some_and(|name| name == wanted.as_str());
        if let (true, Some(id)) = (named, mapping.remote.as_id()) {
            return Ok(id.to_owned());
        }
    }
    Err(ActionError::git(
        source.op,
        &source.remote_name,
        format!(
            "the remote advertises no {wanted}: it is not the repository the pull request was \
             opened on, or that pull request does not exist there"
        ),
    ))
}

/// Reads `ref_name` back after the fetch and checks it holds `expected`, so a success always
/// means the head commit the caller saw is the one written.
fn confirm_written(repo_path: &Path, ref_name: &str, expected: gix::ObjectId) -> ActionResult<()> {
    let repo = open(repo_path, OP, "repository")?;
    let reference = repo
        .find_reference(ref_name)
        .map_err(git_err(OP, ref_name))?;
    let id = reference.id().detach();
    if id != expected {
        return Err(ActionError::git(
            OP,
            ref_name,
            format!("the ref holds {id} after the fetch, not the expected {expected}"),
        ));
    }
    Ok(())
}

/// `oid` in gix's terms.
fn gix_id(oid: Oid) -> ActionResult<gix::ObjectId> {
    gix::ObjectId::try_from(oid.as_bytes()).map_err(git_err(OP, &oid.to_hex()))
}

/// `id` in the core's terms.
fn core_id(id: gix::ObjectId) -> ActionResult<Oid> {
    Oid::from_bytes(id.as_bytes())
        .ok_or_else(|| ActionError::git(OP, id.to_string(), "an object id of an unknown hash"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refspec_names_one_pull_request_by_its_number() {
        assert_eq!(refspec(7), "+refs/pull/7/head:refs/rc-git/pr/7/head");
        assert_eq!(
            refspec(u64::MAX),
            "+refs/pull/18446744073709551615/head:refs/rc-git/pr/18446744073709551615/head"
        );
        for number in [1, 42, u64::MAX] {
            let spec = refspec(number);
            let operation = gix::refspec::parse::Operation::Fetch;
            let parsed = gix::refspec::parse(spec.as_str().into(), operation).unwrap();
            assert!(parsed.to_owned().allow_non_fast_forward(), "{spec}");
            assert_eq!(parsed.source(), Some(remote_ref(number).as_str().into()));
            let destination = pull_head_ref(number);
            assert_eq!(parsed.destination(), Some(destination.as_str().into()));
            assert!(!spec.contains('*'), "{spec}");
        }
    }
}

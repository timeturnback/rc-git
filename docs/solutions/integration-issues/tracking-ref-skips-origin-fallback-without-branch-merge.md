---
title: "Push shows no count on a branch with no upstream config: gix's tracking-ref helper skips the origin fallback"
date: 2026-09-29
category: integration-issues
module: rc-git-guard
problem_type: integration_issue
component: infrastructure
symptoms:
  - "The Push button never shows an unpushed-commit count on a branch made with `git init` and pushed without `-u`, while cloned and `push -u` branches count fine"
  - "`upstream_status` reports has_upstream true with an empty upstream_ref and ahead/behind 0/0"
  - "Before the fix, Pull and Sync failed with NoUpstream on that branch even though Fetch and Push to origin succeeded"
  - "Nothing errors: gix's `branch_remote_tracking_ref_name` answers None, not an Err, when `branch.<name>.merge` is missing"
root_cause: logic_error
resolution_type: code_fix
severity: high
framework_version: gix 0.87.1
related_components:
  - rc-git-kit
  - rc-git-app
tags: [gix, upstream, remote-tracking-ref, ahead-behind, branch-merge-config, origin-fallback, push, pull]
---

# Push shows no count on a branch with no upstream config: gix's tracking-ref helper skips the origin fallback

## Problem

A branch with no `[branch "<name>"]` section in `.git/config` got its remote from one helper and its remote-tracking ref from another, and only the first had a fallback. That happens after `git init` plus a push without `-u`. `configured_remote_name` fell back to `origin`, so fetch and push worked and the app reported an upstream. `tracking_ref_name` asked gix, got nothing, and returned `None`. So the Changes tab showed a bare "Push" with no count, and Pull and Sync failed with `NoUpstream`.

## Symptoms

- The Changes tab's Push button read "Push", with no commit count, while `git rev-list --count origin/master..master` printed 46 at the time. `push_title` shows the bare verb whenever `ahead` is 0 (`crates/rc-git-kit/src/sync/values.rs`).
- It depended on the branch. Counts showed on branches and repositories whose config names an upstream (a clone, or a branch pushed with `-u`) and never on the affected branch, so it looked like a timing problem.
- The affected repository's `.git/config` had `[remote "origin"]` and no `[branch "master"]` section. This repository's own checkout had that shape when this was written.
- Nothing reported an error. `upstream_status` returned `has_upstream: true` with `upstream_ref: ""` and `(ahead, behind) = (0, 0)` (`crates/rc-git-guard/src/actions/remote.rs`). The sync preflight only blocks when `hasUpstream` is false (`SyncModel.adoptFreshUpstream`), so the app treated the branch as tracked.
- Pull failed with `ActionError::NoUpstream` (`upstream_tip` in `crates/rc-git-guard/src/actions/remote/pull.rs`). Sync runs fetch, then pull, then push, and stops at the first failure (`SyncModel.sync()`), so it failed at the pull step and never pushed. Push on its own succeeded.

## What Didn't Work

- **Suspecting refresh timing.** Counts appeared on some branches and not on others, so the first guess was a stale status: `SyncModel.refreshStatus` or the ref watcher not re-reading after a push. Reading the repository's `.git/config` and gix's branch helpers ruled this out. The status was not stale. On this branch it came out wrong on every read, because the tracking-ref lookup could never succeed without a `branch.master.merge` key.
- **Trusting the existing tests.** `push_creates_a_new_branch_from_an_existing_commit` (`crates/rc-git-guard/tests/remote.rs`) already pushes a branch made with `checkout -b` from a local branch, which by git's default gets no upstream config, and it passes. It never calls `upstream_status` afterwards, and that call is where the two helpers disagreed. Before the fix, the status tests that had a remote only used clones, which always have `branch.main.merge`.
- **Setting the upstream by hand.** Wrap-ups in earlier sessions noted this `master` as ahead of `origin/master` with "no upstream tracking set" and suggested `git push -u origin master` (session history). That writes `branch.master.merge` and makes the count appear, but it only hides the bug for that one branch. It also fixes nothing for Pull and Sync on any other branch pushed without `-u`.

## Solution

`tracking_ref_name` now handles gix's `None` itself. It picks the remote with the same `configured_remote_name` that fetch and push use, and returns where that remote's fetch refspecs file `refs/heads/<branch>` (`fetched_into`), e.g. `refs/remotes/origin/master`. Push asks `push_refs` for its tracking ref, which maps the branch through the same `fetched_into`, and `update_tracking_ref` (`crates/rc-git-guard/src/actions/remote/push.rs`) moves that ref after the push.

Before (the first version of the helper):

```rust
fn tracking_ref_name(repo: &gix::Repository, branch: &str) -> Option<String> {
    let full = format!("refs/heads/{branch}");
    let full_name: gix::refs::FullName = full.as_str().try_into().ok()?;
    let tracking = repo
        .branch_remote_tracking_ref_name(full_name.as_ref(), gix::remote::Direction::Fetch)?
        .ok()?;
    Some(tracking.as_bstr().to_string())
}
```

After, as `tracking_ref_name` in `crates/rc-git-guard/src/actions/remote.rs` reads today (doc
comment and lint opt-out trimmed):

```rust
fn tracking_ref_name(
    repo: &gix::Repository,
    branch: &str,
    op: &'static str,
) -> ActionResult<Option<TrackingRef>> {
    let Some(remote) = configured_remote_name(repo, branch, Direction::Fetch) else {
        return Ok(None);
    };
    if remote == LOCAL_REMOTE {
        return Ok(merge_ref_name(repo, branch).map(|name| TrackingRef {
            name: Some(name),
            configured: true,
        }));
    }
    let full = format!("refs/heads/{branch}");
    let full_name: gix::refs::FullName = full.as_str().try_into().map_err(git_err(op, &full))?;
    let configured = repo.branch_remote_tracking_ref_name(full_name.as_ref(), Direction::Fetch);
    let Some(tracking) = configured else {
        if !is_named_remote(repo, &remote) {
            return Ok(None);
        }
        return Ok(Some(TrackingRef {
            name: fetched_into(repo, &remote, &full, op)?,
            configured: false,
        }));
    };
    let name = tracking.map_err(git_err(op, &full))?;
    Ok(Some(TrackingRef {
        name: Some(name.as_bstr().to_string()),
        configured: true,
    }))
}
```

The function has grown since the fix: it returns an error rather than `None` when gix reports a
broken config (`Some(Err(_))`), answers the local branch for a local upstream (`remote = .`),
`None` for a URL upstream, and records in `configured` whether `branch.<name>.merge` named the
ref. The fallback runs on every `None` from gix: a missing merge or remote key, or a remote
whose fetch refspecs are empty or do not map the branch; in the last two cases `fetched_into`
answers `None` and there is no tracking ref to count against.

A branch that was never pushed has no ref at the fallback name yet. That is not an error:
`upstream_status` reports `published: false`, and `pull_fast_forward` returns an
already-up-to-date outcome with `unpublished: true` (`upstream_tip` in `pull.rs`), since
`configured` is false. `NoUpstream` is left for a branch where no remote resolves at all.

Tests in `crates/rc-git-guard/tests/remote.rs`, both failing before the fix:

- The `untrack` helper runs `git branch --unset-upstream main` on a clone. That removes `branch.main.remote` and `branch.main.merge` and leaves `origin` in place, which is the same state as the affected repository.
- `upstream_status_counts_against_origin_when_the_branch_names_no_upstream` untracks the clone, commits locally, advances origin, and fetches. It then asserts `has_upstream`, `remote == "origin"`, `upstream_ref == "refs/remotes/origin/main"`, and `(ahead, behind) == (1, 1)`. Before the fix `upstream_ref` came back `""`.
- `pull_fast_forwards_from_origin_when_the_branch_names_no_upstream` untracks, advances origin, fetches, and pulls. Before the fix this returned `NoUpstream`.
- `upstream_status_reports_a_never_pushed_branch_as_unpublished` and `pull_on_a_never_pushed_branch_is_a_no_op`, added later, pin the never-pushed case: an upstream, nothing published, and a pull that changes nothing.

After the fix, a one-off probe against the affected repository printed `ahead: 46`, matching `git rev-list` at that moment.

## Why This Works

gix 0.87.1 resolves a tracking ref only from branch config. The citations below are into the gix crate's own source (`src/repository/config/branch.rs` under the cargo registry), not this repository:

- `Repository::branch_remote_tracking_ref_name` starts with `self.branch_remote_ref_name(name, direction)?` (gix `branch.rs:124`). For `Direction::Fetch` that reads `branch.<short>.merge` and returns `None` when the key is missing (`branch.rs:50-63`).
- It then needs `self.branch_remote(..)?` (`branch.rs:128`), which goes through `branch_remote_name`. That reads `branch.<name>.remote` and nothing else (`branch.rs:213-229`).
- A remote with no fetch refspecs also gives `None` (`branch.rs:133-134`).

None of these checks for a remote called `origin`. rc-git adds that fallback in `configured_remote_name` (through `default_remote_name`, in `remote.rs`). Before the fix, only fetch, push, and the remote half of `upstream_status` used it (all three through `resolve_upstream` today), and the tracking-ref half went straight to gix. As a result:

1. `upstream_status` found `origin` and a URL and set `has_upstream: true`.
2. `tracking_ref_name` got `None` from gix at the first `?`, so ahead/behind fell through to `(0, 0)` and `upstream_ref` to `""`.
3. Push had already written `refs/remotes/origin/master`, so the ref was there. Nothing looked it up.

The fallback's answer is the ref push writes: push moves the ref `push_refs` returns, which `fetched_into` derives from the same remote's fetch refspecs. On a branch with no upstream config, status, pull, and push now name the same ref by construction. The `let ... else` runs on every `None` from gix, including a remote with no fetch refspecs, and in each of those cases it also picks the ref push would move, or none when push moves none.

Push uploads only `refs/heads/<branch>`. A branch whose `branch.<name>.merge` names a differently named upstream (the default after `git checkout -b topic origin/main`) used to count and pull against `origin/main` while pushing to `origin/topic`; `push_refs` in `remote.rs` now refuses that push before connecting, as git's `push.default=simple` does, when the push goes to the remote the branch pulls from.

The fallback is rc-git's own convention, not a reading of the user's git defaults. The isolated open never reads `~/.gitconfig`, so settings such as `push.default` or `push.autoSetupRemote` cannot supply the answer either (see [fast-forward-only-pull-hands-divergence-to-git.md](../architecture-patterns/fast-forward-only-pull-hands-divergence-to-git.md) for the same constraint on pull).

## Prevention

- **Keep one fallback for "which remote, which ref".** `configured_remote_name` and `tracking_ref_name` in `crates/rc-git-guard/src/actions/remote.rs` are the upstream resolvers, and the second builds on the first. A new entry point that needs a remote or tracking ref should call them (or `resolve_upstream`, which wraps the first) rather than gix's `branch_remote*` helpers. `clippy.toml` bans those four gix methods; the only opt-outs are `configured_remote_name`, `tracking_ref_name` and `merge_ref_name`, each with an inline `#[expect]` reason.
- **`branch_remote_ref_name`, `branch_remote`, and `branch_remote_tracking_ref_name` return `Option<Result<..>>`, and `None` means gix could not derive an answer (not configured, or configured but unresolvable), not an error.** Writing `?.ok()?` merges "not configured" and "configured but broken" into a single `None` and hides the gap. Handle `None` explicitly with `let ... else`, then decide what `Err` means separately.
- **Run every remote entry point on a branch with no tracking config.** Use the `untrack` helper in `crates/rc-git-guard/tests/remote.rs` and assert on `upstream_ref`, not only on counts. `(0, 0)` is also the right answer for a branch that is in sync, so a count-only test on an in-sync fixture passes with the bug. A fixture that sets an upstream with `push -u` or `branch -u` hides the bug the same way the manual workaround did. A new remote action gets the same untracked-branch test.

## Related Issues

- [hand-rolled-git-push-over-gix-https.md](../architecture-patterns/hand-rolled-git-push-over-gix-https.md) — the push that moves the remote-tracking ref the fallback now reads.
- [fast-forward-only-pull-hands-divergence-to-git.md](../architecture-patterns/fast-forward-only-pull-hands-divergence-to-git.md) — the other way a pull is refused (both sides moved), and why user-level git config is out of reach.

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
  - Pull and Sync fail with NoUpstream on that branch even though Fetch and Push to origin succeed
  - "Nothing errors: gix's `branch_remote_tracking_ref_name` answers None, not an Err, when `branch.<name>.merge` is missing"
root_cause: logic_error
resolution_type: code_fix
severity: high
framework_version: gix 0.87.1
related_components:
  - rc-git-ffi
  - RcGitKit
  - RcGitApp
tags: [gix, upstream, remote-tracking-ref, ahead-behind, branch-merge-config, origin-fallback, push, pull]
---

# Push shows no count on a branch with no upstream config: gix's tracking-ref helper skips the origin fallback

## Problem

A branch with no `[branch "<name>"]` section in `.git/config` got its remote from one helper and its remote-tracking ref from another, and only the first had a fallback. That happens after `git init` plus a push without `-u`. `configured_remote_name` fell back to `origin`, so fetch and push worked and the app reported an upstream. `tracking_ref_name` asked gix, got nothing, and returned `None`. So the Changes tab showed a bare "Push" with no count, and Pull and Sync failed with `NoUpstream`.

## Symptoms

- The Changes tab's Push button read "Push", with no commit count, while `git rev-list --count origin/master..master` printed 46 at the time. `SyncTitles.push` shows the bare verb whenever `ahead` is 0 (`mac/Sources/RcGitKit/SyncModel.swift:68-78`).
- It depended on the branch. Counts showed on branches and repositories whose config names an upstream (a clone, or a branch pushed with `-u`) and never on the affected branch, so it looked like a timing problem.
- The affected repository's `.git/config` had `[remote "origin"]` and no `[branch "master"]` section. This repository's own checkout had that shape when this was written.
- Nothing reported an error. `upstream_status` returned `has_upstream: true` with `upstream_ref: ""` and `(ahead, behind) = (0, 0)` (`crates/rc-git-guard/src/actions/remote.rs:191-207`). The sync preflight only blocks when `hasUpstream` is false (`SyncModel.swift:328`), so the app treated the branch as tracked.
- Pull failed with `ActionError::NoUpstream` (`crates/rc-git-guard/src/actions/remote/pull.rs:52-56`). Sync runs fetch, then pull, then push, and stops at the first failure (`SyncModel.swift:272-283`), so it failed at the pull step and never pushed. Push on its own succeeded.

## What Didn't Work

- **Suspecting refresh timing.** Counts appeared on some branches and not on others, so the first guess was a stale status: `SyncModel.refreshStatus` or the ref watcher not re-reading after a push. Reading the repository's `.git/config` and gix's branch helpers ruled this out. The status was not stale. On this branch it came out wrong on every read, because the tracking-ref lookup could never succeed without a `branch.master.merge` key.
- **Trusting the existing tests.** `push_creates_a_new_branch_from_an_existing_commit` (`crates/rc-git-guard/tests/remote.rs:564`) already pushes a branch made with `checkout -b` from a local branch, which by git's default gets no upstream config, and it passes. It never calls `upstream_status` afterwards, and that call is where the two helpers disagreed. Before the fix, the status tests that had a remote only used clones, which always have `branch.main.merge`.
- **Setting the upstream by hand.** Wrap-ups in earlier sessions noted this `master` as ahead of `origin/master` with "no upstream tracking set" and suggested `git push -u origin master` (session history). That writes `branch.master.merge` and makes the count appear, but it only hides the bug for that one branch. It also fixes nothing for Pull and Sync on any other branch pushed without `-u`.

## Solution

`tracking_ref_name` now handles gix's `None` itself. It picks the remote with the same `configured_remote_name` that fetch and push use, and returns `refs/remotes/<remote>/<branch>`. `update_tracking_ref` moves that same ref after a push (`crates/rc-git-guard/src/actions/remote/push.rs:274-303`, called at `push.rs:120` with the remote from `configured_remote_name` at `push.rs:68`).

Before:

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

After (`crates/rc-git-guard/src/actions/remote.rs:264-278`):

```rust
/// Full name of the branch's remote-tracking ref, e.g. `refs/remotes/origin/main`.
/// Without `branch.<name>.merge` — a branch made with `git init` and pushed without
/// `-u` — it is `refs/remotes/<remote>/<branch>` on the remote `configured_remote_name`
/// picks: the ref push uploads to and moves, so ahead/behind and pull agree with push.
fn tracking_ref_name(repo: &gix::Repository, branch: &str) -> Option<String> {
    let full = format!("refs/heads/{branch}");
    let full_name: gix::refs::FullName = full.as_str().try_into().ok()?;
    let Some(tracking) =
        repo.branch_remote_tracking_ref_name(full_name.as_ref(), gix::remote::Direction::Fetch)
    else {
        let remote = configured_remote_name(repo, branch)?;
        return Some(format!("refs/remotes/{remote}/{branch}"));
    };
    Some(tracking.ok()?.as_bstr().to_string())
}
```

Only `Some(Err(_))` (a merge value that is not a valid ref name, or a remote that fails to load) still gives `None`. The fallback runs on every `None` from gix: a missing merge or remote key, a remote name that matches no remote, or a remote whose fetch refspecs are empty or do not map the branch.

Tests in `crates/rc-git-guard/tests/remote.rs`, both failing before the fix:

- The `untrack` helper runs `git branch --unset-upstream main` on a clone. That removes `branch.main.remote` and `branch.main.merge` and leaves `origin` in place, which is the same state as the affected repository.
- `upstream_status_counts_against_origin_when_the_branch_names_no_upstream` untracks the clone, commits locally, advances origin, and fetches. It then asserts `has_upstream`, `remote == "origin"`, `upstream_ref == "refs/remotes/origin/main"`, and `(ahead, behind) == (1, 1)`. Before the fix `upstream_ref` came back `""`.
- `pull_fast_forwards_from_origin_when_the_branch_names_no_upstream` untracks, advances origin, fetches, and pulls. Before the fix this returned `NoUpstream`.

After the fix, a one-off probe against the affected repository printed `ahead: 46`, matching `git rev-list` at that moment.

## Why This Works

gix 0.87.1 resolves a tracking ref only from branch config. The citations below are into the gix crate's own source (`src/repository/config/branch.rs` under the cargo registry), not this repository:

- `Repository::branch_remote_tracking_ref_name` starts with `self.branch_remote_ref_name(name, direction)?` (gix `branch.rs:124`). For `Direction::Fetch` that reads `branch.<short>.merge` and returns `None` when the key is missing (`branch.rs:50-63`).
- It then needs `self.branch_remote(..)?` (`branch.rs:128`), which goes through `branch_remote_name`. That reads `branch.<name>.remote` and nothing else (`branch.rs:213-229`).
- A remote with no fetch refspecs also gives `None` (`branch.rs:133-134`).

None of these checks for a remote called `origin`. rc-git adds that fallback in `configured_remote_name` (`remote.rs:229-242`). Before the fix, only fetch, push, and the remote half of `upstream_status` used it (`fetch.rs:56`, `push.rs:68`, `remote.rs:185`), and the tracking-ref half went straight to gix. As a result:

1. `upstream_status` found `origin` and a URL and set `has_upstream: true`.
2. `tracking_ref_name` got `None` from gix at the first `?`, so ahead/behind fell through to `(0, 0)` and `upstream_ref` to `""`.
3. Push had already written `refs/remotes/origin/master`, so the ref was there. Nothing looked it up.

The fallback's answer is the ref push writes: `update_tracking_ref` always uses `refs/remotes/{remote}/{branch}` with the remote from `configured_remote_name`. On a branch with no upstream config, status, pull, and push now name the same ref by construction. The `let ... else` runs on every `None` from gix, including a remote with no fetch refspecs, and in each of those cases it also picks the ref push would move.

This holds only on the fallback path. Push always uploads `refs/heads/<branch>` (`push.rs:80`) and ignores `branch.<name>.merge`, so a branch whose config names a differently named upstream (the default after `git checkout -b topic origin/main`) still counts and pulls against `origin/main` while pushing to `origin/topic`. That mismatch predates this fix.

The fallback is rc-git's own convention, not a reading of the user's git defaults. The isolated open never reads `~/.gitconfig`, so settings such as `push.default` or `push.autoSetupRemote` cannot supply the answer either (see [fast-forward-only-pull-hands-divergence-to-git.md](../architecture-patterns/fast-forward-only-pull-hands-divergence-to-git.md) for the same constraint on pull).

## Prevention

- **Keep one fallback for "which remote, which ref".** `configured_remote_name` and `tracking_ref_name` in `crates/rc-git-guard/src/actions/remote.rs` are the only upstream helpers, and the second now builds on the first. A new entry point that needs a remote or tracking ref should call them rather than gix's `branch_remote*` helpers. `rg -n "branch_remote" crates` should list only those two helpers; anything else skips the `origin` fallback.
- **`branch_remote_ref_name`, `branch_remote`, and `branch_remote_tracking_ref_name` return `Option<Result<..>>`, and `None` means gix could not derive an answer (not configured, or configured but unresolvable), not an error.** Writing `?.ok()?` merges "not configured" and "configured but broken" into a single `None` and hides the gap. Handle `None` explicitly with `let ... else`, then decide what `Err` means separately.
- **Run every remote entry point on a branch with no tracking config.** Use the `untrack` helper in `crates/rc-git-guard/tests/remote.rs` and assert on `upstream_ref`, not only on counts. `(0, 0)` is also the right answer for a branch that is in sync, so a count-only test on an in-sync fixture passes with the bug. A fixture that sets an upstream with `push -u` or `branch -u` hides the bug the same way the manual workaround did. A new remote action gets the same untracked-branch test.
- **Read `has_upstream` as "a remote was resolved".** Its doc comment says "configured upstream" (`remote.rs:161`), but it is `true` whenever `configured_remote_name` resolves to a remote with a fetch URL, which includes the `origin` fallback. Code that trusts it (the sync preflight at `SyncModel.swift:328`) needs every downstream helper to use the same fallback.

## Related Issues

- [hand-rolled-git-push-over-gix-https.md](../architecture-patterns/hand-rolled-git-push-over-gix-https.md) — the push that writes `refs/remotes/<remote>/<branch>`, which the fallback now reads.
- [fast-forward-only-pull-hands-divergence-to-git.md](../architecture-patterns/fast-forward-only-pull-hands-divergence-to-git.md) — the other way a pull is refused (both sides moved), and why user-level git config is out of reach.

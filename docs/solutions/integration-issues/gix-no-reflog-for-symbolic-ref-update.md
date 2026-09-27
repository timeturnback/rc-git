---
title: "A gix ref transaction writes no reflog for a symbolic update: HEAD switches git cannot follow"
date: 2026-09-27
category: integration-issues
module: rc-git-guard
problem_type: integration_issue
component: infrastructure
severity: high
framework_version: "gix-ref 0.67.1"
symptoms:
  - "`git checkout -` after an rc-git branch switch lands on the wrong branch, and `HEAD@{1}` resolves to the wrong commit"
  - "`.git/logs/HEAD` gains no entry for the switch even though the edit carried `RefLog::AndReference` and returned Ok"
  - "The ref edit itself is correct: HEAD is symbolic to the new branch and git agrees, so nothing errors and nothing is logged"
  - "Only a test asserting the first line of `logs/HEAD` catches it; every other assertion about the switch passes"
  - "`cargo clippy` stays green on a new `OpenOptions::new().append(true).open(..)` write even though `clippy.toml` bans every other write API"
root_cause: wrong_api
resolution_type: code_fix
related_components:
  - rc-git-testkit
  - gix-ref
  - clippy
tags:
  - gix
  - gix-ref
  - reflog
  - symbolic-ref
  - ref-transaction
  - head
  - clippy-disallowed-methods
  - write-ban
---

# A gix ref transaction writes no reflog for a symbolic update: HEAD switches git cannot follow

## Problem

rc-git's first branch switch moved `HEAD` with a normal gix ref transaction —
`Target::Symbolic("refs/heads/<branch>")`, `RefLog::AndReference` — and the transaction
returned `Ok` while writing nothing to `.git/logs/HEAD`. Every switch therefore left
`git checkout -` and `HEAD@{n}` naming the wrong commit, with no error anywhere to say so.

## Symptoms

- `.git/HEAD` is correct (`ref: refs/heads/feature`), the working tree and index are correct,
  `git status --porcelain` is empty, and `edit_references_as` returned `Ok` — nothing reports
  a failure.
- `git reflog show --format=%gs -1 HEAD` still shows the *previous* entry, a `commit:` line.
  The switch is absent from the log entirely, not malformed in it.
- `git checkout -` afterwards returns to whatever branch the older entry named, and every
  `HEAD@{n}` is shifted by one entry for the rest of the repository's life.
- Only a test that asserts the first line of `logs/HEAD` catches it. Asserting the ref, the
  outcome struct, the working tree, or the index all pass on the broken version.
- Separately: `clippy.toml` bans every other write API in the workspace, yet
  `std::fs::OpenOptions::new().append(true).open(..)` — added for the reflog append — drew no
  warning at all. The lint was green on a new write door.

## What Didn't Work

**The ref edit alone, which looks right.** `point_head_at`
(`crates/rc-git-guard/src/actions/checkout.rs:113`) builds the edit the way every other
rc-git ref write does, through the same `apply_edit` funnel: an expectation that `HEAD` still
holds what the switch was planned against (`checkout.rs:119-123`), then `RefLog::AndReference`,
`force_create_reflog: false`, and a message (`checkout.rs:127-129`). It returns `Ok`, writes
`.git/HEAD`, and logs nothing. There is no error, no warning, and no field on `LogChange` left unset.

**Calling gix's own reflog writer.** `file::Store::reflog_create_or_append` is `pub(crate)`
(`gix-ref-0.67.1/src/store/file/loose/reflog.rs:100`), so the function the transaction itself
would have called is unreachable from a downstream crate.

**Looking for a public gix API that logs a symref update.** There is none in gix `=0.87.1`.
The behavior is deliberate and unconditional except for one shape (see *Why This Works*),
and no option on the edit turns it on — `force_create_reflog` governs whether a *missing*
log file is created, not whether a symbolic update is logged.

## Solution

Append the line by hand, using gix's own serializer, and only when the log already exists
(`log_head_switch`, `crates/rc-git-guard/src/actions/checkout.rs:159`):

```rust
let path = repo.git_dir().join("logs").join("HEAD");
if !path.exists() {
    return Ok(());
}
let line = gix::refs::log::Line {
    previous_oid: old.unwrap_or_else(|| ObjectId::null(repo.object_hash())),
    new_oid: new,
    signature: gix::actor::Signature {
        name: "rc-git".into(),
        email: "rc-git@localhost".into(),
        time: gix::date::Time::now_local_or_utc(),
    },
    message: format!("checkout: moving from {from} to {to}").into(),
};
let mut serialized = Vec::new();
line.write_to(&mut serialized)
    .map_err(|source| reflog_error(Box::new(source)))?;
#[expect(
    clippy::disallowed_methods,
    reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
)]
{
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|source| reflog_error(Box::new(source)))?;
    file.write_all(&serialized)
        .map_err(|source| reflog_error(Box::new(source)))?;
}
```

`from` is the previous branch's short name, or the detached commit id, or the literal
`an unborn HEAD` (`checkout.rs:76-79`). The append runs after the ref edit
(`checkout.rs:80-81`); if it fails, the error names `logs/HEAD`, so a half-done switch is
legible rather than silent.

The write door it needed is now on the ban list (`clippy.toml:23`):

```toml
{ path = "std::fs::OpenOptions::open", reason = "core never writes files, and these options may open one for writing; reads use std::fs::read or File::open. See docs/safety.md" },
```

Two sites opt out with a reason: the reflog append above (`checkout.rs:183-186`) and the test
kit's `known_hosts` appender (`crates/rc-git-testkit/src/ssh.rs:179-192`), which had been
using `OpenOptions` unnoticed until the ban named it.

## Why This Works

`gix-ref` 0.67.1 decides what to log, in its own source at
`gix-ref-0.67.1/src/store/file/transaction/commit.rs:51-62`:

```rust
Target::Symbolic(_) => {
    // Special HACK: no reflog for symref changes as there is no OID involved which the reflog needs.
    // Unless, the ref is new and we can obtain a peeled id
    // identified by the expectation of what could be there, as is the case when cloning.
    match expected {
        PreviousValue::ExistingMustMatch(Target::Object(oid)) => {
            Some((Some(gix_hash::ObjectId::null(oid.kind())), oid))
        }
        _ => None,
    }
}
```

A reflog line needs two object ids; a symbolic target supplies neither, so the only symbolic
update gix logs is the clone-shaped one, where the *expectation* carries an oid. rc-git's
expectations are `MustExistAndMatch(Target::Symbolic(..))` for a previous branch,
`MustExistAndMatch(Target::Object(..))` for a previous detached commit, and `MustExist`
otherwise (`checkout.rs:119-123`) — a different `PreviousValue` variant from the one that
logs, so no path through this edit reaches the writer. The switch does have both ids
(`HEAD`'s old commit and the target's tip), which is exactly why writing the line outside the
transaction is possible at all.

**`Line::write_to` rather than a hand-rolled line.** It is public
(`gix-ref-0.67.1/src/store/file/log/line.rs:35`) and documented as "the git serialization
format for ref log lines", so the byte format — field order, oid encoding, signature and
timestamp spelling, message escaping — stays gix's problem, not ours. The message text is
git's own shape, `checkout: moving from <old> to <new>`, because `git checkout -` and
`git reflog` parse it; a friendlier message breaks both.

**The synthetic `rc-git` / `rc-git@localhost` committer** is the one every other rc-git ref
edit uses (`apply_edit` → `apply_edit_as`, `crates/rc-git-guard/src/actions.rs:78-116`).
Repositories open with `gix::open::Options::isolated()`, so no global config and no user
identity is readable — there is nothing else to put in the signature.

**Appending only when `logs/HEAD` exists** matches `force_create_reflog: false` on the edit
itself. Creating a log in a repository that has none would be a change the edit did not ask
for and git did not make.

A missing previous commit becomes `ObjectId::null` of the repository's hash kind, which is
what git writes for an unborn starting point.

## Prevention

- **Assert the reflog line, not the ref.** `crates/rc-git-guard/tests/actions.rs:744` pins the
  exact text:

  ```rust
  assert_eq!(
      fx.git_output(&["reflog", "show", "--format=%gs", "-1", "HEAD"]),
      "checkout: moving from main to feature",
      "HEAD's reflog carries the switch, so `git checkout -` works afterwards"
  );
  ```

  It sits alongside the working-tree and index assertions in the same test
  (`crates/rc-git-guard/tests/actions.rs:717-746`), because those pass on the broken version. Any future ref write
  that git tools read back deserves the same treatment: verify through git, never through the
  ref file.
- **Route every "move the working tree to another commit" through
  `crates/rc-git-guard/src/actions/treeapply.rs`, and move the ref last.** `plan`
  (`treeapply.rs:31`) diffs the trees through `rc_git_core::Repository::diff_trees`, splitting
  renames into a delete plus an add (`treeapply.rs:81-94`) and refusing submodules by name
  (`treeapply.rs:197-201`); `verify_clean` (`treeapply.rs:111`) refuses the whole operation
  with `ActionError::DirtyPath { path }` for the first touched path that differs from `HEAD` on
  disk or in the index (`treeapply.rs:128-150`); `apply` (`treeapply.rs:160`) writes the files
  and updates the index for exactly those paths. Only then does the caller move its ref — the
  branch for a fast-forward pull (`crates/rc-git-guard/src/actions/remote/pull.rs:79-83`),
  `HEAD` for the switch
  (`checkout.rs:72-81`). Ordering is the safety property: an interruption after the files land
  leaves visibly modified files on the branch the user started from, never something that looks
  like lost work.
- **The clippy write ban is the layer that has to list every door.**
  `crates/rc-git-core/tests/read_only.rs` can only prove `.git` is byte-identical for the
  paths the lint actually covers; an unlisted write method means the lint passes while the
  guarantee is unenforced. Add the primitive to `clippy.toml` in the same change that first
  uses it, and opt out at the call site with a reason so the exception is greppable — the test
  kit sat in the `OpenOptions` gap for some time before the ban named it.
- **`.claude/rules/guard.md` carries both rules now**: the `treeapply` sequence with the ref
  moved last, and the fact that a symbolic ref update writes no reflog so `checkout.rs`
  appends the line itself. On a gix upgrade, re-check the symbolic branch in
  `gix-ref`'s `store/file/transaction/commit.rs`: if gix starts logging symref changes, `log_head_switch`
  becomes a duplicate entry rather than a harmless no-op, and must be deleted.

## Related Issues

- [Push over HTTPS with gix that has no push: hand-rolled send-pack](../architecture-patterns/hand-rolled-git-push-over-gix-https.md)
  — the other face of the isolated-open identity problem. There, a missing in-memory
  `user.name`/`user.email` fails a ref write outright with `MissingCommitter`; here, the same
  absence is why the appended reflog line carries a synthetic `rc-git` committer instead of the
  user's.

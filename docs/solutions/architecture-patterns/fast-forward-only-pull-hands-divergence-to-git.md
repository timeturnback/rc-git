---
title: "Pull stays fast-forward only: a diverged branch goes to `git pull --rebase`"
date: 2026-09-29
category: architecture-patterns
module: rc-git-guard
problem_type: architecture_pattern
component: infrastructure
severity: medium
related_components:
  - rc-git-kit
  - rc-git-app
applies_when:
  - "A git operation works in a terminal, VS Code, or Zed but rc-git refuses it"
  - "Considering an in-app rebase or merge for a diverged pull, or any guard action that rewrites commits"
  - "Considering spawning the git binary to cover an operation gix lacks"
  - "An action depends on user-level git config such as pull.rebase, an includeIf identity, or commit signing"
  - "Building a command string for the user to paste or run"
tags:
  - gix
  - pull
  - rebase
  - fast-forward
  - diverged-branch
  - isolated-open
  - commit-signing
  - global-config
---

# Pull stays fast-forward only: a diverged branch goes to `git pull --rebase`

## Context

The Sync sheet reported `cannot fast-forward 'master': local is 2 ahead and 4 behind; reconcile
with git` — `ActionError::Diverged` (`crates/rc-git-guard/src/actions.rs`). On the same
checkout, `git pull` in a terminal worked. Neither tool was broken; they read different config.

- Terminal git read the user's `~/.gitconfig`, which sets `pull.rebase = true`, so it replayed
  the 2 local commits on top of the 4 upstream ones. VS Code and Zed run the git binary, so they
  inherit the same config: pull strategy, committer identity, and signing.
- rc-git never spawns git for a repository operation (`clippy.toml`'s `disallowed-methods`
  bans `std::process::Command::new` in the Rust crates; only the self-updater shells out, to rebuild
  rc-git itself), and every repository goes through an isolated open: `gix::open::Options::isolated()` in
  `open_options` (`crates/rc-git-core/src/repo.rs`) and `open`
  (`crates/rc-git-guard/src/actions.rs`). Only `.git/config` is read; `include`/`includeIf`
  are ignored (`docs/safety.md`, "Untrusted repositories").
  `pull.rebase` never reaches the app.
- The guard's pull only fast-forwards. `pull_fast_forward`
  (`crates/rc-git-guard/src/actions/remote/pull.rs`) returns `Diverged` with ahead/behind
  counts when the local tip is not an ancestor of the upstream. Otherwise it follows the
  dirty-overlap order: plan the paths (`treeapply::plan`), refuse on any overlap
  (`treeapply::verify_clean`), write (`treeapply::apply`), move the ref last (`advance_ref`).

The decision was whether rc-git should do what the terminal did.

## Guidance

Keep the pull fast-forward only, and hand a divergence to git with the exact command.

Three options were weighed:

1. **Hand the user the command.** Chosen.
2. **Spawn `git pull` from the app, the way the editors do.** Rejected. It breaks the
   no-process rule. Git, not the guard, would write the working tree, so the dirty-overlap
   refusal and the ref-last ordering would no longer hold. An app launched from the Dock also
   has no login-shell `PATH` — the updater routes its own shell work through a login `zsh` for
   that reason (`crates/rc-git-mac/src/update.rs`).
3. **Rebase in-process.** Deferred as large. It needs four things the isolated open withholds or
   gix lacks:
   - **Strategy.** `pull.rebase` and `pull.ff` live in user config, so the app cannot tell a
     user who wants rebase from one who wants merge.
   - **Identity.** Each replayed commit needs a committer. The guard takes an explicit author,
     else the repository's own `user.name`/`user.email`
     (`resolve_identity` in `crates/rc-git-guard/src/actions/workfile.rs`). When neither is
     set, the app asks (`ask_identity` in `crates/rc-git-app/src/window/guarded.rs`, called
     from `perform_commit` and `perform_stash_push`) and saves the answer for that repository
     only, once the commit or stash succeeds; the repository's own config wins
     whenever it sets one. It cannot reproduce a per-directory identity set through
     `includeIf`.
   - **Signing.** This checkout's commits are signed (`commit.gpgsign = true`,
     `gpg.format = ssh`; recent commits show `G` under `git log --format=%G?`), but the settings
     come from a file the user's global config pulls in with `includeIf`, not from `.git/config`,
     so the isolated open never sees them. The guard writes commit objects with
     no `gpgsig` header (`extra_headers: Vec::new()` in `write_commit`,
     `crates/rc-git-guard/src/actions/commit.rs`, and the same in `write_stash_commits`,
     `stash.rs`). A rebase
     rewrites existing commits, so it would silently swap signed commits for unsigned ones, and a
     branch rule requiring signed commits would reject the next push. SSHSIG signing through the
     ssh-agent is possible — `russh` is already a workspace dependency in `Cargo.toml` and already
     talks to the agent for auth — but it is new work.
   - **The replay itself.** gix `=0.87.1` offers three-way tree merges behind its `merge`
     feature, which the workspace `gix` entry in `Cargo.toml` does not enable, and has no rebase. Replaying commits,
     aborting on a conflict, refusing when the local side holds a merge commit or a commit
     already reachable from a remote-tracking ref, and recording a backup ref before the branch
     moves would all be written by hand.

   The safety policy is already settled for it: `Action::RewriteHistory`
   (`crates/rc-git-guard/src/lib.rs`) asks for a confirm and a backup ref on unpublished
   history, and denies published history on a protected branch (the "rewrite unpublished/published
   history" rows of the policy table in `docs/safety.md`). Revisit
   the rebase only if divergence happens often enough that the trip to the terminal is a real
   cost, and settle signing first.

What the diverged sheet does now:

- `reconcile_command` (`crates/rc-git-kit/src/sync/values.rs`) returns
  `cd <quoted workdir> && git pull --rebase`. The `cd` lets the line paste into any shell and
  leaves that shell in the checkout when the rebase stops on a conflict. `--rebase` is explicit
  because the app cannot read `pull.rebase`; per this session's reading, current git stops and
  asks for a strategy when a plain `git pull` meets diverged branches with none configured. A
  user who prefers merge edits the line.
- `shell_word::quote` (`crates/rc-git-kit/src/shell_word.rs`) leaves a word bare when every
  scalar is in the set Python's `shlex.quote` leaves bare minus `=`, and otherwise single-quotes
  it with each embedded quote written as `'\''`. `=` is quoted because both consumers run the
  word in zsh (the updater's `/bin/zsh -lc`, Terminal's default shell), whose EQUALS option
  turns `=ls` into `/bin/ls`.
- `present_diverged` (`crates/rc-git-app/src/window/sync.rs`) shows the guard's message and
  the command with Copy Command, Open in Terminal (copies, then opens Terminal in the checkout
  through `rc_git_mac::open_with`), and Close on Escape. A bare repository has no checkout to
  pull into and keeps the plain message. A diverged pull and a rejected push both reach this
  sheet: `classify` in `crates/rc-git-kit/src/guard_bridge.rs` maps `ActionError::Diverged`
  and `ActionError::PushRejected` to `FailureKind::Diverged`, matching the CLI, which exits
  with its diverged code for both.

## Why This Matters

An in-process copy of a git operation that ignores user config gives a different result from the
same click in any tool that runs git, with no warning: unsigned commits where the user expects
signed ones, a different committer, a rebase where they set merge. The difference surfaces later,
as a rejected push or a commit under the wrong name, far from the click that caused it.

The gap already exists in a smaller form. In a repository set up for signing, commits made from
the Changes tab are unsigned (`write_commit` in `commit.rs`). An in-process rebase would widen that from "new
commits are unsigned" to "signed commits are replaced with unsigned ones".

A handed-over command keeps both rules intact: rc-git writes only through the guard and spawns
nothing, and the user's own config makes the decision, in a line they can read before running it.

## When to Apply

- Adding a guard action that writes commit objects on the user's behalf: pull with rebase or
  merge, amend, cherry-pick, revert, squash. Decide identity and signing first.
- A feature whose correct behavior depends on a setting the isolated open never reads:
  `pull.rebase`/`pull.ff`, an `includeIf` identity, `commit.gpgsign`/`gpg.format`/`user.signingkey`.
- Triaging "git does X and rc-git refuses": check whether git's behavior came from user config
  before treating it as an rc-git bug.
- Building any command for the user to paste or run: quote every variable word with
  `shell_word::quote`, never raw interpolation.

## Examples

```sh
# /Users/me/WorkSpace/rc-git
cd /Users/me/WorkSpace/rc-git && git pull --rebase

# /Users/me/My Repos/it's
cd '/Users/me/My Repos/it'\''s' && git pull --rebase
```

The sheet for a checkout 2 ahead and 4 behind:

```text
Branch and upstream have diverged

cannot fast-forward 'master': local is 2 ahead and 4 behind; reconcile with git

rc-git only fast-forwards. Rebase in a terminal, then sync again:

cd /Users/me/WorkSpace/rc-git && git pull --rebase

[Copy Command]  [Open in Terminal]  [Close]
```

`crates/rc-git-kit/tests/shell_word.rs` runs adversarial words — a quote, `$HOME`,
backticks, `;` and `&&`, globs, braces, a tilde, `#`, `!`, a leading `=`, a newline, non-ASCII
text, the empty word, and the bare word itself — through both `/bin/sh -c` and `/bin/zsh -f -c`
running `printf %s <word>`, and checks each shell exits 0 and prints the word back unchanged.
Writing the escape as `\'` instead of `'\''` fails it.

## Related

- `docs/solutions/architecture-patterns/hand-rolled-git-push-over-gix-https.md` — the same
  constraints (gix lacks the operation, no process may be spawned) with the opposite outcome:
  push was built in-process, because it needs nothing from user config.
- `docs/solutions/integration-issues/gix-no-reflog-for-symbolic-ref-update.md` — the isolated
  open's missing identity, as it shows up in reflog lines.
- `docs/solutions/runtime-errors/sync-hangs-with-no-cancel-and-no-idle-timeout.md` — under an
  isolated open, configuration comes from code.
- `CONCEPTS.md` — Isolated open, Remote operations, Dirty overlap.

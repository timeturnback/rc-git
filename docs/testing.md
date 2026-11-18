# Testing

## Suites

| Suite | Runs with | Covers |
|---|---|---|
| `crates/*/src` unit tests | `cargo test` | pure functions: binary rule, line index, sorting, ids, guard policy |
| `crates/rc-git-core/tests/repository.rs` | `cargo test` | open, refs, resolve, trees, blobs, log, commit details and an unreadable index against the standard fixture |
| `crates/rc-git-core/tests/diff_blame.rs` | `cargo test` | tree diffs, blob diffs and blame |
| `crates/rc-git-core/tests/history.rs` | `cargo test` | path history and commit search |
| `crates/rc-git-core/tests/stash.rs` | `cargo test` | the stash list |
| `crates/rc-git-core/tests/worktree.rs` | `cargo test` | the working tree with its status overlay |
| `crates/rc-git-core/tests/read_only.rs` | `cargo test` | `.git` unchanged after every operation |
| `crates/rc-git-guard/tests/actions.rs` | `cargo test` | every write path with its policy and backup refs, against fixtures |
| `crates/rc-git-guard/tests/remote.rs` | `cargo test` | fetch, pull and push end to end over `HttpGitServer`/`SshGitServer` on loopback, serving a bare clone in a sibling temp dir |
| `crates/rc-git-guard/src/actions/remote/github/tests.rs` | `cargo test` | the GitHub client against testkit's fake `GitHubServer` on loopback: hostile remotes, redirects, oversized bodies, secrets in errors, the device flow, refresh, cancel and idle budgets |
| `crates/rc-git-cli/tests/auth.rs` | `cargo test` | `rc-git auth` against the fake GitHub host, with a scratch settings plist and keychain service |
| `crates/rc-git-testkit/tests/fixture.rs`, `http.rs`, `ssh.rs` | `cargo test` | the fixture builder and the test servers, proved against the git CLI and OpenSSH |
| `crates/rc-git-cli/tests/cli.rs` | `cargo test` | output shape and exit codes of the binary |
| `crates/rc-git-kit/tests` | `cargo test` | models, layout policy, markdown and search units against `FakeRepository` or per-model stubs, plus `repository_live.rs`, `repository_actions.rs` and `guard_bridge.rs` against the real core and guard |
| `crates/rc-git-mac/src/*/tests.rs` | `cargo test` | the live platform implementations against the real system: preferences, keychain, FSEvents, the process runner, the update environment |
| `crates/rc-git-app/src/tests` | `cargo test` | views driven through GPUI's test context; the `*_ink` suites check what is drawn |

`scripts/check.sh` runs all of them plus the linters.

## The standard fixture

`rc-git-testkit` builds this repository with the git CLI:

```
commit 1 (main)     "Initial import"         README.md, src/main.rs, bin.dat      tag v0.9
commit 2 (feature)  "Feature work"           + feature.txt
commit 3 (main)     "Add docs and library"   + docs/guide.md, src/lib.rs, alpha.txt,
                                               Zeta.txt, link -> README.md
                                             tag v1.0 (annotated), refs/remotes/origin/main
```

Dates are fixed (2024-01-01, -02, -03 UTC) so ordering is deterministic; the `v1.0` tagger
date is 2024-01-03 as well, so the tag object id is stable too. `bin.dat` carries a
NUL so git and rc-git call it binary. Assertions compare against `git rev-parse`, never a
hard-coded hash.

Every git invocation is hermetic: `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`,
`HOME` inside the fixture, fixed author and committer, `commit.gpgsign=false`,
`GIT_TERMINAL_PROMPT=0`. A developer's signing or template configuration cannot leak into a test,
and a git that wants a credential fails instead of waiting on the terminal.

## What a change needs

- A new core operation: a case in the matching `crates/rc-git-core/tests/<area>.rs`
  (repository, diff_blame, history, worktree, stash), a call in `read_only.rs`, a
  `repository_live.rs` case for the kit's `RepositoryReading` method, a CLI test when there is
  a subcommand.
- A new guard action or rule: a unit test in the guard's `lib.rs`, a case in `tests/actions.rs`
  (`tests/remote.rs` for remote I/O, including the untracked-branch case), the `docs/safety.md`
  table, and a `repository_actions.rs` or `guard_bridge.rs` case in rc-git-kit.
- A new error path: a test that triggers it and checks the message.
- A new model behavior: a case in that model's suite in `crates/rc-git-kit/tests`:
  `browser/` or `changes_model.rs` with `FakeRepository`; `sync_model.rs` and
  `update_model.rs` with their stubs. Include the stale-generation case when the behavior is
  asynchronous.
- A new layout threshold: a `layout_policy.rs` case at the boundary.

## Running a subset

```sh
cargo test -p rc-git-core --test repository refs_are_classified
cargo test -p rc-git-kit --test browser
cargo test -p rc-git-app
```

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
| `crates/rc-git-testkit/tests/fixture.rs`, `http.rs`, `ssh.rs` | `cargo test` | the fixture builder and the test servers, proved against the git CLI and OpenSSH |
| `crates/rc-git-ffi` tests | `cargo test` | every C function, including `NULL` and malformed input |
| `crates/rc-git-cli/tests/cli.rs` | `cargo test` | output shape and exit codes of the binary |
| `mac/Tests/RcGitKitTests` | `scripts/build.sh && swift test` | model, policy and parser units against `FakeRepository` or per-model stubs, plus `RepositoryIntegrationTests` and `RepositoryGuardTests` against the real bridge |

`scripts/check.sh` runs all of them plus the linters.

## The standard fixture

Both `rc-git-testkit` (Rust) and `GitFixture.swift` build this repository with the git CLI:

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
and a git that wants a credential fails instead of waiting on the terminal. The Swift fixture
also nulls stdin, drains both pipes at once, and fails a git that runs past 60 s.

## What a change needs

- A new core operation: a case in the matching `crates/rc-git-core/tests/<area>.rs`
  (repository, diff_blame, history, worktree, stash), a call in `read_only.rs`, an FFI test, a
  Swift bridge case, a CLI test when there is a subcommand.
- A new guard action or rule: a unit test in the guard's `lib.rs`, a case in `tests/actions.rs`
  (`tests/remote.rs` for remote I/O, including the untracked-branch case), the `docs/safety.md`
  table, an FFI test, and a `RepositoryGuardTests` case.
- A new error path: a test that triggers it and checks the message.
- A new model behavior: a case in that model's suite: `BrowserModelTests`, `ChangesModelTests`
  or `WorktreeModelTests` with `FakeRepository`; `SyncModelTests` and `UpdateModelTests` with
  their stubs. Include the stale-generation case when the behavior is asynchronous.
- A new layout threshold: a `LayoutPolicyTests` case at the boundary.

## Running a subset

Swift tests link `target/release/librc_git_ffi.a`. Run `scripts/build.sh` after cloning and
after any Rust change, or `swift test` runs against the previous build.

```sh
cargo test -p rc-git-core --test repository refs_are_classified
cargo test -p rc-git-ffi
swift test --package-path mac --filter BrowserModelTests
```

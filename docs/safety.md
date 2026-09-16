# Safety

Two promises: rc-git cannot damage a repository, and rc-git cannot be tricked by one.

## The core cannot write

Four layers enforce it, and each catches what the others miss.

1. **Lint.** `clippy.toml` lists every gix write method (`edit_reference`, `write_object`,
   `commit`, `tag`, ...), every `std::fs` mutation, and `Command::new`. Clippy rejects a call
   to any of them in any crate. The test kit opts out per function with a written reason, and
   `rc-git-guard` — the one sanctioned write path — opts out at each ref edit the same way.
2. **Test.** `crates/rc-git-core/tests/read_only.rs` snapshots every file under `.git`, runs
   every core operation — working-tree listings and file reads included — and compares the
   snapshot byte for byte.
3. **Features.** `gix` is compiled without network, checkout, or credential features. Code
   that could push, fetch, or check out does not exist in the binary. The `index` and
   `excludes` features are on so the working-tree view can honor ignore rules; both read.
4. **Unsafe.** `unsafe_code` is forbidden in every crate except `rc-git-ffi`, where each block
   carries a documented contract and clippy denies undocumented ones.

## Untrusted repositories

Opening a repository someone else produced must be harmless.

- `Options::isolated()`: only `.git/config` is read. No `~/.gitconfig`, no system config, no
  environment variables, and `include`/`includeIf` directives are ignored, so a repository
  cannot point rc-git at files outside itself.
- File names are bytes, not strings. Non-UTF-8 names display with replacement characters; the
  UI navigates by object id, so a hostile name cannot redirect a lookup.
- Blob reads are capped. The header is checked before decoding; the default cap is 8 MiB, the
  FFI clamps any cap to 4 GiB, and the app refuses "Load anyway" above 512 MiB.
- Binary detection follows git: a NUL in the first 8000 bytes. Binary content is never handed
  to the text view.
- Errors are messages, never panics. Clippy denies `unwrap`, `expect`, `panic`, `todo`, and
  `unimplemented` outside tests; release builds abort on panic rather than unwind across the
  C boundary.
- No network code is compiled in. Nothing rc-git does can leave the machine.

## Guarding mutations

`rc-git-guard` decides what a user must do before any action that changes a repository, and
its `actions` module is the only code in rc-git that performs one. Today that means branch
and tag creation and deletion from the sidebar, and — from the Changes tab — committing
selected files or hunks, the stash lifecycle, and discarding uncommitted changes, all driven
through the `rcgit_guard_*` FFI functions. Every other row of the table is settled policy
waiting for its feature. `Policy::evaluate` maps an `Action` to a `Decision`:

| Action | Protected branch | Elsewhere |
|---|---|---|
| fetch, checkout, plain push | allow | allow |
| pull (fast-forward) | allow (the write path refuses dirty files) | same |
| create branch, create tag | allow (never overwrites) | allow (never overwrites) |
| force push | **deny** | type the branch name |
| delete local branch, merged | **deny** | confirm |
| delete local branch, unmerged | **deny** | type the branch name, backup ref |
| delete remote branch | **deny** | type the branch name, backup ref |
| delete tag | type the tag name, backup ref | same |
| reset --hard | type the branch name, backup ref | same |
| rewrite unpublished history | confirm, backup ref | same |
| rewrite published history | **deny** | type the branch name, backup ref |
| discard working changes | type `discard` | same |
| prune unreachable objects | type `prune` | same |
| commit selected changes | allow (adds objects, advances the checked-out branch) | same |
| stash push | confirm (the stash commit is the record) | same |
| stash apply | allow (the write path refuses dirty files) | same |
| stash pop | confirm, backup ref | same |
| stash drop | type `drop`, backup ref | same |

Protected patterns default to `main`, `master`, and `release/*`. A `Deny` cannot be confirmed
away inside rc-git; the message points the user at git itself. A `Confirm` produces a
`Challenge`; only `Challenge::verify` with the exact phrase yields a `Confirmation`, and the
write path holds one before it edits anything, so the compiler enforces the flow.
`backup_ref` means a ref under `refs/rc-git/backup/` is written before the deletion — the
branch tip, the tag object, or the dropped stash commit — with numbered slots (`-2`, `-3`, …)
so no backup ever overwrites an earlier one. Two more rules live in the write path itself:
the checked-out branch cannot be deleted, and a ref is never created over an existing one.

The Changes tab's write paths add their own guarantees:

- **Raw bytes throughout.** Status, diffs, commits, and restores all work on the file's raw
  bytes: gitattributes filters and `core.autocrlf` are not honored (matching the Files
  view), so what the diff shows is exactly what a commit records and a restore writes.
- **Commit** builds blobs from disk, rewrites only the touched directories, and advances the
  branch through the same single ref-edit funnel — only if `HEAD` still is the commit the
  change list was built against. Hunk selections are re-validated against a fresh diff and
  refused when the file moved. The committed paths are then updated in `.git/index` so
  terminal `git status` agrees; entries staged from the terminal for other paths survive.
- **Stash** entries are written in git's own format — `git stash list`, `apply`, `pop`, and
  `drop` all accept them — and rc-git's apply refuses to touch any file that is not exactly
  at the stash's base state, so it can never overwrite uncommitted work. Ticked untracked
  files go into the stash tree itself rather than git's third-parent scheme; `git stash pop`
  recreates them all the same.
- **Discard** and stash restores write through one helper that removes the old file first,
  so a write never follows a stale symlink, and deleting an already-gone file is not an
  error — a retried action lands in the state it retries from.

## Supply chain

Rust dependencies are pinned to exact versions in `Cargo.toml` and checked by `cargo deny`
for advisories, licenses, and sources on every run of `scripts/check.sh` and in CI. The Swift
package has no dependencies. GitHub Actions are pinned to commit hashes and scanned by
`zizmor`; Dependabot updates both ecosystems weekly with a seven-day cooldown.

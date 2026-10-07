# Safety

Two promises: rc-git cannot damage a repository, and rc-git cannot be tricked by one.

## The core cannot write

Four layers enforce it, and each catches what the others miss.

1. **Lint.** `clippy.toml` lists the gix object and ref writes (`edit_reference`,
   `write_object`, `commit`, `tag`, `edit_tree`, `Reference::delete`, the `objects` and `refs`
   stores' write and transaction methods, `gix::init`, ...), the `gix_lock` lock-file acquire
   and commit methods, the `std::fs` mutations (write, create, copy, link, rename, remove,
   permissions, ownership, times), the std and tokio `Command::new` spawns, and the network
   entry points. Clippy rejects a call to any of them in any crate. The test kit opts out per
   function with a written reason, and `rc-git-guard` — the one sanctioned write path — opts
   out the same way at each write it performs: every ref edit, and the working-tree and reflog
   writes that go with them. A write
   primitive the workspace has not used before joins the list in the change that first uses it;
   an unlisted one passes the lint and leaves the guarantee unenforced for that path.
2. **Test.** `crates/rc-git-core/tests/read_only.rs` snapshots every file under `.git`, runs
   every core operation — working-tree listings and file reads included — and compares the
   snapshot byte for byte.
3. **Features.** A standalone build of `rc-git-core` compiles `gix` without checkout,
   transport or credential features; the `core standalone` step in `scripts/check.sh` and CI
   builds it alone, because Cargo unifies features across a workspace build and the linked
   library carries the guard's. `rc-git-guard` alone enables the transports — HTTPS
   (`blocking-http-transport-curl`, system libcurl) and SSH (russh, in-process; no `ssh`
   child) — and clippy bans the network entry points it lists — gix's `remote_at`,
   `find_remote`, `connect`, `handshake`, plus `russh::client::connect` and the std and
   tokio `TcpStream::connect` — outside the guard's `actions/remote` module. The curl
   feature also compiles gix's credential-helper and prompt machinery into the binary;
   it is unreachable, because every connection installs a credential callback that
   answers from caller-supplied credentials and never consults the helper cascade, and
   `Command::new` stays banned workspace-wide. The ssh side reads `~/.ssh` (config,
   `known_hosts`, keys) and writes none of it; host-key approvals live in the app's own
   defaults. The `index` and `excludes` features are on so the working-tree view can
   honor ignore rules; both read.
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
- Network I/O exists only behind `rc-git-guard`'s fetch, pull, push, and sync actions,
  speaks HTTPS and SSH only (plain `http://` is accepted for loopback hosts, so tests
  can run a local server end to end), and sends credentials only when the caller passed
  them. SSH host keys are verified against a read-only `known_hosts`: an unknown key is
  refused until the user approves its fingerprint, and a contradicting key is always
  fatal. Nothing else rc-git does can leave the machine.

## Guarding mutations

`rc-git-guard` decides what a user must do before any action that changes a repository, and
its `actions` module is the only code in rc-git that performs one. Today that means branch
and tag creation and deletion from the sidebar; from the Changes tab, committing selected
files or hunks, the stash lifecycle, and discarding uncommitted changes; switching `HEAD` to
another local branch, which the Back-to-default control composes with a fetch and a
fast-forward; and the remote operations — fetch, fast-forward pull, plain push, and their sync
composition — all driven through the `rcgit_guard_*` FFI functions. Every other row of the table is settled policy
waiting for its feature. `Policy::evaluate` maps an `Action` to a `Decision`:

| Action | Protected branch | Elsewhere |
|---|---|---|
| fetch, plain push | allow | allow |
| checkout | allow (the write path refuses dirty files) | same |
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
| discard working changes | confirm (no backup: the bytes are nowhere else) | same |
| prune unreachable objects | type `prune` | same |
| commit selected changes | allow (adds objects, advances the checked-out branch) | same |
| stash push | confirm (the stash commit is the record) | same |
| stash apply | allow (the write path refuses dirty files) | same |
| stash pop | confirm, backup ref | same |
| stash drop | type `drop`, backup ref | same |

Protected patterns default to `main`, `master`, and `release/*`. A `Deny` cannot be confirmed
away inside rc-git; the message points the user at git itself. A `Confirm` produces a
`Challenge`; only `Challenge::verify` — with the exact phrase when the challenge names one,
which the rows above spell out — yields a `Confirmation`, and the
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

- **A branch switch** applies the tree diff between the two commits to exactly the changed
  paths, through the same planner the fast-forward pull uses: any dirty or staged overlap
  refuses the whole switch, named by path, before a byte is written, and paths the switch does
  not touch keep their changes. `HEAD` moves last, expecting the branch or detached commit the
  switch was planned against. Its `checkout:` reflog line is appended explicitly, because a
  gix transaction writes none for a symbolic update — without it `git checkout -` and `HEAD@{n}`
  would name the wrong commit
  (`docs/solutions/integration-issues/gix-no-reflog-for-symbolic-ref-update.md`).

The remote operations add their own guarantees:

- **Fetch** runs gix's own fetch machinery: the pack lands in `.git/objects/pack` and the
  remote-tracking refs move inside that one call — the single ref-write path that does not
  go through `apply_edit`. Tracking-ref updates lose nothing. `FETCH_HEAD` is never
  written.
- **Fast-forward pull** applies the tree diff to exactly the changed paths. Any dirty or
  staged overlap — an untracked file in the way included — refuses the whole pull before a
  byte is written. The branch ref moves last, expecting the old tip, so an interruption
  leaves visibly modified files rather than what looks like lost work, and a branch moved
  from the terminal mid-pull fails the ref edit instead of clobbering it.
- **Push** is never force from rc-git: the remote tip must be an ancestor of the local tip,
  checked client-side before anything is sent and enforced again by the server. The pack is
  self-contained (never thin). A rejection surfaces the server's reason verbatim.
- **Credentials** are per-host, supplied by the caller (the app reads the login keychain),
  passed through the FFI per call, and never stored or logged by the Rust side.
- **Every transfer ends.** Each remote call carries an `RcGitCancel` the app can signal
  from another thread; the ssh transport wakes out of its read within 200 ms to look at
  it, and the pack steps check it between objects. A connection that stops moving bytes
  dies on its own after 120 seconds — `http.lowSpeedLimit`/`lowSpeedTime` set in code
  for curl (an isolated open reads no global config), an idle budget per read and write
  for ssh — with 30 seconds for dialing and the handshake. A cancelled operation stops
  between steps: nothing is left half-applied.

## Supply chain

Rust dependencies are pinned to exact versions in `Cargo.toml` and checked by `cargo deny`
for advisories, licenses, and sources on every run of `scripts/check.sh` and in CI. The Swift
package has no dependencies. GitHub Actions are pinned to commit hashes and scanned by
`zizmor`; Dependabot updates both ecosystems weekly with a seven-day cooldown.

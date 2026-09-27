---
description: how any future mutating action must be gated, and what rc-git-guard already decides
paths: crates/rc-git-guard/**
---

# rc-git-guard

The policy for every mutating action, and — in `actions.rs` — the only code in rc-git that
performs one. Branch and tag management go through here; so does everything that comes later.

- Every mutation calls `Policy::evaluate` first and follows the `Decision`. The write only
  happens while holding a `Confirmation`, and the only way to obtain one is
  `Challenge::verify`, so the type system forces the typed-phrase step.
- The front end renders decisions; it never classifies an action itself and never offers a
  one-click path around a challenge. The FFI passes the user's typed input through; the
  verification lives here.
- `Deny` is final. Force-pushing, deleting, or rewriting published history on a protected
  branch is not confirmable inside rc-git; the message tells the user to use git directly.
- A challenge with `backup_ref` writes a ref under `refs/rc-git/backup/<kind>/<name>` before
  the action runs (numbered `-2`, `-3`, … so nothing is overwritten). `write_backup_ref` in
  `actions.rs` is that one implementation; new destructive actions reuse it.
- Ref edits go through `apply_edit`, which carries the `#[expect(clippy::disallowed_methods)]`
  opt-out and a synthetic `rc-git` committer for the reflog. Do not call gix write APIs
  anywhere else.
- Two hard rules live in the write path itself: never delete the checked-out branch, never
  create over an existing ref (`PreviousValue::MustNotExist`).
- Anything that moves the working tree to another commit goes through `actions/treeapply.rs`:
  `plan` the tree diff, `verify_clean` every touched path (a dirty one refuses the whole
  operation by name, before a byte is written), `apply`, then move the ref *last*. The
  fast-forward pull and the branch switch are both that sequence; a new one reuses it rather
  than walking a tree itself.
- A gix ref transaction writes no reflog for a symbolic update — it has no object id to log —
  so `checkout.rs` appends the `checkout:` line to `logs/HEAD` itself, serialized by
  `gix::refs::log::Line` and only when the log already exists. Without it `git checkout -` and
  `HEAD@{1}` would silently point at the wrong commit.
- Protected patterns default to `main`, `master`, `release/*`. Users may add patterns, never
  remove the defaults through the UI.
- Expose the policy to Swift over the FFI when needed; never reimplement it in Swift.
- Every remote entry point takes a `Cancel` and must stay interruptible: no new blocking
  call without a cancel poll and an idle budget (`CANCEL_POLL`, `IDLE_TIMEOUT` in
  `actions/remote.rs`). A blocking read with neither is how a sync gets stuck forever.
  Build a tokio timer inside the `async` block, never as an argument to `block_on`: a
  `Sleep` needs the runtime context that `block_on` has not entered yet.
- Every rule change updates the table in `docs/safety.md`, the tests in `lib.rs`, and the
  tests in `tests/actions.rs` together.
- Remote I/O lives in `actions/remote/` only; the network entry points (`find_remote`,
  `Remote::connect`, `handshake`, …) are clippy-banned everywhere else. HTTPS and SSH —
  `vet_remote_url` routes the scheme, allowing plain http for loopback hosts so tests
  run end to end. The ssh transport (`actions/remote/ssh/`) is in-process russh:
  `known_hosts` is never written (approvals arrive as `RemoteAuth::accepted_hostkey`),
  channel streams must drop inside the transport's runtime, and the passphrase rides
  `Credentials::token`. On the HTTPS side always install the credential closure; never
  let gix's helper cascade run (it spawns).
  `docs/solutions/architecture-patterns/hand-rolled-git-push-over-gix-https.md` has the
  push recipe and its traps (`prevent_pack_unload`, `MissingCommitter`, dev-only
  `http-client-insecure-credentials`).

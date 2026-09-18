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
- Protected patterns default to `main`, `master`, `release/*`. Users may add patterns, never
  remove the defaults through the UI.
- Expose the policy to Swift over the FFI when needed; never reimplement it in Swift.
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

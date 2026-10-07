---
title: "Push over HTTPS with gix that has no push: hand-rolled send-pack"
date: 2026-09-17
category: architecture-patterns
module: rc-git-guard
problem_type: architecture_pattern
component: infrastructure
severity: high
applies_when:
  - "Adding any network operation (fetch, push, clone) on gitoxide 0.87.x"
  - "Generating a pack from an opened gix repository's object database"
  - "Testing HTTPS git operations without a real remote or new dependencies"
tags:
  - gix
  - gitoxide
  - push
  - send-pack
  - pack-generation
  - http-transport
  - credentials
  - smart-http
---

# Push over HTTPS with gix that has no push: hand-rolled send-pack

## Context

rc-git promises to never spawn a process, which rules every git transport out except
HTTPS. gix `=0.87.1` implements fetch but **no push at all**: `gix-0.87.1/src/remote/connection/`
contains only `fetch/`, and `gix-protocol` 0.65.1 has no send-pack and no `report-status`
parsing. Push had to be assembled from the lower gitoxide crates. This doc records the
recipe and the traps, because none of it is discoverable from gix's docs.

## Guidance

The working pipeline (`crates/rc-git-guard/src/actions/remote/push.rs:48`,
`pack.rs`, `report.rs`):

1. **One HTTP connection, two phases.** `gix_transport::client::blocking_io::connect::connect`
   (reached via the `gix::protocol::transport::client::blocking_io` re-export — the
   shorter `client::Transport` path does not exist) builds the transport; **then set the
   HTTP timeouts on it**, because `connect::Options` has no field for them:
   `http_timeouts(&mut options)` + `transport.configure(&options)` with a
   `blocking_io::http::Options`. Skip that step and curl waits forever on a peer that
   goes quiet — an isolated open supplies no `http.lowSpeedLimit`. Then
   `gix::protocol::handshake(transport, Service::ReceivePack, authenticate, …)` performs
   the `GET /info/refs` advertisement, and a later `transport.request(...)` on the *same*
   transport performs the `POST /git-receive-pack`.
2. **Client-side non-force check before sending a byte**: the advertised old tip must
   exist locally and be an ancestor of the local tip (`merge_base == old`), else
   `PushRejected` — the server enforces it again, but the client check gives the
   friendly error.
3. **Pack generation** (`pack.rs`): commits from
   `repo.rev_walk([tip]).with_hidden(advertised_tips_we_have)`, then
   `gix_pack::data::output::count::objects` with
   `ObjectExpansion::TreeAdditionsComparedToAncestor`, then
   `entry::iter_from_counts` wrapped in `gix::features::parallel::InOrderIter`, then
   `bytes::FromEntriesIter` into a `Vec<u8>`. No thin packs (`allow_thin_pack: false`) —
   always valid, slightly larger.
4. **Command line + response** (`report.rs`): one pkt-line
   `"<old> <new> refs/heads/<b>\0report-status agent=…"` written through the
   `RequestWriter` in text mode, `write_message(MessageKind::Flush)`, then
   `into_parts()` for the raw (unframed) pack bytes. Do not request `side-band-64k`:
   without it the reply is plain pkt-line payloads (`unpack ok`, `ok|ng <ref> [reason]`),
   which a ~60-line permissive parser handles for both report-status v1 and v2.

Traps that each cost a debug cycle:

- **`prevent_pack_unload` or panic.** Pack generation stores pack ids for later lookup;
  without it the count thread panics
  `BUG: handle must be configured to 'prevent_pack_unload()' before using this method`.
  Also, `repo.objects` is a memory-`Proxy` wrapper that does **not** implement
  `gix_pack::Find` — deref to the cache underneath and clone that
  (`crates/rc-git-guard/src/actions/remote/pack.rs:59-63`):

  ```rust
  let mut db = (*repo.objects).clone();
  db.prevent_pack_unload();
  ```

- **Reflog needs a committer under `isolated()` opens — and a committer is not what makes a
  ref write log.** Any ref write gix performs itself (fetch's tracking-ref updates included)
  fails with `MissingCommitter` unless an in-memory `user.name`/`user.email` is set first via
  `config_snapshot_mut()` + `commit()` (`set_reflog_identity` in `actions/remote.rs:454`). The
  on-disk config is never touched. Identity is only the precondition: a **symbolic** update
  (`HEAD` -> `refs/heads/<branch>`) carries no object id, so gix skips its reflog by design
  whatever identity is configured, and the line has to be appended by hand
  (`../integration-issues/gix-no-reflog-for-symbolic-ref-update.md`).

- **Empty packs are valid.** Pushing a new branch that points at an existing commit
  uploads zero objects; a `PACK` header + trailing hash with 0 entries is accepted by
  `git http-backend`.

- **Credentials without spawning.** `blocking-http-transport-curl` force-enables gix's
  credential-helper cascade (which spawns `git credential-*`). Always installing a
  `with_credentials`/`authenticate` closure that answers from caller-supplied
  credentials fully replaces the cascade — zero spawn at runtime, though `gix-prompt`
  is still compiled in. gix only invokes the closure after a 401, so anonymous access
  to public remotes works.

- **Basic auth over plain http is refused** ("Will not send credentials in clear text
  over http") unless gix-transport's `http-client-insecure-credentials` feature is on.
  Enable it as a **dev-dependency only** (the feature-less `gix-transport` entry in the
  workspace `Cargo.toml` `[workspace.dependencies]`, plus the `[dev-dependencies]` lines in
  `crates/rc-git-guard/Cargo.toml` and `crates/rc-git-cli/Cargo.toml` that add the feature;
  `deny.toml` fails on a second copy) — dev-dep feature unification covers test builds,
  while release builds keep the refusal. Production code additionally rejects non-HTTPS
  URLs before connecting, loopback hosts excepted for the test server
  (`vet_remote_url`, `actions/remote.rs:317`, which also routes the ssh scheme).

- **Zero-dependency test server.** `HttpGitServer` (`crates/rc-git-testkit/src/http.rs:17`)
  is a `std::net::TcpListener` CGI shim exec-ing `git http-backend` from
  `git --exec-path` (not on `PATH`), handling `Expect: 100-continue` and chunked
  request bodies. The served repo needs `http.receivepack=true` or pushes 403. The
  test kit already holds the workspace's shell-out exemption, so this adds no
  dependency and no new spawn site.

## Why This Matters

Every piece above is undocumented-by-omission in gitoxide: push is simply absent, the
`Find`-impl gap on `Proxy` and the `prevent_pack_unload` requirement surface only as
runtime panics, and the clear-text refusal only as a failed request. Rediscovering this
chain from scratch took a full exploration pass over the vendored crate sources; with
this doc the next network feature (force push with its typed challenge, multi-remote
fetch, clone) starts from a working recipe.

## When to Apply

- Extending rc-git's remote support (force push, remote branch deletion, clone).
- Upgrading gix: check first whether `gix::remote` gained a push API — if it did,
  `push.rs`/`pack.rs`/`report.rs` should shrink into it.
- Any repo that needs git network I/O without spawning processes.

## Examples

End-to-end test shape (`crates/rc-git-guard/tests/remote.rs`): standard fixture →
`git clone --bare` into a **sibling** temp dir (a bare clone inside the origin's working
tree gets swept up by the fixture's `git add -A`) → `HttpGitServer::serve` →
`git clone` from the URL → drive `fetch_upstream`/`pull_fast_forward`/`push_upstream`
against it, asserting with `git rev-parse` on both sides.

## Related

- [Sync spins forever: a cancel flag nobody could set and a transport with no idle budget](../runtime-errors/sync-hangs-with-no-cancel-and-no-idle-timeout.md)
  — what this transport needed afterwards to be stoppable: `connect::Options` carries no
  HTTP settings, so the connect and low-speed timeouts go on through
  `transport.configure(&http::Options { .. })` once it is built.
- [A gix ref transaction writes no reflog for a symbolic update: HEAD switches git cannot follow](../integration-issues/gix-no-reflog-for-symbolic-ref-update.md)
  — the same omission one layer down, in the local write path: the identity this doc sets is
  necessary but not sufficient, and the missing `checkout:` line is serialized with
  `gix::refs::log::Line::write_to` because the transaction writes none.

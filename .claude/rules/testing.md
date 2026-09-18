---
description: how fixtures are built and what every test suite must keep true
paths: crates/**/tests/**, crates/rc-git-testkit/**, mac/Tests/**
---

# Tests

- Fixtures are real repositories built by the git CLI with a hermetic environment: no global or
  system config, fixed identity, fixed dates, signing off. `rc-git-testkit` and
  `mac/Tests/RcGitKitTests/GitFixture.swift` build the same layout; change both.
- Assert against `git rev-parse`, not hard-coded hashes, so a fixture change does not rewrite
  every expectation.
- Test behavior through the public API. A refactor that keeps behavior must not break a test.
- Every error path the code handles has a test that triggers it, including NULL and malformed
  input at the FFI boundary.
- `tests/read_only.rs` must exercise every core operation. When you add one, add it there.
- Unit tests may `unwrap`; library code may not. Integration test files carry
  `#![expect(clippy::unwrap_used, ...)]` only when a helper *outside* `#[test]` fns unwraps —
  `allow-unwrap-in-tests` in `clippy.toml` already covers `#[test]` bodies, so the expect is
  otherwise unfulfilled and fails `-D warnings`.
- Timing-raced tests (slow-load vs fast-load) need wide margins: suites run in parallel and
  a 20 ms hop can overrun a 0.2 s delay under load. Use ≥1 s for the "slow" side.
- Network tests use `HttpGitServer` or `SshGitServer` (testkit): serve a
  `git clone --bare` copy placed in a
  **sibling** temp dir — a bare clone inside the origin's working tree gets swept into the
  next `git add -A` commit and poisons the diff. Set `http.receivepack=true` on the served
  repo or pushes 403. Basic-auth-over-http needs the dev-only
  `http-client-insecure-credentials` gix-transport feature in the testing crate's
  dev-dependencies. Ssh tests clone from the local bare path, re-point `origin` at
  `SshGitServer::url()`, and pass a fixture directory via `RemoteAuth::ssh_dir`
  (CLI: `--ssh-dir`) — never the developer's real `~/.ssh`.

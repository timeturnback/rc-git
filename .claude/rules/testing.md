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
  `#![expect(clippy::unwrap_used, ...)]` for their helpers.

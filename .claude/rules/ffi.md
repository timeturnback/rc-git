---
description: the C ABI contract between crates/rc-git-ffi and the Swift package
paths: crates/rc-git-ffi/**, mac/Sources/CRcGit/**, mac/Sources/RcGitKit/Repository.swift
---

# FFI

- The header `mac/Sources/CRcGit/include/rc_git.h` is generated. Edit the Rust, run
  `scripts/gen-header.sh`, commit both. `--verify` runs in the hook and in CI.
- Every returned pointer is owned by the caller and released by exactly one matching
  `rcgit_*_free`. Every C struct that owns pointers has a `Drop` impl; free functions only
  `release` the outer box.
- Functions that can fail take `char **error`. Set it only on failure, never on success.
  `NULL` for `error` is always acceptable.
- Read every pointer argument through `repo_arg`, `oid_arg`, `bytes_arg`, and write through
  `Out`. One `unsafe` block per function, with a `// SAFETY:` line; clippy denies undocumented
  blocks. Never dereference a caller pointer anywhere else.
- Strings cross as NUL-terminated bytes without UTF-8 validation; the Swift side decodes
  lossily. Interior NULs are stripped, never an error.
- `RcGitRepo` is `Send + Sync` because `rc_git_core::Repository` is; keep it that way so Swift
  may call from any thread.
- The Swift wrapper in `Repository.swift` mirrors each function one to one, converts to
  value types, and frees in a `defer`. Tests in `RepositoryIntegrationTests.swift` drive the
  real library through a fixture; add a case there for every new function.

---
description: the C ABI contract between crates/rc-git-ffi and the Swift package
paths: crates/rc-git-ffi/**, mac/Sources/CRcGit/**, mac/Sources/RcGitKit/Repository.swift, mac/Sources/RcGitKit/RepositoryGuard.swift, mac/Sources/RcGitKit/Highlighting.swift, mac/Sources/RcGitKit/ImportLinks.swift, mac/Sources/RcGitKit/RepositoryError.swift
---

# FFI

- The header `mac/Sources/CRcGit/include/rc_git.h` is generated. Edit the Rust, run
  `scripts/gen-header.sh`, commit both. `--verify` runs in the hook and in CI.
- Every returned pointer is owned by the caller and released by exactly one matching
  `rcgit_*_free`. Every C struct that owns pointers has a `Drop` impl; free functions only
  `release` the outer box.
- Functions that can fail take `char **error`. Set it only on failure, never on success.
  `NULL` for `error` is always acceptable.
- Read every pointer argument only through an `unsafe fn` reader with a `# Safety` contract:
  the generic ones in lib.rs's "Pointer helpers" section (`*_arg`, including `slice_arg` for
  arrays and byte buffers) or a struct reader beside its type (`read_*`, `Owned*::read`,
  `fallback_author`, `credentials_from`, `cancel_flags`). Write through `Out`. A new pointer
  shape gets a new reader; never dereference a caller pointer anywhere else. One `unsafe`
  block per function, with a `// SAFETY:` line; clippy denies undocumented blocks.
- Strings cross as NUL-terminated bytes without UTF-8 validation; the Swift side decodes
  lossily. Interior NULs are stripped, never an error.
- `RcGitRepo` is `Send + Sync` because `rc_git_core::Repository` is; keep it that way so Swift
  may call from any thread.
- The Swift wrappers mirror each function one to one, convert to value types, and free in a
  `defer`. Reads live in `Repository.swift`, tested in `RepositoryIntegrationTests.swift`;
  guard and cancel in `RepositoryGuard.swift`, tested in `RepositoryGuardTests.swift`;
  highlight and import links in `Highlighting.swift` and `ImportLinks.swift`, tested in
  `HighlightingTests.swift` and `ImportLinksTests.swift`. `RepositoryError.swift` holds the
  shared helper that takes and frees a returned string. The tests drive the real library
  through a fixture; add a case for every new function.
- A new Swift file that calls `rcgit_*` joins this rule's `paths:` list.

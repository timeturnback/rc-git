---
description: rc-git-core is a read-only gix wrapper; what may enter it and what never may
paths: crates/rc-git-core/**
---

# rc-git-core

- Never call a gix write API, mutate the working tree, or spawn a process. `clippy.toml` lists
  what is banned; if clippy accepts something that writes, it is still banned. Add it to the
  list. `worktree.rs` reads the working directory with `std::fs` reads only; keep it that way.
- No gix types in the public API except the `bstr` re-export. Wrap in `Oid`, `TreeEntry`,
  `CommitSummary`, so the FFI and Swift never chase gix releases.
- Every operation runs inside `Repository::with`. Never keep a `gix::Repository` beyond one
  call: the pool lends one per call and builds a temporary handle when all are busy.
- Every `Error` variant names the operation (`op`) and the object. A new variant needs a test
  of its message in `error.rs`.
- Read a header before decoding a blob, and honor the caller's limit. A huge blob must cost one
  header read, never a decode.
- Adding a read operation: core fn with `# Errors` docs, test in `tests/repository.rs`, call in
  `tests/read_only.rs`, FFI function and header, Swift wrapper and test, CLI subcommand when
  scriptable. `docs/ffi.md` has the FFI half.
- Fixtures come from `rc-git-testkit`. Write `.git` internals by hand only to simulate
  corruption, as `a_broken_ref_is_reported_without_hiding_the_rest` does.

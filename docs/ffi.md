# FFI

`crates/rc-git-ffi` exposes `rc-git-core` as a C ABI. `cbindgen` turns it into
`mac/Sources/CRcGit/include/rc_git.h`, and `mac/Sources/RcGitKit/Repository.swift` wraps it
in Swift value types.

## Contract

- A pointer returned by an `rcgit_*` function belongs to the caller until it is passed to the
  matching `rcgit_*_free`. Freeing `NULL` is a no-op.
- A `char *` is NUL-terminated, with interior NULs stripped. Outbound tree entry names and
  paths that are not UTF-8 pass through as raw bytes. Ref names are repaired with U+FFFD in
  the core, and `RcGitRef.has_exact_name` says whether that happened. Inbound path and
  ref-name arguments are read as UTF-8, lossily: an invalid byte becomes U+FFFD, so a
  non-UTF-8 name cannot be sent back. The Swift side records which names arrived intact and
  refuses path and ref actions on the rest.
- A function that can fail takes `char **error`. On failure it returns `NULL` (or
  `RCGIT_ERROR`) and, when `error` is not `NULL`, stores a message to release with
  `rcgit_string_free`. On success `*error` is untouched, which is why the Swift side
  distinguishes "not found" from "failed" for `rcgit_entry_at_path`.
- Object ids cross as `RcGitOid`: 32 bytes plus a length, 20 for SHA-1.
- An `RcGitRepo` may be used from any thread, including several at once.
- Statuses: `RCGIT_OK` (0) and `RCGIT_ERROR` (-1) for functions that fill an out parameter.
  Remote functions add `RCGIT_AUTH_REQUIRED` (-2), `RCGIT_DIVERGED` (-3),
  `RCGIT_HOSTKEY_UNKNOWN` (-4, ssh only: the `hostkey` out parameter holds
  `"<key type> <SHA256 fingerprint>"` to show the user, released with
  `rcgit_string_free`), `RCGIT_CANCELLED` (-5), and `RCGIT_PASSPHRASE_REQUIRED` (-7, ssh
  only: an encrypted key stayed locked, and `hostkey` holds that key file's path). Their inputs travel in one
  `RcGitRemoteAuth` struct whose fields may all be `NULL`. Commit and stash push add
  `RCGIT_IDENTITY_REQUIRED` (-6): no author identity, so the app asks for one and retries.
- A status the app branches on is a named code from an exhaustive match on the error, never
  text the app searches for in the message.
- `RcGitCancel` is the one handle meant to be touched from a second thread: the thread
  inside a transfer cannot ask itself to stop. `rcgit_cancel_new` before the call,
  `rcgit_cancel_signal` from the UI thread while it runs, `rcgit_cancel_free` after it
  returns — never before, since the call borrows it for its whole life. `NULL` is
  accepted and means the caller has no way to cancel.

## Memory

Every `#[repr(C)]` struct that owns pointers implements `Drop`, so releasing the outer box
releases everything inside it. Arrays are `Box<[T]>` turned into a pointer and a length by
`export_vec` and rebuilt by `release_vec`. Strings are `CString::into_raw` and
`CString::from_raw`. Nothing is allocated with `malloc`, so nothing may be freed with `free`.

## Adding a function

1. Add the operation to `rc-git-core` with tests (see `.claude/rules/core.md`).
2. In `crates/rc-git-ffi/src/lib.rs`, add the `#[repr(C)]` type if needed with a `Drop` impl,
   then the `pub unsafe extern "C" fn` with a `# Safety` section and one `// SAFETY:` block
   that reads every pointer argument through `repo_arg`, `oid_arg`, `bytes_arg`, `Out::new`.
3. Add a test in the same file that calls it, checks the error path with `NULL` input, and
   frees what it received.
4. Run `scripts/gen-header.sh` and commit the regenerated header.
5. Add the Swift method to `Repository.swift`, free in `defer`, convert to value types.
6. Add a case to `RepositoryIntegrationTests.swift`.

`scripts/gen-header.sh --verify` in the pre-commit hook and CI fails when step 4 is skipped.

## Linking

`mac/Package.swift` links `librc_git_ffi.a` from `target/app` through the `CRcGit` system
library target and its module map; `scripts/rust-lib.sh` copies the release or app-dev build
there. `scripts/build.sh` builds the library and regenerates the header before invoking
`swift build`; run it once after cloning or after touching Rust.

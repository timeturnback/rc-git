---
description: rc-git-core is a read-only gix wrapper; what may enter it and what never may
paths: crates/rc-git-core/**
---

# rc-git-core

- Never call a gix write API, mutate the working tree, or spawn a process. `clippy.toml` lists
  what is banned; if clippy accepts something that writes, it is still banned. Add it to the
  list. `worktree.rs` reads the working directory with `std::fs` reads only; keep it that way.
- No gix types in the public API except the `bstr` re-export. Wrap in `Oid`, `TreeEntry`,
  `CommitSummary`, so rc-git-kit and the app never chase gix releases.
- Every operation runs inside `Repository::with`. Never keep a `gix::Repository` beyond one
  call: the pool lends one per call and builds a temporary handle when all are busy.
- Every `Error` message names what failed and the thing it failed on (path, spec, oid, or op
  plus path/oid). A new variant needs a test of its full message in `error.rs`.
- Read a header before decoding a blob, and honor the caller's limit. A huge blob must cost one
  header read, never a decode.
- Adding a read operation: core fn with `# Errors` docs, a case in the matching
  `crates/rc-git-core/tests/<area>.rs` (repository, diff_blame, history, worktree, stash), call
  in `tests/read_only.rs`, a `RepositoryReading` method (rc-git-kit) with its fake and test, CLI
  subcommand when scriptable.
- Fixtures come from `rc-git-testkit`. Write `.git` internals by hand only to simulate
  corruption, as `a_broken_ref_is_reported_without_hiding_the_rest` does.
- History pages resume through a walk handle (`LogCursor`, `HistoryWalk`) that owns its
  traversal and object-database handle, never a pooled `gix::Repository`; never re-walk from
  the tip with an offset, and never restart from saved frontier oids — without the seen set
  a commit reachable from two of them, or tied on time, comes back twice. Filtered pages
  report `scanned`/`complete` and cap examined commits with `max_scan`, so no call holds a
  pool handle through a long scan. A time floor maps to `Sorting::ByCommitTimeCutoff`, which
  terminates the walk instead of filtering; gix 0.87 has no topo order in `rev_walk`.
- `merge_base` is git's paint-down walk, newest committer time first, bounded by `max_scan`:
  past the budget, or at a shallow edge on a path that still matters, it answers
  `MergeBase::Incomplete`, never a guess. Keep any ancestry question that could walk the whole
  history on the same footing.
- The working-tree status walk reads the index as a stat cache (`StatCache` in `worktree.rs`)
  so an unchanged file costs one `lstat` instead of a full hash. It may only ever *decline*:
  a conflicted, assume-valid, skip-worktree, intent-to-add or racy entry, an id that is not
  the tree's, or any read that fails falls through to hashing. Never let it answer
  `Unchanged` from anything but a full stat match — that hides a real edit. The walk takes
  one `lstat` per entry, as `DiskStat` (gix's `Metadata`, which fills `ctime` as git does),
  and hands it to the cache in `DiskFacts`; never stat the same path again.
- Compare a directory against its tree through `FsRules::index`, not a scan per file. A wide
  directory otherwise costs a pass over the whole tree listing for every file in it.
- Name a disk entry with `FsRules::disk_name` and match it with `TreeIndex::resolve`: macOS
  returns NFD names git stores as NFC, and `core.ignoreCase` matches across case.

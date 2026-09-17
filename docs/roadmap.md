# Roadmap

What has landed, and what is planned. Each item names the layer it touches so the
constraints are clear.

## Done

- **Ref change watching.** `RefWatcher` (RcGitKit) watches the `.git` directory with
  FSEvents, filters to `HEAD`, `packed-refs`, and `refs/` through `RefWatchFilter`, and
  drives `BrowserModel.refreshRefs()`. Every object cache stays; only refs re-read. The
  selection survives by name, reloads when its target moved, and falls back when deleted.
- **Commit list.** The History pane pages through `log(tip, skip, limit)` on scroll, caches
  pages per tip, and offers "Browse Files at This Commit". Selecting a commit shows the
  files it changed against its first parent and each file's line diff.
- **Syntax highlighting.** Tree-sitter in `rc-git-highlight`, exposed as
  `rcgit_highlight`, cached by blob id in `BrowserModel`, applied after first paint so
  plain text never waits on a parse. Languages: bash, C, JavaScript, JSON, Python, Rust,
  Swift, TOML, YAML. Files that are not valid UTF-8 or exceed 2 MiB stay plain.
- **Path search.** ⇧⌘O flattens the selected root once in the background — the one
  feature that walks a whole tree — keyed by root tree id, cancellable, capped at 200 000
  entries, and offers fuzzy jump-to-file. Every directory read lands in the shared tree
  cache.
- **Working tree as a pseudo-ref.** The sidebar's "Working Tree" row shows files on disk
  with a status overlay against `HEAD`: modified, untracked, ignored, deleted. Reads only;
  one directory and one tree object per expand, content hashed only when sizes match.
- **Drag a folder onto the window** to open it.
- **Blame and diff views.** Both read-only. Tree and blob diffs use `gix-diff`
  (`diff_trees`, `diff_blobs`); blame uses `gix-blame` on demand for the open file (⌥⌘B).
  The CLI gained `diff`, `blame`, and `status` verbs.
- **Commit graph.** A lane column in the History pane draws the topology of the paged
  commits: `GraphBuilder` (RcGitKit, pure, tested) assigns lanes strictly sequentially from
  `CommitSummary.parents`, so appending a page never reshuffles drawn rows, and the builder
  is cached per tip alongside the pages. Rendering clamps at 12 drawn lanes; the column
  hides while a path scope or search shows a filtered list. No core or FFI change.
- **Commit search.** A filter bar over the History pane (⇧⌘F) matches message or author as
  the user types, with optional date bounds; matches stream in scan chunks with a progress
  count, and a hex query jumps straight to that revision. `search_commits` shares
  `path_history`'s scan-cursor paging; a since-bound uses gix's commit-time cutoff so the
  walk terminates at the window instead of scanning to the root. No index is ever built.
  The CLI gained `search`.
- **Per-file history.** "Show File History" on tree rows and ⌥⌘H for the open file scope the
  History pane to the commits that changed one path, with a chip to clear. `path_history`
  walks like `log` and compares the entry at the path against every parent (clean merges hide,
  root commits count, renames are not followed), examining at most `max_scan` commits per call
  so pages stream in chunks and a scope change between chunks cancels the rest. Pages cache by
  `(tip, path)`. The CLI gained `filelog`.
- **Commit detail.** Selecting a commit loads `commit_details` alongside the tree diff: full
  message body, committer when it differs from the author, refs pointing at the commit
  (computed from the ref listing, no extra walk), and parent links. Parent links and revision
  jumps go through `BrowserModel.jumpToCommit` with a back stack. The CLI gained `show`.
- **Branch and tag management.** The first mutating actions. Create branch, create tag,
  delete branch, delete tag — each through `rc-git-guard`: the policy decides, destructive
  deletions demand the typed name, and a backup ref under `refs/rc-git/backup/` is written
  before anything is removed. The checked-out branch and protected branches cannot be
  deleted. Exposed over the FFI (`rcgit_guard_*`) and in the sidebar's context menu.
- **Changes tab.** A third pane (⌘3) with the full-repo change list (`worktree_status`, one
  capped background walk with ignored directories pruned unopened), per-file and per-hunk
  ticks, a worktree diff against `HEAD`, a commit box, and the stash list. Commit, stash
  push/apply/pop/drop, and discard all go through `rc-git-guard`; the stash is written in
  git's own format and proven against the real `git stash` in tests. Raw-byte semantics
  throughout: gitattributes filters and eol conversion are not honored. `WorktreeWatcher`
  refreshes the list when files change on disk while the tab is frontmost.
- **Multiple repositories in tabs.** Every repository opens in its own tab of one native
  window group (⌘T for an empty tab, "+" in the tab bar, ⇧⌘[ / ⇧⌘] to move between them).
  Each tab is a full `MainWindowController` — model, caches, and watchers stay alive while
  the tab is in the background — so switching tabs reorders windows and reads nothing.
  Opening a repository that is already open selects its existing tab. `OpenTabs` (RcGitKit)
  persists the tab list and selection through `UserDefaults` — saved on every open, close,
  and at quit — and a relaunch reopens them, silently skipping repositories that no longer
  open.

- **Fetch, pull, push, sync.** HTTPS only, pure gix, no process ever spawned: the one
  transport that needs no `ssh` or `git-upload-pack` child. gix fetches; push is a
  hand-rolled send-pack (gix has none) — handshake via `gix-protocol`, a client-side
  fast-forward check, a self-contained pack from `gix-pack`, `report-status` parsed by
  hand. Pull is fast-forward only and refuses dirty overlap; divergence reports
  ahead/behind and points at git. All four go through `rc-git-guard`
  (`actions/remote/`), are exposed over the FFI with typed auth/diverged status codes,
  and reach the app as a Repository menu plus a toolbar Sync button driven by
  `SyncModel` (RcGitKit). Credentials are per-host PATs in the login keychain, prompted
  on 401 and stored only once the server accepts them. SSH and `file://` remotes are
  refused with a message naming the scheme. The CLI gained `fetch`, `pull`, `push`,
  `sync` (token via `RC_GIT_TOKEN`, auth failures exit 5); tests run against a real
  `git http-backend` behind the test kit's loopback HTTP server.

## Planned

- **Reveal in tree from path search.** Jump-to-file opens the file; expanding the tree to
  show it selected is still missing.
- **Filters in the Changes tab.** Honoring gitattributes filters and `core.autocrlf` in
  status, diff, commit, and restore needs gix's `attributes` feature, which pulls in the
  process-spawning filter-driver machinery — a safety decision before a code change.

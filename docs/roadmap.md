# Roadmap

What has landed, and what is planned. Each item names the layer it touches so the
constraints are clear.

## Done

- **Ref change watching.** `RefWatcher` (RcGitKit) watches the `.git` directory with
  FSEvents, filters to `HEAD`, `packed-refs`, and `refs/` through `RefWatchFilter`, and
  drives `BrowserModel.refreshRefs()`. Every object cache stays; only refs re-read. The
  selection survives by name, reloads when its target moved, and falls back when deleted.
- **Commit list.** The History pane pages through a log cursor on scroll, caches pages per tip with
  the cursor, and offers "Browse Files at This Commit". Selecting a commit shows the files it
  changed against its first parent and each file's line diff.
- **Syntax highlighting.** Tree-sitter in `rc-git-highlight`, exposed as
  `rcgit_highlight`, cached by blob id in `BrowserModel` (LRU-bounded), applied after first
  paint so plain text never waits on a parse — and only for the visible chunk, as temporary
  attributes, so a large file never pays a whole-document pass on the main actor. Languages:
  bash, C, JavaScript, JSON, markdown, Python, Rust, Swift, TOML, TypeScript, TSX, YAML.
  Markdown highlights
  its inline prose and fenced code blocks in their own language (injections resolved by hand;
  `tree_sitter_highlight`'s own layer merge mis-orders events). Files that are not valid
  UTF-8 or exceed 2 MiB stay plain.
- **Import links.** In a JavaScript or TypeScript file, an import's specifier, the names it
  binds, and every JSX element named after one are links: the pointer underlines one and a
  click opens the file it names. `import_links` in `rc-git-highlight` finds them
  (`rcgit_import_links`), sixteen bytes each and no strings; `ImportResolver` (RcGitKit)
  turns a specifier into a path on click — relative paths, extensions and index files, and
  the `paths` and `baseUrl` of the `tsconfig.json` above the file, through `extends` and
  `references`. The parse waits for the first pointer move over the file. A package from a
  registry resolves to nothing, and so does a workspace package with no `paths` entry.
- **Markdown view.** `.md` files open as a rendered preview with a Preview/Source toggle in
  the file pane header; source mode keeps line numbers and syntax color. The parser is our
  own (`MarkdownDocument` in RcGitKit, pure and tested): headings both ways, lists including
  nesting and task items, fenced and indented code, blockquotes, GFM tables, front matter,
  footnotes, reference links, images, autolinks, raw HTML blocks, and the inline set. Fenced
  blocks are colored by the same tree-sitter highlighter source mode uses, and tables lay out
  as real `NSTextTable` cells. Links are live: a repository-relative one opens that file in
  the pane, `#fragment` scrolls to the heading, a URL goes to the browser, and the context
  menu copies either. Building the preview runs off the main actor. Markdown over 8 MiB opens
  in source mode only.
- **Path search.** ⇧⌘O flattens the selected root once in the background — the one
  feature that walks a whole tree — keyed by root tree id, cancellable, capped at 200 000
  entries, and offers fuzzy jump-to-file. Every directory read lands in the shared tree
  cache. In the working tree it walks disk instead (`worktree_files`), once per opening of
  the panel, so gitignored files such as `.env` are found; ignored folders the tree does not
  track are skipped unread.
- **Working tree as a pseudo-ref.** The sidebar's "Working Tree" row shows files on disk
  with a status overlay against `HEAD`: modified, untracked, ignored, deleted. Reads only;
  one directory and one tree object per expand, content hashed only when sizes match. A
  repository with a checkout opens on it; a bare one or an unborn `HEAD` opens on a branch.
- **Hidden files.** `HiddenFiles` (RcGitKit) names what the file lists hide by default: VS
  Code's `files.exclude` defaults plus macOS metadata (`.git`, `.DS_Store`, `._*`, …).
  Gitignored entries are not hidden; they stay visible and dimmed. View ▸ Show Hidden Files
  (⇧⌘.) reveals them app-wide and is remembered across launches. The tree filters rows it
  already read, so the toggle reads nothing; path search and the change list, which leaves
  out untracked hidden files so they never ride along in a commit, follow the same setting.
- **Branch banner.** Files and Changes show a banner at the top of the sidebar in place of
  the ref list, so the tree and the change list get its height. In Files it names what the
  tree browses (the working tree and its branch, a commit, or a ref) and opens the ref list
  in a popover under it on a click or ⌘B; arrows move, Return picks, Escape or a click outside
  closes. In History, ⌘B focuses the sidebar's ref filter instead. In Changes it names
  the checked-out branch and is read-only, since the change list always compares the working
  tree with `HEAD`. `BranchBanner` (RcGitKit, pure, tested) decides what either one says.
- **Drag a folder onto the window** to open it.
- **Blame and diff views.** Both read-only. Tree and blob diffs use `gix-diff`
  (`diff_trees`, `diff_blobs`); blame uses `gix-blame` on demand for the open file (⌥⌘B).
  The CLI gained `diff`, `blame`, and `status` verbs.
- **Commit graph.** A lane column in the History pane draws the topology of the paged
  commits: `GraphBuilder` (RcGitKit, pure, tested) assigns lanes strictly sequentially from
  `CommitSummary.parents`, so appending a page never reshuffles drawn rows, and the builder
  is cached per tip alongside the pages. Rendering clamps at 12 drawn lanes; the column
  hides while a path scope or search shows a filtered list. No core or FFI change.
- **Commit search.** A filter bar over the History pane (⇧⌘F) matches message or author as the user
  types, with optional date bounds; matches stream in scan chunks with a progress count, and a hex
  query jumps straight to that revision. Search pages through the same resumable walk as path
  history, `max_scan` commits per page; a since-bound uses gix's commit-time cutoff so the walk
  terminates at the window instead of scanning to the root. Under a path scope the search applies
  the path history's path-change test too. No index is ever built. The CLI gained `search`.
- **Per-file history.** "Show File History" on tree rows and ⌥⌘Y for the open file scope the History
  pane to the commits that changed one path, with a chip to clear. A history walk filtered by path
  compares the entry at the path against every parent (clean merges hide, root commits count,
  renames are not followed), examining at most `max_scan` commits per page, each page resuming where
  the last stopped, so pages stream in chunks and a scope change between chunks cancels the rest.
  Pages cache by `(tip, path)`. The CLI gained `filelog`.
- **Commit detail.** Selecting a commit loads `commit_details` alongside the tree diff: full
  message body, committer when it differs from the author, refs pointing at the commit
  (computed from the ref listing, no extra walk), and parent links. Parent links and revision
  jumps go through `BrowserModel.jumpToCommit`; the way back from one is the back/forward
  list. The CLI gained `show`.
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
  Opening a repository that is already open selects its existing tab. ⌘W closes the tab on
  screen, ⇧⌘W the whole window group. `OpenTabs` (RcGitKit) persists the tab list and
  selection through `UserDefaults` — recorded on every open, close, and at quit — and a
  relaunch reopens them. ⌘W drops the tab's repository from that list; ⇧⌘W and ⌘Q keep
  theirs, so the next launch comes back with them. The restore also drops the repositories
  that no longer open.
- **Back and forward.** ⌘[ and ⌘], the toolbar arrows, a two-finger swipe, and a mouse's
  fourth and fifth buttons step through the places you went: files opened, commits picked,
  History scoped to a path, the Files/History/Changes pane, the ref browsed, and the tab in
  front. One list covers every tab, so a step back from one repository can land in another.
  `BackForwardList` (RcGitKit, pure, tested) holds the stops; `BrowserModel.location` names
  where a tab is and `restore` returns to one, redoing only what differs. A stop names what
  it showed and reads it again on return: a ref that moved shows its new tip, a file that is
  gone leaves the pane empty. The list lives in memory, keeps 200 stops, and drops a tab's
  stops when the tab closes. The commit detail's back arrow is the same step, shown while
  it lands on another commit. No core or FFI change.

- **Fetch, pull, push, sync.** HTTPS and SSH, both in-process, no child ever spawned:
  HTTPS through gix's curl backend, SSH through russh running `git-upload-pack` /
  `git-receive-pack` on the server with the channel bridged to gix's blocking
  git-protocol `Connection`. gix fetches; push is a hand-rolled send-pack (gix has
  none) — handshake via `gix-protocol`, a client-side fast-forward check, a
  self-contained pack from `gix-pack`, `report-status` parsed by hand. Pull is
  fast-forward only and refuses dirty overlap; divergence reports ahead/behind and
  hands over `git pull --rebase` for the checkout (`ReconcileCommand`), copied or with
  Terminal opened there. All four go through `rc-git-guard` (`actions/remote/`, ssh in
  `actions/remote/ssh/`), are exposed over the FFI with typed
  auth/diverged/host-key status codes, and reach the app as a Repository menu plus a
  toolbar Sync button driven by `SyncModel` (RcGitKit). HTTPS credentials are per-host
  PATs in the login keychain, prompted on 401 and stored only once the server accepts
  them. SSH offers keys in OpenSSH's order — agent keys matching an `IdentityFile`, the
  other agent keys unless `IdentitiesOnly yes`, then the remaining `IdentityFile` keys
  (default keys only when none is configured); an encrypted key prompts for its
  passphrase (stored per host under the ssh keychain protocol). `known_hosts` is read, never written: an unknown host shows its
  fingerprint for trust-on-first-use (approvals persist app-side), a contradicting key
  is fatal with no in-app override. `git://` and `file://` remotes are refused with a
  message naming the scheme. The CLI's `fetch`, `pull`, `push`, `sync` take
  `--accept-hostkey` and `--ssh-dir` (token via `RC_GIT_TOKEN`, passphrase via
  `RC_GIT_KEY_PASSPHRASE`; auth failures exit 5, unverified host keys exit 6); tests
  run against a real `git http-backend` and a russh loopback SSH server in the test
  kit.

- **Back to the default branch.** One toolbar button and one Repository-menu item
  (⇧⌘M) switch `HEAD` to the repository's mainline branch, then fetch and fast-forward
  it: the "give me the latest master to work on" move. The mainline comes from
  `Repository::default_branch` — a remote's `HEAD`, else a local `master` or `main`. The
  switch is `rc-git-guard`'s `checkout_branch`, which reuses the pull's tree-apply path
  (`actions/treeapply.rs`): exactly the paths that differ are written, a dirty overlap
  refuses the whole switch by name, `HEAD` moves last, and its `checkout:` reflog line is
  appended by hand because a gix transaction writes none for a symbolic update.
  `SyncModel.backToDefaultBranch` composes the three steps and re-reads the upstream
  after the switch, since it is the new branch's remote the fetch must authenticate
  against. CLI: `rc-git checkout <repo> <branch>` and `rc-git default-branch <repo>`.

- **Two bundles and self-update.** `scripts/build-app.sh` assembles the release bundle
  (`/Applications/RCGit.app`, blue icon) or, with `--dev`, `RCGit-dev.app` in the checkout
  (orange icon, debug build). Different bundle identifiers keep their tabs, recents, window
  frames, and trusted host keys apart. Both bundles are stamped with the checkout,
  commit, and build date they came from; `AppBuild` (RcGitKit) reads the stamp and picks an
  `UpdateMode`. The release app's "Update rc-git" runs `UpdateModel` in `.update`:
  preflight the toolchain and a clean checkout, fetch and fast-forward through the guard's
  pull, run `scripts/install-app.sh`, relaunch. The dev bundle's "Rebuild and Relaunch"
  (⌘R) runs it in `.rebuild`: preflight, `scripts/build-app.sh --dev` on the working tree
  as it stands, relaunch — no fetch and no dirty check, so the edit-run loop never leaves
  the app. The steps reach the outside through `UpdateEnvironment`, so the state machine is
  tested without building anything. Tabs survive because the identifier does not change.
  Icons are drawn by `scripts/make-icon.swift` and cached in `build/icons/`.

## Planned

- **Reveal in tree from path search.** Jump-to-file opens the file; expanding the tree to
  show it selected is still missing.
- **Filters in the Changes tab.** Honoring gitattributes filters and `core.autocrlf` in
  status, diff, commit, and restore needs gix's `attributes` feature, which pulls in the
  process-spawning filter-driver machinery — a safety decision before a code change.
- **Force push.** `Action::Push { force: true }` is settled in the guard's policy (denied on
  a protected branch, the typed branch name elsewhere); the hand-rolled send-pack still
  refuses a non-fast-forward, so no write path exists yet.
- **Delete a remote branch.** `Action::DeleteRemoteBranch` is settled (denied on a protected
  branch, the typed branch name and a backup ref elsewhere); push has no delete command yet.
- **Reset --hard.** `Action::ResetHard` is settled (the typed branch name and a backup ref);
  the move would reuse `actions/treeapply.rs`.
- **Rewrite history (rebase, amend).** `Action::RewriteHistory` is settled (confirm and a
  backup ref when unpublished; denied on a protected branch, the typed branch name elsewhere,
  when published). Signing must be settled first: guard commits carry no signature.
- **Prune unreachable objects.** `Action::Prune` is settled (type `prune`); nothing deletes
  objects yet.

# Roadmap

What has landed, and what is planned. Each item names the layer it touches so the
constraints are clear.

## Done

- **Ref change watching.** rc-git-mac watches the `.git` directory with FSEvents, filters to
  `HEAD`, `packed-refs`, and `refs/` through `RefFilter` (rc-git-kit), and drives
  `BrowserModel::refresh_refs`. Every object cache stays; only refs re-read. The
  selection survives by name, reloads when its target moved, and falls back when deleted.
- **Commit list.** The History pane pages through a log cursor on scroll, caches pages per tip with
  the cursor, and offers "Browse Files at This Commit". Selecting a commit shows the files it
  changed against its first parent and each file's line diff.
- **Syntax highlighting.** Tree-sitter in `rc-git-highlight`, cached per blob and extension
  in `HighlightStore` (rc-git-kit, LRU-bounded), applied after first paint so plain text never
  waits on a parse — and only to the rows on screen, so a large file never pays a
  whole-document pass on the main thread.
  Languages:
  bash, C, JavaScript, JSON, markdown, Python, Rust, Swift, TOML, TypeScript, TSX, YAML.
  Markdown highlights
  its inline prose and fenced code blocks in their own language (injections resolved by hand;
  `tree_sitter_highlight`'s own layer merge mis-orders events). Files that are not valid
  UTF-8 or exceed 2 MiB stay plain.
- **Import links.** In a JavaScript or TypeScript file, an import's specifier, the names it
  binds, and every JSX element named after one are links: the pointer underlines one and a
  click opens the file it names. `import_links` in `rc-git-highlight` finds them, sixteen
  bytes each and no strings; `ImportResolver` (rc-git-kit)
  turns a specifier into a path on click — relative paths, extensions and index files, and
  the `paths` and `baseUrl` of the `tsconfig.json` above the file, through `extends` and
  `references`. The parse waits for the first pointer move over the file. A package from a
  registry resolves to nothing, and so does a workspace package with no `paths` entry.
- **Markdown view.** `.md` files open as a rendered preview with a Preview/Source toggle in
  the file pane header; source mode keeps line numbers and syntax color. The parser is our
  own (`MarkdownDocument` in rc-git-kit, pure and tested): headings both ways, lists including
  nesting and task items, fenced and indented code, blockquotes, GFM tables, front matter,
  footnotes, reference links, images, autolinks, raw HTML blocks, and the inline set. Fenced
  blocks are colored by the same tree-sitter highlighter source mode uses, and tables lay out
  as real grid rows. Links are live: a repository-relative one opens that file in
  the pane, `#fragment` scrolls to the heading, a URL goes to the browser, and the context
  menu copies either. Building the preview runs off the main thread. Markdown over 8 MiB opens
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
- **Hidden files.** `hidden_files` (rc-git-kit) names what the file lists hide by default: VS
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
  tree with `HEAD`. `BranchBanner` (rc-git-kit, pure, tested) decides what either one says.
- **Drag a folder onto the window** to open it.
- **Blame and diff views.** Both read-only. Tree and blob diffs use `gix-diff`
  (`diff_trees`, `diff_blobs`); blame uses `gix-blame` on demand for the open file (⌥⌘B).
  The CLI gained `diff`, `blame`, and `status` verbs.
- **Commit graph.** A lane column in the History pane draws the topology of the paged
  commits: `GraphBuilder` (rc-git-kit, pure, tested) assigns lanes strictly sequentially from
  `CommitSummary.parents`, so appending a page never reshuffles drawn rows, and the builder
  is cached per tip alongside the pages. Rendering clamps at 12 drawn lanes; the column
  hides while a path scope or search shows a filtered list. No core change.
- **Commit search.** A filter bar over the History pane (⇧⌘F) matches message or author as the user
  types, with optional date bounds; matches stream in scan chunks with a progress count, and a hex
  query jumps straight to that revision. Search pages through the same resumable walk as path
  history, `max_scan` commits per page; a since-bound uses gix's commit-time cutoff so the walk
  terminates at the window instead of scanning to the root. Under a path scope the search applies
  the path history's path-change test too. No index is ever built. The CLI gained `search`.
- **Find in repository.** In Files, ⇧⌘F opens a panel that finds a literal string in every
  file below the browsed root: blobs, or the files on disk while browsing the working tree.
  Smart case: a query with no uppercase letter ignores ASCII case. Return searches; hits land
  per batch, grouped by file with each line's excerpt, and Return on a line opens the file in
  source mode with the match selected. It reads the path-search file list, so nothing new walks
  the tree, then asks the core for one batch at a time (`search_blobs`,
  `search_worktree_files`), each capped at 8 MiB read and cancellable between batches. A blob
  shared by several paths is read once; files over 1 MiB and binary files are skipped, and the
  search stops at 2,000 matching lines. No index is built. The CLI gained `grep`.
- **Per-file history.** "Show File History" on tree rows and ⌥⌘Y for the open file scope the History
  pane to the commits that changed one path, with a chip to clear. A history walk filtered by path
  compares the entry at the path against every parent (clean merges hide, root commits count,
  renames are not followed), examining at most `max_scan` commits per page, each page resuming where
  the last stopped, so pages stream in chunks and a scope change between chunks cancels the rest.
  Pages cache by `(tip, path)`. The CLI gained `filelog`.
- **Commit detail.** Selecting a commit loads `commit_details` alongside the tree diff: full
  message body, committer when it differs from the author, refs pointing at the commit
  (computed from the ref listing, no extra walk), and parent links. Parent links and revision
  jumps go through `BrowserModel::jump_to_commit`; the way back from one is the back/forward
  list. The CLI gained `show`.
- **Branch and tag management.** The first mutating actions. Create branch, create tag,
  delete branch, delete tag — each through `rc-git-guard`: the policy decides, destructive
  deletions demand the typed name, and a backup ref under `refs/rc-git/backup/` is written
  before anything is removed. The checked-out branch and protected branches cannot be
  deleted. Reached from the sidebar's context menu.
- **Changes tab.** A third pane (⌘3) with the full-repo change list (`worktree_status`, one
  capped background walk with ignored directories pruned unopened), per-file and per-hunk
  ticks, a worktree diff against `HEAD`, a commit box, and the stash list. Commit, stash
  push/apply/pop/drop, and discard all go through `rc-git-guard`; the stash is written in
  git's own format and proven against the real `git stash` in tests. Raw-byte semantics
  throughout: gitattributes filters and eol conversion are not honored. The worktree watcher
  refreshes the list when files change on disk while the tab is frontmost.
- **Multiple repositories in tabs.** Every repository opens in its own tab of one native
  window group (⌘T for an empty tab, "+" in the tab bar, ⇧⌘[ / ⇧⌘] to move between them).
  Each tab is a full `RepoWindow` — model, caches, and watchers stay alive while
  the tab is in the background — so switching tabs reorders windows and reads nothing.
  Opening a repository that is already open selects its existing tab. ⌘W closes the tab on
  screen, ⇧⌘W the whole window group. `OpenTabs` (rc-git-kit) persists the tab list and
  selection in the app's preferences — recorded on every open, close, and at quit — and a
  relaunch reopens them. ⌘W drops the tab's repository from that list; ⇧⌘W and ⌘Q keep
  theirs, so the next launch comes back with them. The restore also drops the repositories
  that no longer open.
- **Back and forward.** ⌘[ and ⌘], the toolbar arrows, a two-finger swipe, and a mouse's
  fourth and fifth buttons step through the places you went: files opened, commits picked,
  History scoped to a path, the Files/History/Changes pane, the ref browsed, and the tab in
  front. One list covers every tab, so a step back from one repository can land in another.
  `BackForwardList` (rc-git-kit, pure, tested) holds the stops; `BrowserModel::location` names
  where a tab is and `restore` returns to one, redoing only what differs. A stop names what
  it showed and reads it again on return: a ref that moved shows its new tip, a file that is
  gone leaves the pane empty. The list lives in memory, keeps 200 stops, and drops a tab's
  stops when the tab closes. The commit detail's back arrow is the same step, shown while
  it lands on another commit. No core change.

- **Fetch, pull, push, sync.** HTTPS and SSH, both in-process, no child ever spawned: HTTPS through
  gix's curl backend, SSH through russh running `git-upload-pack` / `git-receive-pack` on the server
  with the channel bridged to gix's blocking git-protocol `Connection`. gix fetches; push is a
  hand-rolled send-pack (gix has none) — handshake via `gix-protocol`, a client-side fast-forward
  check, a self-contained pack from `gix-pack`, `report-status` parsed by hand. Pull is fast-forward
  only and refuses dirty overlap; divergence reports ahead/behind and hands over `git pull --rebase`
  for the checkout (`reconcile_command`), copied or with Terminal opened there. All four go through
  `rc-git-guard` (`actions/remote/`, ssh in `actions/remote/ssh/`), with typed
  auth/diverged/host-key failures, and reach the app as a Repository menu plus a toolbar Sync button
  driven by `SyncModel` (rc-git-kit). HTTPS credentials are per-host PATs in the login keychain,
  prompted on 401 and stored only once the server accepts them. SSH offers keys in OpenSSH's order —
  agent keys matching an `IdentityFile`, the other agent keys unless `IdentitiesOnly yes`, then the
  remaining `IdentityFile` keys (default keys only when none is configured); an encrypted key
  prompts for its passphrase (stored per host under the ssh keychain protocol). `known_hosts` is
  read, never written: an unknown host shows its fingerprint for trust-on-first-use (approvals
  persist app-side), a contradicting key is fatal with no in-app override. `git://` and `file://`
  remotes are refused with a message naming the scheme. The CLI's `fetch`, `pull`, `push`, `sync`
  take `--accept-hostkey` and `--ssh-dir` (token via `RC_GIT_TOKEN`, passphrase via
  `RC_GIT_KEY_PASSPHRASE`; auth failures exit 5, unverified host keys exit 6); tests run against a
  real `git http-backend` and a russh loopback SSH server in the test kit.

- **Back to the default branch.** One toolbar button and one Repository-menu item
  (⇧⌘M) switch `HEAD` to the repository's mainline branch, then fetch and fast-forward
  it: the "give me the latest master to work on" move. The mainline comes from
  `Repository::default_branch` — a remote's `HEAD`, else a local `master` or `main`. The
  switch is `rc-git-guard`'s `checkout_branch`, which reuses the pull's tree-apply path
  (`actions/treeapply.rs`): exactly the paths that differ are written, a dirty overlap
  refuses the whole switch by name, `HEAD` moves last, and its `checkout:` reflog line is
  appended by hand because a gix transaction writes none for a symbolic update.
  `SyncModel::back_to_default_branch` composes the three steps and re-reads the upstream
  after the switch, since it is the new branch's remote the fetch must authenticate
  against. CLI: `rc-git checkout <repo> <branch>` and `rc-git default-branch <repo>`.

- **GPUI app and self-rebuild.** The app is GPUI on Rust end to end: rc-git-app draws,
  rc-git-kit holds the models, rc-git-mac binds macOS. `scripts/build-app.sh` assembles one
  bundle, `RCGit.app`, in the checkout (`/Applications/RCGit.app` links to it), stamped with
  the checkout, commit, and build date it came from; `AppBuild` (rc-git-kit) reads the stamp.
  "Rebuild and Relaunch" (⌥⌘R; the menu item opens the window first) runs `UpdateModel`:
  preflight, `scripts/build-app.sh` on the working tree as it stands, relaunch — no fetch and
  no dirty check, so the edit-run loop never leaves the app. The steps reach the outside
  through `UpdateEnvironment`, so the state machine is tested without building anything. Tabs
  survive because the identifier does not change. The icon is drawn by
  `scripts/make-icon.swift` and cached in `build/icons/`.

- **GitHub sign-in.** Settings ▸ GitHub Accounts and `rc-git auth login|status|logout` sign
  in to github.com or a GitHub Enterprise host through a GitHub App or OAuth App device flow, or
  a pasted token, and keep the token in the login keychain. The client
  (`rc-git-guard` `actions::remote::github`) sends a token only to the account's own host,
  https only, no redirects, bounded and cancellable; `docs/safety.md`, "GitHub API". The first
  of eight pull request slices: the reads, the PR diff, merge, admin merge, auto-merge and
  re-runs, job logs and branch deletion follow.

- **Pull request reads.** The sidebar's Pull Requests section lists the open pull requests of
  the remote fetch uses, with each head commit's check state; a row opens a read-only tab with
  the header, the checks (required first) and the description, which opens only web links on a
  click. The list polls every 60 s and a tab with a running check every 15 s, only while shown in
  the active window. `rc-git pr list|view|checks` reads the same. An ssh alias reaches its
  account through the `HostName` the user's ssh config maps it to. The second slice.

- **Pull request changes.** A pull request tab's Changes view lists the files it changes since
  it left its base (the merge base with the commit GitHub reports its base branch at; the local
  copy of the base branch only for a host that reports none, and it says so) and diffs the one
  picked, from local objects. A head that is not local is fetched only on Fetch Changes, into
  `refs/rc-git/pr/<n>/head`, and only when the remote's head is the one the tab read; Stop stays
  on screen while it runs. `rc-git pr diff <n> [--fetch]` and `rc-git merge-base` do the same
  from Terminal. The third slice.

- **Pull request merge.** A pull request tab's Merge control (the repository's allowed
  methods, squash first) and `rc-git pr merge <n> --method m --head <oid> --base <branch> --yes`
  merge through the guard: a fresh read compared with the stamp and the answered challenge (one
  labelled line per fact, the title last), the blockers (draft, conflicts, a blocked, behind or
  unknown merge state, a disallowed method), one `mergePullRequest` carrying `expectedHeadOid`,
  never resent, and a three-valued outcome a lost answer settles with one read; a merge GitHub
  says landed on another base is a warning of its own. The fourth slice.

- **Admin merge.** When branch protection blocks a pull request and GitHub says the account may
  merge past it (`viewerCanMergeAsAdmin`), its tab shows a separate Merge Without Waiting for
  Requirements (Admin) control, and `rc-git pr merge <n> --admin --confirm <n> --plan <p>` does
  the same (the fingerprint of the plan a run without `--confirm` printed):
  the challenge lists what the fresh read shows it skips (failed and unfinished required checks,
  a missing review, an out-of-date head) and says other rules may be skipped too, and only the
  pull request's number typed confirms it. The same `mergePullRequest`, sent once. The fifth
  slice.

- **Auto-merge and re-runs.** A pull request tab turns GitHub's auto-merge on (Enable
  Auto-Merge, with a method) for a pull request branch protection blocks, shows who turned it on
  and how, and turns it off (Disable Auto-Merge); `rc-git pr auto-merge <n> --enable --method m`
  or `--disable` does the same. Re-run Failed Jobs and `rc-git pr rerun <n>` ask GitHub to re-run
  the failed jobs of the head's GitHub Actions runs, one REST request per run (at most ten),
  each once. Each goes through the guard like the merge: a fresh read against the stamp, a
  challenge (the auto-merge one says later commits merge too, without asking again), the
  answered decision checked, and an outcome of done, refused or unknown. The sixth slice.

- **Job logs.** View Log on a finished GitHub Actions check of a pull request tab, and
  `rc-git pr logs <n> --check <name> | --job <id>`, show that job's log: matched first to the
  head on screen (job read: `head_sha`, `run_id`), then downloaded from the address GitHub's
  redirect names, one hop, `https://` to a public DNS name only, without the token, at most its
  last 2 MiB (its first 2 MiB from one more request when the storage host refuses the range),
  neutralized, held in memory. A re-run now reads each workflow run first and sends only those
  that ran on the confirmed head. The seventh slice.

- **Delete a merged pull request's branch.** Delete Branch on a merged pull request's tab, and
  `rc-git pr delete-branch <n> --head <oid> --base <branch> --confirm <name>`, delete its head
  branch on GitHub with one `deleteRef`, decided by the delete remote branch row's rule (denied
  on a protected branch, the typed name and a backup ref elsewhere). Only a branch of the base
  repository, at the commit the pull request merged, not the default branch, used by no other
  open pull request, and whose commit is in the local repository: the backup ref
  `refs/rc-git/backup/remote-branch/<name>` is written and read back before the request, and the
  note says how to push it back. The eighth and last pull request slice.

## Planned

- **Prune old pull request heads.** Every fetched pull request head stays under
  `refs/rc-git/pr/<n>/head`, keeping its objects alive, until the user deletes it with git. A
  prune of the refs of closed and merged pull requests (a policy row of its own, since it lets
  `git gc` drop their objects) is still missing.

- **Reveal in tree from path search.** Jump-to-file opens the file; expanding the tree to
  show it selected is still missing.
- **Filters in the Changes tab.** Honoring gitattributes filters and `core.autocrlf` in
  status, diff, commit, and restore needs gix's `attributes` feature, which pulls in the
  process-spawning filter-driver machinery — a safety decision before a code change.
- **Force push.** `Action::Push { force: true }` is settled in the guard's policy (denied on
  a protected branch, the typed branch name elsewhere); the hand-rolled send-pack still
  refuses a non-fast-forward, so no write path exists yet.
- **Delete a remote branch.** `Action::DeleteRemoteBranch` is settled (denied on a protected
  branch, the typed branch name and a backup ref elsewhere); push has no delete command yet. A
  merged pull request's head branch can already be deleted through GitHub's API under the same
  rule (`Action::DeletePullRequestBranch`, "Delete a merged pull request's branch" above); a
  branch on any other remote, or one no merged pull request names, still cannot.
- **Reset --hard.** `Action::ResetHard` is settled (the typed branch name and a backup ref);
  the move would reuse `actions/treeapply.rs`.
- **Rewrite history (rebase, amend).** `Action::RewriteHistory` is settled (confirm and a
  backup ref when unpublished; denied on a protected branch, the typed branch name elsewhere,
  when published). Signing must be settled first: guard commits carry no signature.
- **Prune unreachable objects.** `Action::Prune` is settled (type `prune`); nothing deletes
  objects yet.

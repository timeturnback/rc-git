# Architecture

rc-git is layered, each layer with one job. Data moves up as immutable values keyed by object
id; nothing moves down except requests.

```
rc-git-app        GPUI views. Forward clicks to the models, render their events.
rc-git-kit        BrowserModel (state, generations, cache), layout policy, value types,
                  and the platform traits the app needs from macOS. No widgets.
rc-git-mac        Live macOS implementations of those traits. The only `unsafe`.
  rc-git-guard      Mutation policy and the one write and network path (git remotes and the
                    GitHub API). Reads through core.
  rc-git-highlight  tree-sitter highlight spans and import links. Pure.
  rc-git-core       gix wrapper. Repository pool, refs, trees, blobs, log, revision parsing,
                    diff, blame, worktree status, stash listing.
rc-git-cli        Beside the app crates, on core, guard and highlight; on kit and mac for
                  `auth` and `pr`, which share the app's GitHub accounts and keychain items.
```

`rc-git-cli` sits beside the app crates on top of the core, the guard and the highlighter, and gives
scripts and agents the app's reads, its guarded mutations and its highlight and import-link passes.
Its `auth` verbs also use rc-git-kit's account settings and token keeper and rc-git-mac's keychain,
so the CLI and the app sign in to GitHub once between them.
`rc-git-guard` holds the mutation policy and, in its `actions` module, the one write and network
path: every mutation — refs, objects, the index, working-tree files, the stash, checkout, and fetch,
pull and push — goes through it; `safety.md` has the rules. Its actions are branch and tag create
and delete, commit, the stash lifecycle (push, apply, pop, drop), discard of files or hunks, the
branch switch, fetch, fast-forward pull, and push; sync is those last three run in sequence by the
app, not an action of its own. Network I/O exists only in the `actions::remote` module; `safety.md`,
"Guarding mutations", says what each action checks before it writes. The GitHub API client lives
there too, in `actions::remote::github`: sign-in (GitHub App and OAuth App device flow, a pasted
token), the token check, GitHub App token refresh, and the authenticated REST and GraphQL calls
the pull request features build on, every URL built from the account's host; `safety.md`,
"GitHub API", has its rules. rc-git-kit's `github` module holds the account records and the
per-repository choice (`accounts`), the sign-in state machine (`sign_in`) and the token keeper
that serializes refreshes (`tokens`); rc-git-mac keeps the tokens in the login keychain
(`KeychainApiTokenStore`), and Settings and `rc-git auth` drive them. The pull request reads
(`list_open_pull_requests`, `read_pull_request`, and `pull_request_remote` for the remote fetch
uses) answer in the guard's own types, text already neutralized; the stamp of a read
(`PullRequestStamp`: number, head oid, base ref, state) is what later mutations compare a fresh
read against. A pull request's changes come from local objects: `fetch_pull_request_head` (the
guard's one pull request write) fetches `refs/pull/<n>/head` into `refs/rc-git/pr/<n>/head` when
the user asks and the remote's head is the stamp's, and the core's `merge_base` (a bounded
paint-down walk) plus `diff_trees` give GitHub's three-dot view from the commit GitHub reports
the base branch at (`baseRefOid`), or, for a host that reports none, from the local copy of the
base branch (`pull_request_base_ref`). The guard's one GitHub mutation so far, merging a pull
request (`merge_pull_request`, planned by `plan_merge_pull_request`), reads the pull request again,
refuses what changed since the stamp or cannot merge (`merge_blocker`), and sends one
`mergePullRequest` through `Session::mutate`, which takes the `Confirmation` and builds the
request from the action it carries (`mutation`); the outcome is merged, refused or unknown.
rc-git-kit's `pull_requests` module links a repository to its
GitHub repository and account (`link`), holds the list and each open pull request tab as state
machines with a generation per read and one read queued at most (`state`), a tab's changes as a
state machine tied to the stamp they were read for (`changes`: reading, needs fetch, fetching,
moved, no base copy, no merge base, ready, one file diff at a time), a tab's merge as a state
machine tied to its stamp and generation (`merge`: planning, confirming, merging until and after
the send, done) with the Merge control decided from the guard's blockers, runs them for a window
(`PullRequestsModel`, `model`) through the `PullRequestService` seam (`live` over the guard and
the core, a fake in tests), and decides polling (`polling`), check order and summary (`checks`),
which description links open (`body`) and the words (`copy`). The app's sidebar section and pull
request page draw that model, the page's Changes view reusing the shared `DiffView`, and the
window shows a merge's challenge through the shared challenge sheet when the model asks
(`MergeEvent`); `rc-git pr` reads, fetches and merges through the same live service. `rc-git-highlight` is a pure
tree-sitter wrapper the file pane calls through rc-git-kit's `HighlightStore`: highlight spans, and
the import links of a JavaScript or TypeScript file.

## The change list and sync

`ChangesModel` (rc-git-kit) holds the Changes tab: the flat change list, one file's worktree diff,
the stash list and the tick state that becomes a commit or stash selection. It only reads.
`WorktreeFilter` (`platform::watch`) decides which filesystem events can change the list and drops
the rest; what remains marks the list stale. Scans coalesce: a request made while one runs marks it
pending and returns, and the running scan re-reads once when it lands, so a burst of events costs
one scan in flight plus one queued.

Writes go the other way. `SyncModel` (rc-git-kit) runs the sync workflow — preflight, fetch,
fast-forward pull, push — through `RepositoryActions` (`repository_actions`), which calls
`rc_git_guard::actions`; each network step carries a `CancelToken` around the guard's
`Cancel`, the only way to stop a thread blocked inside a transfer. The window runs the other
guarded actions the same way and refreshes the change list afterwards.

## Opening a repository

`Repository::open` calls `gix::ThreadSafeRepository::open_opts` with
`Options::isolated()`, which reads only the repository's own `config`. It then builds a small
pool of `gix::Repository` handles, one per core up to four, each with a 32 MiB object cache
and a 32 MiB pack delta-base cache, so an open repository holds at most 4 × (32 + 32) = 256 MiB
in them. Both are set on every handle after the open, so the repository's own
`gitoxide.objects.cacheLimit` and `core.deltaBaseCacheLimit` cannot raise or remove them.
Every operation borrows a free handle through `Repository::with`; when all are busy a temporary
handle is built so no call waits on another. A handle is single-threaded; the pool is what
makes `Repository` `Send + Sync`.

Opening costs a few reads: `HEAD`, `config`, the pack index headers. Packs are memory-mapped
on demand.

## Selecting a ref

`BrowserModel::attach` loads `refs()` and `head()` off the main thread. A repository with a
working directory and a born `HEAD` opens on the working tree through `select_working_tree`;
otherwise `attach` picks the branch `HEAD` is on and calls `select`. `select` resolves the
ref's target to a tree id with `tree_of` and publishes the root tree. That is the whole cost of
switching branches: one object read. Cached subtrees stay valid because they are keyed by
tree id, not by branch.

## Expanding a folder

The tree view asks `BrowserModel::children` for the folder's tree id. `TreeStore` answers
from its map when it can; otherwise `tree_entries` decodes exactly that one tree object,
sorts it for display (directories first, names ASCII-case-insensitive), stores it, and returns.
Nothing below that folder is touched.

## Opening a file

`open_file` bumps the file generation, publishes the loading state, and calls `read_blob` with
a size limit. The core reads the object header first; anything above the limit returns
`TooLarge`
without decoding. Otherwise the blob is inflated, checked for a NUL in its first 8000 bytes
(git's own binary rule), and, when text, indexed with `memchr` for line starts and validated
as UTF-8 — once, here, so no display layer re-scans the bytes. The result is applied only if
the generation still matches.

Still off the main thread, the bytes decode as UTF-8 with replacement into a `TextDocument`
carrying byte line starts. The file viewer is a `uniform_list`: it shapes a line's row only
while that row is on screen, and draws line numbers from the precomputed starts. Syntax colors
arrive later from `HighlightStore` and apply to the rows on screen. Markdown files can render
as a styled preview instead, toggled from the file pane header: `MarkdownDocument`
(rc-git-kit) parses the source into flat blocks and styled runs, and `Rendered::build`
(rc-git-app) colors their fenced code and indexes their anchors. Both halves are pure, so the
preview is built off the main thread like the plain paint is. `MarkdownLinkTarget` resolves
what a link means — a URL, a repository path, or a heading — and is what the pane, its
context menu, and `BrowserModel::open_path` all agree on.

Source mode has links of its own in JavaScript and TypeScript. The first pointer move over a
file asks `HighlightStore::import_links` for them; `ImportLinkMap` answers which one the
pointer is on, and the viewer underlines it as an overlay on that row. A click hands the
specifier to `BrowserModel::open_import`, where `ImportResolver` reads the directories and
config files it needs off the main thread and `open_path` opens what it found.

## Going back

For every move a tab makes, `BrowserModel` emits `BrowserEvent::LocationChanged` once.
`location`
names the tree browsed (the working tree, a ref by name, or one commit), the open file's
path, the history scope, and the commit in the detail. A move that changes several of those
reports after the last one — a ref switch also closes the open file — so the step in between
is never recorded. The window (`RepoWindow`) adds the repository path and the pane to make a
stop and emits it, and the app's `Shell` records it in one `BackForwardList` for every tab,
from the tab in front only.

Stepping back selects the stop's tab and calls `BrowserModel::restore`, which compares the
stop with where the browser is and redoes only what differs: between two files of one tree
that is one blob read. Restores coalesce the way the change list's scans do. One asked for
while another runs replaces the pending location and returns; the running one stops at its
next step and goes on to it. What a tab reports during a restore is not recorded.

## Threading

- `BrowserModel` is a GPUI entity on the main thread. All state changes and events happen
  there.
- Repository calls, keychain reads and transfers run through `blocking::blocking`, which runs
  each on its own thread and hands the value back to the awaiting task. A blocking call on
  GPUI's background pool holds one of its threads, one per core, and enough of them starve
  every background task. `Repository` is `Send + Sync` through the handle pool.
- Every load carries a generation number captured before the await and compared after.

## Caching

| Cache | Key | Where | Invalidated |
|---|---|---|---|
| decoded objects | object id | gix, per pooled handle | never; LRU past 32 MiB per handle, whatever the repository's config says |
| pack delta bases | object id | gix, per pooled handle | never; LRU past 32 MiB per handle, whatever the repository's config says |
| directory listings | tree id | `TreeStore` in rc-git-kit | on `attach`; least recently used evicted past 100,000 children |
| history pages | tip id | `BrowserModel` | on `attach`; 4 most recent tips kept |
| path-scoped history pages | tip id and path | `BrowserModel` | on `attach`; 4 most recent kept |
| flattened file lists (path search) | root tree id | `BrowserModel` | on `attach`; 2 most recent roots kept |
| syntax spans | blob id | `HighlightStore`, owned by `BrowserModel` | on `attach`; least recently used evicted past 16 files or 1,500,000 spans |
| import links | blob id | `HighlightStore` | on `attach`, and with the blob's spans |

Refs are not cached across calls; `refs()` re-reads `packed-refs` and loose refs each time and
is cheap enough to call on every change. rc-git-mac's FSEvents watcher watches the `.git`
directory; `RefFilter` (`platform::watch`, rc-git-kit) keeps only events on `HEAD`,
`packed-refs`, `config` and `refs/`, and each burst calls `BrowserModel::refresh_refs`, which
re-reads refs and keeps every object cache.

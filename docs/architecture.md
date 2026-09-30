# Architecture

rc-git is four layers, each with one job. Data moves up as immutable values keyed by object
id; nothing moves down except requests.

```
RcGitApp      AppKit views. Forward clicks to the model, render its callbacks.
RcGitKit      BrowserModel (state, generations, cache), LayoutPolicy, value types.
rc-git-ffi    C ABI. Marshals values across; owns nothing after the call returns.
rc-git-core   gix wrapper. Repository pool, refs, trees, blobs, log, revision parsing.
```

`rc-git-cli` sits beside the Swift package on top of the core and exposes the same operations
to scripts. `rc-git-guard` holds the mutation policy and, in its `actions` module, the one
write path: branch and tag management, commits built from working-tree selections, the stash
lifecycle, and discards; `safety.md` has the rules. `rc-git-highlight` is a pure tree-sitter
wrapper the FFI exposes for the file pane: highlight spans, and the import links of a
JavaScript or TypeScript file.

## Opening a repository

`Repository::open` calls `gix::ThreadSafeRepository::open_opts` with
`Options::isolated()`, which reads only the repository's own `config`. It then builds a small
pool of `gix::Repository` handles, one per core up to four, each with a 32 MiB object cache.
Every operation borrows a free handle through `Repository::with`; when all are busy a temporary
handle is built so no call waits on another. A handle is single-threaded; the pool is what
makes `Repository` `Send + Sync`.

Opening costs a few reads: `HEAD`, `config`, the pack index headers. Packs are memory-mapped
on demand.

## Selecting a ref

`BrowserModel.attach` loads `refs()` and `head()` off the main actor. A repository with a
working directory and a born `HEAD` opens on the working tree through `selectWorkingTree`;
otherwise `attach` picks the branch `HEAD` is on and calls `select`. `select` resolves the
ref's target to a tree id with `treeOf` and publishes `rootTree`. That is the whole cost of
switching branches: one object read. Cached subtrees stay valid because they are keyed by
tree id, not by branch.

## Expanding a folder

The tree view asks `BrowserModel.children(of:)` for the folder's tree id. `TreeStore` answers
from its dictionary when it can; otherwise `treeEntries` decodes exactly that one tree object,
sorts it for display (directories first, names ASCII-case-insensitive), stores it, and returns.
Nothing below that folder is touched.

## Opening a file

`openFile` bumps the file generation, publishes `.loading`, and calls `readBlob` with a size
limit. The core reads the object header first; anything above the limit returns `tooLarge`
without decoding. Otherwise the blob is inflated, checked for a NUL in its first 8000 bytes
(git's own binary rule), and, when text, indexed with `memchr` for line starts and validated
as UTF-8 — once, here, so no display layer re-scans the bytes. The result is applied only if
the generation still matches.

Still off the main actor, the bytes decode as UTF-8 with replacement into a `TextDocument`
carrying UTF-16 line starts. The file pane builds the attributed string in the background
too, sets it on a TextKit 1 text storage with non-contiguous layout, and draws line numbers
for visible fragments from the precomputed starts. Syntax colors arrive later as temporary
attributes for the visible chunk only; scrolling colors further chunks. Markdown files can
render as a styled preview instead, toggled from the file pane header: `MarkdownDocument`
(RcGitKit) parses the source into flat blocks and styled runs, and `MarkdownRenderer`
(RcGitApp) turns those into the attributed string, wrapped, with the ruler hidden. Both
halves are pure, so the preview is built off the main actor like the plain paint is.
`MarkdownLinkTarget` resolves what a link means — a URL, a repository path, or a heading —
and is what the pane, its context menu, and `BrowserModel.openPath` all agree on.

Source mode has links of its own in JavaScript and TypeScript. The first pointer move over a
file asks `BrowserModel.importLinks` for them; `ImportLinkMap` answers which one the pointer
is on, and `LinkTextView` underlines it without touching the text storage. A click hands the
specifier to `BrowserModel.openImport`, where `ImportResolver` reads the directories and
config files it needs off the main actor and `openPath` opens what it found.

## Threading

- `BrowserModel` is `@MainActor`. All state changes and callbacks happen there.
- Repository calls run in `Task.detached` closures. `Repository` is `@unchecked Sendable`; the
  Rust side guarantees thread safety through the handle pool.
- Every load carries a generation number captured before the await and compared after.

## Caching

| Cache | Key | Where | Invalidated |
|---|---|---|---|
| decoded objects | object id | gix, per pooled handle | never (LRU eviction only) |
| pack delta bases | object id | gix, per pooled handle | never |
| directory listings | tree id | `TreeStore` in RcGitKit | on `attach` |
| row symbols | symbol name | `Icons` in RcGitApp | never |

Refs are not cached across calls; `refs()` re-reads `packed-refs` and loose refs each time and
is cheap enough to call on every change. Watching `.git` for ref changes is on the roadmap.

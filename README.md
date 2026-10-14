# rc-git

A fast git browser for macOS. Open a repository, pick a branch, tag, or commit, walk the
tree, read files, page through history, diff, blame, and see the working tree's status. It
runs no `git` process for repository operations: a Rust core built on
[gitoxide](https://github.com/GitoxideLabs/gitoxide) reads the object database directly and a
native AppKit window shows the result. Reading never writes a byte; every mutation — refs,
the index, the working tree, or a remote — passes through a policy that decides what you must
confirm first.

## Build

```sh
cargo install cbindgen --version 0.29.4 --locked
brew install cargo-deny
scripts/install-app.sh    # builds and installs /Applications/RCGit.app
open -a RCGit ~/code/some-repo
```

Requires macOS 15 or newer, `rustup` (the Rust version is pinned in `rust-toolchain.toml`),
and Xcode 27 or newer.

Afterwards the app updates itself: **Update rc-git** in the app menu fetches and
fast-forwards the checkout it was built from, rebuilds, reinstalls, and relaunches into the
new version with the same tabs open. `scripts/build-app.sh --dev` builds a separate
`RCGit-dev.app` — its own identifier, its own tabs, an orange icon — for trying changes
without disturbing the installed app.

## Use

Open a repository with ⌘O, from the Open Recent menu, by dragging its folder onto the window,
by passing a path on the command line, or with `open -a RCGit <path>`. The ref list holds the
working tree, branches, remote branches, and tags; selecting one shows its tree, and the app
follows along when refs change on disk. History shows the list in the sidebar. Files puts a
banner there instead, naming what you browse: click it or press ⌘B to pick from the list in a
popover. In Changes the banner names the checked-out branch. Folders load when expanded, files
open with syntax highlighting when selected, ⌘F searches the file, ⇧⌘O jumps to a file by
fuzzy name. In JavaScript and TypeScript, click an import or a component to open its file.
⌘[ and ⌘] step back and forward through the files, commits, panes, and tabs you went
through, across repositories; the toolbar arrows, a two-finger swipe, and a mouse's side
buttons do the same.

The Files/History/Changes toggle (⌘1/⌘2/⌘3) switches panes. History pages the commit list
as you scroll, and selecting a commit shows the files it changed with a line diff;
double-click to browse the tree at that commit. ⌥⌘B blames the open file. The Working Tree
row overlays each file's status against `HEAD` — modified, untracked, ignored, deleted.
Right-click a ref to create or delete branches and tags: deleting an unmerged branch, a
remote branch, or a tag takes the typed name and writes a backup under `refs/rc-git/backup/`
first; a merged branch takes a plain confirm.

The window adapts to its size: narrow windows fold the sidebar away. The file always
sits right of the tree, and the tree widens only as far as its rows need, so the file pane
takes the rest of the window. Drag the divider and that width is yours from then on. Toggle
the sidebar with ⌃⌘S.

The same core ships as a command-line tool for scripts and for measuring:

```sh
cargo run --release -p rc-git-cli -- --time ls ~/code/some-repo main src
cargo run --release -p rc-git-cli -- cat ~/code/some-repo v1.2 README.md
cargo run --release -p rc-git-cli -- log ~/code/some-repo -n 10
cargo run --release -p rc-git-cli -- diff ~/code/some-repo v1.1 v1.2 src/main.rs
cargo run --release -p rc-git-cli -- blame ~/code/some-repo main src/main.rs
cargo run --release -p rc-git-cli -- status ~/code/some-repo
```

## Safety

The core cannot write. The workspace's `clippy.toml` rejects the gix and std write, spawn and
network entry points it lists at lint time, a test compares `.git` byte for byte before and after every
operation, and repositories open in isolated mode so an untrusted checkout cannot pull in
configuration from anywhere else. Every mutation — refs, the index, the working tree, or a
remote — passes through `rc-git-guard`, a policy that denies deletions on protected branches
outright and decides, per action, whether it runs, needs a confirm, or needs a typed phrase
and a backup ref. Deleting a merged branch takes a plain confirm and writes no backup;
discarding working changes takes a plain confirm and cannot be undone; dropping a stash takes
the word `drop` and writes a backup ref. `docs/safety.md` has the full table.

## Layout

```
crates/rc-git-core       read-only gix wrapper, keyed by object id
crates/rc-git-guard      policy for destructive actions, and the one write path
crates/rc-git-highlight  tree-sitter syntax highlighting and import links
crates/rc-git-ffi        C ABI for the Swift app
crates/rc-git-cli        rc-git binary
crates/rc-git-testkit    fixture repositories for tests
mac/                     Swift package: RcGitKit (logic, tested) and RcGitApp (AppKit)
docs/                    architecture, performance, safety, ffi, testing, development, roadmap
```

`scripts/check.sh` runs the checks CI runs; CI also assembles the app with `scripts/build-app.sh`.

# rc-git

Native macOS git browser built for speed: open a repository, pick a branch or tag, walk the
tree, read files, page history, diff, blame, browse the working tree. A Rust core on
`gitoxide` reads the object database directly; a GPUI app renders it. The core spawns no
process (no `git` process for repository operations) and never writes; every write — refs,
objects, the index, working-tree files, stash, fetch/pull/push — goes through `rc-git-guard`,
and so does every GitHub API call (sign-in, token refresh, pull requests), which sends a token
only to the host the user signed into. The in-app rebuild runs `scripts/build-app.sh`, which calls `git` on rc-git's own checkout,
never on a browsed repository.

Two rules define the project. A change that breaks either is wrong even when it works:

- **The core never writes.** `clippy.toml` bans the gix and std write, spawn and network entry
  points it lists; `crates/rc-git-core/tests/read_only.rs` proves `.git` is byte-identical
  after every operation. The one write path is `rc-git-guard`'s `actions` module (see
  `.claude/rules/guard.md`); every mutation goes through it. Say so rather than bypassing it.
- **Nothing scales with repository size except what is on screen.** One tree object per folder
  expand, one blob per file view, a header read before any decode, one log page at a time.
  `docs/performance.md` has the budgets and the lazy rules.

## Commands

```sh
scripts/check.sh                  # CI's checks minus build-app.sh
cargo test --workspace            # Rust tests; fixtures are built with the git CLI
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/build-app.sh [--profile release]  # assemble RCGit.app in the checkout, ad-hoc signed
cargo run -p rc-git-cli -- --time ls <repo> [rev] [path]   # CLI and benchmark tool
```

Needs `rustup` (Rust is pinned in `rust-toolchain.toml`),
`brew install cargo-deny prek shellcheck shfmt actionlint zizmor`, and Xcode 27 or newer.
`docs/development.md` has the rest.

## Layers

| Path | Contains | Tested |
|---|---|---|
| `crates/rc-git-core` | gix wrapper: open, refs, trees, blobs, log, diff, blame, worktree. Keyed by `Oid`. No gix types in the public API. | yes |
| `crates/rc-git-guard` | policy for every mutating action, and the one write and network path (`docs/safety.md`, "Guarding mutations"). | yes |
| `crates/rc-git-highlight` | tree-sitter highlighting and import links: bytes in, spans out. Pure. | yes |
| `crates/rc-git-cli` | `rc-git` binary: scriptable access plus `--time`. | yes |
| `crates/rc-git-testkit` | fixture repositories built with the git CLI, and the fake git and GitHub servers. The only crate that runs `git`. | — |
| `crates/rc-git-kit` | app state and pure text logic: `BrowserModel` and the other models, layout policy, markdown, search, link and import resolution, platform traits. **No widgets.** | yes |
| `crates/rc-git-mac` | live macOS impls of the kit's platform traits: CFPreferences, keychain, FSEvents, process runner, bundle info, window tabs, update environment. The only crate allowed `unsafe`. | yes |
| `crates/rc-git-app` | GPUI windows, views, menus and key bindings. Marshals events to the kit models, nothing else. | yes |

New logic goes in `rc-git-core` when it reads git data and in `rc-git-kit` when it is app
state or pure text logic the views need decided once. `rc-git-app` only wires views to the
models; a macOS system call goes in `rc-git-mac` behind a kit trait.

## Working agreement

- Never launch the app to verify a change. Build, lint, and tests are the scope; the user runs
  `RCGit.app` and reports what breaks. Reproduce and measure with the CLI instead.
- `RCGit.app`'s `CFBundleIdentifier` scopes `UserDefaults`, so it is what carries tabs through
  a rebuild — never make it depend on a version or build number.
- The app rebuilds itself by running `scripts/build-app.sh`, so that script assembles into
  `build/stage/` and deletes the old bundle last. Never delete it before the build: the
  running app is the process running the script.
- Ask before adding a dependency. Versions are pinned exactly and pass `cargo deny`.
- `.claude/rules/` loads the rule for the area you touch. Read it before editing.
- `docs/` is the long-form reference: `architecture.md`, `performance.md`, `safety.md`,
  `testing.md`, `development.md`, `roadmap.md`.
- `docs/solutions/` holds documented solutions to past problems (YAML frontmatter: `module`,
  `tags`, `problem_type`), organized by category — relevant when debugging in a documented area.
- `CONCEPTS.md` defines the words that mean something specific here (guard, challenge, dirty
  overlap, isolated open) — read it before naming a new concept, and use its terms in prose.
- Every public Rust fn documents `# Errors`; every error names what failed and the thing it
  failed on (path, spec, oid, or op plus path/oid).

## Changelog

- `CHANGELOG.md` is what Help ▸ Changelog shows. A change a user would notice is a `feat` or
  `fix` commit and adds an entry. A change no user notices takes another type (`refactor`,
  `test`, `docs`, `build`, `chore`), since every `feat` and `fix` must carry an entry.
- Put the entry under today's `## YYYY-MM-DD` heading (newest first; add the heading if
  missing), in `### Added`, `### Changed` or `### Fixed`, in that order.
- Write for the person using the app: what they can do now or what stopped going wrong, with
  menu names and shortcuts. Never paste the commit subject.
- The entry lands in the same commit as the change. `scripts/changelog-check.sh` refuses a
  `feat`/`fix` commit that leaves the file alone (pre-push hook and CI), and the
  `rc-git-kit` test `the_shipped_changelog_keeps_its_shape` fails on a malformed file.

## Invariants

- Repositories open with `gix::open::Options::isolated()`: only the repository's own config is
  read, no global or system config, no environment, no `include` directives.
- Every cache keys on `Oid`. Only refs are re-read when the repository changes.
- Every async load in `BrowserModel` (rc-git-kit) carries a generation number; a stale result
  is dropped.
- Layout follows `rc_git_kit::layout_policy` and nothing else. Change thresholds there, with
  tests.

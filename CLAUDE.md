# rc-git

Native macOS git browser built for speed: open a repository, pick a branch or tag, walk the
tree, read files, page history, diff, blame, browse the working tree. A Rust core on
`gitoxide` reads the object database directly; a thin AppKit app renders it. No `git`
process is ever spawned; nothing is written except guard-approved ref edits.

Two rules define the project. A change that breaks either is wrong even when it works:

- **The core never writes.** `clippy.toml` bans every gix write API and every filesystem
  mutation; `crates/rc-git-core/tests/read_only.rs` proves `.git` is byte-identical after every
  operation. The one write path is `rc-git-guard`'s `actions` module (see
  `.claude/rules/guard.md`); every mutation goes through it. Say so rather than bypassing it.
- **Nothing scales with repository size except what is on screen.** One tree object per folder
  expand, one blob per file view, a header read before any decode, one log page at a time.
  `docs/performance.md` has the budgets and the lazy rules.

## Commands

```sh
scripts/check.sh                  # the full gate CI runs; --fast skips the Swift half
cargo test --workspace            # Rust tests; fixtures are built with the git CLI
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/build.sh                  # release Rust lib + header + swift build
swift test --package-path mac     # RcGitKit tests; run scripts/build.sh once first
scripts/build-app.sh [--dev]      # assemble RCGit.app, or RCGit-dev.app, ad-hoc signed
scripts/install-app.sh            # build the release bundle into /Applications
cargo run -p rc-git-cli -- --time ls <repo> [rev] [path]   # CLI and benchmark tool
```

Needs `rustup` stable, `brew install cbindgen cargo-deny prek shellcheck shfmt actionlint zizmor`,
and Xcode 16 or newer. `docs/development.md` has the rest.

## Layers

| Path | Contains | Tested |
|---|---|---|
| `crates/rc-git-core` | gix wrapper: open, refs, trees, blobs, log, diff, blame, worktree. Keyed by `Oid`. No gix types in the public API. | yes |
| `crates/rc-git-guard` | policy for every mutating action, and the one write path (branch/tag management, backup refs). | yes |
| `crates/rc-git-highlight` | tree-sitter highlighting: bytes in, spans out. Pure. | yes |
| `crates/rc-git-ffi` | C ABI over core, guard, and highlight. The only crate allowed `unsafe`. | yes |
| `crates/rc-git-cli` | `rc-git` binary: scriptable access plus `--time`. | yes |
| `crates/rc-git-testkit` | fixture repositories built with the git CLI. The only crate that shells out. | — |
| `mac/Sources/RcGitKit` | Swift wrapper, `BrowserModel`, layout policy. **No AppKit.** | yes |
| `mac/Sources/RcGitApp` | AppKit windows and views. Marshals events to the model, nothing else. | no |

New logic goes in `rc-git-core` when it reads git data and in `RcGitKit` when it is app state.
`RcGitApp` only wires views to the model. The FFI layer only marshals; `docs/ffi.md` has the
checklist for adding a function, and `scripts/gen-header.sh --verify` fails when the committed
header drifts.

## Working agreement

- Never launch the app to verify a change. Build, lint, and tests are the scope; the user runs
  `RCGit.app` and reports what breaks. Reproduce and measure with the CLI instead.
- The release and dev bundles differ only by `CFBundleIdentifier`, name, icon, and build config.
  That identifier scopes `UserDefaults`, so it is what keeps their tabs apart and what carries
  tabs through a self-update — never make it depend on a version or build number.
- Both bundles rebuild themselves by running `scripts/build-app.sh`, so that script assembles
  into `build/stage/` and deletes the old bundle last. Never delete it before the build: the
  dev bundle is the process running the script.
- Ask before adding a dependency on either side. Rust versions are pinned exactly and pass
  `cargo deny`; the Swift package has none.
- `.claude/rules/` loads the rule for the area you touch. Read it before editing.
- `docs/` is the long-form reference: `architecture.md`, `performance.md`, `safety.md`,
  `ffi.md`, `testing.md`, `development.md`, `roadmap.md`.
- `docs/solutions/` holds documented solutions to past problems (YAML frontmatter: `module`,
  `tags`, `problem_type`), organized by category — relevant when debugging in a documented area.
- `CONCEPTS.md` defines the words that mean something specific here (guard, challenge, dirty
  overlap, isolated open) — read it before naming a new concept, and use its terms in prose.
- Every public Rust fn documents `# Errors`; every error names the operation and the object.

## Invariants

- Repositories open with `gix::open::Options::isolated()`: only the repository's own config is
  read, no global or system config, no environment, no `include` directives.
- Every cache keys on `Oid`. Only refs are re-read when the repository changes.
- Every async load in `BrowserModel` carries a generation number; a stale result is dropped.
- FFI: the caller owns every returned pointer and releases it with the matching `_free`;
  errors arrive through `char **error` and are released with `rcgit_string_free`.
- Layout follows `LayoutPolicy` (RcGitKit) and nothing else. Change thresholds there, with tests.

# Development

## Setup

```sh
rustup toolchain install   # the version pinned in rust-toolchain.toml
cargo install cbindgen --version 0.29.4 --locked   # brew cannot pin it
brew install cargo-deny prek shellcheck shfmt actionlint zizmor
xcode-select --install     # or Xcode 27+; swift-format ships with it
scripts/build.sh           # Rust lib, header, Swift package
```

Git hooks run through the global `core.hooksPath` dispatcher, which invokes `prek` whenever a
repository has `.pre-commit-config.yaml`; with it there is no per-repository install step.
Without a global dispatcher, run `prek install` once per clone; never run it where a global
`core.hooksPath` already owns the hook slot (prek refuses there anyway).

## Daily loop

```sh
cargo test --workspace                      # after Rust changes
scripts/gen-header.sh                       # after changing rc-git-ffi
swift test --package-path mac               # after Swift changes
scripts/check.sh --fast                     # everything except the Swift half
scripts/check.sh                            # CI's checks; CI also runs build-app.sh
```

`rust-toolchain.toml` pins Rust (with clippy and rustfmt), and `scripts/gen-header.sh` and CI
pin cbindgen 0.29.4, so a new release cannot change the gate's verdict on its own. When another
cbindgen produces a different header, `gen-header.sh` names both versions and leaves the header
alone. Bump either on purpose, in its own commit, and fix what the new lints or header formatting
report in the same change.

`swift build` and `swift test` link `target/release/librc_git_ffi.a`; rebuild it with
`cargo build --release -p rc-git-ffi` whenever the Rust side changes.

## Running the app

There are two bundles, and they are meant to run side by side.

```sh
scripts/install-app.sh             # release: builds, installs /Applications/RCGit.app
scripts/build-app.sh --dev         # dev: builds RCGit-dev.app in the checkout
open RCGit-dev.app                 # or: open -a RCGit ~/code/repo for the release app
mac/.build/debug/RcGitApp ~/code/repo      # without a bundle
```

Build the dev bundle from the terminal once; after that ⌘R inside it rebuilds and relaunches
without losing tabs (see below).

| | release | dev |
|---|---|---|
| Bundle | `/Applications/RCGit.app` | `<checkout>/RCGit-dev.app` |
| Identifier | `com.timeturnback.rcgit` | `com.timeturnback.rcgit.dev` |
| Name | rc-git | rc-git (dev) |
| Icon | blue commit graph | orange, striped |
| Swift config | release | debug |

The identifier is what separates them: tabs, recents, window frames, and trusted ssh host
key fingerprints live in `UserDefaults` under it, so testing in dev never disturbs the tabs
the release app has open. Keychain items are per host, not per app, so both ask for a
remote's secret the first time each new code identity uses it.

Icons come from `scripts/make-icon.swift` — a commit-graph mark drawn with Core Graphics —
cached in `build/icons/` and regenerated whenever that script is newer.

Agents do not launch the app; see the working agreement in `CLAUDE.md`. Reproduce with the
CLI: `cargo run -p rc-git-cli -- --time ls <repo>`.

## Rebuilding from inside the app

Both bundles are stamped with the checkout and commit they were built from, and both have
an app-menu item that rebuilds themselves from there. They differ in what they build.

**Release — "Update rc-git":**

1. **Preflight.** The login shell must find `cargo`, `swift`, and `cbindgen`, and the
   checkout must have no uncommitted tracked changes. The log names each half before it
   runs, and the toolchain probe is killed after 20 seconds: a login shell that hangs
   fails the update with what to run in a terminal instead of spinning forever.
2. **Fetch and fast-forward** through `rc-git-guard`, exactly like the Changes tab's Pull —
   same credential prompts, same refusal when the branch has diverged.
3. **Build and install** by running `scripts/install-app.sh`, with its output streaming into
   the window. Nothing is skipped: it is the same script you would run by hand.
4. **Relaunch.** Tabs are saved, the new bundle is launched, and this one quits. The bundle
   identifier does not change, so the new instance reopens the same tabs.

When the checkout is already at the commit the running bundle was built from, the window
says so and the button becomes "Rebuild Anyway" rather than spending minutes on a rebuild.

**Dev — "Rebuild and Relaunch" (⌘R):** preflight, then `scripts/build-app.sh --dev`, then
relaunch. No fetch, no network, no credentials, and no dirty check — it builds the working
tree exactly as it stands, because putting work in progress into the running app is the
point. It never reports "up to date": the tree moves without the commit. The log names the
uncommitted files going in.

That is the edit-run loop: edit, ⌘R, look. Tabs come back on the other side because they
live in `UserDefaults` under a bundle identifier the rebuild does not change — though only
the open repositories and the selected tab are restored, not what each tab was showing.

`scripts/build-app.sh` assembles into `build/stage/` and swaps at the end. The dev bundle
runs that script to replace itself, and a bundle deleted out from under a live process
loses every resource it has not already loaded.

The app is ad-hoc signed, so every rebuild is a new code identity and the login keychain
re-prompts for each remote host's stored token ("Always Allow" makes it once per rebuild).
A stable signing identity in `scripts/build-app.sh` is the real fix; until then the prompt
churn is expected during development.

## Lint configuration

| File | Governs |
|---|---|
| `Cargo.toml` `[workspace.lints]` | rustc and clippy levels for all crates but rc-git-ffi and rc-git-testkit, whose hand copies `scripts/check.sh` diffs against it |
| `clippy.toml` | banned write APIs, test allowances |
| `rustfmt.toml` | 100 columns, 2024 edition style |
| `deny.toml` | advisories, license allowlist, sources |
| `.swift-format` | 100 columns, 4-space indent, ordered imports |
| `.editorconfig` | 2-space shell and YAML, so `shfmt` agrees with the editor |
| `.pre-commit-config.yaml` | what runs before each commit |

## Continuous integration

`.github/workflows/ci.yml` runs four jobs: Rust (fmt, clippy, test, doc, header verify),
cargo deny, macOS (Rust library, swift-format, build, test, app bundle), and scripts
(shellcheck, shfmt, actionlint, zizmor). Actions are pinned to commit hashes; Dependabot bumps
them and Cargo dependencies weekly.

## Versions

Rust crates share `workspace.package.version`; the FFI reports it through `rcgit_version`.

The app's version needs no bump. `mac/Info.plist` carries the major and minor of
`CFBundleShortVersionString`; `scripts/build-app.sh` replaces the patch with the commit
count, so `0.1.0` in the template ships as `0.1.42` and every commit on master moves it.
Edit the template only to start a new series. The window titlebar shows that version beside
its title, with a `+` when the checkout was dirty at build time.

`scripts/build-app.sh` stamps the rest into the bundle's `Info.plist`: `CFBundleVersion`
(the same commit count, as the monotonic build number), `RCGitVariant`, `RCGitSourceRoot`,
`RCGitSourceCommit`, `RCGitSourceDirty`, and `RCGitBuildDate`. `AppBuild` (RcGitKit) reads
them; a bundle with no stamp reports itself as a dev build with no checkout to rebuild from,
so it gets no menu item at all.

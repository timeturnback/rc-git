# Development

## Setup

```sh
rustup toolchain install   # the version pinned in rust-toolchain.toml
brew install cargo-deny prek shellcheck shfmt actionlint zizmor
xcode-select --install     # or Xcode 27+ (what CI uses); make-icon.swift needs swift
scripts/build-app.sh       # builds RCGit.app in the checkout
```

Git hooks run through the global `core.hooksPath` dispatcher, which invokes `prek` whenever a
repository has `.pre-commit-config.yaml`; with it there is no per-repository install step.
Without a global dispatcher, run `prek install` once per clone; never run it where a global
`core.hooksPath` already owns the hook slot (prek refuses there anyway).

## Daily loop

```sh
cargo test --workspace                      # after any change
scripts/check.sh                            # CI's checks; CI also runs build-app.sh
```

`rust-toolchain.toml` pins Rust (with clippy and rustfmt), so a new release cannot change the
gate's verdict on its own. Bump it on purpose, in its own commit, and fix what the new lints
report in the same change.

## Running the app

There is one bundle, `RCGit.app`, built into the checkout.

```sh
scripts/build-app.sh                       # app-dev profile: release minus fat LTO
scripts/build-app.sh --profile release     # fat LTO
ln -s "$PWD/RCGit.app" /Applications/RCGit.app   # once, so Spotlight and open -a find it
open -a RCGit ~/code/repo
cargo run -p rc-git-app -- ~/code/repo     # without a bundle; no self-update
```

| | |
|---|---|
| Bundle | `<checkout>/RCGit.app`, symlinked from `/Applications/RCGit.app` |
| Identifier | `com.timeturnback.rcgit` |
| Name | rc-git |
| Icon | blue commit graph |

Tabs, recents, window frames, and trusted ssh host key fingerprints live in the preferences
domain of that identifier. Keychain items are per host, not per app, so a new code identity
asks for a remote's secret the first time it uses it.

The icon comes from `scripts/make-icon.swift` — a commit-graph mark drawn with Core Graphics —
cached in `build/icons/` and regenerated whenever that script is newer.

Agents do not launch the app; see the working agreement in `CLAUDE.md`. Reproduce with the
CLI: `cargo run -p rc-git-cli -- --time ls <repo>`.

## Rebuilding from inside the app

The bundle is stamped with the checkout and commit it was built from, and the app menu's
**Rebuild and Relaunch…** rebuilds it from there (it opens the window first; ⌥⌘R, "Rebuild and
Relaunch Now", starts it without the button):

1. **Preflight.** The login shell must find `cargo`, and the checkout must be readable. The
   log names each half before it runs, and the toolchain probe is killed after 20 seconds: a
   login shell that hangs fails the rebuild with what to run in a terminal instead of spinning
   forever.
2. **Build** by running `scripts/build-app.sh` with no arguments, its output streaming into the
   window. It builds the working tree exactly as it stands — no fetch, no network, no
   credentials, no dirty check — because putting work in progress into the running app is the
   point. The log names the branch and the tracked files with uncommitted changes; new
   untracked files are built too but not listed. Pull the checkout yourself to update.
3. **Relaunch.** Tabs are saved, the new bundle is launched, and this one quits. The bundle
   identifier does not change, so the new instance reopens the same tabs — though only the
   open repositories and the selected tab are restored, not what each tab was showing.

That is the edit-run loop: edit, ⌥⌘R, look.

`scripts/build-app.sh` assembles into `build/stage/` and swaps at the end. The running app
runs that script to replace itself, and a bundle deleted out from under a live process loses
every resource it has not already loaded.

The app is ad-hoc signed, so every rebuild is a new code identity and the login keychain
re-prompts for each remote host's stored token ("Always Allow" makes it once per rebuild).
A stable signing identity in `scripts/build-app.sh` is the real fix; until then the prompt
churn is expected during development.

## Changelog

`CHANGELOG.md` at the root lists user-visible changes, newest first: one `## YYYY-MM-DD`
heading per day with `### Added`, `### Changed` and `### Fixed` under it. Help ▸ Changelog shows
it in a read-only window that draws it with the file pane's markdown preview.

`rc_git_kit::changelog` embeds the file with `include_str!`, so the app shows the changelog of
the build it is, `cargo run` included, and `scripts/build-app.sh` copies nothing. Cargo
rebuilds when the file changes. An uncommitted edit to it makes the checkout dirty like any
other tracked file, so the build is stamped with a `+` and never passes for the commit it
names. The same module's `problems` checks the file's shape, and a kit test runs it on the
shipped file.

`scripts/changelog-check.sh` refuses a `feat` or `fix` commit that leaves `CHANGELOG.md`
alone; commits from before the file existed are skipped. With no argument it checks the commits
no remote-tracking ref has, which is what the pre-push hook runs. CI runs it on the pushed or
pull-request range, and `scripts/tests/changelog-check.test.sh` (run by `scripts/check.sh`)
tests it against scratch repositories. Amend the entry into the offending commit, or add it in
a fixup and rebase.

## Lint configuration

| File | Governs |
|---|---|
| `Cargo.toml` `[workspace.lints]` | rustc and clippy levels for all crates but rc-git-mac and rc-git-testkit, whose hand copies `scripts/check.sh` diffs against it |
| `clippy.toml` | banned write APIs, test allowances |
| `rustfmt.toml` | 100 columns, 2024 edition style |
| `deny.toml` | advisories, license allowlist, sources |
| `.editorconfig` | 2-space shell and YAML, so `shfmt` agrees with the editor |
| `.pre-commit-config.yaml` | what runs before each commit, and the changelog check before each push |

## Continuous integration

`.github/workflows/ci.yml` runs four jobs: Rust on Linux (fmt, clippy, core standalone, test,
doc, all but the GPUI crates), cargo deny, macOS (`cargo test --workspace`, clippy and doc for
rc-git-kit, rc-git-app and rc-git-mac, then `scripts/build-app.sh`), and scripts (shellcheck,
shfmt, actionlint, zizmor, the changelog check). Actions are pinned to commit hashes; Dependabot bumps
them and Cargo dependencies weekly.

## Versions

Rust crates share `workspace.package.version`.

The app's version needs no bump. `crates/rc-git-app/Info.plist` carries the major and minor of
`CFBundleShortVersionString`; `scripts/build-app.sh` replaces the patch with the commit
count, so `0.1.0` in the template ships as `0.1.42` and every commit on master moves it.
Edit the template only to start a new series. The window titlebar shows that version beside
its title, with a `+` when the checkout was dirty at build time.

`scripts/build-app.sh` stamps the rest into the bundle's `Info.plist`: `CFBundleVersion`
(the same commit count, as the monotonic build number), `RCGitSourceRoot`,
`RCGitSourceCommit`, `RCGitSourceDirty`, and `RCGitBuildDate`. `AppBuild` (rc-git-kit) reads
them; a bundle with no stamp has no checkout to rebuild from, so its rebuild items are
disabled. `scripts/check.sh` fails when the keys it stamps and the ones `AppBuild` reads drift,
and when the macOS floor in `.cargo/config.toml` and `Info.plist` disagree.

# Development

## Setup

```sh
rustup toolchain install stable
brew install cbindgen cargo-deny prek shellcheck shfmt actionlint zizmor
xcode-select --install     # or Xcode 16+; swift-format ships with it
scripts/build.sh           # Rust lib, header, Swift package
```

Git hooks run through the global `core.hooksPath` dispatcher, which invokes `prek` whenever a
repository has `.pre-commit-config.yaml`; there is no per-repository install step.

## Daily loop

```sh
cargo test --workspace                      # after Rust changes
scripts/gen-header.sh                       # after changing rc-git-ffi
swift test --package-path mac               # after Swift changes
scripts/check.sh --fast                     # everything except the Swift half
scripts/check.sh                            # the whole gate, same as CI
```

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

## Updating the release app

The release bundle is stamped with the checkout and commit it was built from, and
"Update rc-git" in the app menu rebuilds it from there:

1. **Preflight.** The login shell must find `cargo`, `swift`, and `cbindgen`, and the
   checkout must have no uncommitted tracked changes.
2. **Fetch and fast-forward** through `rc-git-guard`, exactly like the Changes tab's Pull —
   same credential prompts, same refusal when the branch has diverged.
3. **Build and install** by running `scripts/install-app.sh`, with its output streaming into
   the window. Nothing is skipped: it is the same script you would run by hand.
4. **Relaunch.** Tabs are saved, the new bundle is launched, and this one quits. The bundle
   identifier does not change, so the new instance reopens the same tabs.

When the checkout is already at the commit the running bundle was built from, the window
says so and the button becomes "Rebuild Anyway" rather than spending minutes on a rebuild.
The dev bundle has no such menu item — it is rebuilt from the terminal.

The app is ad-hoc signed, so every rebuild is a new code identity and the login keychain
re-prompts for each remote host's stored token ("Always Allow" makes it once per rebuild).
A stable signing identity in `scripts/build-app.sh` is the real fix; until then the prompt
churn is expected during development.

## Lint configuration

| File | Governs |
|---|---|
| `Cargo.toml` `[workspace.lints]` | rustc and clippy levels for every crate |
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
`mac/Info.plist` carries the app's `CFBundleShortVersionString`. Bump both together.

`scripts/build-app.sh` stamps the rest into the bundle's `Info.plist`: `CFBundleVersion`
(the commit count), `RCGitVariant`, `RCGitSourceRoot`, `RCGitSourceCommit`,
`RCGitSourceDirty`, and `RCGitBuildDate`. `AppBuild` (RcGitKit) reads them; a bundle with no
stamp reports itself as a dev build that cannot self-update.

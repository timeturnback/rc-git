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

```sh
scripts/build-app.sh
open RCGit.app                     # or: open -a RCGit ~/code/repo
mac/.build/release/RcGitApp ~/code/repo    # without a bundle
```

Agents do not launch the app; see the working agreement in `CLAUDE.md`. Reproduce with the
CLI: `cargo run -p rc-git-cli -- --time ls <repo>`.

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

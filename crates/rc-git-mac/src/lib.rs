//! Live macOS implementations of the `rc-git-kit` platform traits: settings on
//! `CFPreferences`, credentials in the login keychain, ref and worktree watchers on
//! `FSEvents`, and the bounded process runner.
//!
//! The frameworks are bound with `extern "C"` blocks against the system frameworks alone, no
//! binding crates. This is the one crate allowed `unsafe`: every block says why it holds, and
//! every `CoreFoundation` object it creates is owned by [`cf::Owned`], which releases it once.

pub mod cf;
pub mod fsevents;
pub mod keychain;
pub mod process;
pub mod settings;
pub mod update;

pub use fsevents::{FsEventsWatcher, FsEventsWatchers};
pub use keychain::KeychainCredentialStore;
pub use process::ShellRunner;
pub use settings::Preferences;
pub use update::{LiveUpdateEnvironment, TabSession};

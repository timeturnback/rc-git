//! The platform seams the models stand on, each a trait with an in-memory implementation here
//! and a live one in `rc-git-mac`: settings (the successor to `UserDefaults`), credentials
//! (the login keychain), ssh host-key approvals, ref and worktree watchers (`FSEvents`), and
//! a bounded process runner. The pure halves of the Swift platform files (watch filters, line
//! splitting, every persisted key and its shape) live here and are tested here.

pub mod api_tokens;
pub mod credentials;
pub mod host_keys;
pub mod keys;
pub mod process;
pub mod settings;
pub mod stores;
pub mod watch;

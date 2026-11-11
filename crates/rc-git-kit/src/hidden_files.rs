//! Port of `HiddenFiles.swift` (the names and the filter; the show-hidden preference lives in
//! the settings store): entries the file lists hide until the user asks to see hidden files.
//! Version-control metadata and operating-system clutter, matched by name at any depth. The
//! names follow VS Code's `files.exclude` defaults plus the metadata macOS scatters on disks.
//! Gitignored files are deliberately absent: `.env` or `node_modules` stay visible, dimmed as
//! ignored.

/// Names hidden wherever they appear.
pub const NAMES: [&str; 9] = [
    ".git",
    ".svn",
    ".hg",
    "CVS",
    ".DS_Store",
    "Thumbs.db",
    ".Spotlight-V100",
    ".Trashes",
    ".fseventsd",
];

/// `AppleDouble` files: the `._name` resource-fork shadows macOS writes on foreign volumes.
pub const APPLE_DOUBLE_PREFIX: &[u8] = b"._";

/// The key the settings store keeps the show-hidden preference under (off until set).
pub const PREFERENCE_KEY: &str = "rc-git.showHiddenFiles";

/// Whether one file or folder name is hidden. Names are bytes, as git stores them.
#[must_use]
pub fn is_hidden_name(name: &[u8]) -> bool {
    name.starts_with(APPLE_DOUBLE_PREFIX) || NAMES.iter().any(|hidden| hidden.as_bytes() == name)
}

/// Whether any component of a `/`-separated path is hidden, so everything inside a hidden
/// folder is hidden with it. Splits on the `/` byte, as git does.
#[must_use]
pub fn is_hidden_path(path: &[u8]) -> bool {
    path.split(|byte| *byte == b'/').any(is_hidden_name)
}

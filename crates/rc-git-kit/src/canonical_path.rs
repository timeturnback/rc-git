//! Port of `CanonicalPath.swift`: one spelling per directory on disk, the one `FSEvents`
//! reports — absolute, symlinks resolved, `/private` kept. `NSString.standardizingPath` and
//! `resolvingSymlinksInPath` both strip `/private`; `realpath` (here
//! [`std::fs::canonicalize`]) keeps it, which is why it is the one used.

use std::path::{Component, Path, PathBuf};

use rc_git_core::Repository;

/// The realpath of `path`. A relative path resolves against the working directory first, and
/// `.` and `..` are removed lexically, so `.` never comes back even when `path` does not
/// exist; a path that does not exist is returned in that absolute form.
#[must_use]
pub fn resolve(path: &Path) -> PathBuf {
    let absolute = lexically_absolute(path);
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

/// `path` made absolute against the working directory (left relative when that cannot be
/// read) with `.` dropped and each `..` removing the component before it.
fn lexically_absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(dir) => dir.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    let mut normal = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normal.push(component);
            }
        }
    }
    normal
}

/// What tells two open repositories apart: the realpath of this worktree's git directory. A
/// working directory and its `.git`, any symlinked or `..` spelling of either, and `/var`
/// against `/private/var` all give the same key; each linked worktree has its own.
#[must_use]
pub fn tab_key(repository: &Repository) -> PathBuf {
    resolve(repository.git_dir())
}

/// The path a tab shows and saves for `repository`, opened through `opened_as`: the
/// canonical working directory, so opening `<repo>/.git` titles the tab `<repo>` and recents
/// and saved tabs hold the working directory. A bare repository keeps the canonical
/// `opened_as`.
#[must_use]
pub fn tab_path(repository: &Repository, opened_as: &Path) -> PathBuf {
    match repository.work_dir() {
        Some(work_dir) => resolve(work_dir),
        None => resolve(opened_as),
    }
}

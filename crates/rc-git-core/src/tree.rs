//! Directory listings.

use std::borrow::Cow;

use gix::bstr::{BString, ByteSlice};
use gix::objs::tree::EntryKind as GixEntryKind;

use crate::error::{Result, object, typed_object};
use crate::oid::Oid;
use crate::repo::Repository;

/// What a tree entry points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A directory.
    Tree,
    /// A regular file.
    Blob,
    /// A file with the executable bit.
    ExecutableBlob,
    /// A symbolic link; the blob holds the link target.
    Symlink,
    /// A submodule; the id is a commit in another repository.
    Submodule,
}

impl EntryKind {
    pub(crate) fn from_gix(kind: GixEntryKind) -> Self {
        match kind {
            GixEntryKind::Tree => Self::Tree,
            GixEntryKind::Blob => Self::Blob,
            GixEntryKind::BlobExecutable => Self::ExecutableBlob,
            GixEntryKind::Link => Self::Symlink,
            GixEntryKind::Commit => Self::Submodule,
        }
    }
}

/// One child of a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// File name as stored, which git does not require to be UTF-8.
    pub name: BString,
    /// Id of the child object.
    pub oid: Oid,
    /// What the child is.
    pub kind: EntryKind,
}

impl TreeEntry {
    /// The name for display; invalid UTF-8 becomes U+FFFD.
    #[must_use]
    pub fn name_lossy(&self) -> Cow<'_, str> {
        self.name.to_str_lossy()
    }
}

impl Repository {
    /// Lists the direct children of `tree`, ordered for display by [`sort_for_display`].
    ///
    /// Reads one tree object, never the subtrees, so expanding a folder costs the same in a
    /// ten-file repository and a hundred-thousand-file one.
    ///
    /// # Errors
    /// [`Error::WrongKind`](crate::Error::WrongKind) when `tree` is not a tree,
    /// [`Error::Object`](crate::Error::Object) when it is missing or malformed.
    pub fn tree_entries(&self, tree: Oid) -> Result<Vec<TreeEntry>> {
        self.with(|repo| {
            let mut entries = tree_children(repo, tree)?;
            sort_for_display(&mut entries);
            Ok(entries)
        })
    }

    /// Finds the entry at a `/`-separated `path` below `root`. Empty and `.` segments are
    /// ignored, so `a//b`, `./a/b` and `a/b/` all name `a/b`. `None` when any segment is missing
    /// or a segment before the last is a file or a submodule.
    ///
    /// # Errors
    /// [`Error::WrongKind`](crate::Error::WrongKind) when `root` is not a tree,
    /// [`Error::Object`](crate::Error::Object) when a tree on the path cannot be read.
    pub fn entry_at_path(&self, root: Oid, path: &str) -> Result<Option<TreeEntry>> {
        self.with(|repo| {
            let segments = path
                .split('/')
                .filter(|segment| !segment.is_empty() && *segment != ".")
                .map(str::as_bytes);
            let found = lookup_path(repo, root, segments)?;
            Ok(found.map(|entry| TreeEntry {
                name: entry.filename,
                oid: Oid::from_gix(entry.oid),
                kind: EntryKind::from_gix(entry.mode.kind()),
            }))
        })
    }
}

/// The entry at `segments` below the tree `root`, one tree read per segment. `None` when a
/// segment is missing or an earlier segment is not a directory, so a path through a file or
/// a submodule reads as absent, as it does in git. A gitlink's oid names a commit in another
/// repository, so it is never looked up here. No segments at all also yields `None`.
pub(crate) fn lookup_path<'a>(
    repo: &gix::Repository,
    root: Oid,
    segments: impl IntoIterator<Item = &'a [u8]>,
) -> Result<Option<gix::objs::tree::Entry>> {
    let mut tree = repo
        .find_tree(*root.as_gix())
        .map_err(typed_object("read tree", root))?;
    let mut segments = segments.into_iter().peekable();
    while let Some(segment) = segments.next() {
        let Some(entry) = tree.find_entry(segment) else {
            return Ok(None);
        };
        let (mode, oid) = (entry.mode(), entry.object_id());
        if segments.peek().is_none() {
            let filename = entry.filename().to_owned();
            return Ok(Some(gix::objs::tree::Entry {
                mode,
                filename,
                oid,
            }));
        }
        if !mode.is_tree() {
            return Ok(None);
        }
        tree = repo
            .find_tree(oid)
            .map_err(typed_object("read tree", Oid::from_gix(oid)))?;
    }
    Ok(None)
}

/// The direct children of `tree` in git's own order, read on an already-borrowed handle so
/// callers holding one for a longer walk do not re-enter the pool.
pub(crate) fn tree_children(repo: &gix::Repository, tree: Oid) -> Result<Vec<TreeEntry>> {
    let tree_obj = repo
        .find_tree(*tree.as_gix())
        .map_err(typed_object("read tree", tree))?;
    let mut entries = Vec::new();
    for entry in tree_obj.iter() {
        let entry = entry.map_err(object("decode tree", tree))?;
        entries.push(TreeEntry {
            name: entry.filename().to_owned(),
            oid: Oid::from_gix(entry.object_id()),
            kind: EntryKind::from_gix(entry.mode().kind()),
        });
    }
    Ok(entries)
}

/// Orders entries the way a file browser shows them: directories first, then names compared
/// ASCII-case-insensitively. Ties keep git's own order, so the result is deterministic.
pub fn sort_for_display(entries: &mut [TreeEntry]) {
    entries.sort_by_cached_key(|entry| {
        (
            entry.kind != EntryKind::Tree,
            entry.name.to_ascii_lowercase(),
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind) -> TreeEntry {
        let oid = Oid::from_hex("0123456789abcdef0123456789abcdef01234567").unwrap();
        TreeEntry {
            name: name.into(),
            oid,
            kind,
        }
    }

    #[test]
    fn directories_first_then_case_insensitive_names() {
        let mut entries = vec![
            entry("Zeta.txt", EntryKind::Blob),
            entry("src", EntryKind::Tree),
            entry("alpha.txt", EntryKind::Blob),
            entry("Docs", EntryKind::Tree),
            entry("link", EntryKind::Symlink),
            entry("run.sh", EntryKind::ExecutableBlob),
            entry("vendor", EntryKind::Submodule),
        ];
        sort_for_display(&mut entries);
        let names: Vec<_> = entries
            .iter()
            .map(|e| e.name_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "Docs",
                "src",
                "alpha.txt",
                "link",
                "run.sh",
                "vendor",
                "Zeta.txt"
            ]
        );
    }

    #[test]
    fn ties_keep_input_order() {
        let mut entries = vec![entry("a", EntryKind::Blob), entry("A", EntryKind::Blob)];
        sort_for_display(&mut entries);
        assert_eq!(entries[0].name, "a");
        assert_eq!(entries[1].name, "A");
    }

    #[test]
    fn lossy_name_replaces_invalid_utf8() {
        let mut bad = entry("ok", EntryKind::Blob);
        bad.name = BString::from(vec![0x66, 0xff, 0x6f]);
        assert_eq!(bad.name_lossy(), "f\u{fffd}o");
    }
}

//! Opening the file an import link names: resolution through [`ImportResolver`] over the tree
//! on screen, committed or on disk.

use std::sync::Arc;

use gpui::{Context, Task};
use rc_git_core::{BlobContent, EntryKind, Oid, TreeEntry};

use super::{BrowserModel, OpenPathOutcome, call, null_oid, with};
use crate::blocking::blocking;
use crate::import_resolver::{CONFIG_SIZE_LIMIT, ImportResolver, ImportSource};
use crate::repository::{ReadResult, RepositoryError, RepositoryReading};
use crate::tree_store::TreeStore;

/// An [`ImportSource`] over a committed tree: one tree object per directory on the way to a
/// path, each landing in the shared tree cache.
struct TreeSource {
    repository: Arc<dyn RepositoryReading>,
    store: Arc<TreeStore>,
    root: Oid,
}

impl ImportSource for TreeSource {
    type Error = RepositoryError;

    fn list(&self, directory: &str) -> ReadResult<Option<Vec<TreeEntry>>> {
        let read = |tree: Oid| {
            self.store
                .entries(tree, |tree| self.repository.tree_entries(tree))
        };
        let mut current = self.root;
        for component in directory.split('/').filter(|part| !part.is_empty()) {
            let entries = read(current)?;
            let Some(child) = entries
                .iter()
                .find(|entry| entry.name == component.as_bytes() && entry.kind == EntryKind::Tree)
            else {
                return Ok(None);
            };
            current = child.oid;
        }
        Ok(Some(read(current)?.as_ref().clone()))
    }

    fn read(&self, path: &str) -> ReadResult<Option<Vec<u8>>> {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let Some(entries) = self.list(parent)? else {
            return Ok(None);
        };
        let Some(entry) = entries.iter().find(|entry| entry.name == name.as_bytes()) else {
            return Ok(None);
        };
        match self.repository.read_blob(entry.oid, CONFIG_SIZE_LIMIT)? {
            BlobContent::Text { data, .. } => Ok(Some(data)),
            BlobContent::Binary { .. } | BlobContent::TooLarge { .. } => Ok(None),
        }
    }
}

/// An [`ImportSource`] over the files on disk. The core answers a path that is not there with
/// an error, as it does a read that failed, so a failed read is checked against the
/// directories on the way: a missing one is `None`, and the error stands only when everything
/// on the way is there.
struct WorktreeSource {
    repository: Arc<dyn RepositoryReading>,
    root: Oid,
}

impl WorktreeSource {
    /// The kind of the working-tree entry at `path`, listing one directory per component;
    /// `None` when a component is not there or one on the way is not a directory. The root is
    /// a directory with no entry of its own.
    fn kind_at(&self, path: &str) -> ReadResult<Option<EntryKind>> {
        let mut found = EntryKind::Tree;
        let mut dir = String::new();
        for component in path.split('/').filter(|part| !part.is_empty()) {
            if found != EntryKind::Tree {
                return Ok(None);
            }
            let listing = self.repository.worktree_entries(Some(self.root), &dir)?;
            let Some(entry) = listing
                .entries
                .iter()
                .find(|entry| entry.name == component.as_bytes())
            else {
                return Ok(None);
            };
            found = entry.kind;
            if !dir.is_empty() {
                dir.push('/');
            }
            dir.push_str(component);
        }
        Ok(Some(found))
    }
}

impl ImportSource for WorktreeSource {
    type Error = RepositoryError;

    fn list(&self, directory: &str) -> ReadResult<Option<Vec<TreeEntry>>> {
        let Some(unhashed) = null_oid() else {
            return Ok(None);
        };
        match self.repository.worktree_entries(Some(self.root), directory) {
            Ok(listing) => {
                let mut entries = Vec::with_capacity(listing.entries.len());
                for entry in listing.entries {
                    entries.push(TreeEntry {
                        name: entry.name,
                        oid: entry.oid.unwrap_or(unhashed),
                        kind: entry.kind,
                    });
                }
                Ok(Some(entries))
            }
            Err(error) => {
                if self.kind_at(directory)? == Some(EntryKind::Tree) {
                    return Err(error);
                }
                Ok(None)
            }
        }
    }

    fn read(&self, path: &str) -> ReadResult<Option<Vec<u8>>> {
        match self.repository.worktree_file(path, CONFIG_SIZE_LIMIT) {
            Ok(BlobContent::Text { data, .. }) => Ok(Some(data)),
            Ok(BlobContent::Binary { .. } | BlobContent::TooLarge { .. }) => Ok(None),
            Err(error) => match self.kind_at(path)? {
                Some(EntryKind::Tree) | None => Ok(None),
                Some(_) => Err(error),
            },
        }
    }
}

impl BrowserModel {
    /// Opens the file a module specifier names, the way an import link asks for one.
    /// `source_path` is the repository-relative path of the file the specifier is written in.
    /// [`OpenPathOutcome::Missing`] when the specifier names nothing in this tree, which is
    /// what a package from a registry does, and [`OpenPathOutcome::Failed`] when a read on the
    /// way failed. A resolution the user moved past answers [`OpenPathOutcome::Opened`]: the
    /// newer open stands, and there is nothing to report.
    ///
    /// Resolution reads the directories on the way to each candidate, through the shared tree
    /// cache, and the config files above `source_path` when the specifier is not relative. It
    /// keeps nothing: a click is rare, and a config differs from one tree to the next.
    pub fn open_import(
        &mut self,
        specifier: &str,
        source_path: &str,
        cx: &mut Context<Self>,
    ) -> Task<OpenPathOutcome> {
        let (Some(repository), Some(root)) = (self.repository.clone(), self.root_tree) else {
            return Task::ready(OpenPathOutcome::Missing);
        };
        let file_token = self.generations.file;
        let root_token = self.generations.root;
        let on_disk = self.browsing_working_tree;
        let store = Arc::clone(&self.trees);
        let specifier = specifier.to_owned();
        let source_path = source_path.to_owned();
        let resolve = blocking(cx.background_executor(), move || {
            if on_disk {
                let source = WorktreeSource { repository, root };
                ImportResolver::new(source).resolve(&specifier, &source_path)
            } else {
                let source = TreeSource {
                    repository,
                    store,
                    root,
                };
                ImportResolver::new(source).resolve(&specifier, &source_path)
            }
        });
        Self::run(cx, OpenPathOutcome::Missing, async move |this, cx| {
            let resolved = resolve.await;
            let current = with(&this, cx, |model, _| {
                file_token == model.generations.file && root_token == model.generations.root
            });
            if current != Some(true) {
                return OpenPathOutcome::Opened;
            }
            match resolved {
                Err(error) => OpenPathOutcome::Failed(error.message().to_owned()),
                Ok(None) => OpenPathOutcome::Missing,
                Ok(Some(path)) => call(&this, cx, |model, cx| model.open_path(&path, None, cx))
                    .await
                    .unwrap_or(OpenPathOutcome::Missing),
            }
        })
    }
}

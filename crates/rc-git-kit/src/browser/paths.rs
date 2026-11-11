//! Go to File: the flattened file list of a root, built once and shared by every query that
//! waits on it, and the scan of the newest query alone.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Context, Task};
use rc_git_core::{Oid, WorktreeStatus};

use super::files::worktree_file_status;
use super::{
    BlobOpen, BrowserModel, IndexBuild, IndexResult, WorktreeBuild, WorktreeFileOpen, call, with,
};
use crate::blocking::blocking;
use crate::oneshot;
use crate::path_search::{BuildError, PathIndex, PathIndexEntry};
use crate::path_search_session::{PathSearchError, PathSearchResult};
use crate::repository::RepositoryError;
use crate::text_document::FileReveal;

/// How many matches Go to File lists by default.
pub const DEFAULT_PATH_SEARCH_LIMIT: usize = 100;

fn waiting(
    receiver: oneshot::Receiver<IndexResult>,
    cx: &mut Context<BrowserModel>,
) -> Task<IndexResult> {
    cx.spawn(async move |_, _| receiver.await.unwrap_or(Err(PathSearchError::Cancelled)))
}

fn failed(error: &RepositoryError) -> PathSearchError {
    PathSearchError::Failed(error.message().to_owned())
}

impl BrowserModel {
    /// The flattened file list below `root`, built off the UI thread on first use. This is the
    /// one call that walks a whole tree; it checks its cancel flag between directory reads. It
    /// reads through the shared tree cache without storing in it: the index already holds the
    /// flattened result, and a whole repository's listings would push out the folders the user
    /// expanded. A call for the root a build already runs for joins it; a call for another
    /// root cancels it.
    ///
    /// # Errors
    /// [`PathSearchError::Cancelled`] when another root's build or a root change replaced this
    /// build; [`PathSearchError::Failed`] when no repository is open or a directory cannot be
    /// listed.
    pub fn path_index(&mut self, root: Oid, cx: &mut Context<Self>) -> Task<IndexResult> {
        if let Some(cached) = self.path_indices.value(&root) {
            return Task::ready(Ok(Arc::clone(cached)));
        }
        let (sender, receiver) = oneshot::channel();
        if let Some(build) = self.index_build.as_mut()
            && build.root == root
        {
            build.waiters.push(sender);
            return waiting(receiver, cx);
        }
        let Some(repository) = self.repository.clone() else {
            return Task::ready(Err(PathSearchError::Failed(
                "path index: no repository is open".to_owned(),
            )));
        };
        self.cancel_index_build();
        let cancel = Arc::new(AtomicBool::new(false));
        self.next_index_build += 1;
        let id = self.next_index_build;
        self.index_build = Some(IndexBuild {
            id,
            root,
            cancel: Arc::clone(&cancel),
            waiters: vec![sender],
        });
        let store = Arc::clone(&self.trees);
        let build = blocking(cx.background_executor(), move || {
            PathIndex::build(root, PathIndex::MAX_ENTRIES, &cancel, |tree| {
                match store.cached(tree) {
                    Some(listing) => Ok(listing.as_ref().clone()),
                    None => repository.tree_entries(tree),
                }
            })
        });
        cx.spawn(async move |this, cx| {
            let built = build.await;
            with(&this, cx, |model, _| {
                model.finish_index_build(id, root, built);
            });
        })
        .detach();
        waiting(receiver, cx)
    }

    fn finish_index_build(
        &mut self,
        id: u64,
        root: Oid,
        built: Result<PathIndex, BuildError<RepositoryError>>,
    ) {
        let result = match built {
            Ok(index) => {
                let index = Arc::new(index);
                self.path_indices.insert(root, Arc::clone(&index));
                Ok(index)
            }
            Err(BuildError::Cancelled) => Err(PathSearchError::Cancelled),
            Err(BuildError::Read { source, .. }) => Err(failed(&source)),
        };
        let Some(build) = self.index_build.take_if(|build| build.id == id) else {
            return;
        };
        for waiter in build.waiters {
            waiter.send(result.clone());
        }
    }

    /// Stops the running index build; everyone waiting for it hears it was cancelled.
    pub(super) fn cancel_index_build(&mut self) {
        let Some(build) = self.index_build.take() else {
            return;
        };
        build.cancel.store(true, Ordering::Relaxed);
        for waiter in build.waiters {
            waiter.send(Err(PathSearchError::Cancelled));
        }
    }

    /// The best `limit` matches for `query` among every file below the current root: on disk
    /// while browsing the working tree, so gitignored files such as `.env` are found too.
    /// Hidden files match only while they are shown. The result carries the index's
    /// `truncated` flag, so the panel can say matches may be missing.
    ///
    /// Only the newest query scans: a newer call stops the running scan, and a caller whose
    /// query was superseded, before or during its scan, gets [`PathSearchError::Cancelled`].
    /// So a burst of keystrokes during an index build runs one scan, for the last query.
    ///
    /// # Errors
    /// [`PathSearchError::Cancelled`] when a newer query superseded this one;
    /// [`PathSearchError::Failed`] when the index build failed.
    pub fn search_paths(
        &mut self,
        query: &str,
        limit: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<PathSearchResult, PathSearchError>> {
        self.generations.path_search += 1;
        let generation = self.generations.path_search;
        if let Some(cancel) = self.path_search_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let Some(root) = self.root_tree else {
            return Task::ready(Ok(PathSearchResult::default()));
        };
        let index = self.current_path_index(root, cx);
        let query = query.to_owned();
        Self::run(
            cx,
            Err(PathSearchError::Cancelled),
            async move |this, cx| {
                let index = index.await?;
                let scan = with(&this, cx, |model, cx| {
                    if generation != model.generations.path_search {
                        return None;
                    }
                    let cancel = Arc::new(AtomicBool::new(false));
                    model.path_search_cancel = Some(Arc::clone(&cancel));
                    model.path_search_scans += 1;
                    let include_hidden = model.shows_hidden_files;
                    let scanned = Arc::clone(&index);
                    Some(blocking(cx.background_executor(), move || {
                        scanned
                            .search(&query, limit, include_hidden, &cancel)
                            .map(|found| found.into_iter().cloned().collect::<Vec<_>>())
                    }))
                })
                .flatten();
                let Some(scan) = scan else {
                    return Err(PathSearchError::Cancelled);
                };
                let entries = scan.await.map_err(|_| PathSearchError::Cancelled)?;
                let current = with(&this, cx, |model, _| {
                    let current = generation == model.generations.path_search;
                    if current {
                        model.path_search_cancel = None;
                    }
                    current
                });
                if current != Some(true) {
                    return Err(PathSearchError::Cancelled);
                }
                Ok(PathSearchResult {
                    entries,
                    truncated: index.truncated,
                })
            },
        )
    }

    /// Drops the working tree's path index so the next search walks the disk again. Called
    /// each time path search opens: files appear and vanish without any ref moving.
    pub fn forget_worktree_paths(&mut self) {
        self.generations.worktree_index += 1;
        self.worktree_index = None;
        self.worktree_index_build = None;
    }

    /// The index searches read: the files on disk while browsing the working tree, else the
    /// root's tree.
    pub(super) fn current_path_index(
        &mut self,
        root: Oid,
        cx: &mut Context<Self>,
    ) -> Task<IndexResult> {
        if self.browsing_working_tree {
            return self.worktree_path_index(root, cx);
        }
        self.path_index(root, cx)
    }

    /// Every file on disk, walked once per path-search session. Queries during a walk share
    /// it; a walk that [`Self::forget_worktree_paths`] outdated still answers the queries
    /// waiting on it but is not kept, and a failed walk is not kept either.
    fn worktree_path_index(&mut self, root: Oid, cx: &mut Context<Self>) -> Task<IndexResult> {
        if let Some(index) = &self.worktree_index {
            return Task::ready(Ok(Arc::clone(index)));
        }
        let generation = self.generations.worktree_index;
        let (sender, receiver) = oneshot::channel();
        if let Some(build) = self.worktree_index_build.as_mut()
            && build.generation == generation
        {
            build.waiters.borrow_mut().push(sender);
            return waiting(receiver, cx);
        }
        let Some(repository) = self.repository.clone() else {
            return Task::ready(Err(PathSearchError::Failed(
                "path index: no repository is open".to_owned(),
            )));
        };
        let waiters = Rc::new(RefCell::new(vec![sender]));
        self.worktree_index_build = Some(WorktreeBuild {
            generation,
            waiters: Rc::clone(&waiters),
        });
        let walk = blocking(cx.background_executor(), move || {
            repository
                .worktree_files(Some(root), PathIndex::MAX_ENTRIES)
                .map(|list| PathIndex::from_worktree_files(root, &list))
        });
        cx.spawn(async move |this, cx| {
            let result = walk.await.map(Arc::new).map_err(|error| failed(&error));
            with(&this, cx, |model, _| {
                model.finish_worktree_walk(generation, &result);
            });
            // The waiters live here, not only in the model: a walk that
            // `forget_worktree_paths` outdated still answers the queries waiting on it.
            for waiter in waiters.borrow_mut().drain(..) {
                waiter.send(result.clone());
            }
        })
        .detach();
        waiting(receiver, cx)
    }

    /// Keeps a current, successful walk.
    fn finish_worktree_walk(&mut self, generation: u64, result: &IndexResult) {
        if generation != self.generations.worktree_index {
            return;
        }
        self.worktree_index_build = None;
        if let Ok(index) = result {
            self.worktree_index = Some(Arc::clone(index));
        }
    }

    /// Opens a path-search pick: from disk while browsing the working tree, else the blob. A
    /// working-tree pick carries its status against `HEAD`, so an untracked file is not offered
    /// for blame; the lookup lists one directory per path component.
    pub fn open_search_result(
        &mut self,
        entry: &PathIndexEntry,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        self.open_pick(entry, None, cx)
    }

    /// Opens a pick, handing `reveal` to the load it starts.
    pub(super) fn open_pick(
        &mut self,
        entry: &PathIndexEntry,
        reveal: Option<FileReveal>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        if !self.browsing_working_tree {
            let mut open = BlobOpen::new(entry).at(entry.path.clone());
            open.path_is_exact = entry.has_exact_path;
            self.reveal_for_next_load = reveal;
            let task = self.open_file(open, cx);
            self.reveal_for_next_load = None;
            return task;
        }
        let mut open = WorktreeFileOpen::new(entry.name(), entry.path.clone());
        open.path_is_exact = entry.has_exact_path;
        let lookup = self.pick_status(entry, cx);
        Self::run(cx, (), async move |this, cx| {
            open.status = lookup.await;
            call(&this, cx, |model, cx| {
                model.reveal_for_next_load = reveal;
                let task = model.open_worktree_file(open, cx);
                model.reveal_for_next_load = None;
                task
            })
            .await;
        })
    }

    /// The working-tree status of a pick, or `None` when it cannot be known: an inexact path
    /// is refused by the open itself, and a failed lookup leaves the open to report what the
    /// read finds.
    fn pick_status(
        &self,
        entry: &PathIndexEntry,
        cx: &mut Context<Self>,
    ) -> Task<Option<WorktreeStatus>> {
        let (Some(repository), Some(root)) = (self.repository.clone(), self.root_tree) else {
            return Task::ready(None);
        };
        if !entry.has_exact_path {
            return Task::ready(None);
        }
        let path = entry.path.clone();
        let lookup = blocking(cx.background_executor(), move || {
            worktree_file_status(&*repository, root, &path)
        });
        cx.spawn(async move |_, _| lookup.await.ok().flatten())
    }
}

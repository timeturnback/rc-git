//! Attaching a repository, the ref refresh the watcher drives, and switching what the tree
//! pane browses: a ref, one commit, or the working tree.

use std::sync::Arc;

use gpui::{Context, Task};
use rc_git_core::{CommitSummary, Head, Oid, RefEntry, RefKind, RefListing};

use super::{BrowserEvent, BrowserModel, FileState, Selection, call, with};
use crate::blocking::blocking;
use crate::oneshot;
use crate::repository::{ReadResult, RepositoryReading};

/// The branch `HEAD` is on, else the first ref pointing at `HEAD`'s commit, else the first
/// local branch, else the first ref of any kind.
#[must_use]
pub fn initial_ref(listing: &RefListing, head: &Head) -> Option<RefEntry> {
    if let Some(name) = &head.ref_name
        && let Some(entry) = listing.refs.iter().find(|entry| &entry.full_name == name)
    {
        return Some(entry.clone());
    }
    if let Some(target) = head.target
        && let Some(entry) = listing.refs.iter().find(|entry| entry.target == target)
    {
        return Some(entry.clone());
    }
    listing
        .refs
        .iter()
        .find(|entry| entry.kind == RefKind::LocalBranch)
        .or_else(|| listing.refs.first())
        .cloned()
}

fn load_listing(repository: &dyn RepositoryReading) -> ReadResult<(RefListing, Head)> {
    Ok((repository.refs()?, repository.head()?))
}

/// Where a fresh listing sends the tree pane once it is applied.
enum Follow {
    Stay,
    Select(RefEntry),
    WorkingTree,
}

impl BrowserModel {
    /// Attaches `repository` and loads refs and `HEAD`, then shows the working tree when there
    /// is one and `HEAD` is born, else selects the ref [`initial_ref`] picks.
    pub fn attach(
        &mut self,
        repository: Arc<dyn RepositoryReading>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        self.begin_move();
        self.repository = Some(Arc::clone(&repository));
        self.trees.remove_all();
        self.history_cache.clear();
        self.path_history_cache.clear();
        self.path_indices.clear();
        self.cancel_index_build();
        self.forget_worktree_paths();
        self.browsed_commit = None;
        self.browsing_working_tree = false;
        self.reset_history(None, cx);
        self.file_state = FileState::Empty;
        self.pending_reveal = None;
        self.opened_file_path = None;
        self.opened_file_source = None;
        Self::changed(BrowserEvent::FileChanged, cx);
        self.generations.root += 1;
        self.generations.refresh += 1;
        let generation = self.generations.root;
        let has_work_dir = repository.work_dir().is_some();
        let load = blocking(cx.background_executor(), move || load_listing(&*repository));
        Self::run(cx, (), async move |this, cx| {
            let loaded = load.await;
            let next = with(&this, cx, |model, cx| {
                model.apply_attached(generation, loaded, has_work_dir, cx)
            })
            .flatten();
            match next {
                Some(Follow::WorkingTree) => {
                    call(&this, cx, Self::select_working_tree).await;
                }
                Some(Follow::Select(reference)) => {
                    call(&this, cx, |model, cx| model.select(reference, cx)).await;
                }
                Some(Follow::Stay) | None => {}
            }
            with(&this, cx, super::BrowserModel::end_move);
        })
    }

    fn apply_attached(
        &mut self,
        generation: u64,
        loaded: ReadResult<(RefListing, Head)>,
        has_work_dir: bool,
        cx: &mut Context<Self>,
    ) -> Option<Follow> {
        if generation != self.generations.root {
            return None;
        }
        match loaded {
            Ok((listing, head)) => {
                let born = head.target.is_some();
                let initial = initial_ref(&listing, &head);
                self.refs = listing;
                self.head = Some(head);
                self.refs_stale = false;
                Self::changed(BrowserEvent::RefsChanged, cx);
                if has_work_dir && born {
                    return Some(Follow::WorkingTree);
                }
                if let Some(initial) = initial {
                    return Some(Follow::Select(initial));
                }
                self.root_tree = None;
                Self::changed(BrowserEvent::RootChanged, cx);
                Some(Follow::Stay)
            }
            Err(error) => {
                self.root_tree = None;
                Self::changed(BrowserEvent::RootChanged, cx);
                Self::fail(error.message(), cx);
                Some(Follow::Stay)
            }
        }
    }

    /// Reloads refs and `HEAD` after the repository changed on disk, keeping every object
    /// cache: only refs can move. The selection survives by name; when its target moved the
    /// root tree and history reload, and when it disappeared the selection falls back to
    /// [`initial_ref`]. A failed refresh keeps the last good listing and marks it stale
    /// through [`Self::refs_stale`] without alerting: the watcher fires during ref updates,
    /// when transient lock errors are normal.
    ///
    /// Refreshes never overlap each other or a restore. One asked for while another runs
    /// joins it, and the running one reads the refs once more when it lands. One asked for
    /// during a restore returns at once and runs after the restore.
    pub fn refresh_refs(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if self.repository.is_none() {
            return Task::ready(());
        }
        self.refresh_pending = true;
        if self.is_restoring {
            return Task::ready(());
        }
        let (sender, receiver) = oneshot::channel();
        self.refresh_waiters.push(sender);
        if !self.refresh_running {
            self.refresh_running = true;
            Self::run(cx, (), async move |this, cx| {
                loop {
                    let next = with(&this, cx, |model, cx| {
                        if !model.refresh_pending || model.is_restoring {
                            return None;
                        }
                        model.refresh_pending = false;
                        Some(model.reload_refs(cx))
                    })
                    .flatten();
                    let Some(reload) = next else {
                        break;
                    };
                    reload.await;
                }
                with(&this, cx, |model, _| model.end_refresh_run());
            })
            .detach();
        }
        cx.spawn(async move |_, _| {
            receiver.await;
        })
    }

    fn end_refresh_run(&mut self) {
        self.refresh_running = false;
        for waiter in self.refresh_waiters.drain(..) {
            waiter.send(());
        }
    }

    fn reload_refs(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(repository) = self.repository.clone() else {
            return Task::ready(());
        };
        self.begin_move();
        self.generations.refresh += 1;
        let generation = self.generations.refresh;
        let load = blocking(cx.background_executor(), move || load_listing(&*repository));
        Self::run(cx, (), async move |this, cx| {
            let loaded = load.await;
            let next = with(&this, cx, |model, cx| {
                model.apply_refresh(generation, loaded, cx)
            });
            match next {
                Some(Follow::WorkingTree) => {
                    call(&this, cx, Self::select_working_tree).await;
                }
                Some(Follow::Select(reference)) => {
                    call(&this, cx, |model, cx| model.select(reference, cx)).await;
                }
                Some(Follow::Stay) | None => {}
            }
            with(&this, cx, super::BrowserModel::end_move);
        })
    }

    fn apply_refresh(
        &mut self,
        generation: u64,
        loaded: ReadResult<(RefListing, Head)>,
        cx: &mut Context<Self>,
    ) -> Follow {
        if generation != self.generations.refresh {
            return Follow::Stay;
        }
        let Ok((listing, head)) = loaded else {
            // The message is dropped on purpose: a lock during a ref update is the usual
            // cause and heals on the next event, so the views show the quiet stale state.
            self.refs_stale = true;
            Self::changed(BrowserEvent::RefsChanged, cx);
            return Follow::Stay;
        };
        let previous_head = self.head.as_ref().and_then(|head| head.target);
        self.refs = listing;
        self.head = Some(head.clone());
        self.refs_stale = false;
        let follow = if self.browsing_working_tree {
            self.follow_working_tree(&head, previous_head, cx)
        } else {
            self.follow_selection(&head, cx)
        };
        if let Follow::Stay = follow {
            Self::changed(BrowserEvent::RefsChanged, cx);
        }
        follow
    }

    /// Keeps the selected ref by name in a fresh listing: reloads it when its target moved,
    /// and falls back to [`initial_ref`] when it disappeared.
    fn follow_selection(&mut self, head: &Head, cx: &mut Context<Self>) -> Follow {
        let Some(current) = self.selected_ref.clone() else {
            return Follow::Stay;
        };
        let updated = self
            .refs
            .refs
            .iter()
            .find(|entry| entry.full_name == current.full_name)
            .cloned();
        if let Some(updated) = updated {
            if updated.target == current.target {
                self.selected_ref = Some(updated);
                return Follow::Stay;
            }
            return Follow::Select(updated);
        }
        if let Some(fallback) = initial_ref(&self.refs, head) {
            return Follow::Select(fallback);
        }
        self.selected_ref = None;
        self.clear_root(cx);
        Follow::Stay
    }

    /// Follows a moved `HEAD` while the working tree is browsed. An unborn `HEAD` has no tree
    /// to compare against, so the model leaves the working tree for the ref `attach` picks.
    fn follow_working_tree(
        &mut self,
        head: &Head,
        previous_head: Option<Oid>,
        cx: &mut Context<Self>,
    ) -> Follow {
        if head.target.is_none() {
            self.browsing_working_tree = false;
            if let Some(fallback) = initial_ref(&self.refs, head) {
                return Follow::Select(fallback);
            }
            self.clear_root(cx);
            return Follow::Stay;
        }
        if head.target == previous_head {
            Follow::Stay
        } else {
            Follow::WorkingTree
        }
    }

    /// Empties the tree and the history for a selection that is gone.
    fn clear_root(&mut self, cx: &mut Context<Self>) {
        self.root_tree = None;
        self.reset_history(None, cx);
        Self::changed(BrowserEvent::RootChanged, cx);
    }

    /// Switches the browsed ref. Only the root tree id changes; cached subtrees stay valid.
    pub fn select(&mut self, reference: RefEntry, cx: &mut Context<Self>) -> Task<()> {
        if self.repository.is_none() {
            return Task::ready(());
        }
        self.begin_move();
        let previous = self.current_selection();
        let target = reference.target;
        self.selected_ref = Some(reference);
        self.browsed_commit = None;
        self.browsing_working_tree = false;
        Self::changed(BrowserEvent::RefsChanged, cx);
        self.reset_history(Some(target), cx);
        self.finish_switch(target, previous, cx)
    }

    /// Shows the tree of one commit, picked from the history pane. The sidebar deselects and
    /// the history keeps showing the ref the commit came from, so hopping between commits
    /// stays one click.
    pub fn browse_commit(&mut self, commit: CommitSummary, cx: &mut Context<Self>) -> Task<()> {
        if self.repository.is_none() {
            return Task::ready(());
        }
        self.begin_move();
        let previous = self.current_selection();
        let target = commit.oid;
        self.selected_ref = None;
        self.browsed_commit = Some(commit);
        self.browsing_working_tree = false;
        Self::changed(BrowserEvent::RefsChanged, cx);
        let root = self.load_root(target, previous, cx);
        Self::run(cx, (), async move |this, cx| {
            root.await;
            with(&this, cx, super::BrowserModel::end_move);
        })
    }

    /// Shows the files on disk with a status overlay against `HEAD`. Reads only: every
    /// mutation goes through `rc-git-guard`, and the core never writes.
    pub fn select_working_tree(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(target) = self.head.as_ref().and_then(|head| head.target) else {
            return Task::ready(());
        };
        if self.repository.is_none() {
            return Task::ready(());
        }
        self.begin_move();
        let previous = self.current_selection();
        self.selected_ref = None;
        self.browsed_commit = None;
        self.browsing_working_tree = true;
        Self::changed(BrowserEvent::RefsChanged, cx);
        self.reset_history(Some(target), cx);
        self.finish_switch(target, previous, cx)
    }

    /// The end of a switch that moved the history anchor: the root, then a search that was
    /// waiting for an anchor, then the end of the move.
    fn finish_switch(
        &mut self,
        target: Oid,
        previous: Selection,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let root = self.load_root(target, previous, cx);
        Self::run(cx, (), async move |this, cx| {
            root.await;
            call(&this, cx, Self::run_pending_search).await;
            with(&this, cx, super::BrowserModel::end_move);
        })
    }

    /// Loads the root tree of a new selection. When the tree cannot be read (a ref to a blob,
    /// a tree missing from a partial clone) the selection rolls back to `previous`, so the
    /// sidebar, history and location keep naming the tree and file still on screen.
    fn load_root(&mut self, target: Oid, previous: Selection, cx: &mut Context<Self>) -> Task<()> {
        let Some(repository) = self.repository.clone() else {
            return Task::ready(());
        };
        self.generations.root += 1;
        let generation = self.generations.root;
        self.cancel_index_build();
        self.forget_worktree_paths();
        let read = blocking(cx.background_executor(), move || repository.tree_of(target));
        Self::run(cx, (), async move |this, cx| {
            let result = read.await;
            with(&this, cx, |model, cx| {
                if generation != model.generations.root {
                    return;
                }
                match result {
                    Ok(tree) => {
                        model.root_tree = Some(tree);
                        model.file_state = FileState::Empty;
                        model.pending_reveal = None;
                        model.opened_file_path = None;
                        model.opened_file_source = None;
                        Self::changed(BrowserEvent::FileChanged, cx);
                        Self::changed(BrowserEvent::RootChanged, cx);
                    }
                    Err(error) => {
                        model.roll_back(previous, cx);
                        Self::fail(error.message(), cx);
                    }
                }
            });
        })
    }

    fn roll_back(&mut self, previous: Selection, cx: &mut Context<Self>) {
        self.selected_ref = previous.reference;
        self.browsed_commit = previous.commit;
        self.browsing_working_tree = previous.working_tree;
        if self.history_anchor != previous.history_anchor {
            self.reset_history(previous.history_anchor, cx);
        }
        Self::changed(BrowserEvent::RefsChanged, cx);
    }
}

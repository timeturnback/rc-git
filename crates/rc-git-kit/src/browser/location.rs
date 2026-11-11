//! Where the browser is, as a value the back/forward list returns to, and the restore that
//! returns there. Restores and ref refreshes share one queue, so a restore never resolves its
//! file against a root a refresh is still loading.

use gpui::{AsyncApp, Context, Task, WeakEntity};
use rc_git_core::Oid;

use super::{BrowserEvent, BrowserModel, OpenPathOutcome, call, with};
use crate::back_forward::{BrowsedTree, BrowserLocation, HistoryScope};
use crate::oneshot;

impl BrowserModel {
    /// Where the browser is, as a value the back/forward list can return to.
    #[must_use]
    pub fn location(&self) -> BrowserLocation {
        let tree = if self.browsing_working_tree {
            BrowsedTree::WorkingTree
        } else if let Some(reference) = &self.selected_ref {
            BrowsedTree::Ref(reference.full_name.clone())
        } else if let Some(commit) = &self.browsed_commit {
            BrowsedTree::Commit {
                commit: commit.oid,
                history_tip: self.history_anchor,
            }
        } else {
            BrowsedTree::Nothing
        };
        BrowserLocation {
            tree,
            file_path: self.opened_file_path.clone(),
            file_path_is_exact: self.opened_file_path.is_none() || self.opened_path_is_exact,
            history_scope: self.history_scope.clone(),
            detail_commit: self.diff_commit.as_ref().map(|commit| commit.oid),
        }
    }

    /// Returns the browser to a location it was at, changing only what differs: a step between
    /// two files of one tree reads one blob and nothing else. What the location names is read
    /// as it is now, so a ref that moved shows its new tip and a file that is gone leaves the
    /// pane empty. A tree that cannot be shown any more is reported and ends the restore with
    /// nothing else changed, and so does a ref picked on the way.
    ///
    /// Restores never overlap. One asked for while another runs returns at once; the running
    /// one stops at its next step and goes on to the newest location asked for. A restore
    /// waits for a ref refresh under way, and a refresh asked for meanwhile starts once the
    /// restore ends.
    pub fn restore(&mut self, target: BrowserLocation, cx: &mut Context<Self>) -> Task<()> {
        if self.repository.is_none() {
            return Task::ready(());
        }
        self.pending_restore = Some(target);
        if self.is_restoring {
            return Task::ready(());
        }
        self.is_restoring = true;
        let refresh = if self.refresh_running {
            let (sender, receiver) = oneshot::channel();
            self.refresh_waiters.push(sender);
            Some(receiver)
        } else {
            None
        };
        Self::run(cx, (), async move |this, cx| {
            if let Some(refresh) = refresh {
                refresh.await;
            }
            with(&this, cx, |model, _| model.begin_move());
            loop {
                let next = with(&this, cx, |model, _| model.pending_restore.take()).flatten();
                let Some(next) = next else {
                    break;
                };
                move_to(&this, cx, next).await;
            }
            with(&this, cx, super::BrowserModel::end_restore);
        })
    }

    fn end_restore(&mut self, cx: &mut Context<Self>) {
        self.is_restoring = false;
        self.end_move(cx);
        // Another move still under way would report where this restore ended once it lands,
        // after the restore returned, and that report would read as a new stop.
        if self.move_depth > 0 {
            self.reported_location = self.location();
        }
        if self.refresh_pending {
            self.refresh_refs(cx).detach();
        }
    }

    fn restore_scope(&mut self, scope: HistoryScope, cx: &mut Context<Self>) {
        if self.history_scope == scope {
            return;
        }
        match scope {
            HistoryScope::All => self.clear_history_scope(cx),
            HistoryScope::Path(path) => {
                self.scope_history(&path, cx);
            }
        }
    }

    pub(super) fn begin_move(&mut self) {
        self.move_depth += 1;
    }

    pub(super) fn end_move(&mut self, cx: &mut Context<Self>) {
        self.move_depth = self.move_depth.saturating_sub(1);
        self.note_location(cx);
    }

    /// Reports the location when it differs from the last one reported and no move is under
    /// way.
    pub(super) fn note_location(&mut self, cx: &mut Context<Self>) {
        if self.move_depth > 0 {
            return;
        }
        let current = self.location();
        if current == self.reported_location {
            return;
        }
        self.reported_location = current;
        Self::changed(BrowserEvent::LocationChanged, cx);
    }
}

async fn move_to(this: &WeakEntity<BrowserModel>, cx: &mut AsyncApp, target: BrowserLocation) {
    let current_tree = with(this, cx, |model, _| model.location().tree);
    if current_tree.as_ref() != Some(&target.tree) {
        restore_tree(this, cx, target.tree.clone()).await;
        let arrived = with(this, cx, |model, _| {
            model.pending_restore.is_none() && model.location().tree == target.tree
        });
        if arrived != Some(true) {
            return;
        }
    }
    if !restore_file(this, cx, target.file_path, target.file_path_is_exact).await {
        return;
    }
    with(this, cx, |model, cx| {
        model.restore_scope(target.history_scope, cx);
    });
    restore_detail(this, cx, target.detail_commit).await;
}

/// Opens the file a stop names, or empties the pane for a stop without one. `false` when
/// another restore was asked for during the open, so the move stops there. An inexact path is
/// not read: it resolves by spelling and could land on a file literally named with U+FFFD.
async fn restore_file(
    this: &WeakEntity<BrowserModel>,
    cx: &mut AsyncApp,
    path: Option<String>,
    is_exact: bool,
) -> bool {
    let same = with(this, cx, |model, _| {
        let current = model.location();
        current.file_path == path && current.file_path_is_exact == is_exact
    });
    match same {
        None => return false,
        Some(true) => return true,
        Some(false) => {}
    }
    let mut opened = false;
    if let Some(path) = path.filter(|_| is_exact) {
        let outcome = call(this, cx, |model, cx| model.open_path(&path, None, cx)).await;
        opened = outcome == Some(OpenPathOutcome::Opened);
    }
    with(this, cx, |model, cx| {
        // A file that is gone empties the pane: the file of the stop left behind, still open,
        // would read as the one this stop named.
        if !opened {
            model.close_file(cx);
        }
        model.pending_restore.is_none()
    })
    .unwrap_or(false)
}

async fn restore_detail(this: &WeakEntity<BrowserModel>, cx: &mut AsyncApp, commit: Option<Oid>) {
    let Some(shown) = with(this, cx, |model, _| {
        model.diff_commit.as_ref().map(|shown| shown.oid)
    }) else {
        return;
    };
    if shown == commit {
        return;
    }
    match commit {
        Some(commit) => {
            call(this, cx, |model, cx| model.jump_to_commit(commit, cx)).await;
        }
        None => {
            with(this, cx, super::BrowserModel::close_commit_detail);
        }
    }
}

async fn restore_tree(this: &WeakEntity<BrowserModel>, cx: &mut AsyncApp, tree: BrowsedTree) {
    match tree {
        BrowsedTree::Nothing => {}
        BrowsedTree::WorkingTree => {
            call(this, cx, BrowserModel::select_working_tree).await;
        }
        BrowsedTree::Ref(name) => {
            let found = with(this, cx, |model, cx| {
                let found = model
                    .refs
                    .refs
                    .iter()
                    .find(|entry| entry.full_name == name)
                    .cloned();
                if found.is_none() {
                    BrowserModel::fail(
                        format!("cannot return to {name}: the ref no longer exists"),
                        cx,
                    );
                }
                found
            })
            .flatten();
            if let Some(reference) = found {
                call(this, cx, |model, cx| model.select(reference, cx)).await;
            }
        }
        BrowsedTree::Commit {
            commit,
            history_tip,
        } => restore_commit(this, cx, commit, history_tip).await,
    }
}

async fn restore_commit(
    this: &WeakEntity<BrowserModel>,
    cx: &mut AsyncApp,
    commit: Oid,
    history_tip: Option<Oid>,
) {
    let Some(root_token) = with(this, cx, |model, _| model.generations.root) else {
        return;
    };
    let summary = call(this, cx, |model, cx| model.read_commit(commit, cx))
        .await
        .flatten();
    let current = with(this, cx, |model, _| root_token == model.generations.root);
    let (Some(summary), Some(true)) = (summary, current) else {
        return;
    };
    call(this, cx, |model, cx| model.browse_commit(summary, cx)).await;
    let moved = with(this, cx, |model, cx| {
        if model.history_anchor == history_tip {
            return false;
        }
        model.reset_history(history_tip, cx);
        true
    });
    if moved == Some(true) {
        call(this, cx, BrowserModel::run_pending_search).await;
    }
}

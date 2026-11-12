//! The window's half of the Changes mode (`MainWindowController.show`, `wireChanges`): the
//! `ChangesModel` and `CommitAuthorModel` made for each repository, the worktree watcher, and
//! the visibility that lets a hidden list scan nothing.

use std::sync::Arc;

use gpui::{AppContext, Context, Entity, PromptLevel, Subscription, Window};
use rc_git_kit::back_forward::ContentMode;
use rc_git_kit::changes::{ChangesEvent, ChangesModel, CommitAuthorModel};
use rc_git_kit::guard_bridge::READ_FAILURE_TITLE;
use rc_git_kit::platform::stores::{ChangeGroupingStore, CommitIdentityStore};
use rc_git_kit::platform::watch::{WatcherFactory, WatcherStartError, WorktreeDirs};
use rc_git_kit::repository::RepositoryReading;
use rc_git_kit::repository_actions::RepositoryActions;

use crate::app::OpenedRepository;
use crate::changes::list::{ChangeListEvent, ChangeListView, Sources};
use crate::changes::pane::WorktreeDiffPane;
use crate::window::RepoWindow;
use crate::window::guarded::alert;

/// The title of the alert when the worktree watcher could not start.
pub const WATCH_FAILURE_TITLE: &str = "rc-git could not watch the repository";

/// What the Changes models are made from, taken from the repository a window shows.
pub(super) struct ChangesSource {
    reading: Arc<dyn RepositoryReading>,
    tab_path: String,
    dirs: Option<(String, String)>,
    actions: Option<Arc<dyn RepositoryActions>>,
}

impl ChangesSource {
    pub(super) fn of(repository: &OpenedRepository) -> Self {
        Self {
            reading: Arc::clone(&repository.reading),
            tab_path: repository.tab_path.clone(),
            dirs: repository.dirs.clone(),
            actions: repository.actions.clone(),
        }
    }
}

/// One repository's Changes models and views.
pub struct ChangesParts {
    pub(crate) model: Entity<ChangesModel>,
    pub(crate) author: Option<Entity<CommitAuthorModel>>,
    pub(crate) list: Entity<ChangeListView>,
    pub(crate) pane: Entity<WorktreeDiffPane>,
    _subscriptions: Vec<Subscription>,
}

impl RepoWindow {
    /// Makes the Changes models and views for `repository`, and starts the worktree watcher.
    pub(super) fn show_changes(
        &mut self,
        repository: ChangesSource,
        watchers: &dyn WatcherFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(old) = self.changes.take() {
            old.model.update(cx, |model, _| model.stop_watching());
        }
        self.actions.clone_from(&repository.actions);
        self.work_dir = repository
            .reading
            .work_dir()
            .map(|dir| dir.to_string_lossy().into_owned());
        let shows_hidden = self.model.read(cx).shows_hidden_files();
        let grouping = ChangeGroupingStore::worktree(Arc::clone(&self.settings));
        let reading = Arc::clone(&repository.reading);
        let model = cx.new(|_| ChangesModel::new(reading, grouping, shows_hidden));
        let author = repository.actions.clone().map(|actions| {
            let store = CommitIdentityStore::new(Arc::clone(&self.settings));
            let path = repository.tab_path.clone();
            cx.new(|_| CommitAuthorModel::new(path, store, actions as Arc<dyn RepositoryActions>))
        });
        let sources = Sources {
            author: author.clone(),
            sync: self.sync.clone(),
            has_work_dir: self.has_work_dir,
        };
        let list = cx.new(|cx| ChangeListView::new(model.clone(), sources, window, cx));
        let pane = cx.new(|cx| WorktreeDiffPane::new(model.clone(), cx));
        let expanded = self.diff_expanded(cx);
        pane.update(cx, |pane, cx| pane.set_expanded(expanded, cx));
        let subscriptions = Self::changes_subscriptions(&model, &list, &pane, window, cx);
        self.changes = Some(ChangesParts {
            model: model.clone(),
            author: author.clone(),
            list: list.clone(),
            pane,
            _subscriptions: subscriptions,
        });
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_changes(Some(list), cx));
        if let Some(author) = author {
            author.update(cx, CommitAuthorModel::refresh).detach();
        }
        if let Err(error) = self.watch_worktree(&model, repository.dirs, watchers, cx) {
            alert(
                PromptLevel::Warning,
                WATCH_FAILURE_TITLE,
                &error.to_string(),
                window,
                cx,
            )
            .detach();
        }
        self.sync_changes_visibility(cx);
    }

    fn changes_subscriptions(
        model: &Entity<ChangesModel>,
        list: &Entity<ChangeListView>,
        pane: &Entity<WorktreeDiffPane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        vec![
            cx.subscribe_in(model, window, |_, _, event: &ChangesEvent, window, cx| {
                if let ChangesEvent::Error(message) = event {
                    alert(
                        PromptLevel::Warning,
                        READ_FAILURE_TITLE,
                        message,
                        window,
                        cx,
                    )
                    .detach();
                }
            }),
            cx.subscribe_in(
                list,
                window,
                |this, _, event: &ChangeListEvent, window, cx| {
                    let ChangeListEvent::Request(request) = event;
                    this.changes_request(request.clone(), window, cx);
                },
            ),
            cx.subscribe_in(
                pane,
                window,
                |this, _, event: &ChangeListEvent, window, cx| {
                    let ChangeListEvent::Request(request) = event;
                    this.changes_request(request.clone(), window, cx);
                },
            ),
        ]
    }

    fn watch_worktree(
        &self,
        model: &Entity<ChangesModel>,
        dirs: Option<(String, String)>,
        watchers: &dyn WatcherFactory,
        cx: &mut Context<Self>,
    ) -> Result<(), WatcherStartError> {
        let (Some(work_dir), Some((git_dir, common_dir))) = (self.work_dir.clone(), dirs) else {
            return Ok(());
        };
        let dirs = WorktreeDirs {
            work_dir,
            git_dir,
            common_dir,
        };
        model.update(cx, |model, cx| model.watch_worktree(watchers, &dirs, cx))
    }

    /// Tells the change list whether it is on screen: this tab on Changes, and the window
    /// visible (not a background tab, not minimized, not covered). A hidden list scans
    /// nothing; a shown one catches up on what it missed.
    pub(super) fn sync_changes_visibility(&mut self, cx: &mut Context<Self>) {
        let visible = self.mode == ContentMode::Changes && self.visibility.is_visible();
        if let Some(parts) = &self.changes {
            parts
                .model
                .update(cx, |model, cx| model.set_visible(visible, cx))
                .detach();
        }
    }

    /// The change list no longer matches disk, the refs or the hidden-files setting.
    pub(super) fn changes_stale(&mut self, cx: &mut Context<Self>) {
        if let Some(parts) = &self.changes {
            parts.model.update(cx, ChangesModel::mark_stale).detach();
        }
    }

    /// Switching to Changes re-reads what no watcher covers: the sync counts and the
    /// author line, which reads `.git/config`.
    pub(super) fn changes_shown(&mut self, cx: &mut Context<Self>) {
        if let Some(sync) = &self.sync {
            sync.update(cx, rc_git_kit::sync::SyncModel::refresh_status)
                .detach();
        }
        if let Some(author) = self.changes.as_ref().and_then(|parts| parts.author.clone()) {
            author.update(cx, CommitAuthorModel::refresh).detach();
        }
    }

    /// The Changes models, for tests.
    #[cfg(test)]
    pub(crate) fn changes(&self) -> Option<&ChangesParts> {
        self.changes.as_ref()
    }

    /// The sync model, for tests.
    #[cfg(test)]
    pub(crate) fn sync_model(&self) -> Option<&Entity<rc_git_kit::sync::SyncModel>> {
        self.sync.as_ref()
    }

    /// The sheet on screen, for tests.
    #[cfg(test)]
    pub(crate) fn sheet(&self) -> Option<&crate::sheets::OpenSheet> {
        self.sheet.as_ref()
    }
}

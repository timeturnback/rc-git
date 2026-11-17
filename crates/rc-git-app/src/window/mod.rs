//! One repository window (`MainWindowController`): the app-drawn toolbar in the transparent
//! titlebar, the sidebar and the mode's pane in a resizable split, the ref picker under the
//! banner, and the restore notice. It owns the window's `BrowserModel` and `SyncModel`, and
//! tells the shell where it is through [`WindowEvent`]s; the shell owns the app-wide
//! back/forward list.

mod changes;
mod guarded;
pub mod layout;
pub mod panels;
mod ref_actions;
pub(crate) mod sync;
mod tab_bar;
mod toolbar;
mod tree_actions;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, NavigationDirection, ParentElement, Render,
    SharedString, Styled, Subscription, SystemWindowTabController, Task, Window, deferred, div,
    prelude::FluentBuilder,
};
use gpui_component::ActiveTheme;
use gpui_component::resizable::{
    ResizablePanelEvent, ResizableState, h_resizable, resizable_panel,
};
use rc_git_kit::back_forward::{ContentMode, Direction, NavigationStop};
use rc_git_kit::browser::{BrowserEvent, BrowserModel, can_open_ref_picker};
use rc_git_kit::platform::stores::SharedSettings;
use rc_git_kit::platform::watch::{Watcher, WatcherFactory};
use rc_git_kit::restore_notice::RestoreNotice;
use rc_git_kit::sync::{SyncEvent, SyncModel};

use crate::app::OpenedRepository;
use crate::blame::BlamePanel;
use crate::files::pane::FilePane;
use crate::files::tree::FileTreeView;
use crate::history::search::Clock;
use crate::history::{HistoryEvent, HistoryPane};
use crate::inbox::Inbox;
use crate::menus::{
    CloseTab, GoToBranch, MergeAllWindows, Minimize, MoveTabToNewWindow, NextTab, PreviousTab,
    ShowChanges, ShowFiles, ShowHistory, ToggleDiffExpanded, ToggleFullScreen, ToggleSidebar, Zoom,
};
use crate::menus::{SearchCommits, ShowBlame, ShowFileHistory};
use crate::sheets::OpenSheet;
use crate::sidebar::refs::{RefListEvent, RefListView};
use crate::sidebar::{Sidebar, SidebarEvent};
use crate::style;
use rc_git_kit::repository_actions::RepositoryActions;

pub use changes::ChangesParts;
pub use layout::SidebarLayout;

/// What the content area says in a mode whose pane lands in a later phase.
pub const PANE_PENDING: &str = "This pane is not in this build yet.";

/// What a window tells the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowEvent {
    /// The window is somewhere new: a stop for the back/forward list.
    Stop(NavigationStop),
    /// The toolbar, a mouse button or a key asked to step back or forward.
    Navigate(Direction),
    /// Folders were dropped on the window.
    OpenPaths(Vec<PathBuf>),
    /// The window became the main one.
    BecameFront,
    /// Something a menu title names changed: the default branch.
    MenusStale,
}

/// The repository a window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// The path the tab shows and saves.
    pub path: String,
    /// What tells this repository apart from every other open one.
    pub key: String,
}

/// One repository window.
pub struct RepoWindow {
    model: Entity<BrowserModel>,
    sync: Option<Entity<SyncModel>>,
    mode: ContentMode,
    sidebar: Entity<Sidebar>,
    refs: Entity<RefListView>,
    pane: Entity<FilePane>,
    history: Entity<HistoryPane>,
    blame: BlamePanel,
    picker: Option<Entity<RefListView>>,
    split: Entity<ResizableState>,
    layout: SidebarLayout,
    shown: Option<Shown>,
    has_work_dir: bool,
    notice: Option<RestoreNotice>,
    can_go_back: bool,
    can_go_forward: bool,
    watcher: Option<Box<dyn Watcher>>,
    watch_task: Option<Task<()>>,
    picker_events: Option<Subscription>,
    panels: panels::Panels,
    /// The width the Files tree last said shows its rows in full.
    content_width: Option<f64>,
    /// Where the menu commands land when nothing inside the window has the keyboard.
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    settings: SharedSettings,
    changes: Option<ChangesParts>,
    actions: Option<Arc<dyn RepositoryActions>>,
    work_dir: Option<String>,
    visibility: gpui::WindowVisibility,
    sheet: Option<OpenSheet>,
    handle: gpui::AnyWindowHandle,
}

impl EventEmitter<WindowEvent> for RepoWindow {}

impl RepoWindow {
    /// An empty window: no repository yet.
    pub fn new(settings: SharedSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = cx.new(|cx| BrowserModel::new(Arc::clone(&settings), cx));
        let tree = cx.new(|cx| FileTreeView::new(model.clone(), window, cx));
        let refs = cx.new(|cx| RefListView::new(model.clone(), window, cx));
        let sidebar = cx.new(|cx| Sidebar::new(model.clone(), tree.clone(), refs.clone(), cx));
        let pane = cx.new(|cx| FilePane::new(model.clone(), window, cx));
        let (history, history_events, blame_events) =
            Self::history_parts(&model, &settings, window, cx);
        let split = cx.new(|_| ResizableState::default());
        let layout = SidebarLayout::load(Arc::clone(&settings));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut subscriptions = vec![
            cx.subscribe(&model, |this, _, event: &BrowserEvent, cx| {
                if *event == BrowserEvent::LocationChanged {
                    this.record_stop(cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &sidebar,
                window,
                |this, _, event: &SidebarEvent, window, cx| match event {
                    SidebarEvent::BannerClicked => this.toggle_picker(window, cx),
                },
            ),
            cx.subscribe(&refs, |this, _, event: &RefListEvent, cx| {
                this.picked(event, cx);
            }),
            cx.subscribe(&split, |this, split, event: &ResizablePanelEvent, cx| {
                let ResizablePanelEvent::Resized = event;
                if this.layout.take_fit_echo() {
                    return;
                }
                let width = split.read(cx).sizes().first().copied();
                if let Some(width) = width {
                    this.layout.dragged(f64::from(f32::from(width)));
                }
            }),
            history_events,
            blame_events,
            cx.observe_window_visibility(window, |this, visibility, _, cx| {
                this.visibility = visibility;
                this.sync_changes_visibility(cx);
            }),
            cx.subscribe_in(
                &tree,
                window,
                |this, _, action: &crate::files::tree::TreeAction, window, cx| {
                    this.tree_action(action, window, cx);
                },
            ),
        ];
        subscriptions.extend(Self::window_observers(window, cx));
        Self {
            model,
            sync: None,
            mode: ContentMode::Files,
            sidebar,
            refs,
            pane,
            history,
            blame: BlamePanel::new(rc_git_mac::local_utc_offset),
            picker: None,
            split,
            layout,
            shown: None,
            has_work_dir: false,
            notice: None,
            can_go_back: false,
            can_go_forward: false,
            watcher: None,
            watch_task: None,
            picker_events: None,
            panels: panels::Panels::default(),
            content_width: None,
            focus,
            _subscriptions: subscriptions,
            settings,
            changes: None,
            actions: None,
            work_dir: None,
            visibility: if window.is_visible() {
                gpui::WindowVisibility::Visible
            } else {
                gpui::WindowVisibility::Hidden
            },
            sheet: None,
            handle: window.window_handle(),
        }
    }

    /// The window's browser model.
    #[cfg(test)]
    pub(crate) fn model(&self) -> &Entity<BrowserModel> {
        &self.model
    }

    /// The window's file pane.
    #[cfg(test)]
    pub(crate) fn pane(&self) -> &Entity<FilePane> {
        &self.pane
    }

    /// The sidebar.
    #[cfg(test)]
    pub(crate) fn sidebar(&self) -> &Entity<Sidebar> {
        &self.sidebar
    }

    /// The ref list the sidebar shows in History.
    #[cfg(test)]
    pub(crate) fn refs(&self) -> &Entity<RefListView> {
        &self.refs
    }

    /// The ref picker, while it is open.
    #[cfg(test)]
    pub(crate) fn picker(&self) -> Option<&Entity<RefListView>> {
        self.picker.as_ref()
    }

    /// The repository on show, `None` for an empty tab.
    #[must_use]
    pub fn shown(&self) -> Option<&Shown> {
        self.shown.as_ref()
    }

    /// The repository's mainline branch as the last sync status read it, which titles Back to
    /// Default in the menu.
    #[must_use]
    pub fn default_branch(&self, cx: &gpui::App) -> Option<String> {
        let sync = self.sync.as_ref()?;
        sync.read(cx).default_branch().map(str::to_owned)
    }

    /// The pane on show.
    #[must_use]
    pub fn mode(&self) -> ContentMode {
        self.mode
    }

    /// The sidebar's width and collapse.
    #[cfg(test)]
    pub(crate) fn layout(&self) -> &SidebarLayout {
        &self.layout
    }

    /// The restore notice on show.
    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<&RestoreNotice> {
        self.notice.as_ref()
    }

    /// Attaches `repository` and starts loading it: the title, the ref watcher, the sync
    /// status, then the model's attach.
    pub fn show(
        &mut self,
        repository: OpenedRepository,
        watchers: &dyn WatcherFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let name = repository
            .tab_path
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or(&repository.tab_path)
            .to_owned();
        window.set_window_title(&name);
        SystemWindowTabController::update_tab_title(
            cx,
            window.window_handle().window_id(),
            SharedString::from(name),
        );
        self.shown = Some(Shown {
            path: repository.tab_path.clone(),
            key: repository.key.clone(),
        });
        self.has_work_dir = repository.reading.work_dir().is_some();
        self.watch_refs(&repository, watchers, cx);
        let changes_source = changes::ChangesSource::of(&repository);
        self.sync = repository.sync;
        if let Some(sync) = &self.sync {
            cx.observe(sync, |_, _, cx| cx.notify()).detach();
            cx.subscribe(sync, |_, _, event: &SyncEvent, cx| {
                if *event == SyncEvent::StatusChanged {
                    cx.emit(WindowEvent::MenusStale);
                }
            })
            .detach();
            sync.update(cx, rc_git_kit::sync::SyncModel::refresh_status)
                .detach();
        }
        if let Some(sync) = self.sync.clone() {
            Self::set_prompters(&sync, window, cx);
            cx.subscribe_in(&sync, window, |this, _, event: &SyncEvent, window, cx| {
                if let SyncEvent::StateChanged(state) = event {
                    this.sync_state_changed(state, window, cx);
                }
            })
            .detach();
        }
        self.show_changes(changes_source, watchers, window, cx);
        let reading = repository.reading;
        self.model.update(cx, |model, cx| model.attach(reading, cx))
    }

    fn watch_refs(
        &mut self,
        repository: &OpenedRepository,
        watchers: &dyn WatcherFactory,
        cx: &mut Context<Self>,
    ) {
        self.watcher = None;
        self.watch_task = None;
        let Some((git_dir, common_dir)) = &repository.dirs else {
            return;
        };
        let inbox: Inbox<()> = Inbox::default();
        let sender = inbox.clone();
        let watcher = watchers.ref_watcher(git_dir, common_dir, Arc::new(move || sender.push(())));
        if let Err(error) = watcher.start() {
            self.model.update(cx, |_, cx| {
                cx.emit(BrowserEvent::Error(error.to_string()));
            });
        }
        self.watcher = Some(watcher);
        self.watch_task = Some(cx.spawn(async move |this, cx| {
            loop {
                inbox.next().await;
                let refreshed = this.update(cx, RepoWindow::refs_moved);
                let Ok(refresh) = refreshed else {
                    return;
                };
                refresh.await;
            }
        }));
    }

    /// The watcher saw the refs move: re-read them, and the sync status with them.
    fn refs_moved(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if let Some(sync) = &self.sync {
            sync.update(cx, rc_git_kit::sync::SyncModel::refresh_status)
                .detach();
        }
        self.changes_stale(cx);
        self.model
            .update(cx, rc_git_kit::browser::BrowserModel::refresh_refs)
    }

    /// Shows the notice naming the saved tabs a launch restore could not reopen.
    pub fn show_restore_notice(&mut self, notice: RestoreNotice, cx: &mut Context<Self>) {
        self.notice = Some(notice);
        cx.notify();
    }

    /// Shows what the back/forward list allows on the toolbar arrows.
    pub fn render_navigation(&mut self, can_back: bool, can_forward: bool, cx: &mut Context<Self>) {
        self.can_go_back = can_back;
        self.can_go_forward = can_forward;
        cx.notify();
    }

    /// Applies the app-wide hidden-files setting; the tree re-filters the rows it read.
    pub fn set_shows_hidden_files(&mut self, shows: bool, cx: &mut Context<Self>) {
        self.model
            .update(cx, |model, cx| model.set_shows_hidden_files(shows, cx));
        if let Some(parts) = &self.changes {
            parts
                .model
                .update(cx, |model, _| model.set_shows_hidden_files(shows));
        }
        self.changes_stale(cx);
    }

    /// Where this tab is, or `None` while it browses nothing: no repository yet, or one still
    /// attaching. Such a stop would have nothing to return to.
    #[must_use]
    pub fn current_stop(&self, cx: &gpui::App) -> Option<NavigationStop> {
        let shown = self.shown.as_ref()?;
        let browser = self.model.read(cx).location();
        if browser.tree == rc_git_kit::back_forward::BrowsedTree::Nothing {
            return None;
        }
        Some(NavigationStop {
            repository_path: shown.path.clone(),
            mode: self.mode,
            browser,
        })
    }

    fn record_stop(&mut self, cx: &mut Context<Self>) {
        if let Some(stop) = self.current_stop(cx) {
            cx.emit(WindowEvent::Stop(stop));
        }
    }

    /// Shows the pane of `mode` and the section that belongs to it in the sidebar.
    pub fn set_mode(&mut self, mode: ContentMode, cx: &mut Context<Self>) {
        self.close_picker(cx);
        self.mode = mode;
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_mode(mode, cx));
        self.sync_changes_visibility(cx);
        if mode == ContentMode::Changes {
            self.changes_shown(cx);
        }
        cx.notify();
    }

    /// A pane the user asked for by itself, which is a stop of its own.
    pub fn switch_mode(&mut self, mode: ContentMode, cx: &mut Context<Self>) {
        self.set_mode(mode, cx);
        self.record_stop(cx);
    }

    /// Shows a stop of the back/forward list: its pane, then what its browser was on.
    pub fn restore(&mut self, stop: NavigationStop, cx: &mut Context<Self>) -> Task<()> {
        if self.mode != stop.mode {
            self.set_mode(stop.mode, cx);
        }
        self.model
            .update(cx, |model, cx| model.restore(stop.browser, cx))
    }

    /// ⌘B: the picker under the banner in Files; in History the sidebar's own filter.
    pub fn choose_ref(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == ContentMode::History {
            window.focus(&self.refs.focus_handle(cx), cx);
            return;
        }
        self.open_picker(window, cx);
    }

    fn can_open_picker(&self) -> bool {
        can_open_ref_picker(self.shown.is_some(), self.mode, self.picker.is_some())
    }

    fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_open_picker() {
            return;
        }
        if self.layout.collapsed() {
            self.layout.toggle_by_user();
        }
        let picker = cx.new(|cx| RefListView::new(self.model.clone(), window, cx));
        picker.update(cx, RefListView::set_picker);
        self.picker_events = Some(cx.subscribe(&picker, |this, _, event: &RefListEvent, cx| {
            this.picked(event, cx);
        }));
        window.focus(&picker.focus_handle(cx), cx);
        self.picker = Some(picker);
        cx.notify();
    }

    fn toggle_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            self.close_picker(cx);
            return;
        }
        self.open_picker(window, cx);
    }

    fn close_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            self.picker_events = None;
            cx.notify();
        }
    }

    fn picked(&mut self, event: &RefListEvent, cx: &mut Context<Self>) {
        self.close_picker(cx);
        match event {
            RefListEvent::Picked(entry) => {
                let entry = entry.clone();
                self.model
                    .update(cx, |model, cx| model.select(entry, cx))
                    .detach();
            }
            RefListEvent::WorkingTree => {
                self.set_mode(ContentMode::Files, cx);
                self.model
                    .update(cx, BrowserModel::select_working_tree)
                    .detach();
            }
            RefListEvent::Dismissed => {}
            RefListEvent::Action(action, entry) => self.ref_action(*action, entry.clone(), cx),
        }
    }

    /// The window's own changes: its size, whether it is in front, its appearance, and its
    /// tab group, which the tab bar draws.
    fn window_observers(window: &mut Window, cx: &mut Context<Self>) -> [Subscription; 4] {
        let id = window.window_handle().window_id();
        let mut shown = tab_bar::shown_tabs(id, cx);
        [
            cx.observe_window_bounds(window, |this, window, cx| {
                this.layout.window_resized(window, cx);
                cx.notify();
            }),
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    cx.emit(WindowEvent::BecameFront);
                }
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                gpui_component::Theme::sync_system_appearance(Some(window), cx);
            }),
            cx.observe_global::<SystemWindowTabController>(move |_, cx| {
                let now = tab_bar::shown_tabs(id, cx);
                if now != shown {
                    shown = now;
                    cx.notify();
                }
            }),
        ]
    }

    /// The History pane, its events, and the blame window following the model.
    fn history_parts(
        model: &Entity<BrowserModel>,
        settings: &SharedSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<HistoryPane>, Subscription, Subscription) {
        let history = cx.new(|cx| {
            let clock = Clock {
                offset_at: rc_git_mac::local_utc_offset,
                now: crate::history::now(),
            };
            HistoryPane::new(model.clone(), Arc::clone(settings), clock, window, cx)
        });
        let history_events = cx.subscribe(&history, |this, _, event: &HistoryEvent, cx| {
            this.history_event(event, cx);
        });
        let blame_events = cx.subscribe_in(model, window, |this, _, event, window, cx| {
            if *event == BrowserEvent::BlameChanged {
                this.blame_changed(window, cx);
            }
        });
        cx.on_release(|this: &mut Self, cx| {
            this.model.update(cx, BrowserModel::cancel_blame);
            this.blame.close(cx);
        })
        .detach();
        (history, history_events, blame_events)
    }

    /// The History pane.
    #[cfg(test)]
    pub(crate) fn history(&self) -> &Entity<HistoryPane> {
        &self.history
    }

    /// The blame window's controller.
    #[cfg(test)]
    pub(crate) fn blame(&self) -> &BlamePanel {
        &self.blame
    }

    /// Shows the detail's Back button while stepping back returns to another commit.
    pub fn set_detail_can_go_back(&mut self, can: bool, cx: &mut Context<Self>) {
        self.history
            .update(cx, |history, cx| history.set_detail_can_go_back(can, cx));
    }

    fn history_event(&mut self, event: &HistoryEvent, cx: &mut Context<Self>) {
        match event {
            HistoryEvent::BrowseCommit(commit) => {
                self.set_mode(ContentMode::Files, cx);
                let commit = commit.clone();
                let browse = self
                    .model
                    .update(cx, |model, cx| model.browse_commit(commit, cx));
                Self::record_stop_after(browse, cx);
            }
            HistoryEvent::DetailBack => cx.emit(WindowEvent::Navigate(Direction::Back)),
        }
    }

    /// Records where the window is once `task` ends: an action that changed the pane on its
    /// way somewhere is one stop, where it ended.
    fn record_stop_after(task: Task<()>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            task.await;
            this.update(cx, Self::record_stop).ok();
        })
        .detach();
    }

    /// Follows the model's blame: the second window shows, fills or closes; a failure is
    /// presented here.
    fn blame_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.model.read(cx).blame().clone();
        if let Some(message) = self.blame.render(&state, cx) {
            drop(window.prompt(
                gpui::PromptLevel::Warning,
                "Cannot show blame",
                Some(&message),
                &["OK"],
                cx,
            ));
        }
    }

    /// Show Blame (⌥⌘B): blames the open file at the browsed revision.
    fn show_blame(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, BrowserModel::request_blame).detach();
    }

    /// History of This File (⌥⌘Y): scopes History to the open file and shows it. An inexact
    /// path is refused by the model and leaves the pane where it is.
    fn show_file_history(&mut self, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let Some(path) = model.opened_file_path().map(str::to_owned) else {
            return;
        };
        let exact = model.opened_path_is_exact();
        if !exact {
            self.model
                .update(cx, |model, cx| model.show_path_history(&path, false, cx))
                .detach();
            return;
        }
        self.set_mode(ContentMode::History, cx);
        let scope = self
            .model
            .update(cx, |model, cx| model.show_path_history(&path, true, cx));
        Self::record_stop_after(scope, cx);
    }

    /// Search Commits (⇧⌘F) outside Files: shows History and puts the keyboard in its search.
    fn search_commits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switch_mode(ContentMode::History, cx);
        self.history
            .update(cx, |history, cx| history.focus_search(window, cx));
    }

    /// Whether History and Changes show their diff across the whole window, the sidebar and
    /// History's left column out of the way. Files has no diff and keeps its layout. The
    /// History pane holds the flag; the Changes pane is told it.
    #[must_use]
    pub fn diff_expanded(&self, cx: &gpui::App) -> bool {
        self.history.read(cx).diff_expanded()
    }

    /// Whether the content pane draws only its diff now, so the sidebar steps aside: History
    /// with a commit open, or Changes, while expanded.
    fn diff_fills_window(&self, cx: &gpui::App) -> bool {
        match self.mode {
            ContentMode::Files => false,
            ContentMode::History => self.history.read(cx).fills_with_diff(cx),
            ContentMode::Changes => self.diff_expanded(cx),
        }
    }

    /// Shows the diff across the whole window, or puts the sidebar and History's left column
    /// back. Files has no diff, so the toggle does nothing there.
    pub fn toggle_diff_expanded(&mut self, cx: &mut Context<Self>) {
        if self.mode == ContentMode::Files {
            return;
        }
        let expanded = !self.diff_expanded(cx);
        self.history
            .update(cx, |history, cx| history.set_diff_expanded(expanded, cx));
        if let Some(parts) = &self.changes {
            parts
                .pane
                .update(cx, |pane, cx| pane.set_expanded(expanded, cx));
        }
        cx.emit(WindowEvent::MenusStale);
        cx.notify();
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.layout.toggle_by_user();
        cx.notify();
    }

    fn render_content(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.mode == ContentMode::Changes
            && let Some(parts) = &self.changes
        {
            return parts.pane.clone().into_any_element();
        }
        match self.mode {
            ContentMode::Files => self.pane.clone().into_any_element(),
            ContentMode::History => self.history.clone().into_any_element(),
            ContentMode::Changes => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(style::TEXT_BODY)
                .text_color(cx.theme().muted_foreground)
                .child(PANE_PENDING)
                .into_any_element(),
        }
    }

    fn render_notice(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let notice = self.notice.as_ref()?;
        let theme = cx.theme();
        Some(
            div()
                .id("restore-notice")
                .flex()
                .items_center()
                .gap(style::ROW_GAP)
                .px(style::MARGIN)
                .py(style::BAR_PADDING)
                .border_b(style::HAIRLINE)
                .border_color(theme.border)
                .bg(theme.secondary)
                .text_size(style::TEXT_CAPTION)
                .child(
                    gpui_component::Icon::new(crate::icons::icon(crate::icons::Glyph::Warning))
                        .size(style::ICON_SIZE)
                        .text_color(theme.warning),
                )
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .text_color(theme.foreground)
                        .child(SharedString::from(notice.summary.clone())),
                )
                .child(
                    gpui_component::button::Button::new("dismiss-notice")
                        .label("Dismiss")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        })),
                ),
        )
    }

    fn render_picker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let picker = self.picker.clone()?;
        let theme = cx.theme();
        Some(deferred(
            div()
                .id("ref-picker")
                .occlude()
                .absolute()
                .top(style::TOOLBAR_HEIGHT + style::PICKER_OFFSET)
                .left(style::MARGIN)
                .w(style::PICKER_WIDTH)
                .h(style::PICKER_HEIGHT)
                .rounded(style::RADIUS_LG)
                .border(style::HAIRLINE)
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_picker(cx)))
                .child(picker),
        ))
    }
}

/// Which History commands can run now; a command that cannot has no handler, so GPUI shows
/// it disabled (`validateMenuItem` in Swift).
#[derive(Debug, Clone, Copy)]
struct HistoryCommands {
    blame: bool,
    file_history: bool,
    search_commits: bool,
}

impl HistoryCommands {
    fn of(window: &RepoWindow, cx: &gpui::App) -> Self {
        let model = window.model.read(cx);
        Self {
            blame: model.can_blame_opened_file(),
            file_history: model.opened_file_path().is_some(),
            search_commits: window.shown.is_some() && window.mode != ContentMode::Files,
        }
    }
}

fn history_actions(
    root: gpui::Stateful<gpui::Div>,
    commands: HistoryCommands,
    cx: &mut Context<RepoWindow>,
) -> gpui::Stateful<gpui::Div> {
    root.when(commands.blame, |root| {
        root.on_action(cx.listener(|this, _: &ShowBlame, _, cx| this.show_blame(cx)))
    })
    .when(commands.file_history, |root| {
        root.on_action(cx.listener(|this, _: &ShowFileHistory, _, cx| {
            this.show_file_history(cx);
        }))
    })
    .when(commands.search_commits, |root| {
        root.on_action(cx.listener(|this, _: &SearchCommits, window, cx| {
            this.search_commits(window, cx);
        }))
    })
}

impl Render for RepoWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_fit(window, cx);
        let toolbar = self.render_toolbar(window, cx);
        let tab_bar = tab_bar::render(window, cx);
        let notice = self.render_notice(cx);
        let content = self.render_content(cx);
        let picker = self.render_picker(cx);
        let collapsed = self.layout.collapsed() || self.diff_fills_window(cx);
        let sidebar_width = self.layout.width_pixels();
        let (background, foreground) = (cx.theme().background, cx.theme().foreground);
        let commands = HistoryCommands::of(self, cx);
        let availability = self.sync_availability(cx);
        let has_author = self
            .changes
            .as_ref()
            .is_some_and(|parts| parts.author.is_some());
        let (shown, mode) = (self.shown.is_some(), self.mode);
        let panels = self.render_panels();
        div()
            .id("repo-window")
            .key_context("RepoWindow")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .text_color(foreground)
            .on_action(cx.listener(|this, _: &ShowFiles, _, cx| {
                this.switch_mode(ContentMode::Files, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowHistory, _, cx| {
                this.switch_mode(ContentMode::History, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowChanges, _, cx| {
                this.switch_mode(ContentMode::Changes, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToBranch, window, cx| this.choose_ref(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleDiffExpanded, _, cx| {
                this.toggle_diff_expanded(cx);
            }))
            .map(|root| history_actions(root, commands, cx))
            .map(|root| sync::sync_actions(root, availability, has_author, cx))
            .map(|root| panels::panel_actions(root, shown, mode, cx))
            .on_action(cx.listener(|_, _: &ToggleFullScreen, window, _| window.toggle_fullscreen()))
            .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &Zoom, window, _| window.zoom_window()))
            .on_action(cx.listener(|_, _: &CloseTab, window, _| window.remove_window()))
            .on_action(cx.listener(|_, _: &MergeAllWindows, window, _| window.merge_all_windows()))
            .on_action(cx.listener(|_, _: &MoveTabToNewWindow, window, _| {
                window.move_tab_to_new_window();
            }))
            .on_action(cx.listener(|_, _: &NextTab, window, cx| {
                SystemWindowTabController::select_next_tab(cx, window.window_handle().window_id());
            }))
            .on_action(cx.listener(|_, _: &PreviousTab, window, cx| {
                SystemWindowTabController::select_previous_tab(
                    cx,
                    window.window_handle().window_id(),
                );
            }))
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|_, _, _, cx| cx.emit(WindowEvent::Navigate(Direction::Back))),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|_, _, _, cx| cx.emit(WindowEvent::Navigate(Direction::Forward))),
            )
            .on_drop(cx.listener(|_, paths: &ExternalPaths, _, cx| {
                cx.emit(WindowEvent::OpenPaths(paths.paths().to_vec()));
            }))
            .child(toolbar)
            .children(tab_bar)
            .children(notice)
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("main-split")
                        .with_state(&self.split)
                        .child(
                            resizable_panel()
                                .visible(!collapsed)
                                .size(sidebar_width)
                                .size_range(layout::sidebar_range())
                                .child(self.sidebar.clone()),
                        )
                        .child(resizable_panel().child(content)),
                ),
            )
            .when_some(picker, gpui::ParentElement::child)
            .children(panels)
    }
}

//! One repository window (`MainWindowController`): the app-drawn toolbar in the transparent
//! titlebar, the sidebar and the mode's pane in a resizable split, the ref picker under the
//! banner, and the restore notice. It owns the window's `BrowserModel` and `SyncModel`, and
//! tells the shell where it is through [`WindowEvent`]s; the shell owns the app-wide
//! back/forward list.

pub mod layout;
mod toolbar;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, NavigationDirection, ParentElement, Render,
    SharedString, Styled, Subscription, Task, Window, deferred, div, prelude::FluentBuilder,
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
use crate::files::pane::FilePane;
use crate::files::tree::FileTreeView;
use crate::inbox::Inbox;
use crate::menus::{
    CloseTab, GoToBranch, MergeAllWindows, Minimize, MoveTabToNewWindow, NextTab, PreviousTab,
    ShowChanges, ShowFiles, ShowHistory, ToggleFullScreen, ToggleSidebar, Zoom,
};
use crate::sidebar::refs::{RefListEvent, RefListView};
use crate::sidebar::{Sidebar, SidebarEvent};
use crate::style;

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
    /// Where the menu commands land when nothing inside the window has the keyboard.
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
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
        let split = cx.new(|_| ResizableState::default());
        let layout = SidebarLayout::load(settings);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let subscriptions = vec![
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
                let width = split.read(cx).sizes().first().copied();
                if let Some(width) = width {
                    this.layout.dragged(f64::from(f32::from(width)));
                }
            }),
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
        ];
        Self {
            model,
            sync: None,
            mode: ContentMode::Files,
            sidebar,
            refs,
            pane,
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
            focus,
            _subscriptions: subscriptions,
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
        self.shown = Some(Shown {
            path: repository.tab_path.clone(),
            key: repository.key.clone(),
        });
        self.has_work_dir = repository.reading.work_dir().is_some();
        self.watch_refs(&repository, watchers, cx);
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
        }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.layout.toggle_by_user();
        cx.notify();
    }

    fn render_content(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        match self.mode {
            ContentMode::Files => self.pane.clone().into_any_element(),
            ContentMode::History | ContentMode::Changes => div()
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
                .rounded(style::RADIUS)
                .border(style::HAIRLINE)
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_picker(cx)))
                .child(picker),
        ))
    }
}

impl Render for RepoWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toolbar = self.render_toolbar(window, cx);
        let notice = self.render_notice(cx);
        let content = self.render_content(cx);
        let picker = self.render_picker(cx);
        let collapsed = self.layout.collapsed();
        let sidebar_width = self.layout.width_pixels();
        let (background, foreground) = (cx.theme().background, cx.theme().foreground);
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
            .on_action(cx.listener(|_, _: &ToggleFullScreen, window, _| window.toggle_fullscreen()))
            .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &Zoom, window, _| window.zoom_window()))
            .on_action(cx.listener(|_, _: &CloseTab, window, _| window.remove_window()))
            .on_action(cx.listener(|_, _: &MergeAllWindows, window, _| window.merge_all_windows()))
            .on_action(cx.listener(|_, _: &MoveTabToNewWindow, window, _| {
                window.move_tab_to_new_window();
            }))
            .on_action(cx.listener(|_, _: &NextTab, window, cx| {
                gpui::SystemWindowTabController::select_next_tab(
                    cx,
                    window.window_handle().window_id(),
                );
            }))
            .on_action(cx.listener(|_, _: &PreviousTab, window, cx| {
                gpui::SystemWindowTabController::select_previous_tab(
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
    }
}

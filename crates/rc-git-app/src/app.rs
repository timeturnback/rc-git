//! The app shell (`AppDelegate`): every way a repository arrives goes through
//! [`Shell::open`]; it owns the windows, the saved tabs and their restore, the recents, the
//! ⇧⌘T stack, the app-wide hidden-files setting and the one back/forward [`Navigator`].

use std::cell::Cell;
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, AppContext, Bounds, Context, Entity, Global, PathPromptOptions, PromptLevel, Size,
    Subscription, SystemWindowTabController, TitlebarOptions, WindowBounds, WindowHandle, WindowId,
    WindowOptions, point, px,
};
use gpui_component::Root;
use rc_git_kit::back_forward::{ContentMode, Direction};
use rc_git_kit::canonical_path;
use rc_git_kit::navigator::Navigator;
use rc_git_kit::platform::credentials::CredentialStore;
use rc_git_kit::platform::host_keys::HostKeyStore;
use rc_git_kit::platform::keys::MAIN_WINDOW_FRAME;
use rc_git_kit::platform::stores::{
    HiddenFilesPreference, RecentRepositories, SettingsTabStore, SharedSettings, WindowFrame,
};
use rc_git_kit::platform::watch::WatcherFactory;
use rc_git_kit::repository::{LiveRepository, RepositoryReading};
use rc_git_kit::repository_actions::{LiveActions, RepositoryActions};
use rc_git_kit::restore_notice::RestoreNotice;
use rc_git_kit::sync::SyncModel;
use rc_git_kit::tab_routing::{Route, TabIdentity, by_path, route};
use rc_git_kit::tabs::{
    ClosedTabs, OpenTabs, directory_exists, restored_paths, restored_selection,
};
use rc_git_kit::window_geometry::{DEFAULT_HEIGHT, DEFAULT_WIDTH, restorable};
use rc_git_mac::TabSession;

use crate::log;
use crate::menus::MenuState;
use crate::style;
use crate::window::layout::{primary_height, top_left};
use crate::window::{RepoWindow, WindowEvent};

/// Shared by every window, so macOS gathers them into one tab group.
pub const TABBING_IDENTIFIER: &str = "rc-git.browser";

/// What opening a path produced, before a window holds it.
pub struct Opened {
    /// The repository.
    pub reading: Arc<dyn RepositoryReading>,
    /// What tells it apart from every other open repository.
    pub key: String,
    /// The path the tab shows and saves: the working directory even when `.git` was named.
    pub tab_path: String,
    /// The git directory and the common directory, for the ref watcher.
    pub dirs: Option<(String, String)>,
    /// The guarded actions sync runs, `None` where there are none (tests).
    pub actions: Option<Arc<dyn RepositoryActions>>,
}

/// A repository a window shows, with its sync model.
pub struct OpenedRepository {
    /// The repository.
    pub reading: Arc<dyn RepositoryReading>,
    /// What tells it apart from every other open repository.
    pub key: String,
    /// The path the tab shows and saves.
    pub tab_path: String,
    /// The git directory and the common directory, for the ref watcher.
    pub dirs: Option<(String, String)>,
    /// The window's sync workflows.
    pub sync: Option<Entity<SyncModel>>,
    /// The guarded actions commit, stash, discard and the ref menu run, `None` where there
    /// are none (tests).
    pub actions: Option<Arc<dyn RepositoryActions>>,
}

/// Opens a canonical path as a repository.
pub type Opener = Rc<dyn Fn(&str) -> Result<Opened, String>>;

/// Everything outside the app the shell stands on, so tests swap in fakes.
#[derive(Clone)]
pub struct Services {
    /// Where tabs, recents, frames and preferences persist.
    pub settings: SharedSettings,
    /// Opens a repository.
    pub opener: Opener,
    /// Makes the ref watchers.
    pub watchers: Arc<dyn WatcherFactory>,
    /// Secrets for sync.
    pub credentials: Arc<dyn CredentialStore>,
    /// Approved ssh host keys for sync.
    pub host_keys: Arc<dyn HostKeyStore>,
    /// The home directory Open Recent abbreviates.
    pub home: String,
    /// The app's name in the menus.
    pub app_name: String,
}

/// The live opener: the core's repository, keyed and titled by its canonical paths.
#[must_use]
pub fn live_opener() -> Opener {
    Rc::new(|path: &str| {
        let repository = LiveRepository::open(path).map_err(|error| error.message().to_owned())?;
        let key = canonical_path::resolve(repository.git_dir());
        let tab_path = match repository.work_dir() {
            Some(work_dir) => canonical_path::resolve(&work_dir),
            None => canonical_path::resolve(Path::new(path)),
        };
        let tab_path = tab_path.to_string_lossy().into_owned();
        let dirs = Some((
            repository.git_dir().to_string_lossy().into_owned(),
            repository.common_dir().to_string_lossy().into_owned(),
        ));
        let actions: Arc<dyn RepositoryActions> = Arc::new(LiveActions::new(&tab_path));
        Ok(Opened {
            reading: Arc::new(repository),
            key: key.to_string_lossy().into_owned(),
            tab_path,
            dirs,
            actions: Some(actions),
        })
    })
}

/// One open window.
#[derive(Clone)]
pub struct Tab {
    /// The platform window.
    pub handle: WindowHandle<Root>,
    /// Its view.
    pub view: Entity<RepoWindow>,
}

impl Tab {
    /// The window's id, which names the tab to the navigator.
    #[must_use]
    pub fn id(&self) -> WindowId {
        self.handle.window_id()
    }
}

/// The shell, as a global.
pub struct ShellGlobal(pub Entity<Shell>);

impl Global for ShellGlobal {}

/// The app shell. See the module documentation.
pub struct Shell {
    services: Services,
    tabs: TabSession,
    recents: RecentRepositories,
    hidden: HiddenFilesPreference,
    closed: ClosedTabs,
    navigator: Navigator<WindowId>,
    windows: Vec<Tab>,
    /// Set while ⇧⌘W closes a window group and once the app is quitting: tabs closed then
    /// stay in the saved list. A tab closed on its own leaves it.
    keeping_closed_tabs: bool,
    /// True until the launch restore has run; a repository opened before then launched the
    /// app and keeps its tab selected over the saved selection.
    launching: bool,
    launch_document: Option<String>,
    subscriptions: Vec<Subscription>,
    /// The Settings window and its view, once opened.
    settings_window: Option<(WindowHandle<Root>, Entity<crate::settings::SettingsView>)>,
}

impl Shell {
    /// A shell with no window.
    pub fn new(services: Services, cx: &mut Context<Self>) -> Self {
        let settings = Arc::clone(&services.settings);
        let quit = cx.on_app_quit(|this, cx| {
            this.keeping_closed_tabs = true;
            this.save_tabs(None, cx);
            async {}
        });
        Self {
            tabs: TabSession::new(Arc::clone(&settings)),
            recents: RecentRepositories::new(Arc::clone(&settings)),
            hidden: HiddenFilesPreference::new(settings),
            closed: ClosedTabs::new(),
            navigator: Navigator::new(),
            windows: Vec::new(),
            keeping_closed_tabs: false,
            launching: true,
            launch_document: None,
            services,
            subscriptions: vec![quit],
            settings_window: None,
        }
    }

    /// The open windows, in the order they were created.
    #[must_use]
    pub fn windows(&self) -> &[Tab] {
        &self.windows
    }

    /// The back/forward list and what it records.
    #[cfg(test)]
    pub(crate) fn navigator(&self) -> &Navigator<WindowId> {
        &self.navigator
    }

    /// The menu bar's state now.
    #[must_use]
    pub fn menu_state(&self, cx: &App) -> MenuState {
        let front = self.front(cx);
        MenuState {
            app_name: self.services.app_name.clone(),
            recents: self.recents.paths(),
            home: self.services.home.clone(),
            shows_hidden_files: self.hidden.shows(),
            mode: front.map_or(ContentMode::Files, |tab| tab.view.read(cx).mode()),
            default_branch: front.and_then(|tab| tab.view.read(cx).default_branch(cx)),
            sync_titles: front.map(|tab| tab.view.read(cx).sync_titles(cx)),
        }
    }

    /// Rebuilds the menu bar from [`Self::menu_state`].
    pub fn rebuild_menus(&self, cx: &mut App) {
        let state = self.menu_state(cx);
        cx.set_menus(crate::menus::menus(&state));
    }

    fn front(&self, cx: &App) -> Option<&Tab> {
        let active = cx.active_window().map(|window| window.window_id());
        self.windows
            .iter()
            .find(|tab| Some(tab.id()) == active)
            .or_else(|| self.windows.last())
    }

    fn identities(&self, cx: &App) -> Vec<(Option<String>, Option<String>)> {
        self.windows
            .iter()
            .map(|tab| {
                let shown = tab.view.read(cx).shown();
                (
                    shown.map(|shown| shown.path.clone()),
                    shown.map(|shown| shown.key.clone()),
                )
            })
            .collect()
    }

    /// Opens a repository from any arrival — argv, the system's open callback, the Open
    /// panel, a folder drop, Open Recent — and alerts in the front window when it does not
    /// open. A repository already open gets its tab brought forward. A new tab is noted in
    /// the recents and the saved tabs; one opened before the launch restore is the launch
    /// document, whose tab stays selected.
    pub fn open(&mut self, path: &str, cx: &mut Context<Self>) -> Option<WindowId> {
        let (id, is_new) = match self.open_tab(path, cx) {
            Ok(opened) => opened,
            Err(error) => {
                log::error(&format!("open: could not open {path}: {error}"));
                self.alert("Could not open repository", &error, cx);
                return None;
            }
        };
        let shown = self
            .windows
            .iter()
            .find(|tab| tab.id() == id)
            .and_then(|tab| tab.view.read(cx).shown().map(|shown| shown.path.clone()));
        if self.launching {
            self.launch_document.clone_from(&shown);
        }
        if is_new && let Some(shown) = shown {
            log::notice(&format!("opened {shown}"));
            if let Err(error) = self.recents.note(&shown) {
                log::error(&error.to_string());
            }
            self.save_tabs(None, cx);
            self.rebuild_menus(cx);
        }
        Some(id)
    }

    /// Routes `path` to a tab: dedupes by canonical path, then by tab key, and fills an empty
    /// tab before opening a new one. Answers the tab and whether it newly shows the
    /// repository.
    ///
    /// # Errors
    /// The opener's message when `path` is not a repository.
    pub fn open_tab(
        &mut self,
        path: &str,
        cx: &mut Context<Self>,
    ) -> Result<(WindowId, bool), String> {
        let canonical = canonical_path::resolve(Path::new(path))
            .to_string_lossy()
            .into_owned();
        let identities = self.identities(cx);
        let tabs: Vec<TabIdentity<'_>> = identities
            .iter()
            .map(|(path, key)| TabIdentity {
                path: path.as_deref(),
                key: key.as_deref(),
            })
            .collect();
        if let Some(index) = by_path(&tabs, &canonical) {
            return Ok((self.bring_forward(index, cx), false));
        }
        let opened = (self.services.opener)(&canonical)?;
        let target = match route(&tabs, &opened.key) {
            Route::Existing(index) => return Ok((self.bring_forward(index, cx), false)),
            Route::Reuse(index) => self.windows.get(index).cloned(),
            Route::New => self.new_window(cx),
        };
        let Some(tab) = target else {
            return Err("no window could be opened".to_owned());
        };
        self.show_in(&tab, opened, cx);
        Ok((tab.id(), true))
    }

    fn alert(&self, title: &str, detail: &str, cx: &mut Context<Self>) {
        let Some(front) = self.front(cx).cloned() else {
            return;
        };
        let (title, detail) = (title.to_owned(), detail.to_owned());
        front
            .handle
            .update(cx, |_, window, cx| {
                drop(window.prompt(PromptLevel::Warning, &title, Some(&detail), &["OK"], cx));
            })
            .ok();
    }

    fn bring_forward(&mut self, index: usize, cx: &mut Context<Self>) -> WindowId {
        let tab = self.windows[index].clone();
        tab.handle
            .update(cx, |_, window, _| window.activate_window())
            .ok();
        tab.id()
    }

    fn show_in(&mut self, tab: &Tab, opened: Opened, cx: &mut Context<Self>) {
        let actions = opened.actions.clone();
        let sync = opened.actions.map(|actions| {
            let credentials = Arc::clone(&self.services.credentials);
            let host_keys = Arc::clone(&self.services.host_keys);
            cx.new(|_| SyncModel::new(actions, credentials, host_keys))
        });
        let repository = OpenedRepository {
            reading: opened.reading,
            key: opened.key,
            tab_path: opened.tab_path,
            dirs: opened.dirs,
            sync,
            actions,
        };
        let watchers = Arc::clone(&self.services.watchers);
        let view = tab.view.clone();
        tab.handle
            .update(cx, |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.show(repository, &*watchers, window, cx).detach();
                });
                window.activate_window();
            })
            .ok();
    }

    /// Opens an empty window in the tab group, at the saved frame when it meets a screen.
    pub fn new_window(&mut self, cx: &mut Context<Self>) -> Option<Tab> {
        let options = self.window_options(cx);
        let settings = Arc::clone(&self.services.settings);
        let shows_hidden = self.hidden.shows();
        let made: Rc<Cell<Option<Entity<RepoWindow>>>> = Rc::default();
        let slot = Rc::clone(&made);
        let handle = cx.open_window(options, move |window, cx| {
            let view = cx.new(|cx| RepoWindow::new(settings, window, cx));
            view.update(cx, |view, cx| view.set_shows_hidden_files(shows_hidden, cx));
            slot.set(Some(view.clone()));
            cx.new(|cx| Root::new(view, window, cx))
        });
        let handle = match handle {
            Ok(handle) => handle,
            Err(error) => {
                log::error(&format!("open window: {error:#}"));
                return None;
            }
        };
        let view = made.take()?;
        let tab = Tab { handle, view };
        self.watch(&tab, cx);
        self.windows.push(tab.clone());
        let (can_back, can_forward) = self.can_go();
        tab.view.update(cx, |view, cx| {
            view.render_navigation(can_back, can_forward, cx);
        });
        Some(tab)
    }

    fn window_options(&self, cx: &App) -> WindowOptions {
        let saved = WindowFrame::load(&*self.services.settings, MAIN_WINDOW_FRAME);
        let screens: Vec<_> = cx
            .displays()
            .iter()
            .map(|display| top_left(display.visible_bounds()))
            .collect();
        let restored = saved
            .zip(primary_height(cx))
            .and_then(|(saved, primary)| restorable(&saved, primary, &screens));
        let bounds = match restored {
            Some(rect) => Bounds::new(
                point(px_of(rect.x), px_of(rect.y)),
                Size::new(px_of(rect.width), px_of(rect.height)),
            ),
            None => Bounds::centered(
                None,
                Size::new(px_of(DEFAULT_WIDTH), px_of(DEFAULT_HEIGHT)),
                cx,
            ),
        };
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("rc-git".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(style::TRAFFIC_LIGHT_X, style::TRAFFIC_LIGHT_Y)),
            }),
            window_min_size: Some(Size::new(
                px_of(rc_git_kit::layout_policy::WINDOW_MINIMUM_WIDTH),
                px_of(rc_git_kit::layout_policy::WINDOW_MINIMUM_HEIGHT),
            )),
            tabbing_identifier: Some(TABBING_IDENTIFIER.to_owned()),
            ..WindowOptions::default()
        }
    }

    fn watch(&mut self, tab: &Tab, cx: &mut Context<Self>) {
        let id = tab.id();
        self.subscriptions.push(cx.subscribe(
            &tab.view,
            move |this, view, event: &WindowEvent, cx| match event {
                WindowEvent::Stop(stop) => {
                    if this.navigator.report(stop.clone(), &id) {
                        this.render_navigation(cx);
                    }
                }
                WindowEvent::Navigate(direction) => this.go(*direction, cx),
                WindowEvent::OpenPaths(paths) => {
                    for path in paths {
                        this.open(&path.to_string_lossy(), cx);
                    }
                }
                WindowEvent::MenusStale => this.rebuild_menus(cx),
                WindowEvent::BecameFront => {
                    let stop = view.read(cx).current_stop(cx);
                    if this.navigator.tab_came_to_front(&id, stop) {
                        this.render_navigation(cx);
                    }
                    this.rebuild_menus(cx);
                }
            },
        ));
        self.subscriptions
            .push(cx.observe_release(&tab.view, move |this, view, cx| {
                let path = view.shown().map(|shown| shown.path.clone());
                this.window_closed(id, path.as_deref(), cx);
            }));
    }

    fn window_closed(&mut self, id: WindowId, path: Option<&str>, cx: &mut Context<Self>) {
        if let Some(path) = path {
            self.closed.push(path);
        }
        self.windows.retain(|tab| tab.id() != id);
        let dropping = if self.keeping_closed_tabs { None } else { path };
        self.save_tabs(dropping, cx);
        self.navigator.tab_closed(&id, path);
        self.render_navigation(cx);
    }

    fn can_go(&self) -> (bool, bool) {
        (
            self.navigator.can_go(Direction::Back),
            self.navigator.can_go(Direction::Forward),
        )
    }

    fn render_navigation(&self, cx: &mut Context<Self>) {
        let (can_back, can_forward) = self.can_go();
        for tab in &self.windows {
            tab.view.update(cx, |view, cx| {
                view.render_navigation(can_back, can_forward, cx);
                let path = view.shown().map(|shown| shown.path.clone());
                let to_commit =
                    path.is_some_and(|path| self.navigator.list().back_returns_to_commit(&path));
                view.set_detail_can_go_back(to_commit, cx);
            });
        }
    }

    /// Steps to the stop before or after the current one: brings its tab to the front and has
    /// it show the stop.
    pub fn go(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let Some(stop) = self.navigator.go(direction) else {
            return;
        };
        self.render_navigation(cx);
        let Some(tab) = self
            .windows
            .iter()
            .find(|tab| {
                tab.view.read(cx).shown().map(|shown| shown.path.as_str())
                    == Some(stop.repository_path.as_str())
            })
            .cloned()
        else {
            return;
        };
        self.navigator.begin_restore(&tab.id());
        tab.handle
            .update(cx, |_, window, _| window.activate_window())
            .ok();
        let restore = tab.view.update(cx, |view, cx| view.restore(stop, cx));
        cx.spawn(async move |this, cx| {
            restore.await;
            this.update(cx, |this, _| this.navigator.end_restore()).ok();
        })
        .detach();
    }

    /// Shows or hides hidden files in every tab, and remembers the choice for next launch.
    pub fn toggle_hidden_files(&mut self, cx: &mut Context<Self>) {
        let shows = match self.hidden.toggle() {
            Ok(shows) => shows,
            Err(error) => {
                log::error(&error.to_string());
                !self.hidden.shows()
            }
        };
        for tab in &self.windows {
            tab.view
                .update(cx, |view, cx| view.set_shows_hidden_files(shows, cx));
        }
        self.rebuild_menus(cx);
    }

    /// Reopens the repository of the most recently closed tab.
    pub fn reopen_closed_tab(&mut self, cx: &mut Context<Self>) {
        let open = self.open_paths(cx);
        if let Some(path) = self.closed.pop(&open) {
            self.open(&path, cx);
        }
    }

    fn open_paths(&self, cx: &App) -> HashSet<String> {
        self.identities(cx)
            .into_iter()
            .filter_map(|(path, _)| path)
            .collect()
    }

    /// Closes every tab of the front window group and keeps their repositories in the saved
    /// list; ⌘W closes only the tab on screen and drops it.
    pub fn close_window_group(&mut self, cx: &mut Context<Self>) {
        self.keeping_closed_tabs = true;
        for tab in self.windows.clone() {
            tab.handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
        cx.spawn(async move |this, cx| {
            this.update(cx, |this, _| this.keeping_closed_tabs = false)
                .ok();
        })
        .detach();
    }

    /// Asks for a repository with the Open panel.
    pub fn choose_repository(cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            this.update(cx, |this, cx| {
                for path in paths {
                    this.open(&path.to_string_lossy(), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Open Recent Repository (⌃R): the picker over the front window, or over a new empty one
    /// when none is open.
    pub fn show_recent_picker(&mut self, cx: &mut Context<Self>) {
        let front = self.front(cx).cloned();
        let Some(tab) = front.or_else(|| self.new_window(cx)) else {
            return;
        };
        let (recents, home) = (self.recents.paths(), self.services.home.clone());
        let view = tab.view.clone();
        tab.handle
            .update(cx, |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.show_recent_picker(recents, home, window, cx);
                });
                window.activate_window();
            })
            .ok();
    }

    /// The Settings window and its view, while it is open.
    #[cfg(test)]
    pub(crate) fn settings_window(
        &self,
    ) -> Option<&(WindowHandle<Root>, Entity<crate::settings::SettingsView>)> {
        self.settings_window.as_ref()
    }

    /// Settings (⌘,): brings the window forward with the approvals as they stand now, or opens
    /// it. One Settings window serves every tab, and it never joins the tab group.
    pub fn show_settings(&mut self, cx: &mut Context<Self>) {
        if let Some((handle, view)) = self.settings_window.clone() {
            let shown = handle.update(cx, |_, window, cx| {
                view.update(cx, crate::settings::SettingsView::reload);
                window.activate_window();
            });
            if shown.is_ok() {
                return;
            }
        }
        let store = Arc::clone(&self.services.host_keys);
        let made: Rc<Cell<Option<Entity<crate::settings::SettingsView>>>> = Rc::default();
        let slot = Rc::clone(&made);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                Size::new(style::SETTINGS_WIDTH, style::SETTINGS_HEIGHT),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("Settings".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(style::TRAFFIC_LIGHT_X, style::TRAFFIC_LIGHT_Y)),
            }),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, move |window, cx| {
            let view = cx.new(|_| crate::settings::SettingsView::new(store));
            slot.set(Some(view.clone()));
            cx.new(|cx| Root::new(view, window, cx))
        });
        match (opened, made.take()) {
            (Ok(handle), Some(view)) => self.settings_window = Some((handle, view)),
            (Err(error), _) => log::error(&format!("open settings: {error:#}")),
            (Ok(_), None) => {}
        }
    }

    /// Empties Open Recent.
    pub fn clear_recents(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.recents.clear() {
            log::error(&error.to_string());
        }
        self.rebuild_menus(cx);
    }

    /// Reopens the tabs of the previous run and selects the tab that was selected then, or
    /// the launch document's. Falls back to one empty tab. A repository that no longer opens
    /// is logged, dropped or kept by [`restored_paths`], and named in a notice.
    pub fn restore_tabs(&mut self, cx: &mut Context<Self>) {
        let saved = self.tabs.tabs().paths();
        let saved_selection = i64::try_from(self.tabs.tabs().selected_index()).unwrap_or(0);
        let mut failed = Vec::new();
        for path in &saved {
            if let Err(error) = self.open_tab(path, cx) {
                log::error(&format!("restore tab: could not open {path}: {error}"));
                failed.push(path.clone());
            }
        }
        self.launching = false;
        let (live, _) = self.tab_state(cx);
        log::notice(&format!(
            "restore tabs: {} saved, {} open, {} failed",
            saved.len(),
            live.len(),
            failed.len()
        ));
        let kept = restored_paths(&live, &failed, directory_exists);
        let notice = RestoreNotice::make(&failed, &kept);
        let front = if live.is_empty() {
            self.windows.last().cloned().or_else(|| self.new_window(cx))
        } else {
            let selected = restored_selection(
                &saved,
                saved_selection,
                &live,
                self.launch_document.as_deref(),
            );
            self.save_selected(&kept, selected);
            let wanted = live.get(selected).cloned();
            self.windows
                .iter()
                .find(|tab| tab.view.read(cx).shown().map(|shown| &shown.path) == wanted.as_ref())
                .cloned()
        };
        if live.is_empty() {
            self.save_selected(&kept, 0);
        }
        if let Some(front) = front {
            front
                .handle
                .update(cx, |_, window, _| window.activate_window())
                .ok();
            if let Some(notice) = notice {
                front
                    .view
                    .update(cx, |view, cx| view.show_restore_notice(notice, cx));
            }
        }
    }

    /// Replaces the saved list: only the launch restore writes this way, to drop a
    /// repository that no longer opens.
    fn save_selected(&self, kept: &[String], selected: usize) {
        if self.tabs.is_handed_off() {
            return;
        }
        let store = SettingsTabStore::new(Arc::clone(&self.services.settings));
        OpenTabs::new(store).save(kept, selected);
    }

    /// Records the open repositories in on-screen tab order plus the selected tab. `dropping`
    /// names the repository of a tab closed with ⌘W, the one close that removes it.
    pub fn save_tabs(&mut self, dropping: Option<&str>, cx: &App) {
        let (live, selected) = self.tab_state(cx);
        self.tabs.record(&live, selected, dropping);
    }

    /// The repositories in tab order and the selected tab's index among them.
    #[must_use]
    pub fn tab_state(&self, cx: &App) -> (Vec<String>, usize) {
        let mut ordered: Vec<&Tab> = Vec::new();
        let group = self
            .windows
            .first()
            .zip(cx.try_global::<SystemWindowTabController>())
            .and_then(|(tab, tabs)| tabs.tabs(tab.id()).cloned())
            .unwrap_or_default();
        for system_tab in &group {
            if let Some(tab) = self.windows.iter().find(|tab| tab.id() == system_tab.id) {
                ordered.push(tab);
            }
        }
        for tab in &self.windows {
            if !ordered.iter().any(|seen| seen.id() == tab.id()) {
                ordered.push(tab);
            }
        }
        let active = cx.active_window().map(|window| window.window_id());
        let mut paths = Vec::new();
        let mut selected = 0;
        for tab in ordered {
            let Some(shown) = tab.view.read(cx).shown() else {
                continue;
            };
            if Some(tab.id()) == active {
                selected = paths.len();
            }
            paths.push(shown.path.clone());
        }
        (paths, selected)
    }
}

fn px_of(value: f64) -> gpui::Pixels {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "window geometry in points, far inside f32's range"
    )]
    let value = value as f32;
    px(value)
}

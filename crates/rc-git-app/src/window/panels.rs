//! The quick-pick panels over a repository window (`MainWindowController.goToFile`,
//! `findInRepository`, and the app delegate's recent picker shown over the front window): which
//! one is open, and what a pick does. Opening a pick shows Files, then the file, then records one
//! stop.

use gpui::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, Stateful,
    Subscription, Window, prelude::FluentBuilder,
};
use rc_git_kit::back_forward::ContentMode;
use rc_git_kit::browser::BrowserModel;

use crate::menus::{GoToFile, SearchCommits};
use crate::search::find::{FindEvent, FindPanel};
use crate::search::go_to_file::{GoToFileEvent, GoToFilePanel};
use crate::search::overlay;
use crate::search::recent::{RecentEvent, RecentPanel};
use crate::window::{RepoWindow, WindowEvent};

/// Which panel is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Showing {
    /// Go to File.
    GoToFile,
    /// Find in Repository.
    Find,
    /// Open Recent Repository.
    Recent,
}

/// The panels of one window. Go to File and Find are built on first use and kept, so Find's
/// results outlive a close; the recent picker is built fresh each time it opens.
#[derive(Default)]
pub struct Panels {
    go_to_file: Option<Entity<GoToFilePanel>>,
    find: Option<Entity<FindPanel>>,
    recent: Option<Entity<RecentPanel>>,
    showing: Option<Showing>,
    subscriptions: Vec<Subscription>,
}

impl Panels {
    /// The panel on screen.
    #[cfg(test)]
    pub(crate) fn showing(&self) -> Option<Showing> {
        self.showing
    }

    /// Go to File, once it has opened.
    #[cfg(test)]
    pub(crate) fn go_to_file(&self) -> Option<&Entity<GoToFilePanel>> {
        self.go_to_file.as_ref()
    }

    /// Find in Repository, once it has opened.
    #[cfg(test)]
    pub(crate) fn find(&self) -> Option<&Entity<FindPanel>> {
        self.find.as_ref()
    }

    /// The recent picker, while it is open.
    #[cfg(test)]
    pub(crate) fn recent(&self) -> Option<&Entity<RecentPanel>> {
        self.recent.as_ref()
    }
}

impl RepoWindow {
    /// The window's panels.
    #[cfg(test)]
    pub(crate) fn panels(&self) -> &Panels {
        &self.panels
    }

    /// Go to File (⇧⌘O): the panel, empty, over the root browsed now. The working tree's file
    /// list is walked afresh each time, since files come and go without any ref moving.
    pub fn show_go_to_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown.is_none() {
            return;
        }
        self.close_picker(cx);
        self.model
            .update(cx, |model, _| model.forget_worktree_paths());
        let panel = self.go_to_file_panel(window, cx);
        panel.update(cx, |panel, cx| panel.show(window, cx));
        self.panels.showing = Some(Showing::GoToFile);
        cx.notify();
    }

    /// Find in Repository (⇧⌘F in Files): the panel over the root browsed now, keeping the
    /// results of an earlier search under the same root.
    pub fn show_find_in_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown.is_none() {
            return;
        }
        self.close_picker(cx);
        let scope = find_scope(self.model.read(cx));
        if self.model.read(cx).browsing_working_tree() {
            self.model
                .update(cx, |model, _| model.forget_worktree_paths());
        }
        let panel = self.find_panel(window, cx);
        panel.update(cx, |panel, cx| panel.show(scope, window, cx));
        self.panels.showing = Some(Showing::Find);
        cx.notify();
    }

    fn go_to_file_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<GoToFilePanel> {
        if let Some(panel) = &self.panels.go_to_file {
            return panel.clone();
        }
        let model = self.model.clone();
        let panel = cx.new(|cx| GoToFilePanel::new(model, window, cx));
        let events = cx.subscribe_in(&panel, window, |this, _, event, window, cx| {
            this.go_to_file_event(event, window, cx);
        });
        self.panels.subscriptions.push(events);
        self.panels.go_to_file = Some(panel.clone());
        panel
    }

    fn find_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<FindPanel> {
        if let Some(panel) = &self.panels.find {
            return panel.clone();
        }
        let model = self.model.clone();
        let panel = cx.new(|cx| FindPanel::new(model, window, cx));
        let events = cx.subscribe_in(&panel, window, |this, _, event, window, cx| {
            this.find_event(event, window, cx);
        });
        self.panels.subscriptions.push(events);
        self.panels.find = Some(panel.clone());
        panel
    }

    /// Open Recent Repository (⌃R) over this window: `recents` newest first, `home` as `~`.
    pub fn show_recent_picker(
        &mut self,
        recents: Vec<String>,
        home: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_picker(cx);
        let panel = cx.new(|cx| RecentPanel::new(recents, home, window, cx));
        let events = cx.subscribe_in(
            &panel,
            window,
            |this, _, event: &RecentEvent, window, cx| {
                this.close_panel(window, cx);
                if let RecentEvent::Open(path) = event {
                    cx.emit(WindowEvent::OpenPaths(vec![path.into()]));
                }
            },
        );
        self.panels.subscriptions.push(events);
        panel.update(cx, |panel, cx| panel.focus(window, cx));
        self.panels.recent = Some(panel);
        self.panels.showing = Some(Showing::Recent);
        cx.notify();
    }

    /// Hides the panel on screen and gives the keyboard back to the window. A Return Go to
    /// File was holding for results opens nothing; a Find search keeps filling its list.
    pub fn close_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panels.showing.take() == Some(Showing::GoToFile)
            && let Some(panel) = &self.panels.go_to_file
        {
            panel.update(cx, GoToFilePanel::closed);
        }
        self.panels.recent = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn go_to_file_event(
        &mut self,
        event: &GoToFileEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_panel(window, cx);
        if let GoToFileEvent::Open(entry) = event {
            self.set_mode(ContentMode::Files, cx);
            let open = self
                .model
                .update(cx, |model, cx| model.open_search_result(entry, cx));
            Self::record_stop_after(open, cx);
        }
    }

    fn find_event(&mut self, event: &FindEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.close_panel(window, cx);
        if let FindEvent::Open(entry, line) = event {
            self.set_mode(ContentMode::Files, cx);
            let open = self.model.update(cx, |model, cx| {
                model.open_content_match(entry, line.as_ref(), cx)
            });
            Self::record_stop_after(open, cx);
        }
    }

    /// The panel on screen, laid over the window.
    pub(super) fn render_panels(&self) -> Option<AnyElement> {
        let panel = match self.panels.showing? {
            Showing::GoToFile => self.panels.go_to_file.clone()?.into_any_element(),
            Showing::Find => self.panels.find.clone()?.into_any_element(),
            Showing::Recent => self.panels.recent.clone()?.into_any_element(),
        };
        Some(overlay(panel))
    }
}

/// The root a Find in Repository search runs under: results from another one are dropped.
fn find_scope(model: &BrowserModel) -> String {
    let root = model
        .root_tree()
        .map(|oid| oid.to_hex())
        .unwrap_or_default();
    if model.browsing_working_tree() {
        format!("worktree {root}")
    } else {
        root
    }
}

/// Go to File while a repository shows; Find in Repository in Files. Search Commits outside
/// Files is History's.
pub(super) fn panel_actions(
    root: Stateful<gpui::Div>,
    shown: bool,
    mode: ContentMode,
    cx: &mut Context<RepoWindow>,
) -> Stateful<gpui::Div> {
    root.when(shown, |root| {
        root.on_action(cx.listener(|this, _: &GoToFile, window, cx| {
            this.show_go_to_file(window, cx);
        }))
    })
    .when(shown && mode == ContentMode::Files, |root| {
        root.on_action(cx.listener(|this, _: &SearchCommits, window, cx| {
            this.show_find_in_repository(window, cx);
        }))
    })
}

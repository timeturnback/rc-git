//! Open Recent Repository (`RecentRepositoryPicker`, ⌃R): the recents newest first, narrowed
//! by kit's [`matching`] with the path search's own fuzzy scorer as you type. The arrows move
//! the highlight, Return or a double-click opens the pick, Escape closes. The shell decides
//! what opening does.

use std::ops::Range;

use gpui::{
    App, AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, Subscription, UniformListScrollHandle, Window, div,
    uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use rc_git_core::EntryKind;
use rc_git_kit::path_search;
use rc_git_kit::picker;
use rc_git_kit::recent_repositories::{display_path, empty_note, matching};

use crate::icons::Glyph;
use crate::search::{PanelKey, body, field, frame, keys, note, row};
use crate::style;

/// What the panel tells its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecentEvent {
    /// Open the repository at this path.
    Open(String),
    /// Escape or a click outside: the panel closes.
    Dismissed,
}

/// The Open Recent Repository panel.
pub struct RecentPanel {
    input: Entity<InputState>,
    recents: Vec<String>,
    home: String,
    results: Vec<String>,
    highlight: Option<usize>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RecentEvent> for RecentPanel {}

impl Focusable for RecentPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl RecentPanel {
    /// A panel over `recents` (newest first); `home` is shown, and matched, as `~`.
    pub fn new(
        recents: Vec<String>,
        home: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Open recent repository"));
        let subscriptions = vec![cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.filter(cx);
            }
        })];
        let mut panel = Self {
            input,
            recents,
            home,
            results: Vec::new(),
            highlight: None,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        panel.filter(cx);
        panel
    }

    /// The paths listed now, best first.
    #[cfg(test)]
    pub(crate) fn results(&self) -> &[String] {
        &self.results
    }

    /// The highlighted row.
    #[cfg(test)]
    pub(crate) fn highlight(&self) -> Option<usize> {
        self.highlight
    }

    /// Puts the keyboard in the field.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Narrows the list to what the field holds and highlights the best match.
    pub fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        self.results = matching(&self.recents, &query, &self.home, path_search::score);
        self.highlight = (!self.results.is_empty()).then_some(0);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// One of the keys the field hands over.
    pub fn key(&mut self, key: PanelKey, cx: &mut Context<Self>) {
        match key {
            PanelKey::Up | PanelKey::Down => {
                let delta = if key == PanelKey::Up { -1 } else { 1 };
                self.highlight = picker::moved(self.highlight, delta, self.results.len());
                if let Some(row) = self.highlight {
                    self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
                }
                cx.notify();
            }
            PanelKey::Enter => self.open(self.highlight.unwrap_or(0), cx),
            PanelKey::Escape => cx.emit(RecentEvent::Dismissed),
        }
    }

    fn open(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(path) = self.results.get(index) {
            cx.emit(RecentEvent::Open(path.clone()));
        }
    }

    fn clicked(&mut self, index: usize, event: &ClickEvent, cx: &mut Context<Self>) {
        self.highlight = Some(index);
        if event.click_count() >= 2 {
            self.open(index, cx);
        }
        cx.notify();
    }

    fn rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<gpui::Stateful<gpui::Div>> {
        let mut rows = Vec::with_capacity(range.len());
        for index in range {
            let Some(path) = self.results.get(index) else {
                continue;
            };
            let name = path
                .rsplit('/')
                .find(|part| !part.is_empty())
                .unwrap_or(path)
                .to_owned();
            let detail = display_path(path, &self.home);
            let highlighted = self.highlight == Some(index);
            let glyph = Some(Glyph::Entry(EntryKind::Tree));
            rows.push(
                row(
                    ("recent", index),
                    glyph,
                    (SharedString::from(name), SharedString::from(detail)),
                    highlighted,
                    cx,
                )
                .on_click(cx.listener(move |this, event, _, cx| this.clicked(index, event, cx))),
            );
        }
        rows
    }
}

impl Render for RecentPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self
            .results
            .is_empty()
            .then(|| note(empty_note(!self.recents.is_empty()), cx));
        let content = div()
            .size_full()
            .flex()
            .flex_col()
            .child(field(Input::new(&self.input).appearance(false), cx))
            .child(
                body().children(empty).child(
                    uniform_list(
                        "recent-results",
                        self.results.len(),
                        cx.processor(|this, range: Range<usize>, _window, cx| this.rows(range, cx)),
                    )
                    .track_scroll(&self.scroll)
                    .flex_1(),
                ),
            );
        let root = frame(
            "open-recent",
            (style::RECENT_PANEL_WIDTH, style::RECENT_PANEL_HEIGHT),
            content,
            cx,
        )
        .key_context("OpenRecent")
        .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(RecentEvent::Dismissed)));
        keys(root, cx, |this, key, _, cx| this.key(key, cx))
    }
}

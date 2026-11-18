//! Go to File (`GoToFileController`, ⇧⌘O): type a few characters, fuzzy-match every path
//! under the current root, Return opens the pick. The query runs through kit's
//! [`PathSearchSession`] over the browser model's `search_paths`; a Return pressed while the
//! newest query is still in flight opens its first match once it lands.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, Subscription, UniformListScrollHandle, Window, div,
    uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use rc_git_kit::browser::{BrowserModel, DEFAULT_PATH_SEARCH_LIMIT};
use rc_git_kit::path_search::{PathIndex, PathIndexEntry};
use rc_git_kit::path_search_session::{PathSearchEvent, PathSearchProvider, PathSearchSession};
use rc_git_kit::picker;

use crate::icons::entry_glyph;
use crate::search::{PanelKey, body, field, frame, keys, note, row};
use crate::style;

/// What the panel tells its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoToFileEvent {
    /// Open this file.
    Open(PathIndexEntry),
    /// Escape or a click outside: the panel closes.
    Dismissed,
}

/// The Go to File panel of one window.
pub struct GoToFilePanel {
    session: Entity<PathSearchSession>,
    input: Entity<InputState>,
    highlight: Option<usize>,
    note: Option<String>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<GoToFileEvent> for GoToFilePanel {}

impl Focusable for GoToFilePanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl GoToFilePanel {
    /// A panel searching `model`'s paths.
    pub fn new(model: Entity<BrowserModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let provider: PathSearchProvider = Rc::new(move |query: String, cx: &mut App| {
            model.update(cx, |model, cx| {
                model.search_paths(&query, DEFAULT_PATH_SEARCH_LIMIT, cx)
            })
        });
        let session = cx.new(|_| {
            let mut session = PathSearchSession::new();
            session.set_provider(provider);
            session
        });
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Go to file"));
        let subscriptions = vec![
            cx.subscribe(&session, |this, _, event: &PathSearchEvent, cx| {
                this.session_event(event, cx);
            }),
            cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    let query = input.read(cx).value().to_string();
                    this.session
                        .update(cx, |session, cx| session.search(&query, cx));
                }
            }),
        ];
        Self {
            session,
            input,
            highlight: None,
            note: None,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The matches on screen.
    #[must_use]
    pub fn results<'a>(&self, cx: &'a App) -> &'a [PathIndexEntry] {
        self.session.read(cx).results()
    }

    /// The highlighted row.
    #[cfg(test)]
    pub(crate) fn highlight(&self) -> Option<usize> {
        self.highlight
    }

    /// Opens the panel empty and starts the empty query, which lists the first files and warms
    /// the index.
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.session.update(cx, PathSearchSession::reset);
        self.highlight = None;
        self.note = None;
        self.input.update(cx, |input, cx| input.focus(window, cx));
        self.session
            .update(cx, |session, cx| session.search("", cx));
        cx.notify();
    }

    /// The panel closed: a Return waiting for results opens nothing.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        self.session
            .update(cx, |session, _| session.cancel_pending_open());
    }

    /// One of the keys the field hands over.
    pub fn key(&mut self, key: PanelKey, cx: &mut Context<Self>) {
        match key {
            PanelKey::Up => self.move_highlight(-1, cx),
            PanelKey::Down => self.move_highlight(1, cx),
            PanelKey::Enter => {
                let row = self.highlight.unwrap_or(0);
                self.session.update(cx, |session, cx| session.open(row, cx));
            }
            PanelKey::Escape => cx.emit(GoToFileEvent::Dismissed),
        }
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.results(cx).len();
        self.highlight = picker::moved(self.highlight, delta, count);
        if let Some(row) = self.highlight {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn session_event(&mut self, event: &PathSearchEvent, cx: &mut Context<Self>) {
        match event {
            PathSearchEvent::Results { truncated } => {
                self.note = truncated.then(|| PathIndex::TRUNCATION_NOTE.to_owned());
                self.highlight = (!self.results(cx).is_empty()).then_some(0);
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            PathSearchEvent::Failed(message) => {
                self.note = Some(format!("Cannot list files: {message}"));
                self.highlight = None;
            }
            PathSearchEvent::Open(entry) => cx.emit(GoToFileEvent::Open(entry.clone())),
        }
        cx.notify();
    }

    fn clicked(&mut self, index: usize, event: &ClickEvent, cx: &mut Context<Self>) {
        self.highlight = Some(index);
        if event.click_count() >= 2 {
            self.session
                .update(cx, |session, cx| session.open(index, cx));
        }
        cx.notify();
    }

    fn rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<gpui::Stateful<gpui::Div>> {
        let entries: Vec<PathIndexEntry> = self
            .results(cx)
            .get(range.clone())
            .unwrap_or_default()
            .to_vec();
        let mut rows = Vec::with_capacity(entries.len());
        for (index, entry) in range.zip(entries) {
            let glyph = entry_glyph(entry.kind, entry.name());
            let name = SharedString::from(entry.name().to_owned());
            let highlighted = self.highlight == Some(index);
            rows.push(
                row(
                    ("file", index),
                    Some(glyph),
                    (name, SharedString::from(entry.path)),
                    highlighted,
                    cx,
                )
                .on_click(cx.listener(move |this, event, _, cx| this.clicked(index, event, cx))),
            );
        }
        rows
    }
}

impl Render for GoToFilePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.results(cx).len();
        let note = self.note.clone().map(|text| note(text, cx));
        let content = div()
            .size_full()
            .flex()
            .flex_col()
            .child(field(Input::new(&self.input).appearance(false), cx))
            .child(
                body().children(note).child(
                    uniform_list(
                        "go-to-file-results",
                        count,
                        cx.processor(|this, range: Range<usize>, _window, cx| this.rows(range, cx)),
                    )
                    .track_scroll(&self.scroll)
                    .flex_1(),
                ),
            );
        let root = frame(
            "go-to-file",
            (style::GO_TO_FILE_WIDTH, style::GO_TO_FILE_HEIGHT),
            content,
            cx,
        )
        .key_context("GoToFile")
        .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(GoToFileEvent::Dismissed)));
        keys(root, cx, |this, key, _, cx| this.key(key, cx))
    }
}

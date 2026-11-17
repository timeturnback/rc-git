//! The file pane (`FileViewController`): a header naming the open file, the find bar, and the
//! viewer or the placeholder the model's [`FileState`] calls for. What each state says comes
//! from [`rc_git_kit::file_pane`]; the pane only applies it.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Subscription, Window,
    div, prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Icon, Selectable, Sizable};
use rc_git_kit::browser::{BrowserEvent, BrowserModel, FileEntry, FileState, OpenPathOutcome};
use rc_git_kit::file_pane::{header, load_anyway_size, placeholder, shows_preview};
use rc_git_kit::import_links::ImportLinkMap;
use rc_git_kit::markdown::MarkdownDocument;
use rc_git_kit::text_document::{FileReveal, TextDocument};
use rc_git_kit::text_find::{find_all, first_from, step};

use crate::files::markdown::{MarkdownPreview, PreviewEvent};
use crate::files::viewer::{FileViewer, ViewerEvent};
use crate::icons::{Glyph, icon};
use crate::menus::{Find, FindNext, FindPrevious, UseSelectionForFind};
use crate::style;

/// The find bar's state: its field, and the matches of the text in it.
struct FindBar {
    visible: bool,
    input: Entity<InputState>,
    matches: Arc<[Range<usize>]>,
    current: Option<usize>,
}

/// The file pane of one window.
pub struct FilePane {
    model: Entity<BrowserModel>,
    viewer: Option<Entity<FileViewer>>,
    find: FindBar,
    focus: FocusHandle,
    /// The open viewer's events; replaced with the viewer.
    viewer_events: Option<Subscription>,
    /// The markdown preview on screen, in place of the viewer.
    preview: Option<Entity<MarkdownPreview>>,
    /// The preview's events; replaced with the preview.
    preview_events: Option<Subscription>,
    /// Whether markdown shows as the preview; remembered across files.
    prefers_preview: bool,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for FilePane {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FilePane {
    /// A pane showing what `model` has open.
    pub fn new(model: Entity<BrowserModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
        let subscriptions = vec![
            cx.subscribe_in(
                &model,
                window,
                |this, _, event: &BrowserEvent, window, cx| {
                    if *event == BrowserEvent::FileChanged {
                        this.file_changed(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.search(cx),
                    InputEvent::PressEnter { shift, .. } => this.step(!*shift, cx),
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            ),
        ];
        let mut pane = Self {
            model,
            viewer: None,
            find: FindBar {
                visible: false,
                input,
                matches: Arc::new([]),
                current: None,
            },
            focus: cx.focus_handle(),
            viewer_events: None,
            preview: None,
            preview_events: None,
            prefers_preview: true,
            _subscriptions: subscriptions,
        };
        pane.file_changed(window, cx);
        pane
    }

    /// The viewer on screen, while the open file is text.
    #[cfg(test)]
    pub(crate) fn viewer(&self) -> Option<&Entity<FileViewer>> {
        self.viewer.as_ref()
    }

    /// Shows the find bar and puts the keyboard in it (⌘F).
    pub fn show_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.visible = true;
        self.find
            .input
            .update(cx, |input, cx| input.focus(window, cx));
        self.search(cx);
    }

    /// Steps to the next or previous match (⌘G, ⇧⌘G), opening the bar when it is closed.
    pub fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.find.visible = true;
        self.find.current = step(self.find.matches.len(), self.find.current, forward);
        self.show_matches(cx);
    }

    /// Puts the viewer's selection in the find field (⌘E).
    pub fn use_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(viewer) = &self.viewer else {
            return;
        };
        let selected = viewer.read(cx).selected_text().to_owned();
        if selected.is_empty() {
            return;
        }
        self.find.visible = true;
        self.find
            .input
            .update(cx, |input, cx| input.set_value(selected, window, cx));
        self.search(cx);
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.visible = false;
        self.find.matches = Arc::new([]);
        self.find.current = None;
        self.show_matches(cx);
        if let Some(viewer) = &self.viewer {
            window.focus(&viewer.focus_handle(cx), cx);
        }
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        let query = self.find.input.read(cx).value();
        let Some(viewer) = &self.viewer else {
            self.find.matches = Arc::new([]);
            self.find.current = None;
            cx.notify();
            return;
        };
        let (matches, caret) = {
            let viewer = viewer.read(cx);
            let text = &viewer.document().text;
            (find_all(text, &query), viewer.selection().range().start)
        };
        self.find.current = first_from(&matches, caret);
        self.find.matches = matches.into();
        self.show_matches(cx);
    }

    fn show_matches(&mut self, cx: &mut Context<Self>) {
        if let Some(viewer) = &self.viewer {
            let matches = Arc::clone(&self.find.matches);
            let current = self.find.current;
            viewer.update(cx, |viewer, cx| viewer.set_matches(matches, current, cx));
        }
        cx.notify();
    }

    /// Follows the model's file: a new viewer for new text, nothing for any other state.
    fn file_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.model.read(cx).file_state().clone();
        let FileState::Text(entry, document) = state else {
            self.viewer = None;
            self.preview = None;
            cx.notify();
            return;
        };
        if self.shows_preview(&entry, &document, cx) {
            self.viewer = None;
            self.show_preview(&document, window, cx);
            cx.notify();
            return;
        }
        self.preview = None;
        let same = self
            .viewer
            .as_ref()
            .is_some_and(|viewer| Arc::ptr_eq(viewer.read(cx).document(), &document));
        if !same {
            self.show_text(&entry, &document, window, cx);
        }
        let reveal = self
            .model
            .update(cx, |model, _| model.take_pending_reveal());
        if let Some(reveal) = reveal {
            self.reveal(&reveal, cx);
        }
        cx.notify();
    }

    /// The markdown preview on screen, while the open file shows as one.
    #[cfg(test)]
    pub(crate) fn preview(&self) -> Option<&Entity<MarkdownPreview>> {
        self.preview.as_ref()
    }

    /// Switches markdown between the preview and its source (the header's picker); the choice
    /// holds for the next markdown file too.
    pub fn set_prefers_preview(
        &mut self,
        prefers: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if prefers == self.prefers_preview {
            return;
        }
        self.prefers_preview = prefers;
        self.viewer = None;
        self.preview = None;
        self.file_changed(window, cx);
    }

    fn shows_preview(
        &self,
        entry: &FileEntry,
        document: &TextDocument,
        cx: &Context<Self>,
    ) -> bool {
        shows_preview(
            &entry.name_lossy(),
            document.byte_count,
            self.prefers_preview,
            self.model.read(cx).pending_reveal(),
        )
    }

    /// Shows `document` as the preview. A preview already built for it takes the pending
    /// reveal now; a new one takes it once it is built, so a heading link scrolls only after
    /// the blocks it names exist.
    fn show_preview(
        &mut self,
        document: &Arc<TextDocument>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(preview) = &self.preview
            && Arc::ptr_eq(preview.read(cx).document(), document)
        {
            if preview.read(cx).rendered().is_some() {
                self.reveal_in_preview(cx);
            }
            return;
        }
        let path = self.model.read(cx).opened_file_path().map(str::to_owned);
        let preview = cx.new(|cx| MarkdownPreview::new(Arc::clone(document), path, cx));
        self.preview_events = Some(cx.subscribe_in(
            &preview,
            window,
            |this, _, event: &PreviewEvent, window, cx| match event {
                PreviewEvent::Built => this.reveal_in_preview(cx),
                PreviewEvent::OpenPath { path, fragment } => {
                    this.open_link(path, fragment.clone(), window, cx);
                }
            },
        ));
        self.preview = Some(preview);
    }

    fn reveal_in_preview(&mut self, cx: &mut Context<Self>) {
        let reveal = self
            .model
            .update(cx, |model, _| model.take_pending_reveal());
        if let (Some(FileReveal::Fragment(name)), Some(preview)) = (reveal, &self.preview) {
            preview.update(cx, |preview, cx| preview.scroll_to_anchor(&name, cx));
        }
    }

    /// Opens a repository link from the preview; a dead link leaves the file on screen and
    /// says so.
    fn open_link(
        &mut self,
        path: &str,
        fragment: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self
            .model
            .update(cx, |model, cx| model.open_path(path, fragment, cx));
        let path = path.to_owned();
        cx.spawn_in(window, async move |_, cx| {
            let message = match open.await {
                OpenPathOutcome::Opened => return,
                OpenPathOutcome::Missing => format!("{path} is not in this tree."),
                OpenPathOutcome::Failed(message) => message,
            };
            cx.update(|window, cx| {
                drop(window.prompt(
                    gpui::PromptLevel::Warning,
                    "rc-git could not open the link",
                    Some(&message),
                    &["OK"],
                    cx,
                ));
            })
            .ok();
        })
        .detach();
    }

    fn render_mode_picker(&self, state: &FileState, cx: &mut Context<Self>) -> Option<AnyElement> {
        let FileState::Text(entry, document) = state else {
            return None;
        };
        if !MarkdownDocument::is_previewable(&entry.name_lossy(), document.byte_count) {
            return None;
        }
        let showing = self.preview.is_some();
        Some(
            ButtonGroup::new("markdown-mode")
                .small()
                .child(Button::new("preview").label("Preview").selected(showing))
                .child(Button::new("source").label("Source").selected(!showing))
                .on_click(cx.listener(|this, picked: &Vec<usize>, window, cx| {
                    let prefers = picked.first() == Some(&0);
                    this.set_prefers_preview(prefers, window, cx);
                }))
                .into_any_element(),
        )
    }

    fn show_text(
        &mut self,
        entry: &FileEntry,
        document: &Arc<TextDocument>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = entry.name_lossy().into_owned();
        let oid = entry.oid;
        let viewer = cx.new(|cx| FileViewer::new(Arc::clone(document), cx));
        self.viewer_events = Some(cx.subscribe_in(&viewer, window, {
            let name = name.clone();
            move |this, viewer, event: &ViewerEvent, _, cx| match event {
                ViewerEvent::WantsImportLinks => this.load_links(&name, oid, viewer, cx),
                ViewerEvent::OpenImport(specifier) => this.open_import(specifier, cx),
            }
        }));
        let highlights = self.model.read(cx).highlight_store().clone();
        let task = highlights.update(cx, |store, cx| store.highlights(&name, oid, document, cx));
        let weak = viewer.downgrade();
        cx.spawn(async move |_, cx| {
            if let Some(spans) = task.await {
                weak.update(cx, |viewer, cx| viewer.set_spans(spans, cx))
                    .ok();
            }
        })
        .detach();
        self.viewer = Some(viewer);
        if self.find.visible {
            self.search(cx);
        }
    }

    fn load_links(
        &mut self,
        name: &str,
        oid: Option<rc_git_core::Oid>,
        viewer: &Entity<FileViewer>,
        cx: &mut Context<Self>,
    ) {
        let document = Arc::clone(viewer.read(cx).document());
        let highlights = self.model.read(cx).highlight_store().clone();
        let task = highlights.update(cx, |store, cx| store.import_links(name, oid, &document, cx));
        let weak = viewer.downgrade();
        cx.spawn(async move |_, cx| {
            if let Some(links) = task.await {
                let text: Arc<str> = Arc::from(document.text.as_str());
                let map = ImportLinkMap::new(links.to_vec(), text);
                weak.update(cx, |viewer, cx| viewer.set_import_links(map, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn open_import(&mut self, specifier: &str, cx: &mut Context<Self>) {
        let Some(source) = self.model.read(cx).opened_file_path().map(str::to_owned) else {
            return;
        };
        self.model
            .update(cx, |model, cx| model.open_import(specifier, &source, cx))
            .detach();
    }

    fn reveal(&mut self, reveal: &FileReveal, cx: &mut Context<Self>) {
        let FileReveal::Match {
            line,
            byte_column,
            byte_length,
        } = reveal
        else {
            return;
        };
        let Some(viewer) = &self.viewer else {
            return;
        };
        let range = viewer
            .read(cx)
            .document()
            .match_range(*line, *byte_column, *byte_length);
        if let Some(range) = range {
            viewer.update(cx, |viewer, cx| viewer.reveal(range, cx));
        }
    }

    fn load_anyway(&mut self, size: u64, cx: &mut Context<Self>) {
        self.model
            .update(cx, |model, cx| model.load_anyway(size, cx))
            .detach();
    }

    fn render_header(
        state: &FileState,
        mode_picker: Option<AnyElement>,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let header = header(state)?;
        let theme = cx.theme();
        Some(
            div()
                .flex()
                .items_center()
                .gap(style::ROW_GAP)
                .px(style::MARGIN)
                .py(style::BAR_PADDING)
                .border_b(style::HAIRLINE)
                .border_color(theme.border)
                .child(
                    div()
                        .text_size(style::TEXT_BODY)
                        .text_color(theme.foreground)
                        .child(SharedString::from(header.title)),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(style::TEXT_CAPTION)
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(header.detail)),
                )
                .children(mode_picker),
        )
    }

    fn render_find_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let count = match (self.find.current, self.find.matches.len()) {
            (_, 0) => "No matches".to_owned(),
            (Some(index), total) => format!("{} of {total}", index + 1),
            (None, total) => format!("{total} matches"),
        };
        div()
            .key_context("FindBar")
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .px(style::MARGIN)
            .py(style::BAR_PADDING)
            .border_b(style::HAIRLINE)
            .border_color(theme.border)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_find(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .w(style::FIND_FIELD_WIDTH)
                    .child(Input::new(&self.find.input).small()),
            )
            .child(
                div()
                    .text_size(style::TEXT_CAPTION)
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(count)),
            )
            .child(
                Button::new("find-previous")
                    .ghost()
                    .small()
                    .icon(Icon::new(icon(Glyph::Back)))
                    .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
            )
            .child(
                Button::new("find-next")
                    .ghost()
                    .small()
                    .icon(Icon::new(icon(Glyph::Forward)))
                    .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
            )
            .child(
                Button::new("find-done")
                    .ghost()
                    .small()
                    .label("Done")
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            )
    }

    fn render_body(&self, state: &FileState, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        if let Some(viewer) = &self.viewer {
            return div()
                .flex_1()
                .min_h_0()
                .child(viewer.clone())
                .into_any_element();
        }
        if let Some(preview) = &self.preview {
            return div()
                .flex_1()
                .min_h_0()
                .child(preview.clone())
                .into_any_element();
        }
        let tertiary = crate::theme::tertiary(cx);
        let text = placeholder(state).unwrap_or_default();
        let load = load_anyway_size(state);
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(style::ROW_GAP)
            .px(style::MARGIN)
            .child(
                div()
                    .text_size(style::TEXT_BODY)
                    .text_color(tertiary)
                    .child(SharedString::from(text)),
            )
            .when_some(load, |body, size| {
                body.child(
                    Button::new("load-anyway")
                        .label("Load anyway")
                        .on_click(cx.listener(move |this, _, _, cx| this.load_anyway(size, cx))),
                )
            })
            .into_any_element()
    }
}

impl Render for FilePane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.model.read(cx).file_state().clone();
        let mode_picker = self.render_mode_picker(&state, cx);
        let header = Self::render_header(&state, mode_picker, cx);
        let find = self.find.visible.then(|| self.render_find_bar(cx));
        let body = self.render_body(&state, cx);
        div()
            .id("file-pane")
            .key_context("FilePane")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .on_action(cx.listener(|this, _: &Find, window, cx| this.show_find(window, cx)))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &UseSelectionForFind, window, cx| {
                this.use_selection(window, cx);
            }))
            .children(header)
            .children(find)
            .child(body)
    }
}

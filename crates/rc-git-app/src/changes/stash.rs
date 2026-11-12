//! The stash group in the Changes sidebar: the heading, the list (or the failed read's message
//! in its place) and Apply, Pop and Drop for the picked stash. The pick lives in the model, by
//! stash commit, so a push from elsewhere that shifts every row keeps it on the same stash.
//! A row's tooltip is its stash commit, as in the Swift list.

use gpui::{
    Context, InteractiveElement, IntoElement, MouseButton, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder,
};
use gpui_component::button::Button;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Disableable, Sizable};
use rc_git_core::StashEntry;
use rc_git_kit::changes::StashListState;
use rc_git_kit::changes_view::{shows_stash_group, stash_label};

use crate::changes::list::ChangeListView;
use crate::changes::{ChangesRequest, heading, placeholder};
use crate::style;

impl ChangeListView {
    /// Picks the stash at `index`.
    pub(crate) fn pick_stash(&mut self, index: usize, cx: &mut Context<Self>) {
        self.model
            .update(cx, |model, _| model.select_stash(Some(index)));
        cx.notify();
    }

    /// Drop… on the picked stash, as its button does.
    pub(crate) fn drop_picked_stash(&mut self, cx: &mut Context<Self>) {
        self.stash_request(ChangesRequest::StashDrop, cx);
    }

    fn picked_stash(&self, cx: &gpui::App) -> Option<StashEntry> {
        let model = self.model.read(cx);
        let index = model.selected_stash_index()?;
        model.stashes().get(index).cloned()
    }

    fn stash_request(&mut self, make: fn(StashEntry) -> ChangesRequest, cx: &mut Context<Self>) {
        if let Some(entry) = self.picked_stash(cx) {
            Self::request(make(entry), cx);
        }
    }

    fn render_stash_rows(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let model = self.model.read(cx);
        if let StashListState::Failed(message) = model.stash_state() {
            return placeholder(SharedString::from(message.clone()), cx).into_any_element();
        }
        let picked = model.selected_stash_index();
        let theme = cx.theme();
        let mut rows = div().flex().flex_col();
        for (index, entry) in model.stashes().iter().enumerate() {
            let oid = SharedString::from(entry.oid.to_hex());
            rows = rows.child(
                div()
                    .id(("stash", index))
                    .px(style::ROW_GAP)
                    .rounded(style::RADIUS)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(style::TEXT_CAPTION)
                    .when(picked == Some(index), |row| row.bg(theme.list_active))
                    .hover(|style| style.bg(theme.list_hover))
                    .child(SharedString::from(stash_label(entry)))
                    .tooltip(move |window, cx| Tooltip::new(oid.clone()).build(window, cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.pick_stash(index, cx)),
                    ),
            );
        }
        div()
            .id("stash-rows")
            .size_full()
            .overflow_y_scroll()
            .child(rows)
            .into_any_element()
    }

    /// The stash group, or `None` when a repository with no stash hides it.
    pub(super) fn render_stashes(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let model = self.model.read(cx);
        if !shows_stash_group(!model.stashes().is_empty(), model.stash_state()) {
            return None;
        }
        let picked = self.picked_stash(cx).is_some();
        let border = cx.theme().border;
        let button = |id: &'static str, label: &'static str| {
            Button::new(id).small().label(label).disabled(!picked)
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap(style::ROW_GAP)
                .child(heading("Stashes", cx))
                .child(
                    div()
                        .h(style::CHANGES_STASH_HEIGHT)
                        .rounded(style::RADIUS)
                        .border(style::HAIRLINE)
                        .border_color(border)
                        .child(self.render_stash_rows(cx)),
                )
                .child(
                    div()
                        .flex()
                        .gap(style::ROW_GAP)
                        .child(button("stash-apply", "Apply").on_click(cx.listener(
                            |this, _, _, cx| this.stash_request(ChangesRequest::StashApply, cx),
                        )))
                        .child(button("stash-pop", "Pop").on_click(cx.listener(
                            |this, _, _, cx| this.stash_request(ChangesRequest::StashPop, cx),
                        )))
                        .child(
                            button("stash-drop", "Drop\u{2026}")
                                .on_click(cx.listener(|this, _, _, cx| this.drop_picked_stash(cx))),
                        ),
                ),
        )
    }
}

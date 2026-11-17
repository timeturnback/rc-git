//! The change list's rows: a disclosure on folders, a tick box, the status letter in its
//! color and the name. A press selects (⌘ toggles, ⇧ extends), a double press opens or
//! closes a folder, a press on the tick box ticks or unticks everything beneath the row, and
//! the context menu discards.

use std::ops::Range;

use gpui::{
    Context, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, SharedString, Styled, div, prelude::FluentBuilder, uniform_list,
};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Icon};
use rc_git_core::StatusEntry;
use rc_git_kit::change_tree::{ChangeNode, discard_paths};
use rc_git_kit::changes_view::tick_target;
use rc_git_kit::diff_text::worktree_letter;

use crate::changes::list::ChangeListView;
use crate::changes::{ChangesRequest, placeholder, tick_box};
use crate::icons::{Glyph, icon};
use crate::status::worktree_color;
use crate::style;

/// The node of `key` in `nodes`, children included.
fn find<'a>(
    nodes: &'a [ChangeNode<StatusEntry>],
    key: &str,
) -> Option<&'a ChangeNode<StatusEntry>> {
    let mut stack: Vec<&ChangeNode<StatusEntry>> = nodes.iter().collect();
    while let Some(node) = stack.pop() {
        if node.key() == key {
            return Some(node);
        }
        stack.extend(node.children.iter());
    }
    None
}

impl ChangeListView {
    /// The files a press on row `index`'s tick box acts on, and which way.
    pub(crate) fn toggle_tick(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let tick = self.model.read(cx).row_tick(&row.node);
        if !tick.is_enabled {
            return;
        }
        let Some(node) = find(&self.nodes, &row.key) else {
            return;
        };
        let paths = node.file_paths();
        let ticked = tick_target(tick);
        self.model
            .update(cx, |model, cx| model.set_ticked(&paths, ticked, cx));
    }

    /// Opens or closes the folder at row `index`.
    pub(super) fn toggle_folder(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if !row.node.is_folder() {
            return;
        }
        let key = row.key.clone();
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.relayout();
        cx.notify();
    }

    /// Selects as a press at row `index` does, and shows a pressed file's diff.
    pub(crate) fn press(&mut self, index: usize, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if event.click_count == 2 {
            self.toggle_folder(index, cx);
            return;
        }
        let key = row.key.clone();
        let entry = row.node.entry.clone();
        if event.modifiers.platform {
            if !self.selected.remove(&key) {
                self.selected.insert(key);
            }
        } else if event.modifiers.shift
            && let Some(anchor) = self.anchor
        {
            let (start, end) = (anchor.min(index), anchor.max(index));
            self.selected = self.rows[start..=end]
                .iter()
                .map(|row| row.key.clone())
                .collect();
        } else {
            self.selected = std::iter::once(key).collect();
            self.anchor = Some(index);
        }
        cx.notify();
        if let Some(entry) = entry {
            self.model
                .update(cx, |model, cx| model.select(entry, cx))
                .detach();
        }
    }

    /// Discards what the rows cover: the clicked row alone when it is outside the selection,
    /// else the whole selection.
    fn discard(&mut self, clicked: &str, cx: &mut Context<Self>) {
        let selected: Vec<&ChangeNode<StatusEntry>> = self
            .selected
            .iter()
            .filter_map(|key| find(&self.nodes, key))
            .collect();
        let paths = discard_paths(&selected, find(&self.nodes, clicked));
        if paths.is_empty() {
            return;
        }
        Self::request(ChangesRequest::Discard(paths), cx);
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(row) = self.rows.get(index) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let tick = self.model.read(cx).row_tick(&row.node);
        let selected = self.selected.contains(&row.key);
        let depth = f32::from(u16::try_from(row.depth).unwrap_or(u16::MAX));
        let indent = style::MARGIN + style::FOLDER_INDENT * depth;
        let disclosure = row.node.is_folder().then(|| {
            let glyph = if row.expanded {
                Glyph::Expanded
            } else {
                Glyph::Collapsed
            };
            div()
                .id(("disclosure", index))
                .child(Icon::new(icon(glyph)).size(style::ICON_SIZE))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_folder(index, cx);
                    }),
                )
        });
        let letter = row.node.entry.as_ref().map(|entry| {
            div()
                .w(style::ICON_SIZE)
                .flex_none()
                .flex()
                .justify_center()
                .text_size(style::TEXT_STATUS_MARK)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(worktree_color(entry.status, cx))
                .child(worktree_letter(entry.status).unwrap_or(" "))
        });
        let name_color = if row.node.is_folder() {
            theme.muted_foreground
        } else {
            theme.foreground
        };
        let clicked = row.key.clone();
        div()
            .id(("change", index))
            .h(style::CHANGE_ROW_HEIGHT)
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .pl(indent)
            .pr(style::ROW_GAP)
            .rounded(style::RADIUS)
            .text_size(style::TEXT_CAPTION)
            .when(selected, |row| row.bg(theme.list_active))
            .when(!selected, |row| {
                row.hover(|style| style.bg(theme.list_hover))
            })
            .children(disclosure)
            .child(
                div()
                    .id(("tick", index))
                    .child(tick_box(tick.state, tick.is_enabled, cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_tick(index, cx);
                        }),
                    ),
            )
            .children(letter)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(name_color)
                    .child(SharedString::from(row.node.name.clone())),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, window, cx| {
                    window.focus(&this.focus, cx);
                    this.press(index, event, cx);
                }),
            )
            .context_menu({
                let weak = cx.entity().downgrade();
                move |menu, _, _| {
                    let weak = weak.clone();
                    let clicked = clicked.clone();
                    menu.item(PopupMenuItem::new("Discard Changes\u{2026}").on_click(
                        move |_, _, cx| {
                            weak.update(cx, |this, cx| this.discard(&clicked, cx)).ok();
                        },
                    ))
                }
            })
            .into_any_element()
    }

    /// The list, with the notice's line over it when there is one.
    pub(super) fn render_files(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let notice = self.notice.placeholder.clone();
        let theme = cx.theme();
        div()
            .id("change-list")
            .track_focus(&self.focus)
            .relative()
            .flex_1()
            .min_h(style::CHANGES_FILES_FLOOR)
            .rounded(style::RADIUS)
            .border(style::HAIRLINE)
            .border_color(theme.border)
            .child(
                uniform_list(
                    "change-rows",
                    self.rows.len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        let mut rows = Vec::with_capacity(range.len());
                        for index in range {
                            rows.push(this.render_row(index, cx));
                        }
                        rows
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full(),
            )
            .child(Scrollbar::vertical(&self.scroll))
            .when_some(notice, |list, notice| {
                list.child(div().absolute().inset_0().child(placeholder(notice, cx)))
            })
    }
}

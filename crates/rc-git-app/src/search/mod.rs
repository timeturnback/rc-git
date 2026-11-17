//! The quick-pick panels (`GoToFileController`, `FindInRepositoryController`,
//! `RecentRepositoryPicker`): a field over a result list, drawn over the window. The arrow keys
//! move the highlight by [`rc_git_kit::picker`], Return opens, Escape and a click outside close.
//! Each panel's session (kit) decides what the list holds; the panel only applies it.

pub mod find;
pub mod go_to_file;
pub mod recent;

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, Div, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement,
    Pixels, SharedString, Stateful, Styled, Window, deferred, div,
};
use gpui_component::input::{Enter, Escape, MoveDown, MoveUp};
use gpui_component::{ActiveTheme, Icon};

use crate::icons::{Glyph, icon, is_accented};
use crate::style;

/// A key a panel answers from its field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKey {
    /// The up arrow.
    Up,
    /// The down arrow.
    Down,
    /// Return.
    Enter,
    /// Escape.
    Escape,
}

type KeyHandler<V> = Rc<dyn Fn(&mut V, PanelKey, &mut Window, &mut Context<V>)>;

/// Takes the arrows, Return and Escape from the field before it moves its caret or emits its
/// own Return, and hands them to `on`.
pub fn keys<V: 'static>(
    root: Stateful<Div>,
    cx: &mut Context<V>,
    on: impl Fn(&mut V, PanelKey, &mut Window, &mut Context<V>) + 'static,
) -> Stateful<Div> {
    let on: KeyHandler<V> = Rc::new(on);
    let (up, down, enter, escape) = (Rc::clone(&on), Rc::clone(&on), Rc::clone(&on), on);
    root.capture_action(cx.listener(move |this, _: &MoveUp, window, cx| {
        cx.stop_propagation();
        up(this, PanelKey::Up, window, cx);
    }))
    .capture_action(cx.listener(move |this, _: &MoveDown, window, cx| {
        cx.stop_propagation();
        down(this, PanelKey::Down, window, cx);
    }))
    .capture_action(cx.listener(move |this, _: &Enter, window, cx| {
        cx.stop_propagation();
        enter(this, PanelKey::Enter, window, cx);
    }))
    .capture_action(cx.listener(move |this, _: &Escape, window, cx| {
        cx.stop_propagation();
        escape(this, PanelKey::Escape, window, cx);
    }))
}

/// The box a panel draws in: centered under the toolbar, over the window, with a shadow.
pub fn frame(
    id: impl Into<ElementId>,
    size: (Pixels, Pixels),
    content: impl IntoElement,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .occlude()
        .w(size.0)
        .h(size.1)
        .flex()
        .flex_col()
        .gap(style::ROW_GAP)
        .p(style::MARGIN)
        .rounded(style::RADIUS_LG)
        .border(style::HAIRLINE)
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .shadow_lg()
        .child(content)
}

/// Lays a panel over the window: centered across it, hanging under the toolbar. The layer has
/// no hitbox of its own, so only the panel takes the pointer.
pub fn overlay(panel: impl IntoElement) -> AnyElement {
    deferred(
        div()
            .absolute()
            .top(style::TOOLBAR_HEIGHT + style::PANEL_TOP)
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(panel),
    )
    .with_priority(1)
    .into_any_element()
}

/// One result row: an icon, the name, and a dimmed detail (`(name, detail)`), highlighted
/// when it is the pick.
pub fn row(
    id: impl Into<ElementId>,
    glyph: Option<Glyph>,
    (name, detail): (SharedString, SharedString),
    highlighted: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let mut element = div()
        .id(id)
        .h(style::LIST_ROW_HEIGHT)
        .flex()
        .items_center()
        .gap(style::ROW_GAP)
        .px(style::ROW_GAP)
        .rounded(style::RADIUS)
        .overflow_hidden()
        .text_size(style::TEXT_BODY)
        .hover(|row| row.bg(theme.list_hover));
    if highlighted {
        element = element.bg(theme.list_active);
    }
    if let Some(glyph) = glyph {
        element = element.child(glyph_icon(glyph, theme.primary, theme.muted_foreground));
    }
    element.child(div().flex_none().child(name)).child(
        div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .text_size(style::TEXT_CAPTION)
            .text_color(theme.muted_foreground)
            .child(detail),
    )
}

fn glyph_icon(glyph: Glyph, accent: Hsla, muted: Hsla) -> Icon {
    let tint = if is_accented(glyph) { accent } else { muted };
    Icon::new(icon(glyph))
        .size(style::ICON_SIZE)
        .text_color(tint)
}

/// A dimmed line under the field or the list: a status, a note, a failure.
pub fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_size(style::TEXT_CAPTION)
        .text_color(cx.theme().muted_foreground)
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(text.into())
}

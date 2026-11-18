//! The window tab strip under the toolbar. GPUI hides the native tab bar of a tabbed window
//! and leaves drawing it to the app, so each repository window draws its tab group from
//! `SystemWindowTabController`: one tab per window, the selected one is this window, a click
//! brings that window's tab forward, and each tab's close button closes that window. Tabs are
//! rounded pills on the window chrome: the selected one filled, the rest quiet until hovered,
//! the close button showing on hover. They share the width equally up to
//! [`style::TAB_MAX_WIDTH`], left-aligned, and a plus button right after the last opens a
//! repository as Open (⌘O) does. The strip shows only while the group holds two or more tabs,
//! as the native bar does by default.

use gpui::{
    AnyWindowHandle, App, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, SystemWindowTabController, Window, WindowId, div,
    prelude::FluentBuilder as _,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Icon, Sizable};

use crate::icons::{Glyph, icon};
use crate::log;
use crate::menus::OpenRepository;
use crate::style;

/// What the strip of window `id` shows: each tab of its group, in order, by window and title.
/// The controller also changes on every window activation, which the strip does not show, so
/// a window redraws only when this changes.
pub(super) fn shown_tabs(id: WindowId, cx: &App) -> Vec<(WindowId, SharedString)> {
    cx.try_global::<SystemWindowTabController>()
        .and_then(|controller| controller.tabs(id))
        .map(|tabs| tabs.iter().map(|tab| (tab.id, tab.title.clone())).collect())
        .unwrap_or_default()
}

/// The tab strip for `window`'s tab group, or `None` while it holds a single tab.
pub(super) fn render(window: &Window, cx: &App) -> Option<impl IntoElement + use<>> {
    let current = window.window_handle().window_id();
    let tabs = cx
        .try_global::<SystemWindowTabController>()?
        .tabs(current)?;
    if tabs.len() < 2 {
        return None;
    }
    let mut strip = div()
        .debug_selector(|| "window-tabs".to_owned())
        .flex_none()
        .w_full()
        .h(style::TAB_STRIP_HEIGHT)
        .flex()
        .items_center()
        .gap(style::TAB_GAP)
        .px(style::CARD_INSET);
    for (index, tab) in tabs.iter().enumerate() {
        let selected = tab.id == current;
        strip = strip.child(render_tab(
            index,
            tab.title.clone(),
            tab.handle,
            selected,
            cx,
        ));
    }
    Some(
        strip.child(
            Button::new("open-repository-tab")
                .debug_selector(|| "open-repository-tab".to_owned())
                .flex_none()
                .xsmall()
                .ghost()
                .icon(Icon::new(icon(Glyph::OpenTab)))
                .tooltip("Open Repository (\u{2318}O)")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenRepository), cx)),
        ),
    )
}

/// One tab: the repository glyph, the title cut with an ellipsis, and a close button that
/// shows while the pointer is on the tab.
fn render_tab(
    index: usize,
    title: SharedString,
    handle: AnyWindowHandle,
    selected: bool,
    cx: &App,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let (current, other, hover) = (theme.foreground, theme.muted_foreground, theme.list_hover);
    let group = SharedString::from(format!("window-tab-{index}"));
    div()
        .id(("window-tab", index))
        .debug_selector(move || format!("window-tab-{index}"))
        .group(group.clone())
        .flex_1()
        .min_w_0()
        .max_w(style::TAB_MAX_WIDTH)
        .h(style::TAB_HEIGHT)
        .flex()
        .items_center()
        .gap(style::ROW_GAP)
        .pl(style::BAR_PADDING)
        .pr(style::SEGMENT_INSET)
        .rounded(style::RADIUS)
        .border(style::HAIRLINE)
        .border_color(theme.transparent)
        .text_size(style::TEXT_CAPTION)
        .when(selected, |tab| {
            tab.bg(theme.tab_active)
                .border_color(theme.border)
                .text_color(current)
        })
        .when(!selected, |tab| {
            tab.text_color(other)
                .hover(move |tab| tab.bg(hover).text_color(current))
        })
        .child(
            Icon::new(icon(Glyph::Repository))
                .size(style::TAB_ICON_SIZE)
                .flex_none()
                .text_color(other),
        )
        .child(div().flex_1().min_w_0().truncate().child(title))
        .child(
            div()
                .flex_none()
                .opacity(0.)
                .group_hover(group, |close| close.opacity(1.))
                .child(
                    Button::new(("close-window-tab", index))
                        .debug_selector(move || format!("close-window-tab-{index}"))
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(icon(Glyph::CloseTab)))
                        .tooltip("Close Tab")
                        .on_click(move |_, window, cx| {
                            for_tab(handle, window, cx, "close", Window::remove_window);
                        }),
                ),
        )
        .on_click(move |_, window, cx| {
            for_tab(handle, window, cx, "select", |window| {
                window.activate_window();
            });
        })
}

/// Runs `act` on the window behind a tab: directly when it is `window`, the one being clicked,
/// which cannot be updated through its handle while it handles the click. A failure is logged
/// with `verb`.
fn for_tab(
    handle: AnyWindowHandle,
    window: &mut Window,
    cx: &mut App,
    verb: &str,
    act: fn(&mut Window),
) {
    if handle.window_id() == window.window_handle().window_id() {
        act(window);
        return;
    }
    if let Err(error) = handle.update(cx, |_, window, _| act(window)) {
        log::error(&format!(
            "tab bar: could not {verb} window {:?}: {error:#}",
            handle.window_id()
        ));
    }
}

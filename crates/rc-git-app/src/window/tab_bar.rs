//! The window tab strip under the toolbar. GPUI hides the native tab bar of a tabbed window
//! and leaves drawing it to the app, so each repository window draws its tab group from
//! `SystemWindowTabController`: one tab per window, the selected one is this window, a click
//! brings that window's tab forward, and each tab's close button closes that window. The tabs
//! share the window's width equally, and a plus button after them opens a repository as Open
//! (⌘O) does. The strip shows only while the group holds two or more tabs, as the native bar
//! does by default.

use gpui::{
    AnyWindowHandle, App, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    SystemWindowTabController, Window, WindowId,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{ActiveTheme, Icon, Sizable};

use crate::icons::{Glyph, icon};
use crate::log;
use crate::menus::OpenRepository;

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
    let selected = tabs.iter().position(|tab| tab.id == current)?;
    let handles: Vec<AnyWindowHandle> = tabs.iter().map(|tab| tab.handle).collect();
    let mut bar = TabBar::new("window-tabs")
        .small()
        .selected_index(selected)
        .on_click(move |index, window, cx| {
            if let Some(handle) = handles.get(*index) {
                for_tab(*handle, window, cx, "select", |window| {
                    window.activate_window();
                });
            }
        })
        .last_empty_space(gpui::div())
        .suffix(
            Button::new("open-repository-tab")
                .debug_selector(|| "open-repository-tab".to_owned())
                .xsmall()
                .ghost()
                .icon(Icon::new(icon(Glyph::OpenTab)))
                .tooltip("Open Repository (\u{2318}O)")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenRepository), cx)),
        );
    for (index, tab) in tabs.iter().enumerate() {
        let handle = tab.handle;
        bar = bar.child(
            Tab::new()
                .debug_selector(move || format!("window-tab-{index}"))
                .flex_1()
                .label(tab.title.clone())
                .suffix(
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
        );
    }
    Some(
        gpui::div()
            .debug_selector(|| "window-tabs".to_owned())
            .flex_none()
            .w_full()
            .bg(cx.theme().sidebar)
            .child(bar),
    )
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

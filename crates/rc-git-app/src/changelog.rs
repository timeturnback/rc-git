//! Help ▸ Changelog: a read-only window drawing kit's built-in [`changelog::SOURCE`] through the
//! file pane's markdown preview, so it scrolls, wraps and styles headings the way a README
//! does. One window serves every ask.

use gpui::{App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowHandle, WindowOptions};
use gpui_component::Root;
use rc_git_kit::changelog;

use crate::files::markdown::MarkdownPreview;
use crate::log;
use crate::style;

/// The Changelog window, once opened.
#[derive(Default)]
struct ChangelogWindow(Option<WindowHandle<Root>>);

impl gpui::Global for ChangelogWindow {}

/// Brings the Changelog window forward, or opens it.
pub fn show(cx: &mut App) {
    let open = cx.try_global::<ChangelogWindow>().and_then(|shown| shown.0);
    if let Some(handle) = open
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            gpui::size(style::CHANGELOG_WIDTH, style::CHANGELOG_HEIGHT),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Changelog".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        window_min_size: Some(gpui::size(
            style::CHANGELOG_MIN_WIDTH,
            style::CHANGELOG_MIN_HEIGHT,
        )),
        ..WindowOptions::default()
    };
    let document = std::sync::Arc::new(changelog::document());
    match cx.open_window(options, move |window, cx| {
        let preview = cx.new(|cx| MarkdownPreview::new(document, None, cx));
        cx.new(|cx| Root::new(preview, window, cx))
    }) {
        Ok(handle) => cx.set_global(ChangelogWindow(Some(handle))),
        Err(error) => log::error(&format!("open changelog window: {error:#}")),
    }
}

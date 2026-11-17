//! About rc-git: the Swift app showed `NSApplication.orderFrontStandardAboutPanel`, which
//! reads the bundle's name and version itself. GPUI has no such panel, so this is a plain
//! window built from [`AppBuild`]: the name, and the version line with the build date. One
//! window serves every ask.

use gpui::{
    App, AppContext, Bounds, Context, Global, IntoElement, ParentElement, Render, SharedString,
    Styled, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div,
};
use gpui_component::{ActiveTheme, Root, v_flex};
use rc_git_kit::app_build::AppBuild;
use rc_git_kit::update_copy::installed_line;

use crate::log;
use crate::style;

/// The About window, once opened.
#[derive(Default)]
struct AboutWindow(Option<WindowHandle<Root>>);

impl Global for AboutWindow {}

/// The About window's content.
pub struct AboutView {
    name: SharedString,
    version: SharedString,
}

impl AboutView {
    /// About `build`.
    #[must_use]
    pub fn new(build: &AppBuild) -> Self {
        Self {
            name: build.name.clone().into(),
            version: installed_line(build, "Version", rc_git_mac::local_utc_offset).into(),
        }
    }

    /// The lines it shows.
    #[cfg(test)]
    pub(crate) fn text(&self) -> (String, String) {
        (self.name.to_string(), self.version.to_string())
    }
}

impl Render for AboutView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(style::ROW_GAP)
            .p(style::GROUP_GAP)
            .bg(theme.background)
            .child(
                div()
                    .text_size(style::MARKDOWN_HEADINGS[1])
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(self.name.clone()),
            )
            .child(
                div()
                    .text_size(style::TEXT_CAPTION)
                    .text_color(theme.muted_foreground)
                    .child(self.version.clone()),
            )
    }
}

/// Brings the About window forward, or opens it for `build`.
pub fn show(build: &AppBuild, cx: &mut App) {
    let open = cx.try_global::<AboutWindow>().and_then(|about| about.0);
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
            gpui::size(style::ABOUT_WIDTH, style::ABOUT_HEIGHT),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(format!("About {}", build.name).into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        is_resizable: false,
        ..WindowOptions::default()
    };
    let view = AboutView::new(build);
    match cx.open_window(options, move |window, cx| {
        let view = cx.new(|_| view);
        cx.new(|cx| Root::new(view, window, cx))
    }) {
        Ok(handle) => cx.set_global(AboutWindow(Some(handle))),
        Err(error) => log::error(&format!("open about window: {error:#}")),
    }
}

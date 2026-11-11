//! The sidebar's width and collapse, applied from `LayoutPolicy` and nothing else, and the
//! window frame and main split saved under the keys the `AppKit` app used.

use std::ops::Range;

use gpui::{App, Pixels, Window, px};
use rc_git_kit::layout_policy::{
    Layout, SIDEBAR_MAXIMUM_WIDTH, SIDEBAR_MINIMUM_WIDTH, SidebarCollapse, Size, layout,
};
use rc_git_kit::platform::keys::{MAIN_SPLIT, MAIN_WINDOW_FRAME};
use rc_git_kit::platform::stores::{SharedSettings, SidebarWidthPin, SubviewFrame, WindowFrame};
use rc_git_kit::window_geometry::{TopLeftRect, saved_sidebar, sidebar_frames, to_appkit};

use crate::log;

/// The range a drag may move the sidebar's edge through.
#[must_use]
pub fn sidebar_range() -> Range<Pixels> {
    points(SIDEBAR_MINIMUM_WIDTH)..points(SIDEBAR_MAXIMUM_WIDTH)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "window geometry in points, far inside f32's range"
)]
fn points(value: f64) -> Pixels {
    px(value as f32)
}

fn value(pixels: Pixels) -> f64 {
    f64::from(f32::from(pixels))
}

/// The sidebar of one window.
pub struct SidebarLayout {
    settings: SharedSettings,
    width: f64,
    collapsed: bool,
    collapse: SidebarCollapse,
    current: Option<Layout>,
    window: Size,
}

impl SidebarLayout {
    /// The sidebar as the main split last saved it, or open at its minimum width.
    #[must_use]
    pub fn load(settings: SharedSettings) -> Self {
        let saved = SubviewFrame::load_split(&*settings, MAIN_SPLIT)
            .as_deref()
            .and_then(saved_sidebar);
        Self {
            settings,
            width: saved.map_or(SIDEBAR_MINIMUM_WIDTH, |saved| saved.width),
            collapsed: saved.is_some_and(|saved| saved.collapsed),
            collapse: SidebarCollapse::new(),
            current: None,
            window: Size {
                width: 0.0,
                height: 0.0,
            },
        }
    }

    /// Whether the sidebar is collapsed.
    #[must_use]
    pub fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// The sidebar's width for the split.
    #[must_use]
    pub fn width_pixels(&self) -> Pixels {
        points(self.width)
    }

    /// Applies the layout for the window's new size: a window too narrow for the sidebar
    /// collapses it, and widening re-opens only a sidebar the policy closed. Saves the frame.
    pub fn window_resized(&mut self, window: &Window, cx: &App) {
        let size = window.viewport_size();
        self.window = Size {
            width: value(size.width),
            height: value(size.height),
        };
        let next = layout(self.window);
        if let Some(collapsed) = self.collapse.apply(next, self.current, self.collapsed) {
            self.collapsed = collapsed;
            self.save_split();
        }
        self.current = Some(next);
        if !window.is_fullscreen() {
            save_frame(&*self.settings, window, cx);
        }
    }

    /// The user dragged the divider to `width`: the width is theirs from now on.
    pub fn dragged(&mut self, width: f64) {
        self.width = width.clamp(SIDEBAR_MINIMUM_WIDTH, SIDEBAR_MAXIMUM_WIDTH);
        if let Err(error) = SidebarWidthPin::new(std::sync::Arc::clone(&self.settings)).pin() {
            log::error(&error.to_string());
        }
        self.save_split();
    }

    /// The user opened or closed the sidebar by hand.
    pub fn toggle_by_user(&mut self) {
        self.collapsed = !self.collapsed;
        self.collapse.user_toggled();
        self.save_split();
    }

    fn save_split(&self) {
        let frames = sidebar_frames(
            self.width,
            self.collapsed,
            self.window.width,
            self.window.height,
        );
        if let Err(error) = SubviewFrame::save_split(&*self.settings, MAIN_SPLIT, &frames) {
            log::error(&error.to_string());
        }
    }
}

/// Saves `window`'s frame the way `AppKit` autosaved it: bottom-left origin, plus the visible
/// frame of the screen it is on.
fn save_frame(
    settings: &dyn rc_git_kit::platform::settings::SettingsStore,
    window: &Window,
    cx: &App,
) {
    let (Some(display), Some(primary)) = (window.display(cx), primary_height(cx)) else {
        return;
    };
    let bounds = window.bounds();
    let visible = display.visible_bounds();
    let frame = WindowFrame {
        frame: to_appkit(top_left(bounds), primary),
        screen: to_appkit(top_left(visible), primary),
    };
    if let Err(error) = frame.save(settings, MAIN_WINDOW_FRAME) {
        log::error(&error.to_string());
    }
}

/// A GPUI rectangle as kit's top-left one.
#[must_use]
pub fn top_left(bounds: gpui::Bounds<Pixels>) -> TopLeftRect {
    TopLeftRect {
        x: value(bounds.origin.x),
        y: value(bounds.origin.y),
        width: value(bounds.size.width),
        height: value(bounds.size.height),
    }
}

/// The primary display's height, which the `AppKit` origin is measured from.
#[must_use]
pub fn primary_height(cx: &App) -> Option<f64> {
    cx.primary_display()
        .map(|display| value(display.bounds().size.height))
}

//! The sidebar's width and collapse, applied from `LayoutPolicy` and nothing else, and the
//! window frame and main split saved under the keys the `AppKit` app used.

use std::ops::Range;

use gpui::{App, Pixels, Window};
use rc_git_kit::layout_policy::{
    Layout, SIDEBAR_MAXIMUM_WIDTH, SIDEBAR_MINIMUM_WIDTH, SidebarCollapse, Size, layout,
};
use rc_git_kit::platform::keys::{MAIN_SPLIT, MAIN_WINDOW_FRAME};
use rc_git_kit::platform::stores::{SharedSettings, SidebarWidthPin, SubviewFrame, WindowFrame};
use rc_git_kit::window_geometry::{TopLeftRect, saved_sidebar, sidebar_frames, to_appkit};

use crate::log;
use crate::style::{points, value};

/// The range a drag may move the sidebar's edge through.
#[must_use]
pub fn sidebar_range() -> Range<Pixels> {
    points(SIDEBAR_MINIMUM_WIDTH)..points(SIDEBAR_MAXIMUM_WIDTH)
}

/// The sidebar of one window.
pub struct SidebarLayout {
    settings: SharedSettings,
    width: f64,
    collapsed: bool,
    collapse: SidebarCollapse,
    current: Option<Layout>,
    window: Size,
    /// Set while the split resizes to a width the fit applied, so that resize is no drag.
    fit_echo: bool,
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
            fit_echo: false,
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
    /// collapses it, and widening re-opens only a sidebar the policy closed. Saves the frame,
    /// except in full screen and after the tabs were handed to a relaunching build.
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
        // Once the tabs went to a relaunching build, the frame is that build's to write too.
        if !window.is_fullscreen() && !crate::app::SessionGlobal::handed_off(cx) {
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

    /// The sidebar's width in points.
    #[must_use]
    pub fn width(&self) -> f64 {
        self.width
    }

    /// Whether the user dragged the divider, which makes the width the user's: the fit stops.
    #[must_use]
    pub fn is_pinned(&self) -> bool {
        SidebarWidthPin::new(std::sync::Arc::clone(&self.settings)).is_pinned()
    }

    /// The fit sized the sidebar to `width`. `resizes` says the split resizes to it now, and
    /// that resize is the fit's, not a drag.
    pub fn fit(&mut self, width: f64, resizes: bool) {
        self.width = width.clamp(SIDEBAR_MINIMUM_WIDTH, SIDEBAR_MAXIMUM_WIDTH);
        self.fit_echo = resizes;
        self.save_split();
    }

    /// Whether the split's resize now is the fit's own; answers once per fit.
    pub fn take_fit_echo(&mut self) -> bool {
        std::mem::take(&mut self.fit_echo)
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

//! The toolbar row drawn in the transparent titlebar: sidebar toggle, back and forward, the
//! Files/History/Changes switch, then Back to Default and Sync at the trailing edge. The sync
//! controls follow `SyncModel`'s availability and titles; while a workflow runs, Sync becomes
//! the way to cancel it and stays on screen.

use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Disableable, Icon, Selectable, Sizable};
use rc_git_kit::back_forward::{ContentMode, Direction};
use rc_git_kit::guard_copy::{back_to_default_tooltip, sync_tooltip};
use rc_git_kit::sync::back_to_default_title;

use crate::icons::{Glyph, icon};
use crate::style;
use crate::window::sync::SyncCommand;
use crate::window::{RepoWindow, WindowEvent};

/// The mode switch's segments, in order.
const MODES: [(ContentMode, &str); 3] = [
    (ContentMode::Files, "Files"),
    (ContentMode::History, "History"),
    (ContentMode::Changes, "Changes"),
];

impl RepoWindow {
    pub(super) fn render_toolbar(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let mut modes = div().flex().items_center();
        for (mode, label) in MODES {
            modes = modes.child(
                Button::new(label)
                    .small()
                    .ghost()
                    .label(label)
                    .selected(self.mode == mode)
                    .on_click(cx.listener(move |this, _, _, cx| this.switch_mode(mode, cx))),
            );
        }
        div()
            .id("toolbar")
            .h(style::TOOLBAR_HEIGHT)
            .flex_none()
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .pl(style::TRAFFIC_LIGHT_INSET)
            .pr(style::MARGIN)
            .border_b(style::HAIRLINE)
            .border_color(theme.title_bar_border)
            .bg(theme.title_bar)
            .on_click(|event: &ClickEvent, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
            .children(version_badge(cx))
            .child(
                Button::new("toggle-sidebar")
                    .small()
                    .ghost()
                    .icon(Icon::new(icon(Glyph::Sidebar)))
                    .tooltip("Toggle Sidebar")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            )
            .child(self.navigation(cx))
            .child(div().w(style::GROUP_GAP))
            .child(modes)
            .child(div().flex_1())
            .child(self.sync_controls(cx))
    }

    fn navigation(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex()
            .items_center()
            .child(
                Button::new("back")
                    .small()
                    .ghost()
                    .icon(Icon::new(icon(Glyph::Back)))
                    .tooltip("Back (\u{2318}[)")
                    .disabled(!self.can_go_back)
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(WindowEvent::Navigate(Direction::Back));
                    })),
            )
            .child(
                Button::new("forward")
                    .small()
                    .ghost()
                    .icon(Icon::new(icon(Glyph::Forward)))
                    .tooltip("Forward (\u{2318}])")
                    .disabled(!self.can_go_forward)
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(WindowEvent::Navigate(Direction::Forward));
                    })),
            )
    }

    fn sync_controls(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let availability = self.sync_availability(cx);
        let (tooltip, default_title, busy) = match &self.sync {
            Some(sync) => {
                let sync = sync.read(cx);
                (
                    sync_tooltip(sync.state(), sync.remote_status(), sync.status_error()),
                    back_to_default_title(sync.default_branch()),
                    sync.is_busy(),
                )
            }
            None => ("Sync".to_owned(), back_to_default_title(None), false),
        };
        let can_back = availability.is_some_and(|a| a.can_back_to_default);
        let sync_enabled = availability.is_some_and(|a| a.sync_button_enabled());
        div()
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .child(
                Button::new("back-to-default")
                    .small()
                    .ghost()
                    .icon(Icon::new(icon(Glyph::BackToDefault)))
                    .tooltip(back_to_default_tooltip(&default_title))
                    .disabled(!can_back)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.run_sync(SyncCommand::BackToDefault, cx);
                    })),
            )
            .child(
                Button::new("sync")
                    .small()
                    .ghost()
                    .icon(Icon::new(icon(if busy {
                        Glyph::Busy
                    } else {
                        Glyph::Sync
                    })))
                    .tooltip(tooltip)
                    .disabled(!sync_enabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if busy {
                            this.cancel_sync(cx);
                        } else {
                            this.run_sync(SyncCommand::Sync, cx);
                        }
                    })),
            )
    }
}

/// The version the bundle was stamped with, right of the traffic lights
/// (`MainWindowController.buildVersionBadge`): the short version, with the full version line
/// as its tooltip. `None` before the stamp was read (tests).
fn version_badge(cx: &gpui::App) -> Option<impl IntoElement + use<>> {
    let build = crate::update::BuildInfo::build(cx)?;
    let line = SharedString::from(build.version_line());
    Some(
        div()
            .id("version-badge")
            .text_size(style::TEXT_CAPTION)
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from(build.short_version()))
            .tooltip(move |window, cx| Tooltip::new(line.clone()).build(window, cx)),
    )
}

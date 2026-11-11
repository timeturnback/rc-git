//! The toolbar row drawn in the transparent titlebar: sidebar toggle, back and forward, the
//! Files/History/Changes switch, then Back to Default and Sync at the trailing edge. The sync
//! controls show `SyncModel`'s availability and titles; their dialogs land in phase 5, so they
//! stay disabled here.

use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Disableable, Icon, Selectable, Sizable};
use rc_git_kit::back_forward::{ContentMode, Direction};
use rc_git_kit::sync::{back_to_default_title, idle_sync_tooltip};

use crate::icons::{Glyph, icon};
use crate::style;
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
        let (tooltip, default_title, busy) = match &self.sync {
            Some(sync) => {
                let sync = sync.read(cx);
                (
                    idle_sync_tooltip(sync.remote_status(), sync.status_error()),
                    back_to_default_title(sync.default_branch()),
                    sync.is_busy(),
                )
            }
            None => ("Sync".to_owned(), back_to_default_title(None), false),
        };
        div()
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .child(
                Button::new("back-to-default")
                    .small()
                    .ghost()
                    .icon(Icon::new(icon(Glyph::BackToDefault)))
                    .tooltip(default_title)
                    .disabled(true),
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
                    .disabled(true),
            )
    }
}

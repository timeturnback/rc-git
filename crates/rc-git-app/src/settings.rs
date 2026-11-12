//! The Settings window (`SettingsWindowController`): the ssh hosts whose keys the user approved
//! in the app, with Remove. Removing a host is how a legitimately rotated key is recovered
//! from: the next fetch from that host offers the trust sheet again. Kit's
//! [`ApprovedHostsModel`] owns the list and the removal; this view only shows it.

use std::collections::BTreeSet;
use std::sync::Arc;

use gpui::{
    ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
};
use gpui_component::ActiveTheme;
use gpui_component::button::Button;
use gpui_component::{Disableable, h_flex, v_flex};
use rc_git_kit::platform::host_keys::{ApprovedHostsModel, HostKeyStore};

use crate::style;

/// What Settings says when no host is approved.
pub const NO_HOSTS: &str = "No host keys approved in RCGit.";

/// The note under the heading.
pub const HOSTS_NOTE: &str = "Keys you trusted in RCGit, per host. Remove a host after its key \
                              was rotated: the next fetch asks you to approve the new key. Keys \
                              in known_hosts are managed with ssh, not here.";

/// The Settings window's content.
pub struct SettingsView {
    hosts: ApprovedHostsModel<Arc<dyn HostKeyStore>>,
    selected: BTreeSet<usize>,
}

impl SettingsView {
    /// Settings over the app's host-key store.
    #[must_use]
    pub fn new(store: Arc<dyn HostKeyStore>) -> Self {
        Self {
            hosts: ApprovedHostsModel::new(store),
            selected: BTreeSet::new(),
        }
    }

    /// The hosts on screen, sorted.
    #[cfg(test)]
    pub(crate) fn hosts(&self) -> Vec<String> {
        self.hosts
            .rows()
            .iter()
            .map(|row| row.host.clone())
            .collect()
    }

    /// Re-reads the approvals: a repository window may have approved a host since Settings was
    /// last shown.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.hosts.reload();
        self.selected.clear();
        cx.notify();
    }

    /// Selects row `index` alone, or with ⌘ or ⇧ adds it to the selection or takes it out.
    pub fn select(&mut self, index: usize, extend: bool, cx: &mut Context<Self>) {
        if !extend {
            self.selected.clear();
            self.selected.insert(index);
        } else if !self.selected.remove(&index) {
            self.selected.insert(index);
        }
        cx.notify();
    }

    /// Remove: forgets the selected hosts. A store that refused a write is said in a prompt;
    /// the list shows what the store holds either way.
    pub fn remove_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen: Vec<usize> = self.selected.iter().copied().collect();
        self.selected.clear();
        if let Err(error) = self.hosts.remove(&chosen) {
            drop(window.prompt(
                gpui::PromptLevel::Warning,
                "Could not remove the host key",
                Some(&error.to_string()),
                &["OK"],
                cx,
            ));
        }
        cx.notify();
    }

    fn clicked(&mut self, index: usize, event: &ClickEvent, cx: &mut Context<Self>) {
        let modifiers = event.modifiers();
        self.select(index, modifiers.platform || modifiers.shift, cx);
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let theme = cx.theme();
        let (border, active, muted) = (theme.border, theme.list_active, theme.muted_foreground);
        let mut list = div()
            .id("approved-hosts")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .border(style::HAIRLINE)
            .border_color(border)
            .child(
                h_flex()
                    .px(style::ROW_GAP)
                    .h(style::LIST_ROW_HEIGHT)
                    .border_b(style::HAIRLINE)
                    .border_color(border)
                    .text_size(style::TEXT_CAPTION)
                    .text_color(muted)
                    .child(div().w(style::SETTINGS_HOST_WIDTH).child("Host"))
                    .child(div().flex_1().child("Fingerprint")),
            );
        for (index, row) in self.hosts.rows().iter().enumerate() {
            let selected = self.selected.contains(&index);
            list = list.child(
                h_flex()
                    .id(("host", index))
                    .px(style::ROW_GAP)
                    .h(style::LIST_ROW_HEIGHT)
                    .text_size(style::TEXT_BODY)
                    .when(selected, |row| row.bg(active))
                    .child(
                        div()
                            .w(style::SETTINGS_HOST_WIDTH)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(SharedString::from(row.host.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .font_family(style::MONOSPACE_FONT)
                            .text_size(style::TEXT_CAPTION)
                            .child(SharedString::from(row.fingerprint.clone())),
                    )
                    .on_click(
                        cx.listener(move |this, event, _, cx| this.clicked(index, event, cx)),
                    ),
            );
        }
        list
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, foreground, muted) =
            (theme.background, theme.foreground, theme.muted_foreground);
        let empty = self.hosts.rows().is_empty();
        let nothing_selected = self.selected.is_empty();
        let rows = self.render_rows(cx);
        v_flex()
            .size_full()
            .gap(style::BAR_PADDING)
            .p(style::MARGIN * 2.)
            .pt(style::TOOLBAR_HEIGHT)
            .bg(background)
            .text_color(foreground)
            .child(
                div()
                    .text_size(style::TEXT_BODY)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Approved SSH Host Keys"),
            )
            .child(
                div()
                    .text_size(style::TEXT_CAPTION)
                    .text_color(muted)
                    .child(HOSTS_NOTE),
            )
            .child(rows)
            .child(
                h_flex()
                    .child(
                        div()
                            .flex_1()
                            .text_size(style::TEXT_CAPTION)
                            .text_color(muted)
                            .when(empty, |note| note.child(NO_HOSTS)),
                    )
                    .child(
                        Button::new("remove-host")
                            .label("Remove")
                            .disabled(nothing_selected)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.remove_selected(window, cx);
                            })),
                    ),
            )
    }
}

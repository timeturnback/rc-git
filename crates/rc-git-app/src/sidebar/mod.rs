//! The sidebar (`SidebarViewController`, `RefBannerView`): the navigation of whichever mode is
//! showing. History shows the ref list alone; Files and Changes show the banner over the
//! mode's section. What the banner says comes from [`BranchBanner`] and nowhere else.

pub mod refs;

use gpui::{
    Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
};
use gpui_component::{ActiveTheme, Icon};
use rc_git_kit::back_forward::ContentMode;
use rc_git_kit::branch_banner::BranchBanner;
use rc_git_kit::browser::BrowserModel;

use crate::changes::list::ChangeListView;
use crate::files::tree::FileTreeView;
use crate::icons::{Glyph, banner_glyph, icon};
use crate::sidebar::refs::RefListView;
use crate::style;

/// What the Changes section says while the window shows no repository.
pub const CHANGES_PENDING: &str = "Open a repository to see its changes.";

/// What the sidebar tells its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarEvent {
    /// The banner was clicked in Files: open the ref picker.
    BannerClicked,
}

/// The sidebar of one window.
pub struct Sidebar {
    model: Entity<BrowserModel>,
    mode: ContentMode,
    tree: Entity<FileTreeView>,
    refs: Entity<RefListView>,
    changes: Option<Entity<ChangeListView>>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    /// A sidebar over `model` showing Files.
    pub fn new(
        model: Entity<BrowserModel>,
        tree: Entity<FileTreeView>,
        refs: Entity<RefListView>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        Self {
            model,
            mode: ContentMode::Files,
            tree,
            refs,
            changes: None,
        }
    }

    /// The Files tree.
    #[cfg(test)]
    pub(crate) fn tree(&self) -> &Entity<FileTreeView> {
        &self.tree
    }

    /// Puts the repository's Changes section under the banner in Changes.
    pub fn set_changes(&mut self, changes: Option<Entity<ChangeListView>>, cx: &mut Context<Self>) {
        self.changes = changes;
        cx.notify();
    }

    /// Shows the section of `mode`.
    pub fn set_mode(&mut self, mode: ContentMode, cx: &mut Context<Self>) {
        self.mode = mode;
        cx.notify();
    }

    /// The banner's text in the current mode: what Files browses, or the branch Changes
    /// compares with; marked stale when the refs could not be re-read.
    #[must_use]
    pub fn banner(&self, cx: &gpui::App) -> BranchBanner {
        let model = self.model.read(cx);
        let banner = match self.mode {
            ContentMode::Changes => BranchBanner::checked_out(model.head()),
            ContentMode::Files | ContentMode::History => BranchBanner::browsing(
                model.selected_ref(),
                model.browsed_commit(),
                model.browsing_working_tree(),
                model.head(),
            ),
        };
        banner.marking_refs_stale(model.refs_stale())
    }

    fn render_banner(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let banner = self.banner(cx);
        let theme = cx.theme();
        let pickable = self.mode == ContentMode::Files;
        let glyph = banner_glyph(banner.symbol);
        div()
            .id("ref-banner")
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .mx(style::MARGIN)
            .my(style::BAR_PADDING)
            .px(style::MARGIN)
            .py(style::BAR_PADDING)
            .rounded(style::RADIUS)
            .when_some(glyph, |row, glyph| {
                row.child(
                    Icon::new(icon(glyph))
                        .size(style::ICON_SIZE)
                        .text_color(theme.muted_foreground),
                )
            })
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(
                        div()
                            .text_size(style::TEXT_BODY)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(SharedString::from(banner.title.clone())),
                    )
                    .when_some(banner.detail.clone(), |column, detail| {
                        column.child(
                            div()
                                .text_size(style::TEXT_CAPTION)
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(detail)),
                        )
                    }),
            )
            .when(pickable, |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(theme.list_hover))
                    .child(
                        Icon::new(icon(Glyph::Disclosure))
                            .size(style::ICON_SIZE)
                            .text_color(theme.muted_foreground),
                    )
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::BannerClicked)))
            })
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let column = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground);
        match self.mode {
            ContentMode::History => column.child(self.refs.clone()),
            ContentMode::Files => column
                .child(self.render_banner(cx))
                .child(div().flex_1().min_h_0().child(self.tree.clone())),
            ContentMode::Changes => match self.changes.clone() {
                Some(changes) => column
                    .child(self.render_banner(cx))
                    .child(div().flex_1().min_h_0().child(changes)),
                None => column.child(self.render_banner(cx)).child(
                    div()
                        .p(style::MARGIN)
                        .text_size(style::TEXT_BODY)
                        .text_color(muted)
                        .child(CHANGES_PENDING),
                ),
            },
        }
    }
}

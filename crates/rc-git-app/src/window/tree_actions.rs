//! What the Files tree asks of the window (`MainWindowController`'s
//! `treeController.onShowHistory` and `onContentWidthChanged`): Show File History scopes
//! History to the row's path, and the sidebar fits the width the tree's rows need.

use gpui::{Context, Window, px};
use rc_git_kit::back_forward::ContentMode;
use rc_git_kit::layout_policy::{SidebarFit, sidebar_width};

use crate::files::tree::TreeAction;
use crate::window::RepoWindow;

impl RepoWindow {
    pub(super) fn tree_action(
        &mut self,
        action: &TreeAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            TreeAction::ShowHistory {
                path,
                path_is_exact,
            } => self.show_history_of(path, *path_is_exact, cx),
            TreeAction::ContentWidth(width) => {
                self.content_width = Some(*width);
                self.apply_fit(window, cx);
            }
        }
    }

    /// Shows History scoped to `path` and records one stop once it is scoped. An inexact path
    /// is refused by the model and leaves the pane where it is.
    fn show_history_of(&mut self, path: &str, path_is_exact: bool, cx: &mut Context<Self>) {
        if !path_is_exact {
            self.model
                .update(cx, |model, cx| model.show_path_history(path, false, cx))
                .detach();
            return;
        }
        self.set_mode(ContentMode::History, cx);
        let scope = self
            .model
            .update(cx, |model, cx| model.show_path_history(path, true, cx));
        Self::record_stop_after(scope, cx);
    }

    /// Sizes the sidebar to the tree's rows by `LayoutPolicy`, while [`SidebarFit::applies`]:
    /// a divider the user dragged is the user's, only Files shows the tree, and a collapsed
    /// sidebar stays collapsed. Runs on every frame and changes nothing once the width is
    /// right, so the fit catches up the moment Files shows or the sidebar re-opens.
    pub(super) fn apply_fit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(content) = self.content_width else {
            return;
        };
        let applies = SidebarFit::applies(
            self.layout.is_pinned(),
            self.mode == ContentMode::Files,
            self.layout.collapsed(),
        );
        let width = sidebar_width(content);
        if !applies || (width - self.layout.width()).abs() < 0.5 {
            return;
        }
        let laid_out = self.split.read(cx).sizes().len() > 1;
        self.layout.fit(width, laid_out);
        if laid_out {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a sidebar width in points, far inside f32's range"
            )]
            let points = px(width as f32);
            self.split
                .update(cx, |split, cx| split.resize_panel(0, points, window, cx));
        }
    }
}

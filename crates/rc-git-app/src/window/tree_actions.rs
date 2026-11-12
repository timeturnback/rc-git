//! What the Files tree's context menu asks of the window (`MainWindowController`'s
//! `treeController.onShowHistory`): Show File History scopes History to the row's path.

use gpui::Context;
use rc_git_kit::back_forward::ContentMode;

use crate::files::tree::TreeAction;
use crate::window::RepoWindow;

impl RepoWindow {
    /// Shows History scoped to the row's path and records one stop once it is scoped. An
    /// inexact path is refused by the model and leaves the pane where it is.
    pub(super) fn tree_action(&mut self, action: &TreeAction, cx: &mut Context<Self>) {
        let TreeAction::ShowHistory {
            path,
            path_is_exact,
        } = action;
        if !*path_is_exact {
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
}

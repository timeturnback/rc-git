//! New Branch, New Tag, Delete Branch and Delete Tag from the ref list's context menu
//! (`MainWindowController.performRefAction`). Names are asked for with the name sheet,
//! deletions follow the guard's plan (a branch's plan is read off the UI thread, since it
//! walks the branch's commits), and success needs no handling: the ref watcher sees the
//! change and reloads the list.

use std::sync::Arc;

use gpui::{AppContext, Context, PromptLevel, Window};
use rc_git_core::RefEntry;
use rc_git_kit::blocking::blocking;
use rc_git_kit::guard_bridge::failure_title;
use rc_git_kit::ref_menu::{RefMenuAction, name_prompt, verb};
use rc_git_kit::repository_actions::plan_tag_delete;

use crate::sheets::ref_name::RefNameSheet;
use crate::sheets::{self, OpenSheet};
use crate::window::RepoWindow;
use crate::window::guarded::alert;
use crate::window::guarded::{Guarded, Perform};

impl RepoWindow {
    /// A request from the ref list's context menu. The list raised it from inside its own
    /// event, so the sheet opens once that event has returned.
    pub(super) fn ref_action(
        &mut self,
        action: RefMenuAction,
        entry: RefEntry,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let handle = self.handle;
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        this.run_ref_action(action, &entry, window, cx);
                    })
                })
                .ok();
        });
    }

    /// Runs `action` on `entry` now.
    pub(crate) fn run_ref_action(
        &mut self,
        action: RefMenuAction,
        entry: &RefEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            RefMenuAction::NewBranch | RefMenuAction::NewTag => {
                self.ask_ref_name(action, entry, window, cx);
            }
            RefMenuAction::DeleteBranch => self.delete_branch(entry.short_name.clone(), window, cx),
            RefMenuAction::DeleteTag => {
                let name = entry.short_name.clone();
                let expected = entry.target;
                let perform: Perform = Arc::new(move |actions, input| {
                    actions.delete_tag(&name, expected, input).map(|_| None)
                });
                let guarded = plain(verb(action), perform);
                self.follow_decision(plan_tag_delete(&entry.short_name), guarded, window, cx);
            }
        }
    }

    fn ask_ref_name(
        &mut self,
        action: RefMenuAction,
        entry: &RefEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((title, message)) = name_prompt(action, entry) else {
            return;
        };
        let revspec = entry.target.to_hex();
        let this = cx.entity().downgrade();
        let sheet = cx.new(|cx| {
            RefNameSheet::new(
                message,
                Box::new(move |name, window, cx| {
                    let perform: Perform = Arc::new(move |actions, _| {
                        let created = if action == RefMenuAction::NewTag {
                            actions.create_tag(&name, &revspec)
                        } else {
                            actions.create_branch(&name, &revspec)
                        };
                        created.map(|()| None)
                    });
                    let guarded = plain(verb(action), perform);
                    this.update(cx, |this, cx| this.run_guarded(guarded, None, window, cx))
                        .ok();
                }),
                window,
                cx,
            )
        });
        self.sheet = Some(OpenSheet::RefName(sheet.downgrade()));
        sheets::open(title.into(), &sheet, window, cx);
    }

    /// Plans the deletion off the UI thread, then follows the decision it returns.
    fn delete_branch(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(actions) = self.actions.clone() else {
            return;
        };
        let planned = name.clone();
        let plan = blocking(cx.background_executor(), move || {
            actions.plan_branch_delete(&planned)
        });
        cx.spawn_in(window, async move |this, cx| {
            let plan = plan.await;
            this.update_in(cx, |this, window, cx| match plan {
                Ok(decision) => {
                    let perform: Perform = Arc::new(move |actions, input| {
                        actions.delete_branch(&name, input).map(|_| None)
                    });
                    let guarded = plain(verb(RefMenuAction::DeleteBranch), perform);
                    this.follow_decision(decision, guarded, window, cx);
                }
                Err(error) => {
                    let title = failure_title(verb(RefMenuAction::DeleteBranch));
                    alert(PromptLevel::Warning, &title, &error.to_string(), window, cx).detach();
                }
            })
            .ok();
        })
        .detach();
    }
}

/// `perform` under `verb` with the default failure alert and nothing after success.
fn plain(verb: &str, perform: Perform) -> Guarded {
    Guarded {
        verb: verb.to_owned(),
        perform,
        on_success: None,
        on_failure: None,
    }
}

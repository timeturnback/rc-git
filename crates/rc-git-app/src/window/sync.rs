//! Fetch, Pull, Push, Sync, Back to Default and Cancel Sync (`MainWindowController`, "Sync"):
//! the commands, the prompts a network step asks through, and what a finished workflow shows.
//! Which controls can run comes from `SyncModel::availability` alone, so the menu, the
//! toolbar and the Changes sync row never disagree.

use std::rc::Rc;

use gpui::{
    AppContext, ClipboardItem, Context, Div, InteractiveElement, PromptLevel, Stateful, Window,
    prelude::FluentBuilder,
};
use rc_git_kit::guard_copy::{DIVERGED_TITLE, SYNC_FAILURE_TITLE, diverged_message};
use rc_git_kit::oneshot;
use rc_git_kit::sync::{
    CredentialReply, CredentialRequest, HostKeyReply, HostKeyRequest, SyncAvailability, SyncModel,
    SyncState, pull_title, push_title, reconcile_command,
};

use crate::changes::ChangesRequest;
use crate::menus::{BackToDefault, CancelSync, CommitIdentity, Fetch, Pull, Push, Sync};
use crate::sheets::credentials::{self, CredentialSheet};
use crate::sheets::host_key::HostKeySheet;
use crate::sheets::{self, OpenSheet};
use crate::window::RepoWindow;
use crate::window::guarded::alert;

/// One sync command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyncCommand {
    Fetch,
    Pull,
    Push,
    Sync,
    BackToDefault,
}

impl RepoWindow {
    /// What the sync controls can do now.
    pub(super) fn sync_availability(&self, cx: &gpui::App) -> Option<SyncAvailability> {
        let sync = self.sync.as_ref()?;
        Some(sync.read(cx).availability(self.has_work_dir))
    }

    /// Pull's and Push's titles, with the counts of the last status.
    #[must_use]
    pub fn sync_titles(&self, cx: &gpui::App) -> (String, String) {
        let status = self
            .sync
            .as_ref()
            .and_then(|sync| sync.read(cx).remote_status());
        (pull_title(status), push_title(status))
    }

    /// Runs `command`; a no-op while another workflow runs, as the model decides.
    pub(super) fn run_sync(&mut self, command: SyncCommand, cx: &mut Context<Self>) {
        let Some(sync) = &self.sync else {
            return;
        };
        sync.update(cx, |sync, cx| match command {
            SyncCommand::Fetch => sync.fetch(cx),
            SyncCommand::Pull => sync.pull(cx),
            SyncCommand::Push => sync.push(cx),
            SyncCommand::Sync => sync.sync(cx),
            SyncCommand::BackToDefault => sync.back_to_default_branch(cx),
        })
        .detach();
    }

    /// Stops the running fetch, pull or push.
    pub(super) fn cancel_sync(&mut self, cx: &mut Context<Self>) {
        if let Some(sync) = &self.sync {
            sync.update(cx, SyncModel::cancel_running);
        }
    }

    /// The Changes sync row's buttons.
    pub(super) fn sync_request(&mut self, request: &ChangesRequest, cx: &mut Context<Self>) {
        let command = match request {
            ChangesRequest::Fetch => SyncCommand::Fetch,
            ChangesRequest::Pull => SyncCommand::Pull,
            ChangesRequest::Push => SyncCommand::Push,
            ChangesRequest::Commit(_)
            | ChangesRequest::StashPush(_)
            | ChangesRequest::StashApply(_)
            | ChangesRequest::StashPop(_)
            | ChangesRequest::StashDrop(_)
            | ChangesRequest::Discard(_)
            | ChangesRequest::DiscardHunk { .. } => return,
        };
        self.run_sync(command, cx);
    }

    /// Hands the sync model the window's credential and host-key sheets. A prompt whose
    /// window is gone drops its sender, which the model reads as nobody to ask.
    pub(super) fn set_prompters(
        sync: &gpui::Entity<SyncModel>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let handle = window.window_handle();
        let this = cx.entity().downgrade();
        let host_this = this.clone();
        sync.update(cx, |sync, _| {
            sync.set_credential_prompter(Rc::new(move |request, sender, cx| {
                let this = this.clone();
                handle
                    .update(cx, |_, window, cx| {
                        this.update(cx, |this, cx| {
                            this.prompt_credentials(request, sender, window, cx);
                        })
                    })
                    .ok();
            }));
            sync.set_host_key_prompter(Rc::new(move |request, sender, cx| {
                let this = host_this.clone();
                handle
                    .update(cx, |_, window, cx| {
                        this.update(cx, |this, cx| {
                            this.prompt_host_key(request, sender, window, cx);
                        })
                    })
                    .ok();
            }));
        });
    }

    fn prompt_credentials(
        &mut self,
        request: CredentialRequest,
        sender: oneshot::Sender<CredentialReply>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = credentials::title(&request);
        let sheet = cx.new(|cx| CredentialSheet::new(request, sender, window, cx));
        self.sheet = Some(OpenSheet::Credentials(sheet.downgrade()));
        sheets::open(title.into(), &sheet, window, cx);
    }

    fn prompt_host_key(
        &mut self,
        request: HostKeyRequest,
        sender: oneshot::Sender<HostKeyReply>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sheet = cx.new(|_| HostKeySheet::new(request, sender));
        let title = sheet.read(cx).title();
        self.sheet = Some(OpenSheet::HostKey(sheet.downgrade()));
        sheets::open(title.into(), &sheet, window, cx);
    }

    /// Follows the workflow: a finished run is reported, then the model goes back to idle
    /// once the report closes; a branch it switched to is followed however it ended, since
    /// `HEAD` and the working tree have moved either way.
    pub(super) fn sync_state_changed(
        &mut self,
        state: &SyncState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let report = match state {
            SyncState::Done(summary) => {
                if summary.pulled_files > 0 {
                    self.changes_stale(cx);
                }
                summary.report().map(|report| {
                    let level = if report.is_problem {
                        PromptLevel::Warning
                    } else {
                        PromptLevel::Info
                    };
                    alert(level, &report.title, &report.message, window, cx)
                })
            }
            SyncState::Diverged(message) => Some(self.present_diverged(message, window, cx)),
            SyncState::Failed { message, .. } => Some(alert(
                PromptLevel::Warning,
                SYNC_FAILURE_TITLE,
                message,
                window,
                cx,
            )),
            SyncState::Idle | SyncState::Running(_) | SyncState::Cancelling(_) => return,
        };
        self.follow_switch(cx);
        let Some(sync) = self.sync.clone() else {
            return;
        };
        match report {
            Some(closed) => cx
                .spawn(async move |_, cx| {
                    closed.await;
                    sync.update(cx, SyncModel::reset);
                })
                .detach(),
            None => sync.update(cx, SyncModel::reset),
        }
    }

    /// rc-git only fast-forwards, so a diverged branch is reconciled in git: the sheet names
    /// the command and copies it. A bare repository gets the message alone.
    fn present_diverged(
        &self,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Task<Option<usize>> {
        let text = diverged_message(message, self.work_dir.as_deref());
        let Some(work_dir) = self.work_dir.clone() else {
            return alert(PromptLevel::Info, DIVERGED_TITLE, &text, window, cx);
        };
        let answer = window.prompt(
            PromptLevel::Info,
            DIVERGED_TITLE,
            Some(&text),
            &["Copy Command", "Close"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            let answer = answer.await.ok();
            if answer == Some(0) {
                cx.update(|cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(reconcile_command(&work_dir)));
                });
            }
            answer
        })
    }

    /// Points the browser at the branch a workflow just checked out: the ref watcher reloads
    /// the listing, but it keeps whatever was selected, and the user wants the switched branch.
    fn follow_switch(&mut self, cx: &mut Context<Self>) {
        let Some(sync) = &self.sync else {
            return;
        };
        let Some(branch) = sync.update(cx, |sync, _| sync.take_switched_branch()) else {
            return;
        };
        let model = self.model.clone();
        let full_name = format!("refs/heads/{branch}");
        cx.spawn(async move |_, cx| {
            let refreshed = model.update(cx, rc_git_kit::browser::BrowserModel::refresh_refs);
            refreshed.await;
            let select = model.update(cx, |model, cx| {
                let entry = model
                    .refs()
                    .refs
                    .iter()
                    .find(|entry| entry.full_name == full_name)
                    .cloned();
                entry.map(|entry| model.select(entry, cx))
            });
            if let Some(select) = select {
                select.await;
            }
        })
        .detach();
    }
}

/// The sync and identity commands on the window root, each only while it can run, so GPUI
/// shows the menu item disabled otherwise.
pub(super) fn sync_actions(
    root: Stateful<Div>,
    availability: Option<SyncAvailability>,
    has_author: bool,
    cx: &mut Context<RepoWindow>,
) -> Stateful<Div> {
    let can = |pick: fn(&SyncAvailability) -> bool| availability.as_ref().is_some_and(pick);
    root.when(can(|a| a.can_fetch), |root| {
        root.on_action(cx.listener(|this, _: &Fetch, _, cx| this.run_sync(SyncCommand::Fetch, cx)))
    })
    .when(can(|a| a.can_pull), |root| {
        root.on_action(cx.listener(|this, _: &Pull, _, cx| this.run_sync(SyncCommand::Pull, cx)))
    })
    .when(can(|a| a.can_push), |root| {
        root.on_action(cx.listener(|this, _: &Push, _, cx| this.run_sync(SyncCommand::Push, cx)))
    })
    .when(can(|a| a.can_sync), |root| {
        root.on_action(cx.listener(|this, _: &Sync, _, cx| this.run_sync(SyncCommand::Sync, cx)))
    })
    .when(can(|a| a.can_back_to_default), |root| {
        root.on_action(cx.listener(|this, _: &BackToDefault, _, cx| {
            this.run_sync(SyncCommand::BackToDefault, cx);
        }))
    })
    .when(can(|a| a.can_cancel), |root| {
        root.on_action(cx.listener(|this, _: &CancelSync, _, cx| this.cancel_sync(cx)))
    })
    .when(has_author, |root| {
        root.on_action(cx.listener(|this, _: &CommitIdentity, window, cx| {
            this.edit_commit_identity(window, cx);
        }))
    })
}

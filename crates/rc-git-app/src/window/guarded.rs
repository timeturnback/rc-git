//! Every mutation a window starts (`MainWindowController`, "Guarded mutations" and the Changes
//! tab's commit, stash and discard): plan, follow the guard's decision (run it, challenge
//! first, or show why not), run the action off the UI thread through
//! [`rc_git_kit::repository_actions::RepositoryActions`], then refresh what it changed. A
//! failure is answered as kit decides: a stale repository reloads the lists and gets a soft
//! notice, anything else an alert titled by the action's verb. Nothing here classifies an
//! action or offers a way around a challenge.
//!
//! A pull request merge comes through the same challenge sheet, when its model asks
//! ([`MergeEvent`]): the guard's decision from its fresh read, a confirm that hands the typed
//! text back to the model, which sends the merge once, and a cancel that ends it.

use std::sync::Arc;

use gpui::{AppContext, Context, PromptLevel, Task, Window};
use rc_git_kit::blocking::blocking;
use rc_git_kit::changes_view::{
    CommitStep, NO_MESSAGE_TEXT, NO_MESSAGE_TITLE, commit_step, stash_step,
};
use rc_git_kit::commit_identity::CommitIdentity;
use rc_git_kit::guard_actions::{
    ActionError, ActionResult, Author, CommitRequest, Identity, StashRequest,
};
use rc_git_kit::guard_bridge::{
    DecisionKind, FailureKind, GuardDecision, GuardFailure, READ_FAILURE_TITLE, STALE_TITLE,
    classify, commit_warning, failure_response, failure_title,
};
use rc_git_kit::guard_copy::{
    IDENTITY_ASK_MESSAGE, IDENTITY_ASK_TITLE, IDENTITY_EDIT_TITLE, deny_message,
    identity_edit_message,
};
use rc_git_kit::pull_requests::copy::MERGE;
use rc_git_kit::pull_requests::merge::MergeFlow;
use rc_git_kit::pull_requests::model::MergeEvent;
use rc_git_kit::repository_actions::{
    RepositoryActions, plan_commit, plan_discard, plan_stash_apply, plan_stash_drop,
    plan_stash_pop, plan_stash_push,
};

use crate::changes::ChangesRequest;
use crate::log;
use crate::sheets::challenge::ChallengeSheet;
use crate::sheets::identity::{IdentityAsk, IdentityOutcome, IdentitySheet};
use crate::sheets::{self, OpenSheet};
use crate::window::RepoWindow;

/// One guarded action: what it runs with the challenge's answer, which returns a warning to
/// show beside a success that landed with its housekeeping incomplete.
pub(super) type Perform =
    Arc<dyn Fn(&dyn RepositoryActions, Option<&str>) -> ActionResult<Option<String>> + Send + Sync>;

/// What runs after the action succeeded.
pub(super) type OnSuccess = Box<dyn FnOnce(&mut RepoWindow, &mut Window, &mut Context<RepoWindow>)>;

/// What runs in place of the default alert after the action failed.
pub(super) type OnFailure =
    Box<dyn FnOnce(&mut RepoWindow, ActionError, &mut Window, &mut Context<RepoWindow>)>;

/// What runs with an identity the user typed.
type WithIdentity =
    Box<dyn FnOnce(&mut RepoWindow, CommitIdentity, &mut Window, &mut Context<RepoWindow>)>;

/// A guarded action and what follows it.
pub(super) struct Guarded {
    pub(super) verb: String,
    pub(super) perform: Perform,
    pub(super) on_success: Option<OnSuccess>,
    pub(super) on_failure: Option<OnFailure>,
}

impl Guarded {
    /// `perform` under `verb`, refreshing the Changes lists once it lands.
    fn refreshing(verb: &str, perform: Perform) -> Self {
        Self {
            verb: verb.to_owned(),
            perform,
            on_success: Some(Box::new(|this, _, cx| this.refresh_changes(cx))),
            on_failure: None,
        }
    }
}

fn identity(identity: CommitIdentity) -> Identity {
    Identity {
        name: identity.name,
        email: identity.email,
    }
}

/// Shows a plain alert and logs it; the answer arrives through the returned task.
pub(super) fn alert(
    level: PromptLevel,
    title: &str,
    message: &str,
    window: &mut Window,
    cx: &mut gpui::App,
) -> Task<Option<usize>> {
    log::error(&format!("{title}: {message}"));
    let answer = window.prompt(level, title, Some(message), &["OK"], cx);
    cx.background_spawn(async move { answer.await.ok() })
}

impl RepoWindow {
    /// Obeys `decision`: run, challenge first, or show why not.
    pub(super) fn follow_decision(
        &mut self,
        decision: GuardDecision,
        guarded: Guarded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match decision.kind {
            DecisionKind::Allow => self.run_guarded(guarded, None, window, cx),
            DecisionKind::Confirm => self.present_challenge(decision, guarded, window, cx),
            DecisionKind::Deny => {
                alert(
                    PromptLevel::Critical,
                    &guarded.verb,
                    &deny_message(&decision),
                    window,
                    cx,
                )
                .detach();
            }
        }
    }

    fn present_challenge(
        &mut self,
        decision: GuardDecision,
        guarded: Guarded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let verb = guarded.verb.clone();
        let sheet = cx.new(|cx| {
            ChallengeSheet::new(
                verb.clone(),
                decision,
                Box::new(move |input, window, cx| {
                    this.update(cx, |this, cx| this.run_guarded(guarded, input, window, cx))
                        .ok();
                }),
                window,
                cx,
            )
        });
        self.sheet = Some(OpenSheet::Challenge(sheet.downgrade()));
        sheets::open(verb.into(), &sheet, window, cx);
    }

    /// A pull request merge's turn for the window: show its challenge, the guard's decision
    /// from the read it just made, or close it when the merge ended before an answer. The
    /// confirm hands the typed text to the model, which sends the merge once; a cancel or a
    /// dismissal ends the merge there.
    pub(super) fn merge_event(
        &mut self,
        event: MergeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            MergeEvent::Confirm(number) => self.present_merge_challenge(number, window, cx),
            MergeEvent::Withdrawn(number) => {
                let open = matches!(
                    &self.sheet,
                    Some(OpenSheet::Merge { number: shown, sheet })
                        if *shown == number && sheet.upgrade().is_some()
                );
                if open {
                    self.sheet = None;
                    sheets::close(window, cx);
                }
            }
        }
    }

    fn present_merge_challenge(
        &mut self,
        number: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let model = self.pull_requests.clone();
        let Some(decision) = model
            .read(cx)
            .merge_flow(number)
            .and_then(MergeFlow::plan)
            .map(|plan| GuardDecision::new(&plan.decision, false))
        else {
            return;
        };
        let (confirm, cancel) = (model.clone(), model);
        let sheet = cx.new(|cx| {
            ChallengeSheet::new(
                MERGE.to_owned(),
                decision,
                Box::new(move |typed, _, cx| {
                    confirm.update(cx, |model, cx| {
                        model.confirm_merge(number, typed.unwrap_or_default(), cx);
                    });
                }),
                window,
                cx,
            )
            .on_cancel(Box::new(move |cx| {
                cancel.update(cx, |model, cx| model.cancel_merge(number, cx));
            }))
        });
        self.sheet = Some(OpenSheet::Merge {
            number,
            sheet: sheet.downgrade(),
        });
        sheets::open(MERGE.into(), &sheet, window, cx);
    }

    /// Runs one guarded action off the UI thread with the challenge's `input`.
    pub(super) fn run_guarded(
        &mut self,
        guarded: Guarded,
        input: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(actions) = self.actions.clone() else {
            return;
        };
        let perform = Arc::clone(&guarded.perform);
        let work = blocking(cx.background_executor(), move || {
            perform(actions.as_ref(), input.as_deref())
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = work.await;
            this.update_in(cx, |this, window, cx| {
                this.guarded_landed(guarded, outcome, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn guarded_landed(
        &mut self,
        guarded: Guarded,
        outcome: ActionResult<Option<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok(warning) => {
                if let Some(on_success) = guarded.on_success {
                    on_success(self, window, cx);
                }
                if let Some(warning) = warning {
                    let title = failure_title(&guarded.verb);
                    alert(PromptLevel::Warning, &title, &warning, window, cx).detach();
                }
            }
            Err(error) => match guarded.on_failure {
                Some(on_failure) => on_failure(self, error, window, cx),
                None => self.present_guard_failure(&error, &guarded.verb, window, cx),
            },
        }
    }

    /// Shows a failed guarded action as kit decides: a stale repository reloads the lists
    /// first and gets a soft notice; anything else an alert titled by `verb`.
    pub(super) fn present_guard_failure(
        &mut self,
        error: &ActionError,
        verb: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let response = match &self.changes {
            Some(parts) => parts.model.update(cx, |model, cx| model.recover(error, cx)),
            None => Task::ready(failure_response(error)),
        };
        let verb = verb.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let response = response.await;
            this.update_in(cx, |_, window, cx| {
                let (level, title, message) = match response {
                    GuardFailure::Stale { notice } => {
                        (PromptLevel::Info, STALE_TITLE.to_owned(), notice)
                    }
                    GuardFailure::Failed { message } => {
                        (PromptLevel::Warning, failure_title(&verb), message)
                    }
                };
                alert(level, &title, &message, window, cx).detach();
            })
            .ok();
        })
        .detach();
    }

    /// Reloads the change and stash lists after an action changed them.
    pub(super) fn refresh_changes(&mut self, cx: &mut Context<Self>) {
        if let Some(parts) = &self.changes {
            parts
                .model
                .update(cx, rc_git_kit::changes::ChangesModel::refresh)
                .detach();
        }
    }

    fn refresh_author(&mut self, cx: &mut Context<Self>) {
        if let Some(author) = self.changes.as_ref().and_then(|parts| parts.author.clone()) {
            author
                .update(cx, rc_git_kit::changes::CommitAuthorModel::refresh)
                .detach();
        }
    }

    /// Runs a request from the Changes views.
    pub(super) fn changes_request(
        &mut self,
        request: ChangesRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match request {
            ChangesRequest::Commit(message) => self.perform_commit(message, None, window, cx),
            ChangesRequest::StashPush(message) => {
                self.perform_stash_push(message, None, window, cx);
            }
            ChangesRequest::StashApply(entry) => {
                let perform: Perform = Arc::new(move |actions, input| {
                    actions.stash_apply(&entry, input).map(|()| None)
                });
                let guarded = Guarded::refreshing("Apply Stash", perform);
                self.follow_decision(plan_stash_apply(), guarded, window, cx);
            }
            ChangesRequest::StashPop(entry) => {
                let decision = plan_stash_pop(&entry);
                let perform: Perform =
                    Arc::new(move |actions, input| actions.stash_pop(&entry, input).map(|_| None));
                self.follow_decision(
                    decision,
                    Guarded::refreshing("Pop Stash", perform),
                    window,
                    cx,
                );
            }
            ChangesRequest::StashDrop(entry) => {
                let decision = plan_stash_drop(&entry);
                let perform: Perform =
                    Arc::new(move |actions, input| actions.stash_drop(&entry, input).map(|_| None));
                let guarded = Guarded::refreshing("Drop Stash", perform);
                self.follow_decision(decision, guarded, window, cx);
            }
            ChangesRequest::Discard(paths) => self.discard(paths, window, cx),
            ChangesRequest::DiscardHunk { path, stamp } => {
                self.discard_hunk(path, stamp, window, cx);
            }
            ChangesRequest::Fetch | ChangesRequest::Pull | ChangesRequest::Push => {
                self.sync_request(&request, cx);
            }
        }
    }

    fn refuse_discard(
        &self,
        paths: &[String],
        verb: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let refusal = self
            .changes
            .as_ref()
            .and_then(|parts| parts.model.read(cx).discard_refusal(paths));
        let Some(refusal) = refusal else {
            return false;
        };
        alert(
            PromptLevel::Warning,
            &failure_title(verb),
            &refusal,
            window,
            cx,
        )
        .detach();
        true
    }

    fn discard(&mut self, paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_discard(&paths, "Discard Changes", window, cx) {
            return;
        }
        let decision = plan_discard(paths.len());
        let perform: Perform =
            Arc::new(move |actions, input| actions.discard_files(&paths, input).map(|_| None));
        self.follow_decision(
            decision,
            Guarded::refreshing("Discard Changes", perform),
            window,
            cx,
        );
    }

    fn discard_hunk(
        &mut self,
        path: String,
        stamp: rc_git_kit::guard_bridge::HunkStamp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.refuse_discard(std::slice::from_ref(&path), "Discard Hunk", window, cx) {
            return;
        }
        let perform: Perform = Arc::new(move |actions, input| {
            actions.discard_hunks(&path, &[stamp], input).map(|()| None)
        });
        self.follow_decision(
            plan_discard(1),
            Guarded::refreshing("Discard Hunk", perform),
            window,
            cx,
        );
    }

    /// The identity the guard falls back on: the one just typed, else the saved one.
    fn fallback(&self, prompted: Option<&CommitIdentity>, cx: &gpui::App) -> Option<Identity> {
        let saved = self
            .changes
            .as_ref()
            .and_then(|parts| parts.author.as_ref())
            .and_then(|author| author.read(cx).saved());
        prompted.cloned().or(saved).map(identity)
    }

    fn author_step(&self, cx: &gpui::App) -> Option<rc_git_kit::commit_identity::CommitAuthor> {
        let author = self.changes.as_ref()?.author.as_ref()?;
        author.read(cx).author().cloned()
    }

    /// Commits the ticked selection. With nobody to record, it asks first, and saves the
    /// answer for this repository only once the commit it was asked for lands.
    fn perform_commit(
        &mut self,
        message: String,
        prompted: Option<CommitIdentity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(parts) = &self.changes else {
            return;
        };
        let model = parts.model.read(cx);
        let selections = model.selections();
        let expected_head = model.expected_head();
        let author = self.author_step(cx);
        match commit_step(
            !selections.is_empty(),
            &message,
            author.as_ref(),
            prompted.is_some(),
        ) {
            CommitStep::Nothing => return,
            CommitStep::NeedsMessage => {
                alert(
                    PromptLevel::Info,
                    NO_MESSAGE_TITLE,
                    NO_MESSAGE_TEXT,
                    window,
                    cx,
                )
                .detach();
                return;
            }
            CommitStep::AskIdentity => {
                self.ask_identity(
                    Box::new(move |this, typed, window, cx| {
                        this.perform_commit(message, Some(typed), window, cx);
                    }),
                    window,
                    cx,
                );
                return;
            }
            CommitStep::Run => {}
        }
        let decision = plan_commit(selections.len());
        let fallback = self.fallback(prompted.as_ref(), cx);
        let text = message.clone();
        let perform: Perform = Arc::new(move |actions, input| {
            let request = CommitRequest {
                message: &text,
                author: Author {
                    explicit: None,
                    fallback: fallback.clone(),
                },
                expected_head,
                selections: &selections,
            };
            actions
                .commit(request, input)
                .map(|committed| commit_warning(&committed))
        });
        let retry = prompted.is_none();
        let remember = prompted;
        let guarded = Guarded {
            verb: "Commit".to_owned(),
            perform,
            on_success: Some(Box::new(move |this, window, cx| {
                this.changes_landed(remember, window, cx);
            })),
            on_failure: Some(Box::new(move |this, error, window, cx| {
                this.refresh_author(cx);
                if retry && classify(&error) == FailureKind::IdentityRequired {
                    this.ask_identity(
                        Box::new(move |this, typed, window, cx| {
                            this.perform_commit(message, Some(typed), window, cx);
                        }),
                        window,
                        cx,
                    );
                } else {
                    this.present_guard_failure(&error, "Commit", window, cx);
                }
            })),
        };
        self.follow_decision(decision, guarded, window, cx);
    }

    /// Stashes the ticked files, asking for an identity first as a commit does.
    fn perform_stash_push(
        &mut self,
        message: Option<String>,
        prompted: Option<CommitIdentity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(parts) = &self.changes else {
            return;
        };
        let paths = parts.model.read(cx).ticked_paths();
        let author = self.author_step(cx);
        match stash_step(!paths.is_empty(), author.as_ref(), prompted.is_some()) {
            CommitStep::Nothing | CommitStep::NeedsMessage => return,
            CommitStep::AskIdentity => {
                self.ask_identity(
                    Box::new(move |this, typed, window, cx| {
                        this.perform_stash_push(message, Some(typed), window, cx);
                    }),
                    window,
                    cx,
                );
                return;
            }
            CommitStep::Run => {}
        }
        let decision = plan_stash_push(paths.len());
        let fallback = self.fallback(prompted.as_ref(), cx);
        let text = message.clone();
        let perform: Perform = Arc::new(move |actions, input| {
            let request = StashRequest {
                paths: &paths,
                message: text.as_deref(),
                author: Author {
                    explicit: None,
                    fallback: fallback.clone(),
                },
            };
            actions.stash_push(request, input).map(|_| None)
        });
        let retry = prompted.is_none();
        let remember = prompted;
        let guarded = Guarded {
            verb: "Stash".to_owned(),
            perform,
            on_success: Some(Box::new(move |this, window, cx| {
                this.changes_landed(remember, window, cx);
            })),
            on_failure: Some(Box::new(move |this, error, window, cx| {
                this.refresh_author(cx);
                if retry && classify(&error) == FailureKind::IdentityRequired {
                    this.ask_identity(
                        Box::new(move |this, typed, window, cx| {
                            this.perform_stash_push(message, Some(typed), window, cx);
                        }),
                        window,
                        cx,
                    );
                } else {
                    this.present_guard_failure(&error, "Stash", window, cx);
                }
            })),
        };
        self.follow_decision(decision, guarded, window, cx);
    }

    /// A commit or stash landed: the box empties, the lists reload, and an identity typed
    /// for it is saved for this repository.
    fn changes_landed(
        &mut self,
        prompted: Option<CommitIdentity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(parts) = &self.changes else {
            return;
        };
        parts
            .list
            .update(cx, |list, cx| list.clear_message(window, cx));
        self.refresh_changes(cx);
        let Some(prompted) = prompted else {
            return;
        };
        let author = self.changes.as_ref().and_then(|parts| parts.author.clone());
        if let Some(author) = author {
            match author.update(cx, |author, cx| author.save(&prompted, cx)) {
                Ok(refresh) => refresh.detach(),
                Err(error) => log::error(&format!("save the commit identity: {error}")),
            }
        }
    }

    /// Asks for an identity before the first commit or stash in a repository that has none.
    fn ask_identity(&mut self, then: WithIdentity, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let sheet = cx.new(|cx| {
            IdentitySheet::new(
                IdentityAsk {
                    message: IDENTITY_ASK_MESSAGE.to_owned(),
                    initial: None,
                    allows_clear: false,
                },
                Box::new(move |outcome, window, cx| {
                    if let IdentityOutcome::Save(typed) = outcome {
                        this.update(cx, |this, cx| then(this, typed, window, cx))
                            .ok();
                    }
                }),
                window,
                cx,
            )
        });
        self.sheet = Some(OpenSheet::Identity(sheet.downgrade()));
        sheets::open(IDENTITY_ASK_TITLE.into(), &sheet, window, cx);
    }

    /// Repository > Commit Identity: shows, edits or clears the identity saved for this
    /// repository, a fallback the repository's own config always wins over.
    pub(super) fn edit_commit_identity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(author) = self.changes.as_ref().and_then(|parts| parts.author.clone()) else {
            return;
        };
        let (current, saved) = {
            let author = author.read(cx);
            (author.author().cloned(), author.saved())
        };
        let message = identity_edit_message(current.as_ref());
        let allows_clear = saved.is_some();
        let this = cx.entity().downgrade();
        let sheet = cx.new(|cx| {
            IdentitySheet::new(
                IdentityAsk {
                    message,
                    initial: saved,
                    allows_clear,
                },
                Box::new(move |outcome, window, cx| {
                    let saved = author.update(cx, |author, cx| match outcome {
                        IdentityOutcome::Save(identity) => author.save(&identity, cx),
                        IdentityOutcome::Clear => author.clear(cx),
                    });
                    match saved {
                        Ok(refresh) => refresh.detach(),
                        Err(error) => {
                            this.update(cx, |_, cx| {
                                alert(
                                    PromptLevel::Warning,
                                    READ_FAILURE_TITLE,
                                    &error.to_string(),
                                    window,
                                    cx,
                                )
                                .detach();
                            })
                            .ok();
                        }
                    }
                }),
                window,
                cx,
            )
        });
        self.sheet = Some(OpenSheet::Identity(sheet.downgrade()));
        sheets::open(IDENTITY_EDIT_TITLE.into(), &sheet, window, cx);
    }
}

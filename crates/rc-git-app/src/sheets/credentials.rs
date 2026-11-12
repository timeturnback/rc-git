//! The credentials prompt (`SyncPrompts.credentials`): a username and a masked token over
//! HTTPS, or a masked passphrase for a locked ssh key. It answers the sync model through the
//! sender it was handed: Provided once [`CredentialEntry::check`] accepts the fields,
//! Cancelled on Cancel or Escape, and nothing at all when it is dropped unanswered, which the
//! model reads as Unavailable.

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, Render, Subscription,
    Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use rc_git_kit::guard_copy::credential_copy;
use rc_git_kit::oneshot;
use rc_git_kit::platform::credentials::CredentialKind;
use rc_git_kit::sync::{CredentialEntry, CredentialReply, CredentialRequest};

use crate::sheets::{Dismissible, SheetButton, body, close};

/// The credentials sheet.
pub struct CredentialSheet {
    request: CredentialRequest,
    username: Entity<InputState>,
    secret: Entity<InputState>,
    problem: Option<&'static str>,
    sender: Option<oneshot::Sender<CredentialReply>>,
    _subscriptions: Vec<Subscription>,
}

impl CredentialSheet {
    /// A sheet asking for `request`, answering through `sender`.
    pub fn new(
        request: CredentialRequest,
        sender: oneshot::Sender<CredentialReply>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let copy = credential_copy(request.kind, &request.host, request.key.as_deref(), None);
        let username = cx.new(|cx| InputState::new(window, cx).placeholder("Username"));
        let secret = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(copy.secret_placeholder)
        });
        let subscriptions =
            [&username, &secret]
                .into_iter()
                .map(|field| {
                    cx.subscribe_in(field, window, |this, _, event: &InputEvent, window, cx| {
                        match event {
                            InputEvent::Change => cx.notify(),
                            InputEvent::PressEnter { .. } => this.submit(window, cx),
                            InputEvent::Focus | InputEvent::Blur => {}
                        }
                    })
                })
                .collect();
        Self {
            request,
            username,
            secret,
            problem: None,
            sender: Some(sender),
            _subscriptions: subscriptions,
        }
    }

    fn entry(&self, cx: &App) -> CredentialEntry {
        CredentialEntry::check(
            self.request.kind,
            &self.username.read(cx).value(),
            &self.secret.read(cx).value(),
        )
    }

    /// Fills the fields, as typing would.
    #[cfg(test)]
    pub fn fill(
        &mut self,
        username: &str,
        secret: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.username
            .update(cx, |field, cx| field.set_value(username, window, cx));
        self.secret
            .update(cx, |field, cx| field.set_value(secret, window, cx));
        cx.notify();
    }

    /// What the last confirm left out, shown above the fields.
    #[must_use]
    #[cfg(test)]
    pub fn problem(&self) -> Option<&'static str> {
        self.problem
    }

    /// Confirms: hands the credentials on and closes, or names what is missing and stays.
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.entry(cx) {
            CredentialEntry::Valid(credentials) => {
                if let Some(sender) = self.sender.take() {
                    sender.send(CredentialReply::Provided(credentials));
                }
                close(window, cx);
            }
            CredentialEntry::Invalid(missing) => {
                self.problem = Some(missing);
                cx.notify();
            }
        }
    }

    /// Declines: the step fails as cancelled.
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.answer_cancelled();
        close(window, cx);
    }

    fn answer_cancelled(&mut self) {
        if let Some(sender) = self.sender.take() {
            sender.send(CredentialReply::Cancelled);
        }
    }
}

impl Dismissible for CredentialSheet {
    fn dismissed(&mut self, _cx: &mut Context<Self>) {
        self.answer_cancelled();
    }

    fn first_field(&self, cx: &App) -> Option<FocusHandle> {
        let asks_username = self.request.kind == CredentialKind::HttpsToken;
        let field = if asks_username && self.username.read(cx).value().is_empty() {
            &self.username
        } else {
            &self.secret
        };
        Some(field.focus_handle(cx))
    }
}

impl Render for CredentialSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let copy = credential_copy(
            self.request.kind,
            &self.request.host,
            self.request.key.as_deref(),
            self.problem,
        );
        let mut fields = Vec::new();
        if copy.asks_username {
            fields.push(Input::new(&self.username).into_any_element());
        }
        fields.push(Input::new(&self.secret).mask_toggle().into_any_element());
        let valid = matches!(self.entry(cx), CredentialEntry::Valid(_));
        let weak = cx.entity().downgrade();
        let cancel = weak.clone();
        body(
            Some(copy.message),
            fields,
            None,
            vec![
                SheetButton {
                    id: "credentials-cancel",
                    label: "Cancel".into(),
                    primary: false,
                    danger: false,
                    enabled: true,
                    on_click: Box::new(move |window, cx| {
                        cancel.update(cx, |sheet, cx| sheet.cancel(window, cx)).ok();
                    }),
                },
                SheetButton {
                    id: "credentials-confirm",
                    label: copy.confirm.into(),
                    primary: true,
                    danger: false,
                    enabled: valid,
                    on_click: Box::new(move |window, cx| {
                        weak.update(cx, |sheet, cx| sheet.submit(window, cx)).ok();
                    }),
                },
            ],
            cx,
        )
    }
}

/// The title the sheet opens under.
#[must_use]
pub fn title(request: &CredentialRequest) -> String {
    credential_copy(request.kind, &request.host, request.key.as_deref(), None).title
}

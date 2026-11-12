//! The unknown host key prompt (`SyncPrompts.hostKey`): the key to verify, Trust and Continue
//! or Cancel. Like the credentials sheet it answers once through its sender, and a sheet
//! dropped unanswered reads as nobody to ask.

use gpui::{App, Context, FocusHandle, IntoElement, Render, Window};
use rc_git_kit::guard_copy::host_key_copy;
use rc_git_kit::oneshot;
use rc_git_kit::sync::{HostKeyReply, HostKeyRequest};

use crate::sheets::{Dismissible, SheetButton, body, close};

/// The host-key sheet.
pub struct HostKeySheet {
    request: HostKeyRequest,
    sender: Option<oneshot::Sender<HostKeyReply>>,
}

impl HostKeySheet {
    /// A sheet asking to trust `request`'s key, answering through `sender`.
    #[must_use]
    pub fn new(request: HostKeyRequest, sender: oneshot::Sender<HostKeyReply>) -> Self {
        Self {
            request,
            sender: Some(sender),
        }
    }

    /// The title the sheet opens under.
    #[must_use]
    pub fn title(&self) -> String {
        host_key_copy(&self.request.host, &self.request.host_key).0
    }

    fn answer(&mut self, reply: HostKeyReply, window: &mut Window, cx: &mut App) {
        if let Some(sender) = self.sender.take() {
            sender.send(reply);
        }
        close(window, cx);
    }

    /// Trusts the key and continues.
    pub fn trust(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.answer(HostKeyReply::Trusted, window, cx);
    }

    /// Refuses the key.
    pub fn refuse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.answer(HostKeyReply::Refused, window, cx);
    }
}

impl Dismissible for HostKeySheet {
    fn dismissed(&mut self, _cx: &mut Context<Self>) {
        if let Some(sender) = self.sender.take() {
            sender.send(HostKeyReply::Refused);
        }
    }

    fn first_field(&self, _cx: &App) -> Option<FocusHandle> {
        None
    }
}

impl Render for HostKeySheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (_, message) = host_key_copy(&self.request.host, &self.request.host_key);
        let weak = cx.entity().downgrade();
        let refuse = weak.clone();
        body(
            Some(message),
            Vec::new(),
            None,
            vec![
                SheetButton {
                    id: "host-key-cancel",
                    label: "Cancel".into(),
                    primary: false,
                    danger: false,
                    enabled: true,
                    on_click: Box::new(move |window, cx| {
                        refuse.update(cx, |sheet, cx| sheet.refuse(window, cx)).ok();
                    }),
                },
                SheetButton {
                    id: "host-key-trust",
                    label: "Trust and Continue".into(),
                    primary: true,
                    danger: false,
                    enabled: true,
                    on_click: Box::new(move |window, cx| {
                        weak.update(cx, |sheet, cx| sheet.trust(window, cx)).ok();
                    }),
                },
            ],
            cx,
        )
    }
}

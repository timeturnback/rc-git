//! The commit identity sheet (`IdentitySheet.swift`): asks for, edits, or clears the identity
//! saved for one repository. Save takes a click only while [`identity_entry`] accepts the
//! fields, and the reason shows under them, so an identity git cannot record is never handed
//! back.

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, Render, Subscription,
    Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use rc_git_kit::commit_identity::CommitIdentity;
use rc_git_kit::guard_copy::identity_entry;

use crate::sheets::{Dismissible, SheetButton, body, close};

/// What the sheet hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityOutcome {
    /// Save this identity.
    Save(CommitIdentity),
    /// Forget the saved one.
    Clear,
}

/// What the sheet runs with its outcome; Cancel runs nothing.
pub type OnDone = Box<dyn FnOnce(IdentityOutcome, &mut Window, &mut App)>;

/// What the sheet asks: its text, the identity it starts from, and whether Clear shows.
pub struct IdentityAsk {
    /// The text above the fields.
    pub message: String,
    /// The identity the fields start from.
    pub initial: Option<CommitIdentity>,
    /// Whether Clear is offered, which forgets the saved identity.
    pub allows_clear: bool,
}

/// The identity sheet.
pub struct IdentitySheet {
    message: String,
    name: Entity<InputState>,
    email: Entity<InputState>,
    allows_clear: bool,
    on_done: Option<OnDone>,
    _subscriptions: Vec<Subscription>,
}

impl IdentitySheet {
    /// A sheet asking `ask`, running `on_done` with the answer.
    pub fn new(
        ask: IdentityAsk,
        on_done: OnDone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let IdentityAsk {
            message,
            initial,
            allows_clear,
        } = ask;
        let name = initial
            .as_ref()
            .map(|identity| identity.name.clone())
            .unwrap_or_default();
        let email = initial.map(|identity| identity.email).unwrap_or_default();
        let name = cx.new(|cx| {
            let mut field = InputState::new(window, cx).placeholder("Name");
            field.set_value(name, window, cx);
            field
        });
        let email = cx.new(|cx| {
            let mut field = InputState::new(window, cx).placeholder("email@example.com");
            field.set_value(email, window, cx);
            field
        });
        let subscriptions =
            [&name, &email]
                .into_iter()
                .map(|field| {
                    cx.subscribe_in(field, window, |this, _, event: &InputEvent, window, cx| {
                        match event {
                            InputEvent::Change => cx.notify(),
                            InputEvent::PressEnter { .. } => this.save(window, cx),
                            InputEvent::Focus | InputEvent::Blur => {}
                        }
                    })
                })
                .collect();
        Self {
            message,
            name,
            email,
            allows_clear,
            on_done: Some(on_done),
            _subscriptions: subscriptions,
        }
    }

    /// Fills the fields, as typing would.
    #[cfg(test)]
    pub fn fill(&mut self, name: &str, email: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name
            .update(cx, |field, cx| field.set_value(name, window, cx));
        self.email
            .update(cx, |field, cx| field.set_value(email, window, cx));
        cx.notify();
    }

    fn entered(&self, cx: &App) -> rc_git_kit::guard_copy::IdentityEntry {
        identity_entry(&self.name.read(cx).value(), &self.email.read(cx).value())
    }

    fn finish(&mut self, outcome: IdentityOutcome, window: &mut Window, cx: &mut Context<Self>) {
        let Some(on_done) = self.on_done.take() else {
            return;
        };
        close(window, cx);
        on_done(outcome, window, cx);
    }

    /// Saves the identity in the fields; nothing while they hold none.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(identity) = self.entered(cx).identity {
            self.finish(IdentityOutcome::Save(identity), window, cx);
        }
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish(IdentityOutcome::Clear, window, cx);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_done = None;
        close(window, cx);
    }
}

impl Dismissible for IdentitySheet {
    fn dismissed(&mut self, _cx: &mut Context<Self>) {
        self.on_done = None;
    }

    fn first_field(&self, cx: &App) -> Option<FocusHandle> {
        Some(self.name.focus_handle(cx))
    }
}

impl Render for IdentitySheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entered = self.entered(cx);
        let weak = cx.entity().downgrade();
        let (cancel, clear) = (weak.clone(), weak.clone());
        let mut buttons = vec![SheetButton {
            id: "identity-cancel",
            label: "Cancel".into(),
            primary: false,
            danger: false,
            enabled: true,
            on_click: Box::new(move |window, cx| {
                cancel.update(cx, |sheet, cx| sheet.cancel(window, cx)).ok();
            }),
        }];
        if self.allows_clear {
            buttons.push(SheetButton {
                id: "identity-clear",
                label: "Clear".into(),
                primary: false,
                danger: false,
                enabled: true,
                on_click: Box::new(move |window, cx| {
                    clear.update(cx, |sheet, cx| sheet.clear(window, cx)).ok();
                }),
            });
        }
        buttons.push(SheetButton {
            id: "identity-save",
            label: "Save".into(),
            primary: true,
            danger: false,
            enabled: entered.identity.is_some(),
            on_click: Box::new(move |window, cx| {
                weak.update(cx, |sheet, cx| sheet.save(window, cx)).ok();
            }),
        });
        body(
            Some(self.message.clone()),
            vec![
                Input::new(&self.name).into_any_element(),
                Input::new(&self.email).into_any_element(),
            ],
            entered.problem,
            buttons,
            cx,
        )
    }
}

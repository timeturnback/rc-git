//! The guard's challenge (`MainWindowController.presentChallenge`): the policy's summary, the
//! backup ref, and the phrase to type when the policy names one. The confirm button takes a
//! click only once [`phrase_matches`] says the field holds the phrase, so a challenge that
//! needs it blocks until it matches; the guard verifies the phrase again when the action runs.

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, Render, SharedString,
    Subscription, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use rc_git_kit::guard_bridge::GuardDecision;
use rc_git_kit::guard_copy::{challenge_message, is_destructive, phrase_matches};

use crate::sheets::{Dismissible, SheetButton, body, close};

/// What a confirmed challenge runs: the typed phrase when the policy asked for one.
pub type OnConfirm = Box<dyn FnOnce(Option<String>, &mut Window, &mut App)>;

/// What a challenge cancelled or dismissed without an answer runs.
pub type OnCancel = Box<dyn FnOnce(&mut App)>;

/// The challenge sheet.
pub struct ChallengeSheet {
    verb: String,
    decision: GuardDecision,
    phrase: Option<Entity<InputState>>,
    on_confirm: Option<OnConfirm>,
    on_cancel: Option<OnCancel>,
    _subscriptions: Vec<Subscription>,
}

impl ChallengeSheet {
    /// A challenge for `verb` following `decision`; `on_confirm` runs once it is confirmed.
    pub fn new(
        verb: String,
        decision: GuardDecision,
        on_confirm: OnConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let phrase = decision
            .typed_phrase
            .clone()
            .map(|phrase| cx.new(|cx| InputState::new(window, cx).placeholder(phrase)));
        let subscriptions =
            phrase
                .iter()
                .map(|field| {
                    cx.subscribe_in(field, window, |this, _, event: &InputEvent, window, cx| {
                        match event {
                            InputEvent::Change => cx.notify(),
                            InputEvent::PressEnter { .. } => this.confirm(window, cx),
                            InputEvent::Focus | InputEvent::Blur => {}
                        }
                    })
                })
                .collect();
        Self {
            verb,
            decision,
            phrase,
            on_confirm: Some(on_confirm),
            on_cancel: None,
            _subscriptions: subscriptions,
        }
    }

    /// The same sheet, running `on_cancel` when it closes without a confirm: its Cancel button,
    /// Escape, or the window closing it.
    #[must_use]
    pub fn on_cancel(mut self, on_cancel: OnCancel) -> Self {
        self.on_cancel = Some(on_cancel);
        self
    }

    /// Runs the cancel hook once, and drops the confirm.
    fn withdrawn(&mut self, cx: &mut App) {
        self.on_confirm = None;
        if let Some(on_cancel) = self.on_cancel.take() {
            on_cancel(cx);
        }
    }

    fn typed(&self, cx: &App) -> Option<String> {
        self.phrase
            .as_ref()
            .map(|field| field.read(cx).value().to_string())
    }

    /// Types `text` into the phrase field, as the keyboard would.
    #[cfg(test)]
    pub fn type_phrase(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(field) = &self.phrase {
            field.update(cx, |field, cx| field.set_value(text, window, cx));
        }
        cx.notify();
    }

    /// Whether the confirm button takes a click now.
    #[must_use]
    pub fn can_confirm(&self, cx: &App) -> bool {
        phrase_matches(&self.decision, &self.typed(cx).unwrap_or_default())
    }

    /// Confirms: runs the action with the typed phrase and closes. Does nothing while the
    /// phrase does not match.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_confirm(cx) {
            return;
        }
        let Some(on_confirm) = self.on_confirm.take() else {
            return;
        };
        self.on_cancel = None;
        let typed = self.typed(cx);
        close(window, cx);
        on_confirm(typed, window, cx);
    }

    /// Cancels: nothing runs but the cancel hook.
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.withdrawn(cx);
        close(window, cx);
    }
}

impl Dismissible for ChallengeSheet {
    fn dismissed(&mut self, cx: &mut Context<Self>) {
        self.withdrawn(cx);
    }

    fn first_field(&self, cx: &App) -> Option<FocusHandle> {
        self.phrase.as_ref().map(|field| field.focus_handle(cx))
    }
}

impl Render for ChallengeSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let cancel = weak.clone();
        let fields = self
            .phrase
            .iter()
            .map(|field| Input::new(field).into_any_element())
            .collect();
        body(
            Some(challenge_message(&self.decision)),
            fields,
            None,
            vec![
                SheetButton {
                    id: "challenge-cancel",
                    label: "Cancel".into(),
                    primary: false,
                    danger: false,
                    enabled: true,
                    on_click: Box::new(move |window, cx| {
                        cancel.update(cx, |sheet, cx| sheet.cancel(window, cx)).ok();
                    }),
                },
                SheetButton {
                    id: "challenge-confirm",
                    label: SharedString::from(self.verb.clone()),
                    primary: true,
                    danger: is_destructive(&self.decision),
                    enabled: self.can_confirm(cx),
                    on_click: Box::new(move |window, cx| {
                        weak.update(cx, |sheet, cx| sheet.confirm(window, cx)).ok();
                    }),
                },
            ],
            cx,
        )
    }
}

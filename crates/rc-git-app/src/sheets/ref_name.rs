//! The one-field name sheet for New Branch and New Tag (`MainWindowController.promptForName`).
//! Create takes a click only while [`entered_ref_name`] finds a name; whether git accepts it
//! is the guard's call when the ref is created.

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, Render, Subscription,
    Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use rc_git_kit::guard_bridge::entered_ref_name;
use rc_git_kit::ref_menu::NO_NAME;

use crate::sheets::{Dismissible, SheetButton, body, close};

/// What the sheet runs with the entered name.
pub type OnCreate = Box<dyn FnOnce(String, &mut Window, &mut App)>;

/// The name sheet.
pub struct RefNameSheet {
    message: String,
    field: Entity<InputState>,
    problem: Option<&'static str>,
    on_create: Option<OnCreate>,
    _subscription: Subscription,
}

impl RefNameSheet {
    /// A sheet saying `message`, running `on_create` with the name.
    pub fn new(
        message: String,
        on_create: OnCreate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = cx.new(|cx| InputState::new(window, cx).placeholder("name"));
        let subscription = cx.subscribe_in(
            &field,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.create(window, cx),
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        Self {
            message,
            field,
            problem: None,
            on_create: Some(on_create),
            _subscription: subscription,
        }
    }

    /// Fills the field, as typing would.
    #[cfg(test)]
    pub fn fill(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.field
            .update(cx, |field, cx| field.set_value(name, window, cx));
        cx.notify();
    }

    fn name(&self, cx: &App) -> Option<String> {
        entered_ref_name(&self.field.read(cx).value()).map(str::to_owned)
    }

    /// Creates the ref with the entered name, or says none was entered.
    pub fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.name(cx) else {
            self.problem = Some(NO_NAME);
            cx.notify();
            return;
        };
        let Some(on_create) = self.on_create.take() else {
            return;
        };
        close(window, cx);
        on_create(name, window, cx);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_create = None;
        close(window, cx);
    }
}

impl Dismissible for RefNameSheet {
    fn dismissed(&mut self, _cx: &mut Context<Self>) {
        self.on_create = None;
    }

    fn first_field(&self, cx: &App) -> Option<FocusHandle> {
        Some(self.field.focus_handle(cx))
    }
}

impl Render for RefNameSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let cancel = weak.clone();
        body(
            Some(self.message.clone()),
            vec![Input::new(&self.field).into_any_element()],
            self.problem,
            vec![
                SheetButton {
                    id: "ref-name-cancel",
                    label: "Cancel".into(),
                    primary: false,
                    danger: false,
                    enabled: true,
                    on_click: Box::new(move |window, cx| {
                        cancel.update(cx, |sheet, cx| sheet.cancel(window, cx)).ok();
                    }),
                },
                SheetButton {
                    id: "ref-name-create",
                    label: "Create".into(),
                    primary: true,
                    danger: false,
                    enabled: self.name(cx).is_some(),
                    on_click: Box::new(move |window, cx| {
                        weak.update(cx, |sheet, cx| sheet.create(window, cx)).ok();
                    }),
                },
            ],
            cx,
        )
    }
}

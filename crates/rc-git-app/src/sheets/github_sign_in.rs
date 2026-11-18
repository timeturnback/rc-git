//! The GitHub sign-in sheet: pick the host and the method, then either enter the code a device
//! flow shows on the host's own `/login/device` page or paste a token. Kit's
//! [`SignInFlow`] decides every step and the words come from kit's GitHub copy; this sheet
//! only runs the steps off the UI thread and shows the phase. Cancel stays on screen until
//! the save, and Escape answers as Cancel does.

use std::sync::Arc;

use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, WeakEntity, Window, div,
};
use gpui_component::ActiveTheme;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Selectable, h_flex};
use rc_git_kit::blocking::blocking;
use rc_git_kit::github::accounts::{GitHubAccounts, SignInMethod};
use rc_git_kit::github::copy::{
    CLIENT_ID_PLACEHOLDER, HOST_PLACEHOLDER, TOKEN_PLACEHOLDER, confirm_label, method_label,
    method_note, open_page_label, phase_message,
};
use rc_git_kit::github::sign_in::{SignInFlow, SignInPhase, SignInStart, SignInStep};
use rc_git_kit::github::{DeviceFlow, save_sign_in, start_device_flow, validate_token};
use rc_git_kit::guard_bridge::CancelToken;
use rc_git_kit::platform::api_tokens::ApiTokenStore;

use crate::sheets::{Dismissible, SheetButton, body, close};
use crate::style;

/// What a finished sign-in runs: the accounts list reloads.
pub type OnSignedIn = Box<dyn Fn(&mut App)>;

/// The sign-in sheet.
pub struct GitHubSignInSheet {
    flow: SignInFlow,
    method: SignInMethod,
    host: Entity<InputState>,
    client_id: Entity<InputState>,
    token: Entity<InputState>,
    /// What the form started, for the steps that need its host or app.
    start: Option<SignInStart>,
    /// The running device flow, shared with the poll thread.
    device: Option<Arc<DeviceFlow>>,
    cancel: Arc<CancelToken>,
    tokens: Arc<dyn ApiTokenStore>,
    accounts: GitHubAccounts,
    on_signed_in: OnSignedIn,
    _subscriptions: Vec<Subscription>,
}

impl GitHubSignInSheet {
    /// A sheet saving into `tokens` and `accounts`, running `on_signed_in` once it has.
    pub fn new(
        tokens: Arc<dyn ApiTokenStore>,
        accounts: GitHubAccounts,
        on_signed_in: OnSignedIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(HOST_PLACEHOLDER)
                .default_value("github.com")
        });
        let client_id = cx.new(|cx| InputState::new(window, cx).placeholder(CLIENT_ID_PLACEHOLDER));
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(TOKEN_PLACEHOLDER)
        });
        let subscriptions = [&host, &client_id, &token]
            .into_iter()
            .map(|field| {
                cx.subscribe_in(
                    field,
                    window,
                    |this, _, event: &InputEvent, _, cx| match event {
                        InputEvent::Change => cx.notify(),
                        InputEvent::PressEnter { .. } => this.submit(cx),
                        InputEvent::Focus | InputEvent::Blur => {}
                    },
                )
            })
            .collect();
        Self {
            flow: SignInFlow::new(),
            method: SignInMethod::GitHubApp,
            host,
            client_id,
            token,
            start: None,
            device: None,
            cancel: Arc::new(CancelToken::new()),
            tokens,
            accounts,
            on_signed_in,
            _subscriptions: subscriptions,
        }
    }

    fn form(&self, cx: &App) -> Result<SignInStart, String> {
        SignInStart::from_form(
            &self.host.read(cx).value(),
            self.method,
            &self.client_id.read(cx).value(),
            &self.token.read(cx).value(),
        )
    }

    /// Picks the sign-in method, on the form only.
    pub fn choose(&mut self, method: SignInMethod, cx: &mut Context<Self>) {
        if matches!(self.flow.phase(), SignInPhase::Form { .. }) {
            self.method = method;
            cx.notify();
        }
    }

    /// Starts what the form says, or does nothing while it is incomplete.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let Ok(start) = self.form(cx) else {
            return;
        };
        self.cancel = Arc::new(CancelToken::new());
        self.start = Some(start.clone());
        let step = self.flow.submit(start);
        self.run(step, cx);
        cx.notify();
    }

    /// Stops whatever runs; the form shows again.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.flow.cancel() {
            self.cancel.signal();
            self.device = None;
        }
        cx.notify();
    }

    fn run(&mut self, step: SignInStep, cx: &mut Context<Self>) {
        let cancel = Arc::clone(&self.cancel);
        match step {
            SignInStep::Idle => {}
            SignInStep::RequestCode { generation } => {
                let Some(SignInStart::Device {
                    host,
                    method,
                    client,
                }) = self.start.clone()
                else {
                    return;
                };
                let work = blocking(cx.background_executor(), move || {
                    start_device_flow(&host, &client, method, cancel.remote())
                });
                cx.spawn(async move |this, cx| {
                    let started = work.await;
                    this.update(cx, |this, cx| this.code_landed(generation, started, cx))
                        .ok();
                })
                .detach();
            }
            SignInStep::Poll { generation } => {
                let Some(device) = self.device.clone() else {
                    return;
                };
                let work = blocking(cx.background_executor(), move || {
                    device.poll(cancel.remote())
                });
                cx.spawn(async move |this, cx| {
                    let answer = work.await;
                    this.update(cx, |this, cx| {
                        let step = this.flow.polled(generation, answer);
                        this.run(step, cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            SignInStep::Check { generation, token } => {
                let Some(host) = self.start.as_ref().map(|start| start.host().clone()) else {
                    return;
                };
                let work = blocking(cx.background_executor(), move || {
                    validate_token(&host, &token, cancel.remote())
                });
                cx.spawn(async move |this, cx| {
                    let answer = work.await;
                    this.update(cx, |this, cx| {
                        let step = this.flow.checked(generation, answer);
                        this.run(step, cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            SignInStep::Save {
                generation,
                account,
                tokens,
            } => self.save(generation, account, tokens, cx),
        }
    }

    fn code_landed(
        &mut self,
        generation: u64,
        started: Result<DeviceFlow, rc_git_kit::github::GitHubError>,
        cx: &mut Context<Self>,
    ) {
        let (device, shown) = match started {
            Ok(device) => {
                let shown = (device.user_code().to_owned(), device.verification_url());
                (Some(device), Ok(shown))
            }
            Err(error) => (None, Err(error)),
        };
        let step = self.flow.code_shown(generation, shown);
        // A flow that lands after a cancel is dropped here, freeing the process's one slot.
        if let (SignInStep::Poll { .. }, Some(device)) = (&step, device) {
            self.device = Some(Arc::new(device));
        }
        self.run(step, cx);
        cx.notify();
    }

    fn save(
        &mut self,
        generation: u64,
        account: rc_git_kit::github::accounts::GitHubAccount,
        tokens: rc_git_kit::github::TokenSet,
        cx: &mut Context<Self>,
    ) {
        let (store, accounts) = (Arc::clone(&self.tokens), self.accounts.clone());
        let saved_account = account.clone();
        let work = blocking(cx.background_executor(), move || {
            save_sign_in(store.as_ref(), &accounts, &account, &tokens)
        });
        cx.spawn(async move |this, cx| {
            let answer = work.await;
            this.update(cx, |this, cx| {
                this.flow.saved(generation, saved_account, answer);
                this.device = None;
                if matches!(this.flow.phase(), SignInPhase::SignedIn { .. }) {
                    (this.on_signed_in)(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn method_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().gap(style::ROW_GAP);
        for method in SignInMethod::ALL {
            let weak: WeakEntity<Self> = cx.entity().downgrade();
            row = row.child(
                Button::new(method.word())
                    .label(method_label(method))
                    .ghost()
                    .selected(self.method == method)
                    .on_click(move |_, _, cx| {
                        weak.update(cx, |sheet, cx| sheet.choose(method, cx)).ok();
                    }),
            );
        }
        row
    }

    fn fields(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let muted = cx.theme().muted_foreground;
        match self.flow.phase() {
            SignInPhase::Form { .. } => {
                let secret = if self.method.uses_device_flow() {
                    Input::new(&self.client_id).into_any_element()
                } else {
                    Input::new(&self.token).mask_toggle().into_any_element()
                };
                vec![
                    self.method_picker(cx).into_any_element(),
                    div()
                        .text_size(style::TEXT_CAPTION)
                        .text_color(muted)
                        .whitespace_normal()
                        .child(SharedString::from(method_note(self.method)))
                        .into_any_element(),
                    Input::new(&self.host).into_any_element(),
                    secret,
                ]
            }
            SignInPhase::WaitingForUser { user_code, .. } => vec![
                div()
                    .font_family(style::MONOSPACE_FONT)
                    .text_size(style::TEXT_PANEL_FIELD)
                    .child(SharedString::from(user_code.clone()))
                    .into_any_element(),
            ],
            SignInPhase::RequestingCode
            | SignInPhase::Checking
            | SignInPhase::Saving
            | SignInPhase::SignedIn { .. } => Vec::new(),
        }
    }

    fn buttons(&self, cx: &mut Context<Self>) -> Vec<SheetButton> {
        let weak = cx.entity().downgrade();
        let button = |id,
                      label: SharedString,
                      primary,
                      enabled,
                      run: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let weak = weak.clone();
            SheetButton {
                id,
                label,
                primary,
                danger: false,
                enabled,
                on_click: Box::new(move |window, cx| {
                    weak.update(cx, |sheet, cx| run(sheet, window, cx)).ok();
                }),
            }
        };
        match self.flow.phase() {
            SignInPhase::Form { .. } => vec![
                button(
                    "github-close",
                    "Cancel".into(),
                    false,
                    true,
                    |_, window, cx| close(window, cx),
                ),
                button(
                    "github-confirm",
                    confirm_label(self.method).into(),
                    true,
                    self.form(cx).is_ok(),
                    |sheet, _, cx| sheet.submit(cx),
                ),
            ],
            SignInPhase::WaitingForUser {
                user_code,
                verification_url,
                ..
            } => {
                let (code, url) = (user_code.clone(), verification_url.clone());
                vec![
                    button(
                        "github-cancel",
                        "Cancel".into(),
                        false,
                        true,
                        |sheet, _, cx| sheet.cancel(cx),
                    ),
                    SheetButton {
                        id: "github-copy-code",
                        label: "Copy Code".into(),
                        primary: false,
                        danger: false,
                        enabled: true,
                        on_click: Box::new(move |_, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                        }),
                    },
                    SheetButton {
                        id: "github-open-page",
                        label: open_page_label(&url).into(),
                        primary: true,
                        danger: false,
                        enabled: true,
                        on_click: Box::new(move |_, cx| cx.open_url(&url)),
                    },
                ]
            }
            SignInPhase::RequestingCode | SignInPhase::Checking | SignInPhase::Saving => {
                vec![button(
                    "github-cancel",
                    "Cancel".into(),
                    false,
                    self.flow.can_cancel(),
                    |sheet, _, cx| sheet.cancel(cx),
                )]
            }
            SignInPhase::SignedIn { .. } => vec![button(
                "github-done",
                "Done".into(),
                true,
                true,
                |_, window, cx| close(window, cx),
            )],
        }
    }
}

impl Drop for GitHubSignInSheet {
    /// A sheet closed with its window still stops its poll, so the device flow's one slot per
    /// process is free again within a fifth of a second.
    fn drop(&mut self) {
        self.cancel.signal();
    }
}

impl Dismissible for GitHubSignInSheet {
    fn dismissed(&mut self, cx: &mut Context<Self>) {
        self.cancel(cx);
    }

    fn first_field(&self, cx: &App) -> Option<FocusHandle> {
        Some(self.host.focus_handle(cx))
    }
}

impl Render for GitHubSignInSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let message = phase_message(self.flow.phase());
        let fields = self.fields(cx);
        let buttons = self.buttons(cx);
        body(
            (!message.is_empty()).then_some(message),
            fields,
            None,
            buttons,
            cx,
        )
    }
}

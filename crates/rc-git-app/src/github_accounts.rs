//! Settings' GitHub accounts pane: the accounts signed into, Add Account… (the sign-in sheet)
//! and Sign Out. Kit's [`GitHubAccounts`] owns the records and the token store the tokens;
//! this pane only lists them and forwards the clicks.

use std::sync::Arc;

use gpui::{
    AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, WeakEntity, Window, div, prelude::FluentBuilder,
};
use gpui_component::ActiveTheme;
use gpui_component::button::Button;
use gpui_component::{Disableable, h_flex, v_flex};
use rc_git_kit::blocking::blocking;
use rc_git_kit::github::accounts::{GitHubAccount, GitHubAccounts};
use rc_git_kit::github::copy::{
    ACCOUNTS_HEADING, ACCOUNTS_NOTE, NO_ACCOUNTS, SIGN_IN_TITLE, account_row,
};
use rc_git_kit::github::sign_out;
use rc_git_kit::platform::api_tokens::ApiTokenStore;

use crate::sheets::github_sign_in::GitHubSignInSheet;
use crate::{sheets, style};

/// The accounts pane.
pub struct GitHubAccountsPane {
    accounts: GitHubAccounts,
    tokens: Arc<dyn ApiTokenStore>,
    rows: Vec<GitHubAccount>,
    selected: Option<usize>,
    /// Why the last sign-out failed, shown under the list until the next one.
    problem: Option<String>,
}

impl GitHubAccountsPane {
    /// The pane over the app's account records and token store.
    #[must_use]
    pub fn new(accounts: GitHubAccounts, tokens: Arc<dyn ApiTokenStore>) -> Self {
        let rows = accounts.load();
        Self {
            accounts,
            tokens,
            rows,
            selected: None,
            problem: None,
        }
    }

    /// The accounts on screen, as `login@host`.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|account| format!("{}@{}", account.key.login(), account.key.host()))
            .collect()
    }

    /// Re-reads the accounts: the CLI or a sign-in may have changed them.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.rows = self.accounts.load();
        self.selected = None;
        cx.notify();
    }

    /// Selects row `index`.
    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = Some(index);
        cx.notify();
    }

    /// Add Account…: opens the sign-in sheet; a finished sign-in reloads the list.
    pub fn add_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane: WeakEntity<Self> = cx.entity().downgrade();
        let (tokens, accounts) = (Arc::clone(&self.tokens), self.accounts.clone());
        let sheet = cx.new(|cx| {
            GitHubSignInSheet::new(
                tokens,
                accounts,
                Box::new(move |cx| {
                    pane.update(cx, Self::reload).ok();
                }),
                window,
                cx,
            )
        });
        sheets::open(SIGN_IN_TITLE.into(), &sheet, window, cx);
    }

    /// Sign Out: forgets the selected account and deletes its token, off the UI thread (the
    /// keychain may ask). A failure is said under the list; the list shows what is stored
    /// either way.
    pub fn sign_out_selected(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self
            .selected
            .and_then(|index| self.rows.get(index))
            .cloned()
        else {
            return;
        };
        let (tokens, accounts) = (Arc::clone(&self.tokens), self.accounts.clone());
        let work = blocking(cx.background_executor(), move || {
            sign_out(tokens.as_ref(), &accounts, &account.key)
        });
        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                this.reload(cx);
                this.problem = outcome.err();
            })
            .ok();
        })
        .detach();
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let theme = cx.theme();
        let (border, active, muted) = (theme.border, theme.list_active, theme.muted_foreground);
        let mut list = div()
            .id("github-accounts")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .border(style::HAIRLINE)
            .border_color(border)
            .child(
                h_flex()
                    .px(style::ROW_GAP)
                    .h(style::LIST_ROW_HEIGHT)
                    .border_b(style::HAIRLINE)
                    .border_color(border)
                    .text_size(style::TEXT_CAPTION)
                    .text_color(muted)
                    .child(div().w(style::SETTINGS_HOST_WIDTH).child("Account"))
                    .child(div().w(style::SETTINGS_HOST_WIDTH).child("Host"))
                    .child(div().flex_1().child("Signed in with")),
            );
        for (index, account) in self.rows.iter().enumerate() {
            let (login, host, method) = account_row(account);
            let cell = |text: String| {
                div()
                    .w(style::SETTINGS_HOST_WIDTH)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(text))
            };
            list = list.child(
                h_flex()
                    .id(("github-account", index))
                    .px(style::ROW_GAP)
                    .h(style::LIST_ROW_HEIGHT)
                    .text_size(style::TEXT_BODY)
                    .when(self.selected == Some(index), |row| row.bg(active))
                    .child(cell(login))
                    .child(cell(host))
                    .child(div().flex_1().min_w_0().child(method))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(index, cx))),
            );
        }
        list
    }
}

impl Render for GitHubAccountsPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, red) = (cx.theme().muted_foreground, cx.theme().red);
        let empty = self.rows.is_empty();
        let nothing_selected = self.selected.is_none();
        let problem = self.problem.clone();
        let rows = self.render_rows(cx);
        v_flex()
            .flex_1()
            .min_h_0()
            .gap(style::BAR_PADDING)
            .child(
                div()
                    .text_size(style::TEXT_BODY)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(ACCOUNTS_HEADING),
            )
            .child(
                div()
                    .text_size(style::TEXT_CAPTION)
                    .text_color(muted)
                    .child(ACCOUNTS_NOTE),
            )
            .child(rows)
            .when_some(problem, |pane, problem| {
                pane.child(
                    div()
                        .text_size(style::TEXT_CAPTION)
                        .text_color(red)
                        .child(SharedString::from(problem)),
                )
            })
            .child(
                h_flex()
                    .gap(style::ROW_GAP)
                    .child(
                        div()
                            .flex_1()
                            .text_size(style::TEXT_CAPTION)
                            .text_color(muted)
                            .when(empty, |note| note.child(NO_ACCOUNTS)),
                    )
                    .child(
                        Button::new("github-sign-out")
                            .label("Sign Out")
                            .disabled(nothing_selected)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sign_out_selected(cx);
                            })),
                    )
                    .child(
                        Button::new("github-add-account")
                            .label("Add Account…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_account(window, cx);
                            })),
                    ),
            )
    }
}

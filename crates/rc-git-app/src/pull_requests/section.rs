//! The sidebar's Pull Requests section: hidden for a repository whose remote names no GitHub
//! repository; the sign-in or account choice when that is what stands in the way; else the
//! open pull requests, one row each, and Load More. Every word comes from kit's
//! [`rc_git_kit::pull_requests::copy`]; this view only places them.

use gpui::{
    Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    px,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme, Icon, h_flex, v_flex};
use rc_git_kit::github::GitHubHost;
use rc_git_kit::platform::api_tokens::AccountKey;
use rc_git_kit::pull_requests::PullRequestSummary;
use rc_git_kit::pull_requests::checks::row_state;
use rc_git_kit::pull_requests::copy::{
    LOAD_MORE, LOADING, NO_PULL_REQUESTS, SECTION_HEADING, STALE_NOTE, choose_account_row,
    freshness_problem, row_detail, sign_in_row, use_account_label,
};
use rc_git_kit::pull_requests::link::RepositoryLink;
use rc_git_kit::pull_requests::model::PullRequestsModel;
use rc_git_kit::pull_requests::state::Freshness;

use super::check_icon;
use crate::icons::{Glyph, icon};
use crate::style;

/// What the section asks of its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionEvent {
    /// Open pull request `number`'s tab.
    Open(u64),
    /// Open Settings, where accounts are signed in.
    OpenSettings,
}

/// The section.
pub struct PullRequestSection {
    model: Entity<PullRequestsModel>,
    expanded: bool,
    now: fn() -> u64,
}

impl EventEmitter<SectionEvent> for PullRequestSection {}

impl PullRequestSection {
    /// The section over `model`, expanded; `now` tells the time for rate-limit notes.
    pub fn new(model: Entity<PullRequestsModel>, now: fn() -> u64, cx: &mut Context<Self>) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        Self {
            model,
            expanded: true,
            now,
        }
    }

    /// The titles of the rows on show, for tests.
    #[cfg(test)]
    pub(crate) fn row_titles(&self, cx: &gpui::App) -> Vec<String> {
        let model = self.model.read(cx);
        model
            .list()
            .rows()
            .iter()
            .map(|row| row.title.clone())
            .collect()
    }

    /// The text the section shows above its rows, for tests: the problem or the empty note.
    #[cfg(test)]
    pub(crate) fn notes(&self, cx: &gpui::App) -> Vec<String> {
        self.status_lines(cx)
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        self.expanded = !self.expanded;
        let expanded = self.expanded;
        self.model
            .update(cx, |model, cx| model.set_section_visible(expanded, cx));
        cx.notify();
    }

    fn status_lines(&self, cx: &gpui::App) -> Vec<String> {
        let list = self.model.read(cx).list();
        let mut lines = Vec::new();
        if let Some(problem) = freshness_problem(list.freshness(), (self.now)()) {
            lines.push(problem);
            if !list.rows().is_empty() {
                lines.push(STALE_NOTE.to_owned());
            }
        }
        match list.freshness() {
            Freshness::Loading if list.rows().is_empty() => lines.push(LOADING.to_owned()),
            Freshness::Current { .. } if list.rows().is_empty() => {
                lines.push(NO_PULL_REQUESTS.to_owned());
            }
            Freshness::Loading | Freshness::Current { .. } | Freshness::Failed(_) => {}
        }
        lines
    }

    fn header(&self, count: Option<usize>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let glyph = if self.expanded {
            Glyph::Expanded
        } else {
            Glyph::Collapsed
        };
        let title = match count {
            Some(count) if count > 0 => format!("{SECTION_HEADING} ({count})"),
            Some(_) | None => SECTION_HEADING.to_owned(),
        };
        h_flex()
            .id("pull-requests-header")
            .gap(style::ROW_GAP)
            .px(style::MARGIN)
            .py(style::BAR_PADDING)
            .border_t(style::HAIRLINE)
            .border_color(theme.border)
            .cursor_pointer()
            .text_size(style::TEXT_CAPTION)
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.muted_foreground)
            .child(Icon::new(icon(glyph)).size(style::ICON_SIZE))
            .child(SharedString::from(title))
            .on_click(cx.listener(|this, _, _, cx| this.toggle(cx)))
    }

    fn caption(text: String, cx: &Context<Self>) -> impl IntoElement + use<> {
        div()
            .px(style::MARGIN)
            .pb(style::BAR_PADDING)
            .text_size(style::TEXT_CAPTION)
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from(text))
    }

    fn row(row: &PullRequestSummary, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let number = row.number;
        let state = row_state(row.checks);
        h_flex()
            .id(SharedString::from(format!("pull-request-{number}")))
            .w_full()
            .items_start()
            .gap(style::ROW_GAP)
            .px(style::MARGIN)
            .py(px(4.))
            .cursor_pointer()
            .hover(|style| style.bg(theme.list_hover))
            .child(
                div()
                    .w(style::ICON_SIZE)
                    .flex_none()
                    .when_some(state, |cell, state| cell.child(check_icon(state, theme))),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(style::TEXT_BODY)
                            .text_color(theme.foreground)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(row.title.clone())),
                    )
                    .child(
                        div()
                            .text_size(style::TEXT_CAPTION)
                            .text_color(theme.muted_foreground)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(row_detail(row))),
                    ),
            )
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(SectionEvent::Open(number))))
    }

    fn linked(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let rows: Vec<PullRequestSummary> = self.model.read(cx).list().rows().to_vec();
        let has_more = self.model.read(cx).list().has_more();
        let lines = self.status_lines(cx);
        let mut body = v_flex()
            .id("pull-request-rows")
            .max_h(style::PULL_REQUESTS_SECTION_MAX_HEIGHT)
            .overflow_y_scroll();
        for line in lines {
            body = body.child(Self::caption(line, cx));
        }
        for row in &rows {
            body = body.child(Self::row(row, cx));
        }
        if has_more {
            body = body.child(
                div().px(style::MARGIN).py(style::BAR_PADDING).child(
                    Button::new("pull-requests-more")
                        .label(LOAD_MORE)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.model.update(cx, PullRequestsModel::load_more);
                        })),
                ),
            );
        }
        v_flex()
            .child(self.header(Some(rows.len()), cx))
            .when(self.expanded, |column| column.child(body))
            .into_any_element()
    }

    fn choose(
        &self,
        host: &GitHubHost,
        keys: &[AccountKey],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut buttons = h_flex()
            .px(style::MARGIN)
            .pb(style::BAR_PADDING)
            .gap(style::ROW_GAP)
            .flex_wrap();
        for (index, key) in keys.iter().enumerate() {
            let chosen = key.clone();
            buttons = buttons.child(
                Button::new(("pull-requests-use-account", index))
                    .label(use_account_label(key))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let key = chosen.clone();
                        this.model
                            .update(cx, |model, cx| model.choose_account(key, cx))
                            .detach();
                    })),
            );
        }
        let problem = self.model.read(cx).choice_problem().map(str::to_owned);
        let text = choose_account_row(host);
        v_flex()
            .child(self.header(None, cx))
            .child(Self::caption(text, cx))
            .child(buttons)
            .when_some(problem, |column, problem| {
                column.child(Self::caption(problem, cx))
            })
            .into_any_element()
    }
}

impl Render for PullRequestSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let link = self.model.read(cx).link().cloned();
        match link {
            None | Some(RepositoryLink::Hidden) => div().into_any_element(),
            Some(RepositoryLink::SignIn { host }) => v_flex()
                .child(self.header(None, cx))
                .child(Self::caption(sign_in_row(&host), cx))
                .child(
                    div().px(style::MARGIN).pb(style::BAR_PADDING).child(
                        Button::new("pull-requests-sign-in")
                            .label("Open Settings…")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(SectionEvent::OpenSettings);
                            })),
                    ),
                )
                .into_any_element(),
            Some(RepositoryLink::ChooseAccount { host, accounts }) => {
                self.choose(&host, &accounts, cx)
            }
            Some(RepositoryLink::Linked(_)) => self.linked(cx),
        }
    }
}

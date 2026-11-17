//! The commit list half of the History pane: the search area, the scope and error bars, and
//! the commit rows with their graph cells, paged as the list nears its end.

use std::ops::Range;

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    uniform_list,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Selectable, Sizable};
use rc_git_core::CommitSummary;
use rc_git_kit::back_forward::HistoryScope;
use rc_git_kit::browser::BrowserModel;
use rc_git_kit::dates::format_list_date_time;
use rc_git_kit::history_text::{
    ListSource, history_error_label, list_placeholder, scope_label, search_progress, short_hash,
};
use rc_git_kit::layout_policy::HistoryColumns;
use rc_git_kit::ref_filter::{CommitSearchScope, listed_time};

use super::search::SearchControls;
use super::{HistoryEvent, HistoryPane, graph};
use crate::style;

impl HistoryPane {
    /// The search area, the scope and error bars, and the list or its placeholder.
    pub(super) fn render_commits(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let search = self.render_search(cx);
        let scope = self.render_scope_bar(cx);
        let error = self.render_error_bar(cx);
        let body = self.render_list_body(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(search)
            .children(scope)
            .children(error)
            .child(div().flex_1().min_h_0().child(body))
    }

    fn render_search(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let scope = self.search.scope;
        let model = self.model.read(cx);
        let progress = model.search_filter().map(|_| {
            search_progress(
                model.search_results().len(),
                model.search_scanned(),
                model.is_searching(),
                model.search_error(),
            )
        });
        let muted = theme.muted_foreground;
        div()
            .flex()
            .flex_col()
            .gap(style::ROW_GAP)
            .px(style::MARGIN)
            .py(style::BAR_PADDING)
            .border_b(style::HAIRLINE)
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(style::ROW_GAP)
                    .child(
                        div()
                            .flex_1()
                            .min_w(style::SEARCH_FIELD_MIN_WIDTH)
                            .child(Input::new(&self.search.query).small()),
                    )
                    .child(Self::scope_button("search-message", "Message", scope, cx))
                    .child(Self::scope_button("search-author", "Author", scope, cx))
                    .child(
                        Button::new("search-dates")
                            .small()
                            .ghost()
                            .label("Dates")
                            .selected(self.search.dates_open)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.search.toggle_dates(now(), window, cx);
                                this.emit_filter(cx);
                                cx.notify();
                            })),
                    )
                    .children(progress.map(|progress| {
                        div()
                            .flex_none()
                            .text_size(style::TEXT_CAPTION)
                            .text_color(muted)
                            .child(SharedString::from(progress))
                    })),
            )
            .when(self.search.dates_open, |area| {
                area.child(self.render_dates(cx))
            })
    }

    fn scope_button(
        id: &'static str,
        label: &'static str,
        scope: CommitSearchScope,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let mine = if label == "Author" {
            CommitSearchScope::Author
        } else {
            CommitSearchScope::Message
        };
        Button::new(id)
            .small()
            .ghost()
            .label(label)
            .selected(scope == mine)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.search.scope = mine;
                this.emit_filter(cx);
                cx.notify();
            }))
    }

    fn render_dates(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let since_valid = SearchControls::is_valid(&self.search.since, cx);
        let until_valid = SearchControls::is_valid(&self.search.until, cx);
        let danger = cx.theme().danger;
        let tip = "Filters on the commit date. While a date bound is set, the Date column \
                   shows the commit date too; otherwise it shows the author date.";
        div()
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .child(
                Checkbox::new("search-since")
                    .label("Committed since")
                    .tooltip(tip)
                    .checked(self.search.since_on)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.search.since_on = *checked;
                        this.emit_filter(cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .w(style::DAY_FIELD_WIDTH)
                    .rounded(style::RADIUS)
                    .when(!since_valid, |field| {
                        field.border(style::HAIRLINE).border_color(danger)
                    })
                    .child(Input::new(&self.search.since).small()),
            )
            .child(
                Checkbox::new("search-until")
                    .label("Committed until")
                    .tooltip(tip)
                    .checked(self.search.until_on)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.search.until_on = *checked;
                        this.emit_filter(cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .w(style::DAY_FIELD_WIDTH)
                    .rounded(style::RADIUS)
                    .when(!until_valid, |field| {
                        field.border(style::HAIRLINE).border_color(danger)
                    })
                    .child(Input::new(&self.search.until).small()),
            )
    }

    fn render_scope_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let HistoryScope::Path(path) = self.model.read(cx).history_scope().clone() else {
            return None;
        };
        let theme = cx.theme();
        Some(
            div()
                .flex()
                .items_center()
                .gap(style::ROW_GAP)
                .px(style::MARGIN)
                .py(style::ROW_GAP)
                .border_b(style::HAIRLINE)
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(style::TEXT_CAPTION)
                        .text_color(theme.foreground)
                        .child(SharedString::from(scope_label(&path))),
                )
                .child(
                    Button::new("clear-scope")
                        .small()
                        .ghost()
                        .label("\u{2715}")
                        .tooltip("Show the whole history")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.model.update(cx, BrowserModel::clear_history_scope);
                        })),
                ),
        )
    }

    /// The failed history page in place of the list's end. Hidden while a search shows its
    /// own results.
    fn render_error_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let model = self.model.read(cx);
        if model.search_filter().is_some() {
            return None;
        }
        let error = model.history_error()?.to_owned();
        let theme = cx.theme();
        Some(
            div()
                .flex()
                .items_center()
                .gap(style::ROW_GAP)
                .px(style::MARGIN)
                .py(style::ROW_GAP)
                .border_b(style::HAIRLINE)
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(style::TEXT_CAPTION)
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(history_error_label(&error))),
                )
                .child(
                    Button::new("retry-history")
                        .small()
                        .label("Retry")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.model.update(cx, BrowserModel::retry_history).detach();
                        })),
                ),
        )
    }

    fn placeholder(&self, cx: &gpui::App) -> Option<&'static str> {
        let model = self.model.read(cx);
        let rows = self.displayed(cx).len();
        if model.search_filter().is_some() {
            return list_placeholder(
                rows,
                model.search_error().is_some(),
                ListSource::Search {
                    walking: model.is_searching(),
                },
            );
        }
        list_placeholder(
            rows,
            model.history_error().is_some(),
            ListSource::History {
                complete: model.history_complete(),
                scoped: *model.history_scope() != HistoryScope::All,
            },
        )
    }

    fn render_list_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(text) = self.placeholder(cx) {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(style::TEXT_BODY)
                .text_color(crate::theme::tertiary(cx))
                .child(text)
                .into_any_element();
        }
        let count = self.displayed(cx).len();
        let header = self.render_column_header(cx);
        let list = div()
            .id("commit-list")
            .key_context("CommitList")
            .track_focus(&self.list_focus)
            .size_full()
            .on_key_down(cx.listener(Self::list_key))
            .child(
                uniform_list(
                    "commits",
                    count,
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        this.built_rows.set(this.built_rows.get() + range.len());
                        this.page_if_near_end(range.end, cx);
                        let columns = this.columns.current();
                        let mut rows = Vec::with_capacity(range.len());
                        for index in range {
                            rows.push(this.render_row(index, columns, cx));
                        }
                        rows
                    }),
                )
                .track_scroll(&self.list_scroll)
                .size_full(),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(list)
                    .child(Scrollbar::vertical(&self.list_scroll)),
            )
            .into_any_element()
    }

    /// Up and Down move the open commit through the list, as the Swift table's selection did.
    fn list_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let step: isize = match event.keystroke.key.as_str() {
            "up" => -1,
            "down" => 1,
            _ => return,
        };
        let displayed = self.displayed(cx);
        let current = self
            .model
            .read(cx)
            .diff_commit()
            .and_then(|open| displayed.iter().position(|commit| commit.oid == open.oid));
        let next = match current {
            Some(index) => index.checked_add_signed(step),
            None => Some(0),
        };
        let Some(commit) = next.and_then(|index| displayed.get(index)).cloned() else {
            return;
        };
        if let Some(index) = next {
            self.list_scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
        }
        cx.stop_propagation();
        self.select_commit(commit, cx);
    }

    fn row_click(&mut self, commit: &CommitSummary, event: &ClickEvent, cx: &mut Context<Self>) {
        if event.click_count() >= 2 {
            cx.emit(HistoryEvent::BrowseCommit(commit.clone()));
            return;
        }
        self.select_commit(commit.clone(), cx);
    }

    fn render_row(
        &self,
        index: usize,
        columns: HistoryColumns,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(commit) = self.displayed(cx).get(index).cloned() else {
            return div().into_any_element();
        };
        let model = self.model.read(cx);
        let selected = model.diff_commit().map(|open| open.oid) == Some(commit.oid);
        let graph_row = model.graph_rows().get(index).cloned();
        let graphed = !model.graph_rows().is_empty();
        let lanes = model.graph_max_lane_count();
        let time = listed_time(&commit, model.search_filter());
        let date = format_list_date_time(time, (self.offset_at)(time));
        let theme = cx.theme();
        let width = graph::width(lanes);
        let weak = cx.entity().downgrade();
        let menu_commit = commit.clone();
        let click_commit = commit.clone();
        div()
            .id(("commit", index))
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .w_full()
            .h(style::LIST_ROW_HEIGHT)
            .pr(style::MARGIN)
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(style::TEXT_CAPTION)
            .text_color(theme.muted_foreground)
            .when(selected, |row| row.bg(theme.list_active))
            .when(!selected, |row| {
                row.hover(|style| style.bg(theme.list_hover))
            })
            .when(graphed, |row| match graph_row {
                Some(graph_row) => row.child(graph::cell(graph_row, width)),
                None => row.child(graph::empty(width)),
            })
            .when(!graphed, |row| row.pl(style::MARGIN))
            .child(
                div()
                    .w(style::HASH_COLUMN_WIDTH)
                    .flex_none()
                    .font_family(style::MONOSPACE_FONT)
                    .child(SharedString::from(short_hash(&commit.oid))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(style::TEXT_BODY)
                    .text_color(theme.foreground)
                    .child(SharedString::from(commit.title.clone())),
            )
            .child(
                div()
                    .w(style::points(columns.author))
                    .flex_none()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(commit.author_name.clone())),
            )
            .child(
                div()
                    .w(style::points(columns.date))
                    .flex_none()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(date)),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                window.focus(&this.list_focus, cx);
                this.row_click(&click_commit, event, cx);
            }))
            .context_menu(move |menu, _, _| {
                let browse = (weak.clone(), menu_commit.clone());
                let copy = menu_commit.clone();
                menu.item(PopupMenuItem::new("Browse Files at This Commit").on_click(
                    move |_, _, cx| {
                        let (weak, commit) = &browse;
                        weak.update(cx, |_, cx| {
                            cx.emit(HistoryEvent::BrowseCommit(commit.clone()));
                        })
                        .ok();
                    },
                ))
                .item(
                    PopupMenuItem::new("Copy Commit Hash")
                        .on_click(move |_, _, cx| Self::copy_hash(&copy, cx)),
                )
            })
            .into_any_element()
    }
}

/// Seconds since the Unix epoch now; the date filter's fields start from it.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0))
}

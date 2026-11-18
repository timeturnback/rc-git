//! The open commit's detail under the History commit list: a tab bar over one of two tabs.
//! Commit shows the header (title, refs, identities, body, parents) beside the files it
//! changed; Changes shows the files beside the picked file's path and diff. File Tree in the
//! bar shows the commit's files in the Files pane.

use std::ops::Range;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, relative,
    uniform_list,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Icon, Selectable, Sizable};
use rc_git_core::{CommitDetail, CommitSummary, EntryKind, RefEntry, TreeChange};
use rc_git_kit::browser::{BrowserModel, CommitDetailState};
use rc_git_kit::change_tree::{ChangeGrouping, last_component};
use rc_git_kit::diff_text::change_letter;
use rc_git_kit::history_text::{
    change_path_label, changes_count, commit_line, committer_differs, identity_line, parents_label,
    short_hash,
};
use rc_git_kit::layout_policy::{HISTORY_COMMIT_FILES_SHARE, HISTORY_FILES_MIN_WIDTH};
use rc_git_kit::models::FileDiffState;

use super::{DetailTab, HistoryEvent, HistoryPane, splits};
use crate::icons::{Glyph, entry_glyph, icon, listing_icon};
use crate::status::change_color;
use crate::style::{self, points};

/// What the header shows for one detail state.
struct Header {
    summary: CommitSummary,
    detail: Option<CommitDetail>,
    refs: Vec<RefEntry>,
    /// The body, or the failure in its place.
    body: Option<String>,
    count: String,
}

fn header(state: &CommitDetailState, changes: usize) -> Option<Header> {
    match state {
        CommitDetailState::Empty => None,
        CommitDetailState::Loading(summary) => Some(Header {
            summary: summary.clone(),
            detail: None,
            refs: Vec::new(),
            body: None,
            count: "Loading changes\u{2026}".to_owned(),
        }),
        CommitDetailState::Loaded { detail, refs } => Some(Header {
            summary: detail.summary.clone(),
            body: (!detail.body.is_empty()).then(|| detail.body.clone()),
            detail: Some(detail.clone()),
            refs: refs.clone(),
            count: changes_count(changes),
        }),
        CommitDetailState::Failed(summary, message) => Some(Header {
            summary: summary.clone(),
            detail: None,
            refs: Vec::new(),
            body: Some(message.clone()),
            count: String::new(),
        }),
    }
}

/// The change the diff shows, whatever its state.
fn shown_change(state: &FileDiffState) -> Option<&TreeChange> {
    match state {
        FileDiffState::Empty => None,
        FileDiffState::Loading(change)
        | FileDiffState::Loaded(change, _)
        | FileDiffState::Submodule(change)
        | FileDiffState::Failed(change, _) => Some(change),
    }
}

impl HistoryPane {
    /// The tab bar over the open tab.
    pub(super) fn render_detail(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.read(cx);
        let Some(header) = header(model.commit_detail(), model.commit_changes().len()) else {
            return div().into_any_element();
        };
        let border = cx.theme().border;
        let bar = self.render_tab_bar(cx);
        let content = match self.detail_tab {
            DetailTab::Commit => self.render_commit_tab(&header, cx),
            DetailTab::Changes => self.render_changes_tab(&header, window, cx),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .border_t(style::HAIRLINE)
            .border_color(border)
            .child(bar)
            .child(div().flex_1().min_h_0().child(content))
            .into_any_element()
    }

    /// File Tree in the tab bar: shows the open commit's files in the Files pane.
    pub(crate) fn browse_open_commit(&mut self, cx: &mut Context<Self>) {
        let summary = self.model.read(cx).commit_detail().summary().cloned();
        if let Some(summary) = summary {
            cx.emit(HistoryEvent::BrowseCommit(summary));
        }
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let border = cx.theme().border;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(style::ROW_GAP)
            .px(style::BAR_PADDING)
            .h(style::LIST_ROW_HEIGHT + style::BAR_PADDING * 2.)
            .border_b(style::HAIRLINE)
            .border_color(border)
            .child(self.tab_button("detail-tab-commit", DetailTab::Commit, cx))
            .child(self.tab_button("detail-tab-changes", DetailTab::Changes, cx))
            .child(
                Button::new("detail-tab-file-tree")
                    .small()
                    .ghost()
                    .label("File Tree")
                    .tooltip("Browse Files at This Commit")
                    .on_click(cx.listener(|this, _, _, cx| this.browse_open_commit(cx))),
            )
            .child(div().flex_1())
            .child(
                Button::new("close-commit")
                    .small()
                    .ghost()
                    .icon(Icon::new(icon(Glyph::CloseTab)))
                    .tooltip("Hide Commit Details")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model.update(cx, BrowserModel::close_commit_detail);
                    })),
            )
    }

    fn tab_button(
        &self,
        id: &'static str,
        tab: DetailTab,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let label = match tab {
            DetailTab::Commit => "Commit",
            DetailTab::Changes => "Changes",
        };
        Button::new(id)
            .small()
            .ghost()
            .label(label)
            .selected(self.detail_tab == tab)
            .on_click(cx.listener(move |this, _, _, cx| this.set_detail_tab(tab, cx)))
    }

    /// The header on the left, scrolling when the body is long, and the files on the right.
    fn render_commit_tab(&self, header: &Header, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let info = self.render_header(header, cx);
        let files = self.render_file_column(&header.count, cx);
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .id("commit-info")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(info),
            )
            .child(
                div()
                    .h_full()
                    .flex_none()
                    .w(relative(HISTORY_COMMIT_FILES_SHARE))
                    .min_w(points(HISTORY_FILES_MIN_WIDTH))
                    .border_l(style::HAIRLINE)
                    .border_color(border)
                    .child(files),
            )
            .into_any_element()
    }

    /// One line naming the commit over the files beside the diff, split as last dragged.
    fn render_changes_tab(
        &self,
        header: &Header,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let summary_line = Self::render_summary_line(&header.summary, cx);
        let files = self.render_file_column(&header.count, cx);
        let diff = self.render_diff(cx);
        let diff_width = self.splits.diff_width(window, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(summary_line)
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("history-split")
                        .with_state(self.splits.diff())
                        .child(
                            resizable_panel()
                                .size_range(splits::files_range())
                                .child(files),
                        )
                        .child(
                            resizable_panel()
                                .size(diff_width)
                                .size_range(splits::diff_range())
                                .child(diff),
                        ),
                ),
            )
            .into_any_element()
    }

    /// The commit's short hash, author and title, so the Changes tab says what it shows.
    fn render_summary_line(
        summary: &CommitSummary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (border, muted, foreground) = (theme.border, theme.muted_foreground, theme.foreground);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(style::ROW_GAP)
            .px(style::MARGIN)
            .h(style::LIST_ROW_HEIGHT)
            .border_b(style::HAIRLINE)
            .border_color(border)
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(style::TEXT_CAPTION)
            .child(
                div()
                    .flex_none()
                    .text_color(muted)
                    .child(SharedString::from(summary.author_name.clone())),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(style::MONOSPACE_FONT)
                    .text_color(muted)
                    .child(SharedString::from(short_hash(&summary.oid))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(foreground)
                    .child(SharedString::from(summary.title.clone())),
            )
    }

    /// The changed-file count and the List/Tree choice over the files.
    fn render_file_column(&self, count: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let grouping = self.files.grouping;
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(style::ROW_GAP)
                    .pl(style::MARGIN)
                    .pr(style::BAR_PADDING)
                    .h(style::LIST_ROW_HEIGHT)
                    .border_b(style::HAIRLINE)
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(style::TEXT_CAPTION)
                            .text_color(muted)
                            .child(SharedString::from(count.to_owned())),
                    )
                    .child(Self::grouping_button(
                        "group-flat",
                        ChangeGrouping::Flat,
                        grouping,
                        cx,
                    ))
                    .child(Self::grouping_button(
                        "group-folders",
                        ChangeGrouping::Folders,
                        grouping,
                        cx,
                    )),
            )
            .child(div().flex_1().min_h_0().child(self.render_files(cx)))
    }

    fn grouping_button(
        id: &'static str,
        grouping: ChangeGrouping,
        current: ChangeGrouping,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let label = match grouping {
            ChangeGrouping::Flat => "List",
            ChangeGrouping::Folders => "Tree",
        };
        Button::new(id)
            .xsmall()
            .ghost()
            .label(label)
            .selected(grouping == current)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.files.set_grouping(grouping);
                cx.notify();
            }))
    }

    fn render_header(&self, header: &Header, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let (muted, foreground) = (theme.muted_foreground, theme.foreground);
        let summary = &header.summary;
        let author = identity_line(
            "Author",
            &summary.author_name,
            &summary.author_email,
            summary.author_time,
            (self.offset_at)(summary.author_time),
        );
        let committer = header
            .detail
            .as_ref()
            .filter(|detail| committer_differs(detail))
            .map(|detail| {
                identity_line(
                    "Committer",
                    &detail.committer_name,
                    &detail.committer_email,
                    detail.committer_time,
                    (self.offset_at)(detail.committer_time),
                )
            });
        let refs = (!header.refs.is_empty()).then(|| {
            let names: Vec<&str> = header.refs.iter().map(|r| r.short_name.as_str()).collect();
            names.join(" \u{b7} ")
        });
        let caption = |text: String, color| {
            div()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(style::TEXT_CAPTION)
                .text_color(color)
                .child(SharedString::from(text))
        };
        let parents = self.render_parents(summary, cx);
        div()
            .flex()
            .flex_col()
            .gap(style::DETAIL_LINE_GAP)
            .px(style::MARGIN)
            .py(style::BAR_PADDING)
            .child(
                div()
                    .text_size(style::TEXT_BODY)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(foreground)
                    .line_clamp(style::DETAIL_TITLE_LINES)
                    .child(SharedString::from(summary.title.clone())),
            )
            .children(refs.map(|refs| caption(refs, muted).font_weight(gpui::FontWeight::MEDIUM)))
            .child(caption(author, muted))
            .children(committer.map(|line| caption(line, muted)))
            .child(caption(commit_line(&summary.oid), muted).font_family(style::MONOSPACE_FONT))
            .children(parents)
            .children(header.body.clone().map(|body| {
                div()
                    .pt(style::BAR_PADDING)
                    .text_size(style::TEXT_CAPTION)
                    .text_color(foreground)
                    .child(SharedString::from(body))
            }))
    }

    fn render_parents(
        &self,
        summary: &CommitSummary,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let label = parents_label(summary.parents.len());
        if label.is_none() && !self.detail_can_go_back {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let mut row = div().flex().items_center().gap(style::ROW_GAP);
        if self.detail_can_go_back {
            row = row.child(
                Button::new("detail-back")
                    .small()
                    .ghost()
                    .label("\u{2039} Back")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(HistoryEvent::DetailBack))),
            );
        }
        if let Some(label) = label {
            row = row.child(
                div()
                    .text_size(style::TEXT_CAPTION)
                    .text_color(muted)
                    .child(label),
            );
        }
        for (index, parent) in summary.parents.iter().copied().enumerate() {
            row = row.child(
                Button::new(("parent", index))
                    .small()
                    .ghost()
                    .font_family(style::MONOSPACE_FONT)
                    .label(short_hash(&parent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model
                            .update(cx, |model, cx| model.jump_to_commit(parent, cx))
                            .detach();
                    })),
            );
        }
        Some(row)
    }

    fn render_files(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .relative()
            .size_full()
            .child(
                uniform_list(
                    "changed-files",
                    self.files.rows.len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        let mut rows = Vec::with_capacity(range.len());
                        for index in range {
                            rows.push(this.render_file_row(index, cx));
                        }
                        rows
                    }),
                )
                .track_scroll(&self.files_scroll)
                .size_full(),
            )
            .child(Scrollbar::vertical(&self.files_scroll))
    }

    fn render_file_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.files.rows.get(index).cloned() else {
            return div().into_any_element();
        };
        let selected = row.change.is_some()
            && shown_change(self.model.read(cx).file_diff()) == row.change.as_ref();
        let theme = cx.theme();
        let (foreground, muted, active) =
            (theme.foreground, theme.muted_foreground, theme.list_active);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a folder depth, far inside f32's exact range"
        )]
        let indent = style::FOLDER_INDENT * row.depth as f32;
        let label: AnyElement = match &row.change {
            Some(change) => div()
                .flex()
                .items_center()
                .gap(style::ROW_GAP)
                .overflow_hidden()
                .child(
                    div()
                        .w(style::ICON_SIZE)
                        .flex_none()
                        .flex()
                        .justify_center()
                        .text_size(style::TEXT_STATUS_MARK)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(change_color(change.kind, cx))
                        .child(change_letter(change.kind)),
                )
                .child(listing_icon(
                    entry_glyph(change.entry, last_component(&row.name)),
                    cx,
                ))
                .child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(foreground)
                        .child(SharedString::from(row.name.clone())),
                )
                .into_any_element(),
            None => div()
                .flex()
                .items_center()
                .gap(style::ROW_GAP)
                .text_color(muted)
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(
                    Icon::new(icon(if row.expanded {
                        Glyph::Expanded
                    } else {
                        Glyph::Collapsed
                    }))
                    .size(style::ICON_SIZE),
                )
                .child(listing_icon(Glyph::Entry(EntryKind::Tree), cx))
                .child(SharedString::from(row.name.clone()))
                .into_any_element(),
        };
        let key = row.key.clone();
        let change = row.change.clone();
        div()
            .id(("changed-file", index))
            .flex()
            .items_center()
            .h(style::LIST_ROW_HEIGHT)
            .pl(style::MARGIN + indent)
            .pr(style::MARGIN)
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(style::TEXT_CAPTION)
            .when(selected, |row| row.bg(active))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(change) = &change {
                    this.open_file_diff(change.clone(), cx);
                } else {
                    this.files.toggle(&key);
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    /// The picked file's path bar over its diff.
    pub(super) fn render_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        let path = shown_change(self.model.read(cx).file_diff())
            .map(change_path_label)
            .map(SharedString::from);
        let bar = self.diff.read(cx).path_bar(path, self.diff_expanded, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(bar)
            .child(div().flex_1().min_h_0().child(self.diff.clone()))
            .into_any_element()
    }
}

//! The open commit's half of the History pane: the detail header (title, refs, identities,
//! body, parents) over the files it changed, and the picked file's path over its diff.

use std::ops::Range;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Icon, Selectable, Sizable};
use rc_git_core::{CommitDetail, CommitSummary, RefEntry, TreeChange};
use rc_git_kit::browser::{BrowserModel, CommitDetailState};
use rc_git_kit::change_tree::ChangeGrouping;
use rc_git_kit::diff_text::change_letter;
use rc_git_kit::history_text::{
    change_path_label, changes_count, committer_differs, detail_title, identity_line,
    parents_label, short_hash,
};
use rc_git_kit::models::FileDiffState;

use super::{HistoryEvent, HistoryPane};
use crate::icons::{Glyph, icon};
use crate::status::change_color;
use crate::style;

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
    /// The header over the file list.
    pub(super) fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.read(cx);
        let Some(header) = header(model.commit_detail(), model.commit_changes().len()) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let header_view = self.render_header(&header, cx);
        let grouping = self.files.grouping;
        div()
            .size_full()
            .flex()
            .flex_col()
            .border_t(style::HAIRLINE)
            .border_color(border)
            .child(header_view)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(style::ROW_GAP)
                    .px(style::MARGIN)
                    .pb(style::ROW_GAP)
                    .child(
                        div()
                            .flex_1()
                            .text_size(style::TEXT_CAPTION)
                            .text_color(muted)
                            .child(SharedString::from(header.count)),
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
            .into_any_element()
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
            .small()
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
        let (muted, accent, foreground) = (theme.muted_foreground, theme.primary, theme.foreground);
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
            .gap(style::HAIRLINE * 3.)
            .px(style::MARGIN)
            .py(style::BAR_PADDING)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(style::ROW_GAP)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(style::TEXT_BODY)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(foreground)
                            .line_clamp(style::DETAIL_TITLE_LINES)
                            .child(SharedString::from(detail_title(summary))),
                    )
                    .child(
                        Button::new("close-commit")
                            .small()
                            .ghost()
                            .label("\u{2715}")
                            .tooltip("Close commit")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.update(cx, BrowserModel::close_commit_detail);
                            })),
                    ),
            )
            .children(refs.map(|refs| caption(refs, accent)))
            .child(caption(author, muted))
            .children(committer.map(|line| caption(line, muted)))
            .children(header.body.clone().map(|body| {
                div()
                    .text_size(style::TEXT_CAPTION)
                    .text_color(foreground)
                    .line_clamp(style::DETAIL_BODY_LINES)
                    .child(SharedString::from(body))
            }))
            .children(parents)
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
                .gap(style::ROW_GAP)
                .overflow_hidden()
                .child(
                    div()
                        .flex_none()
                        .font_family(style::MONOSPACE_FONT)
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(change_color(change.kind, cx))
                        .child(change_letter(change.kind)),
                )
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
                    let change = change.clone();
                    this.model
                        .update(cx, |model, cx| model.load_file_diff(change, cx))
                        .detach();
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

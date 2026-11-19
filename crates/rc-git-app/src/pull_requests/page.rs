//! A pull request's page in the content card: the strip of open pull request tabs, the header
//! (title, author, branches, state, review), then one of two views. Overview shows the checks
//! and the description in a markdown preview that opens only web links, on a click
//! ([`LinkPolicy::WebOnly`]); Changes shows the files the pull request changes and their diffs
//! ([`super::changes`]), fetched only when the user clicks Fetch Changes. Nothing on the page
//! loads a URL: a details link and the host's page open in the browser when clicked, their
//! host named on the button.
//!
//! Under the header sits the Merge control, as kit decides it ([`MergeButton`]): hidden while
//! nothing current is on screen, disabled with kit's reason, or the repository's methods and
//! Merge. Below it, only when kit shows it ([`AdminMergeButton`]), the separate admin merge
//! control, worded so it never reads as Merge. Then, as kit decides them, the auto-merge control
//! ([`AutoMergeControl`]: turn it on with a method, or its state and a switch to turn it off),
//! the re-run control ([`RerunControl`]), and for a merged pull request the delete branch control
//! ([`DeleteBranchControl`]: the button, or kit's reason there is none). A click asks the model to
//! start; the challenge,
//! the only way to send, comes up through the window ([`crate::window::RepoWindow`]), and the
//! note under the controls is kit's. A tab runs one action at a time.
//!
//! A check kit offers it for ([`PullRequestsModel::log_check`]) has a View Log button. Its log
//! shows in place of the description, in the file viewer as plain text with no link (nothing
//! subscribes to the viewer's link events), under kit's note while it reads and kit's words for
//! what was left out; Stop and Close Log drop it.
//!
//! [`LinkPolicy::WebOnly`]: crate::files::markdown::LinkPolicy::WebOnly

use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{ActiveTheme, Disableable, Icon, h_flex, v_flex};
use rc_git_kit::pull_requests::auto_merge::AutoMergeControl;
use rc_git_kit::pull_requests::body::body_document;
use rc_git_kit::pull_requests::checks::{check_state, ordered, summary};
use rc_git_kit::pull_requests::copy::{
    ADMIN_MERGE, ADMIN_MERGE_NOTE, AUTO_MERGE_NOTE, CLOSE_LOG, DELETE_BRANCH, DISABLE_AUTO_MERGE,
    ENABLE_AUTO_MERGE, MERGE, MERGE_CANCEL, MERGE_DISMISS, RERUN_FAILED_JOBS, STOP_LOG, VIEW_LOG,
    auto_merge_on_line, delete_branch_line, flow_note, flow_note_alarms, log_cut_notes, log_note,
    log_title, merge_method_label, rerun_line,
};
use rc_git_kit::pull_requests::copy::{
    CHANGES_VIEW, DETAIL_LOADING, NO_DESCRIPTION, OVERVIEW_VIEW, STALE_NOTE, check_label,
    details_label, fields_note, freshness_problem, mergeable_label, review_label, row_detail,
    state_label, tab_title,
};
use rc_git_kit::pull_requests::delete_branch::DeleteBranchControl;
use rc_git_kit::pull_requests::flow::TabFlow;
use rc_git_kit::pull_requests::link::RepositoryLink;
use rc_git_kit::pull_requests::logs::{LogPhase, LogState, ShownLog};
use rc_git_kit::pull_requests::merge::{AdminMergeButton, MergeButton};
use rc_git_kit::pull_requests::model::PullRequestsModel;
use rc_git_kit::pull_requests::rerun::RerunControl;
use rc_git_kit::pull_requests::state::DetailState;
use rc_git_kit::pull_requests::{Check, JobId, MergeMethod, PullRequestDetail, ServerLink};

use super::changes::{self, ChangesView};
use super::check_icon;
use crate::files::markdown::MarkdownPreview;
use crate::files::viewer::FileViewer;
use crate::icons::{Glyph, icon};
use crate::style;

/// Which view the page shows under the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageView {
    /// The checks and the description.
    Overview,
    /// The changed files and their diffs.
    Changes,
}

/// The page.
pub struct PullRequestPage {
    model: Entity<PullRequestsModel>,
    /// The description on show: its pull request, its text, and its preview.
    preview: Option<(u64, String, Entity<MarkdownPreview>)>,
    view: PageView,
    changes: ChangesView,
    now: fn() -> u64,
    /// The merge method picked for a tab, until another is.
    merge_method: Option<(u64, MergeMethod)>,
    /// The log on show and its viewer, rebuilt only when kit hands another log.
    log_viewer: Option<(Arc<ShownLog>, Entity<FileViewer>)>,
}

impl PullRequestPage {
    /// The page over `model`; `now` tells the time for rate-limit notes.
    pub fn new(model: Entity<PullRequestsModel>, now: fn() -> u64, cx: &mut Context<Self>) -> Self {
        cx.observe(&model, |this, model, cx| {
            let detail = model
                .read(cx)
                .selected_tab()
                .and_then(DetailState::detail)
                .cloned();
            this.sync_preview(detail.as_ref(), cx);
            this.sync_log(cx);
            this.follow_changes(cx);
            cx.notify();
        })
        .detach();
        Self {
            changes: ChangesView::new(cx),
            model,
            preview: None,
            view: PageView::Overview,
            now,
            merge_method: None,
            log_viewer: None,
        }
    }

    /// The viewer of the selected tab's log, kept while kit hands the same log.
    fn sync_log(&mut self, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let shown = model
            .selected_tab()
            .and_then(|tab| model.tab_log(tab.number()))
            .and_then(|log| match log.phase() {
                LogPhase::Shown(shown) => Some(Arc::clone(shown)),
                LogPhase::Matching
                | LogPhase::Downloading
                | LogPhase::NotMatched(_)
                | LogPhase::NotFinished
                | LogPhase::Failed(_) => None,
            });
        let Some(shown) = shown else {
            self.log_viewer = None;
            return;
        };
        if self
            .log_viewer
            .as_ref()
            .is_some_and(|(on_show, _)| Arc::ptr_eq(on_show, &shown))
        {
            return;
        }
        let document = Arc::clone(&shown.document);
        let viewer = cx.new(|cx| FileViewer::new(document, cx));
        self.log_viewer = Some((shown, viewer));
    }

    /// The log viewer on show, for tests.
    #[cfg(test)]
    pub(crate) fn log_viewer(&self) -> Option<&Entity<FileViewer>> {
        self.log_viewer.as_ref().map(|(_, viewer)| viewer)
    }

    /// Tab `number`'s log panel: kit's title, note and words for what was left out, Stop while
    /// it reads or Close Log after, and the log in the viewer once it shows.
    fn log_panel(&self, number: u64, log: &LogState, cx: &mut Context<Self>) -> gpui::Div {
        let theme = cx.theme();
        let (muted, red, border) = (theme.muted_foreground, theme.red, theme.border);
        let label = if log.is_running() {
            STOP_LOG
        } else {
            CLOSE_LOG
        };
        let close = Button::new("pull-request-log-close")
            .ghost()
            .label(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.model
                    .update(cx, |model, cx| model.close_log(number, cx));
            }));
        let heading = h_flex()
            .gap(style::ROW_GAP)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(style::TEXT_BODY)
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from(log_title(log))),
            )
            .child(close);
        let mut words = v_flex()
            .gap(style::DETAIL_LINE_GAP)
            .px(style::MARGIN)
            .py(style::BAR_PADDING)
            .child(heading);
        if let Some(note) = log_note(log, (self.now)()) {
            let alarm = match log.phase() {
                LogPhase::NotMatched(_) | LogPhase::Failed(_) => true,
                LogPhase::Matching
                | LogPhase::Downloading
                | LogPhase::Shown(_)
                | LogPhase::NotFinished => false,
            };
            words = words.child(Self::line(note, if alarm { red } else { muted }));
        }
        let mut panel = v_flex()
            .flex_1()
            .min_h_0()
            .border_t(style::HAIRLINE)
            .border_color(border);
        let Some((shown, viewer)) = &self.log_viewer else {
            return panel.child(words);
        };
        for note in log_cut_notes(shown.cut, shown.lines_left_out, shown.lines_shortened) {
            words = words.child(Self::line(note, muted));
        }
        panel = panel.child(words);
        panel.child(div().flex_1().min_h_0().child(viewer.clone()))
    }

    /// The method Merge uses for tab `number`: the one picked, while the repository still
    /// allows it, else kit's first.
    fn chosen_method(&self, number: u64, methods: &[MergeMethod]) -> Option<MergeMethod> {
        match self.merge_method {
            Some((tab, method)) if tab == number && methods.contains(&method) => Some(method),
            Some(_) | None => methods.first().copied(),
        }
    }

    /// The Merge, auto-merge and re-run controls and the note of tab `number`'s action.
    fn merge_controls(&self, number: u64, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model = self.model.read(cx);
        let button = model.merge_button(number);
        let admin = model.admin_merge_button(number);
        let auto = model.auto_merge_control(number);
        let rerun = model.rerun_control(number);
        let delete = model.delete_branch_control(number);
        let flow = model.tab_flow(number).cloned();
        let running = flow.as_ref().is_some_and(TabFlow::is_running);
        let theme = cx.theme();
        let (muted, red) = (theme.muted_foreground, theme.red);
        let mut column = v_flex().gap(style::DETAIL_LINE_GAP);
        match button {
            MergeButton::Hidden => {}
            MergeButton::Disabled { reason } => column = column.child(Self::line(reason, muted)),
            MergeButton::Enabled { methods } => {
                let chosen = self.chosen_method(number, &methods);
                let row = Self::method_picks(number, methods, (chosen, running), cx);
                let merge = Button::new("pull-request-merge")
                    .label(MERGE)
                    .danger()
                    .disabled(running || chosen.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(method) = chosen {
                            this.model
                                .update(cx, |model, cx| model.start_merge(method, cx));
                        }
                    }));
                column = column.child(row.child(merge));
            }
        }
        if let AdminMergeButton::Shown { methods } = admin {
            column = column.child(self.admin_merge_control(number, methods, running, cx));
        }
        if let Some(control) = self.auto_merge_control(number, auto, running, cx) {
            column = column.child(control);
        }
        if let RerunControl::Shown { runs, left_out } = rerun {
            column = column.child(Self::rerun_control(runs, left_out, running, cx));
        }
        match delete {
            DeleteBranchControl::Hidden => {}
            DeleteBranchControl::Unavailable { reason } => {
                column = column.child(Self::line(reason, muted));
            }
            DeleteBranchControl::Shown { branch } => {
                column = column.child(Self::delete_branch_control(&branch, running, cx));
            }
        }
        let Some(flow) = flow else {
            return column;
        };
        let Some(note) = flow_note(&flow) else {
            return column;
        };
        let failed = flow_note_alarms(&flow);
        let mut row = h_flex()
            .gap(style::ROW_GAP)
            .child(Self::line(note, if failed { red } else { muted }));
        if flow.is_cancellable() {
            row = row.child(
                Button::new("pull-request-merge-cancel")
                    .ghost()
                    .label(MERGE_CANCEL)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model
                            .update(cx, |model, cx| model.cancel_flow(number, cx));
                    })),
            );
        }
        if !flow.is_running() {
            row = row.child(
                Button::new("pull-request-merge-dismiss")
                    .ghost()
                    .label(MERGE_DISMISS)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model
                            .update(cx, |model, cx| model.dismiss_flow(number, cx));
                    })),
            );
        }
        column.child(row)
    }

    /// One button per method in `methods`, the `chosen` one primary, none clickable while a
    /// merge of tab `number` runs.
    fn method_picks(
        number: u64,
        methods: Vec<MergeMethod>,
        (chosen, running): (Option<MergeMethod>, bool),
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut row = h_flex().gap(style::ROW_GAP);
        for (index, method) in methods.into_iter().enumerate() {
            let pick = Button::new(("pull-request-merge-method", index))
                .label(merge_method_label(method))
                .disabled(running)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.merge_method = Some((number, method));
                    cx.notify();
                }));
            row = row.child(if chosen == Some(method) {
                pick.primary()
            } else {
                pick.ghost()
            });
        }
        row
    }

    /// The admin merge control of tab `number`, which kit shows only when branch protection
    /// blocks Merge and GitHub says the account may merge past it: kit's note, the methods, and
    /// a button worded unlike Merge. Its click only asks the model to start; the challenge that
    /// asks for the number is the only way to send.
    fn admin_merge_control(
        &self,
        number: u64,
        methods: Vec<MergeMethod>,
        running: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let chosen = self.chosen_method(number, &methods);
        let muted = cx.theme().muted_foreground;
        let row = Self::method_picks(number, methods, (chosen, running), cx);
        let admin = Button::new("pull-request-admin-merge")
            .label(ADMIN_MERGE)
            .danger()
            .disabled(running || chosen.is_none())
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(method) = chosen {
                    this.model
                        .update(cx, |model, cx| model.start_admin_merge(method, cx));
                }
            }));
        v_flex()
            .gap(style::DETAIL_LINE_GAP)
            .child(Self::line(ADMIN_MERGE_NOTE.to_owned(), muted))
            .child(row.child(admin))
    }

    /// The auto-merge control of tab `number` as kit decides it: off with the methods it may be
    /// turned on with, the note saying GitHub merges later commits too and a button; or on, with
    /// kit's line naming the method and who turned it on, and a button to turn it off when kit
    /// allows. A click only asks the model to start; the challenge is the only way to send.
    fn auto_merge_control(
        &self,
        number: u64,
        control: AutoMergeControl,
        running: bool,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let muted = cx.theme().muted_foreground;
        match control {
            AutoMergeControl::Hidden => None,
            AutoMergeControl::Offer { methods } => {
                let chosen = self.chosen_method(number, &methods);
                let row = Self::method_picks(number, methods, (chosen, running), cx);
                let enable = Button::new("pull-request-enable-auto-merge")
                    .label(ENABLE_AUTO_MERGE)
                    .disabled(running || chosen.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(method) = chosen {
                            this.model.update(cx, |model, cx| {
                                model.start_enable_auto_merge(method, cx);
                            });
                        }
                    }));
                Some(
                    v_flex()
                        .gap(style::DETAIL_LINE_GAP)
                        .child(Self::line(AUTO_MERGE_NOTE.to_owned(), muted))
                        .child(row.child(enable)),
                )
            }
            AutoMergeControl::On {
                method,
                enabled_by,
                can_turn_off,
            } => {
                let line = auto_merge_on_line(method, enabled_by.as_deref());
                let mut row = h_flex().gap(style::ROW_GAP).child(Self::line(line, muted));
                if can_turn_off {
                    row = row.child(
                        Button::new("pull-request-disable-auto-merge")
                            .ghost()
                            .label(DISABLE_AUTO_MERGE)
                            .disabled(running)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model
                                    .update(cx, PullRequestsModel::start_disable_auto_merge);
                            })),
                    );
                }
                Some(row)
            }
        }
    }

    /// The re-run control kit shows for a pull request with failed GitHub Actions runs: kit's
    /// line and a button. A click only asks the model to start; the challenge is the only way to
    /// send.
    fn rerun_control(
        runs: usize,
        left_out: usize,
        running: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let muted = cx.theme().muted_foreground;
        let rerun = Button::new("pull-request-rerun-failed-jobs")
            .label(RERUN_FAILED_JOBS)
            .disabled(running)
            .on_click(cx.listener(|this, _, _, cx| {
                this.model.update(cx, PullRequestsModel::start_rerun);
            }));
        h_flex()
            .gap(style::ROW_GAP)
            .child(rerun)
            .child(Self::line(rerun_line(runs, left_out), muted))
    }

    /// The delete branch control kit shows for a merged pull request's own head branch: a
    /// destructive button and kit's line saying it deletes `branch` for everyone after a backup
    /// ref. A click only asks the model to start; the challenge that asks for the branch's name
    /// typed is the only way to send.
    fn delete_branch_control(branch: &str, running: bool, cx: &mut Context<Self>) -> gpui::Div {
        let muted = cx.theme().muted_foreground;
        let delete = Button::new("pull-request-delete-branch")
            .label(DELETE_BRANCH)
            .danger()
            .disabled(running)
            .on_click(cx.listener(|this, _, _, cx| {
                this.model
                    .update(cx, PullRequestsModel::start_delete_branch);
            }));
        h_flex()
            .gap(style::ROW_GAP)
            .child(delete)
            .child(Self::line(delete_branch_line(branch), muted))
    }

    /// The view on show.
    #[cfg(test)]
    pub(crate) fn view(&self) -> PageView {
        self.view
    }

    /// Shows `view`; Changes reads what the repository holds for the selected tab.
    pub fn set_view(&mut self, view: PageView, cx: &mut Context<Self>) {
        self.view = view;
        self.follow_changes(cx);
        cx.notify();
    }

    /// Keeps the Changes view on the selected tab's current read and its diff on the picked
    /// file. Reads only local objects: a fetch starts from its button alone.
    fn follow_changes(&mut self, cx: &mut Context<Self>) {
        if self.view == PageView::Changes {
            self.model.update(cx, PullRequestsModel::show_changes);
        }
        let model = self.model.read(cx);
        let changes = model.selected_changes().cloned();
        self.changes.sync(changes.as_ref(), cx);
    }

    /// The Changes view's diff, for tests.
    #[cfg(test)]
    pub(crate) fn changes_diff(&self) -> &Entity<crate::diff::DiffView> {
        &self.changes.diff
    }

    /// The description preview on show, for tests.
    #[cfg(test)]
    pub(crate) fn preview(&self) -> Option<&Entity<MarkdownPreview>> {
        self.preview.as_ref().map(|(_, _, preview)| preview)
    }

    /// The description's preview for `detail`, rebuilt only when the text changed.
    fn sync_preview(&mut self, detail: Option<&PullRequestDetail>, cx: &mut Context<Self>) {
        let Some(detail) = detail.filter(|detail| !detail.body.is_empty()) else {
            self.preview = None;
            return;
        };
        let number = detail.summary.number;
        let same = self
            .preview
            .as_ref()
            .is_some_and(|(shown, body, _)| *shown == number && *body == detail.body);
        if same {
            return;
        }
        let document = Arc::new(body_document(&detail.body));
        let preview = cx.new(|cx| MarkdownPreview::web_only(document, cx));
        self.preview = Some((number, detail.body.clone(), preview));
    }

    fn tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model = self.model.read(cx);
        let selected = model.selected_tab().map(DetailState::number);
        let tabs: Vec<(u64, String)> = model
            .tabs()
            .iter()
            .map(|tab| {
                let summary = tab.detail().map(|detail| &detail.summary);
                (tab.number(), tab_title(tab.number(), summary))
            })
            .collect();
        let theme = cx.theme();
        let mut strip = h_flex()
            .id("pull-request-tabs")
            .gap(style::TAB_GAP)
            .px(style::MARGIN)
            .py(style::BAR_PADDING)
            .border_b(style::HAIRLINE)
            .border_color(theme.border)
            .overflow_x_scroll();
        for (number, title) in tabs {
            let active = selected == Some(number);
            strip = strip.child(
                h_flex()
                    .id(SharedString::from(format!("pull-request-tab-{number}")))
                    .h(style::TAB_HEIGHT)
                    .max_w(style::TAB_MAX_WIDTH)
                    .px(style::ROW_GAP)
                    .gap(style::ROW_GAP)
                    .rounded(style::RADIUS)
                    .when(active, |tab| tab.bg(theme.list_active))
                    .cursor_pointer()
                    .text_size(style::TEXT_CAPTION)
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(title)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("pull-request-close-{number}")))
                            .child(Icon::new(icon(Glyph::CloseTab)).size(style::TAB_ICON_SIZE))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.model.update(cx, |model, cx| model.close(number, cx));
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.update(cx, |model, cx| model.select(number, cx));
                    })),
            );
        }
        strip
    }

    fn view_switch(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut switch = h_flex()
            .gap(style::TAB_GAP)
            .px(style::MARGIN)
            .pb(style::BAR_PADDING);
        for (id, label, view) in [
            ("pull-request-overview", OVERVIEW_VIEW, PageView::Overview),
            ("pull-request-changes", CHANGES_VIEW, PageView::Changes),
        ] {
            let button = Button::new(id)
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| this.set_view(view, cx)));
            let button = if self.view == view {
                button.primary()
            } else {
                button.ghost()
            };
            switch = switch.child(button);
        }
        switch
    }

    /// The remote the linked repository reads from, for the Changes view's words.
    fn remote(&self, cx: &Context<Self>) -> String {
        match self.model.read(cx).link() {
            Some(RepositoryLink::Linked(linked)) => linked.remote.clone(),
            Some(_) | None => "the remote".to_owned(),
        }
    }

    fn line(text: String, color: gpui::Hsla) -> impl IntoElement {
        div()
            .text_size(style::TEXT_CAPTION)
            .text_color(color)
            .child(SharedString::from(text))
    }

    fn link_button(id: &'static str, label: String, url: String) -> impl IntoElement {
        Button::new(id)
            .ghost()
            .label(label)
            .icon(Icon::new(icon(Glyph::OpenInBrowser)))
            .tooltip(url.clone())
            .on_click(move |_, _, cx| cx.open_url(&url))
    }

    fn header(&self, tab: &DetailState, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let merge = self.merge_controls(tab.number(), cx);
        let theme = cx.theme();
        let (muted, red) = (theme.muted_foreground, theme.red);
        let detail = tab.detail();
        let mut column = v_flex()
            .gap(style::DETAIL_LINE_GAP)
            .px(style::MARGIN)
            .py(style::BAR_PADDING);
        let title =
            detail.map_or_else(|| format!("#{}", tab.number()), |d| d.summary.title.clone());
        column = column.child(
            div()
                .text_size(style::TEXT_BODY + gpui::px(3.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from(title)),
        );
        if let Some(detail) = detail {
            let summary = &detail.summary;
            let facts = format!(
                "{} · {} · {} · {}",
                state_label(summary.state, summary.draft),
                row_detail(summary),
                review_label(summary.review),
                mergeable_label(detail.mergeable)
            );
            column = column.child(Self::line(facts, muted));
            if let Some(note) = fields_note(detail.fields) {
                column = column.child(Self::line(note.to_owned(), muted));
            }
            if let Some(url) = &detail.url {
                let label = match url {
                    ServerLink::OnHost(_) => "Open in Browser".to_owned(),
                    ServerLink::Elsewhere { host, .. } => format!("Open on {host}"),
                };
                column = column.child(Self::link_button(
                    "pull-request-open",
                    label,
                    url.url().to_owned(),
                ));
            }
        } else if tab.is_loading() {
            column = column.child(Self::line(DETAIL_LOADING.to_owned(), muted));
        }
        if let Some(problem) = freshness_problem(tab.freshness(), (self.now)()) {
            column = column.child(Self::line(problem, red));
            if detail.is_some() {
                column = column.child(Self::line(STALE_NOTE.to_owned(), muted));
            }
        }
        column.child(merge)
    }

    fn check_row(
        index: usize,
        check: &Check,
        log: Option<JobId>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let view_log = log.map(|job| {
            Button::new(("pull-request-check-log", index))
                .ghost()
                .label(VIEW_LOG)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.model.update(cx, |model, cx| model.view_log(job, cx));
                }))
        });
        let theme = cx.theme();
        let state = check_state(&check.progress);
        let mut name = check.name.clone();
        if check.required == Some(true) {
            name.push_str(" · required");
        }
        let detail = check
            .description
            .clone()
            .unwrap_or_else(|| check_label(state).to_owned());
        let details = check
            .details
            .as_ref()
            .map(|link| (details_label(link), link.url().to_owned()));
        h_flex()
            .id(("pull-request-check", index))
            .w_full()
            .gap(style::ROW_GAP)
            .py(gpui::px(2.))
            .child(div().flex_none().child(check_icon(state, theme)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(style::TEXT_BODY)
                            .child(SharedString::from(name)),
                    )
                    .child(Self::line(detail, theme.muted_foreground)),
            )
            .when_some(view_log, ParentElement::child)
            .when_some(details, |row, (label, url)| {
                row.child(
                    Button::new(("pull-request-check-details", index))
                        .ghost()
                        .label(label)
                        .tooltip(url.clone())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
            })
    }

    fn checks(
        &self,
        detail: &PullRequestDetail,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let number = detail.summary.number;
        let mut rows = v_flex()
            .id("pull-request-checks")
            .max_h(style::PULL_REQUEST_CHECKS_MAX_HEIGHT)
            .overflow_y_scroll();
        for (index, check) in ordered(&detail.checks.checks).into_iter().enumerate() {
            let log = self
                .model
                .read(cx)
                .log_check(number, check)
                .map(|offered| offered.job);
            rows = rows.child(Self::check_row(index, check, log, cx));
        }
        let theme = cx.theme();
        v_flex()
            .px(style::MARGIN)
            .py(style::BAR_PADDING)
            .border_t(style::HAIRLINE)
            .border_color(theme.border)
            .child(
                div()
                    .text_size(style::TEXT_BODY)
                    .font_weight(FontWeight::MEDIUM)
                    .child("Checks"),
            )
            .child(Self::line(summary(&detail.checks), theme.muted_foreground))
            .child(rows)
    }
}

impl Render for PullRequestPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.model.read(cx).selected_tab().cloned();
        let strip = self.tab_strip(cx);
        let theme = cx.theme();
        let mut page = v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(strip);
        let Some(tab) = tab else {
            return page;
        };
        page = page.child(self.header(&tab, cx));
        page = page.child(self.view_switch(cx));
        if self.view == PageView::Changes {
            let remote = self.remote(cx);
            let view = changes::render(&self.changes, &self.model, &remote, cx);
            return page.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .border_t(style::HAIRLINE)
                    .border_color(cx.theme().border)
                    .child(view),
            );
        }
        if let Some(detail) = tab.detail() {
            page = page.child(self.checks(detail, cx));
        }
        let log = self.model.read(cx).tab_log(tab.number()).cloned();
        if let Some(log) = log {
            return page.child(self.log_panel(tab.number(), &log, cx));
        }
        let description = v_flex()
            .flex_1()
            .min_h_0()
            .border_t(style::HAIRLINE)
            .border_color(cx.theme().border);
        let description = match (&self.preview, tab.detail()) {
            (Some((_, _, preview)), _) => {
                description.child(div().size_full().child(preview.clone()))
            }
            (None, Some(_)) => description.child(
                div()
                    .p(style::MARGIN)
                    .text_size(style::TEXT_BODY)
                    .text_color(cx.theme().muted_foreground)
                    .child(NO_DESCRIPTION),
            ),
            (None, None) => description,
        };
        page.child(description)
    }
}

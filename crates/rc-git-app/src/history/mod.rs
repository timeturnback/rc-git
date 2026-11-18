//! The History pane (`HistoryViewController`): the commit list of the anchor, its lane graph,
//! search and date filters, the path scope, and an open commit's detail across the pane under
//! the list, in two tabs: Commit (the header beside the changed files) and Changes (the
//! changed files beside the picked file's diff).
//!
//! Commits load a page at a time as the list nears its end (`history_text::wants_next_page`),
//! and only the rows on screen are built, so nothing scales with history length. Everything
//! the pane shows is the model's state; the pane keeps only what the model has no word for:
//! the search controls, the folders a user closed in the file list, the detail's tab, the
//! split sizes and the column widths.
//!
//! `list` draws the commit list and its bars, `columns` the list's header and column widths,
//! `detail` the commit's tabs, header, files and diff, `graph` the lane cells, `search` holds the
//! search controls, `splits` the two saved splits.

mod columns;
mod detail;
mod graph;
mod list;
pub mod search;
mod splits;

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AppContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    UniformListScrollHandle, Window, div,
};
use gpui_component::input::InputEvent;
use gpui_component::resizable::{resizable_panel, v_resizable};
use rc_git_core::bstr::ByteSlice;
use rc_git_core::{CommitSummary, TreeChange};
use rc_git_kit::browser::{BrowserEvent, BrowserModel, CommitDetailState};
use rc_git_kit::change_tree::{ChangeGrouping, ChangeNode, nodes, visible_rows};
use rc_git_kit::history_text::wants_next_page;
use rc_git_kit::models::FileDiffState;
use rc_git_kit::platform::stores::{ChangeGroupingStore, SharedSettings};
use rc_git_kit::ref_filter::{filter_is_empty, revision};

use crate::diff::{DiffContent, DiffView};
use crate::log;
use columns::ColumnWidths;
use search::{Clock, OffsetAt, SearchControls};
use splits::HistorySplits;

pub use list::now;

/// How long typing must pause before the query is searched, so a burst of keystrokes starts
/// one commit walk rather than one per key.
pub const TYPING_PAUSE: Duration = Duration::from_millis(250);

/// What the pane asks of its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryEvent {
    /// Show the files of this commit: double-click, or Browse Files at This Commit.
    BrowseCommit(CommitSummary),
    /// The detail's Back button: step back through the app-wide list.
    DetailBack,
}

/// The tab an open commit's detail shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    /// The commit's title, identities, body and parents beside the files it changed.
    Commit,
    /// The files the commit changed beside the picked file's diff.
    Changes,
}

/// One row of the changed-file list, flattened from the kit tree.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRow {
    depth: usize,
    name: String,
    key: String,
    change: Option<TreeChange>,
    expanded: bool,
}

/// The changed-file list: its grouping, the folders a user closed, and the rows they leave.
struct FileList {
    store: ChangeGroupingStore,
    grouping: ChangeGrouping,
    changes: Vec<TreeChange>,
    collapsed: HashSet<String>,
    rows: Vec<FileRow>,
}

impl FileList {
    fn new(settings: SharedSettings) -> Self {
        let store = ChangeGroupingStore::commit(settings);
        Self {
            grouping: store.grouping(),
            store,
            changes: Vec::new(),
            collapsed: HashSet::new(),
            rows: Vec::new(),
        }
    }

    /// Takes the model's changes; a new list opens every folder.
    fn set_changes(&mut self, changes: &[TreeChange]) {
        if changes == self.changes.as_slice() {
            return;
        }
        changes.clone_into(&mut self.changes);
        self.collapsed.clear();
        self.rebuild();
    }

    fn set_grouping(&mut self, grouping: ChangeGrouping) {
        self.grouping = grouping;
        if let Err(error) = self.store.set_grouping(grouping) {
            log::error(&error.to_string());
        }
        self.rebuild();
    }

    fn toggle(&mut self, key: &str) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_owned());
        }
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let tree: Vec<ChangeNode<TreeChange>> = nodes(&self.changes, self.grouping);
        let mut rows = Vec::new();
        for row in visible_rows(&tree, &self.collapsed) {
            rows.push(FileRow {
                depth: row.depth,
                name: row.node.name.clone(),
                key: row.node.key(),
                change: row.node.entry.clone(),
                expanded: row.expanded,
            });
        }
        self.rows = rows;
    }
}

/// The History pane of one window.
pub struct HistoryPane {
    model: Entity<BrowserModel>,
    offset_at: OffsetAt,
    search: SearchControls,
    /// The query typing scheduled, sent once typing pauses; any other emit replaces it.
    typed: Option<Task<()>>,
    list_focus: FocusHandle,
    list_scroll: UniformListScrollHandle,
    files: FileList,
    files_scroll: UniformListScrollHandle,
    diff: Entity<DiffView>,
    /// The file diff the diff view was last handed, so an unrelated detail change keeps it.
    shown_diff: Option<FileDiffState>,
    splits: HistorySplits,
    columns: ColumnWidths,
    detail_tab: DetailTab,
    /// Whether the diff fills the pane, the commit list and detail out of the way.
    diff_expanded: bool,
    detail_can_go_back: bool,
    /// Commit rows handed to the list since creation: the "build only what is on screen"
    /// budget.
    built_rows: Rc<Cell<usize>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<HistoryEvent> for HistoryPane {}

impl Focusable for HistoryPane {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.list_focus.clone()
    }
}

impl HistoryPane {
    /// A pane over `model`, reading dates through `clock`.
    pub fn new(
        model: Entity<BrowserModel>,
        settings: SharedSettings,
        clock: Clock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = SearchControls::new(clock.offset_at, clock.now, window, cx);
        let splits = HistorySplits::load(settings.clone(), cx);
        let diff = cx.new(DiffView::new);
        let mut subscriptions = splits.watch(|this: &mut Self| &mut this.splits, cx);
        subscriptions.push(cx.subscribe_in(&model, window, Self::model_changed));
        subscriptions.push(cx.subscribe_in(&search.query, window, Self::query_event));
        for field in [&search.since, &search.until] {
            subscriptions.push(cx.subscribe(field, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.emit_filter(cx);
                }
            }));
        }
        let mut pane = Self {
            model,
            offset_at: clock.offset_at,
            search,
            typed: None,
            list_focus: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            columns: ColumnWidths::load(settings.clone()),
            files: FileList::new(settings),
            files_scroll: UniformListScrollHandle::new(),
            diff,
            shown_diff: None,
            splits,
            detail_tab: DetailTab::Commit,
            diff_expanded: false,
            detail_can_go_back: false,
            built_rows: Rc::default(),
            _subscriptions: subscriptions,
        };
        pane.detail_changed(cx);
        pane
    }

    /// Shows the diff across the whole pane, or puts the commit list and detail back beside it.
    pub fn set_diff_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        if self.diff_expanded != expanded {
            self.diff_expanded = expanded;
            cx.notify();
        }
    }

    /// Whether the diff fills the pane.
    #[must_use]
    pub fn diff_expanded(&self) -> bool {
        self.diff_expanded
    }

    /// Whether the pane draws only the diff now: expanded, with a commit open to show one.
    #[must_use]
    pub fn fills_with_diff(&self, cx: &gpui::App) -> bool {
        self.diff_expanded && *self.model.read(cx).commit_detail() != CommitDetailState::Empty
    }

    /// Whether the detail's Back button shows: stepping back returns to another commit.
    pub fn set_detail_can_go_back(&mut self, can: bool, cx: &mut Context<Self>) {
        if self.detail_can_go_back != can {
            self.detail_can_go_back = can;
            cx.notify();
        }
    }

    /// The tab the open commit's detail shows, for tests.
    #[cfg(test)]
    pub(crate) fn detail_tab(&self) -> DetailTab {
        self.detail_tab
    }

    /// Shows `tab` in the open commit's detail.
    pub fn set_detail_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        if self.detail_tab != tab {
            self.detail_tab = tab;
            cx.notify();
        }
    }

    /// Shows `change`'s diff in the Changes tab: a click on a changed file in either tab. A
    /// diff already loaded or loading for `change` is kept, so its scroll position stays.
    pub(crate) fn open_file_diff(&mut self, change: TreeChange, cx: &mut Context<Self>) {
        self.set_detail_tab(DetailTab::Changes, cx);
        let shown = match self.model.read(cx).file_diff() {
            FileDiffState::Loading(shown) | FileDiffState::Loaded(shown, _) => *shown == change,
            FileDiffState::Empty | FileDiffState::Submodule(_) | FileDiffState::Failed(..) => false,
        };
        if shown {
            return;
        }
        self.model
            .update(cx, |model, cx| model.load_file_diff(change, cx))
            .detach();
    }

    /// Puts the keyboard in the search field (Search Commits, ⇧⌘F).
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .query
            .update(cx, |field, cx| field.focus(window, cx));
    }

    /// The diff view, for tests.
    #[cfg(test)]
    pub(crate) fn diff(&self) -> &Entity<DiffView> {
        &self.diff
    }

    /// The Changes tab's file list | diff split state, for tests.
    #[cfg(test)]
    pub(crate) fn diff_split(&self) -> &Entity<gpui_component::resizable::ResizableState> {
        self.splits.diff()
    }

    /// The commit list over detail split's state, for tests.
    #[cfg(test)]
    pub(crate) fn column_split(&self) -> &Entity<gpui_component::resizable::ResizableState> {
        self.splits.column()
    }

    /// How many commit rows have been built, for tests of the budget.
    #[cfg(test)]
    pub(crate) fn built_rows(&self) -> usize {
        self.built_rows.get()
    }

    /// The changed-file rows' labels, indented two spaces a level, for tests.
    #[cfg(test)]
    pub(crate) fn file_labels(&self) -> Vec<String> {
        self.files
            .rows
            .iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), row.name))
            .collect()
    }

    /// The split sizes as saved, for tests.
    #[cfg(test)]
    pub(crate) fn saved_splits(&self) -> (Option<f64>, Option<f64>) {
        self.splits.saved()
    }

    fn model_changed(
        &mut self,
        _: &Entity<BrowserModel>,
        event: &BrowserEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            BrowserEvent::DiffChanged => self.detail_changed(cx),
            BrowserEvent::SearchChanged => self.search_changed(window, cx),
            BrowserEvent::HistoryChanged => cx.notify(),
            BrowserEvent::RefsChanged
            | BrowserEvent::RootChanged
            | BrowserEvent::FileChanged
            | BrowserEvent::BlameChanged
            | BrowserEvent::LocationChanged
            | BrowserEvent::Error(_) => {}
        }
    }

    /// Follows the model's commit detail: the file list, and the diff when it changed.
    fn detail_changed(&mut self, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        self.files.set_changes(model.commit_changes());
        let state = model.file_diff().clone();
        let content = diff_content(&state, model.commit_detail(), model.commit_changes().len());
        if self.shown_diff.as_ref() != Some(&state) || matches!(state, FileDiffState::Empty) {
            self.shown_diff = Some(state);
            self.diff
                .update(cx, |diff, cx| diff.set_content(content, cx));
        }
        cx.notify();
    }

    /// The model dropped its search (a ref pick, a moved tip, a path scope) while the
    /// controls may still hold one: clear them, so the field never shows a query over the
    /// unfiltered list.
    fn search_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.read(cx).search_filter().is_none() {
            self.typed = None;
            if !filter_is_empty(&self.search.filter(cx)) {
                self.search.clear(window, cx);
            }
        }
        cx.notify();
    }

    fn query_event(
        &mut self,
        _: &Entity<gpui_component::input::InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => self.emit_after_typing_pause(cx),
            InputEvent::PressEnter { .. } => self.submit(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    fn emit_after_typing_pause(&mut self, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(TYPING_PAUSE);
        self.typed = Some(cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, Self::emit_filter).ok();
        }));
    }

    /// Hands the model the filter the controls describe; an empty one clears the search.
    fn emit_filter(&mut self, cx: &mut Context<Self>) {
        self.typed = None;
        let filter = self.search.filter(cx);
        self.model.update(cx, |model, cx| {
            if filter_is_empty(&filter) {
                model.clear_search(cx);
            } else {
                model.search(filter, cx).detach();
            }
        });
    }

    /// Return in the field: a hex word jumps to that revision, anything else searches now.
    fn submit(&mut self, cx: &mut Context<Self>) {
        let query = self.search.query.read(cx).value();
        let Some(spec) = revision(&query, self.search.scope).map(str::to_owned) else {
            self.emit_filter(cx);
            return;
        };
        self.model
            .update(cx, |model, cx| model.jump_to_revision(&spec, cx))
            .detach();
    }

    fn search_active(&self, cx: &gpui::App) -> bool {
        self.model.read(cx).search_filter().is_some()
    }

    /// The commits the list shows: search matches while a search is active.
    fn displayed<'a>(&self, cx: &'a gpui::App) -> &'a [CommitSummary] {
        let model = self.model.read(cx);
        if model.search_filter().is_some() {
            model.search_results()
        } else {
            model.history()
        }
    }

    /// Whether the list has nothing more to ask for: complete, or stopped on a failure.
    fn list_done(&self, cx: &gpui::App) -> bool {
        let model = self.model.read(cx);
        if model.search_filter().is_some() {
            model.search_complete() || model.search_error().is_some()
        } else {
            model.history_complete() || model.history_error().is_some()
        }
    }

    /// Asks for the next page once rows up to `visible_end` are on screen and the end is
    /// near. Deferred: it runs while the list lays out.
    fn page_if_near_end(&self, visible_end: usize, cx: &mut gpui::App) {
        let loaded = self.displayed(cx).len();
        if !wants_next_page(visible_end, loaded, self.list_done(cx)) {
            return;
        }
        let model = self.model.clone();
        let searching = self.search_active(cx);
        cx.defer(move |cx| {
            model.update(cx, |model, cx| {
                if searching {
                    model.load_more_search_results(cx).detach();
                } else {
                    model.load_more_history(cx).detach();
                }
            });
        });
    }

    fn select_commit(&mut self, commit: CommitSummary, cx: &mut Context<Self>) {
        self.model
            .update(cx, |model, cx| model.load_commit_changes(commit, cx))
            .detach();
    }

    fn copy_hash(commit: &CommitSummary, cx: &mut gpui::App) {
        cx.write_to_clipboard(ClipboardItem::new_string(commit.oid.to_hex()));
    }

    /// The commit list over the open commit's detail; the diff alone while it fills the pane.
    fn render_splits(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.fills_with_diff(cx) {
            return self.render_diff(cx);
        }
        let open = *self.model.read(cx).commit_detail() != CommitDetailState::Empty;
        let commits = self.render_commits(cx);
        let detail = open.then(|| self.render_detail(window, cx));
        let detail_height = self.splits.detail_height(window, cx);
        v_resizable("history-column")
            .with_state(self.splits.column())
            .child(
                resizable_panel()
                    .size_range(splits::commits_range())
                    .child(commits),
            )
            .child(
                resizable_panel()
                    .visible(open)
                    .size(detail_height)
                    .size_range(splits::detail_range())
                    .children(detail),
            )
            .into_any_element()
    }
}

/// What the diff view shows for the model's file diff: hunks, or the line in their place.
fn diff_content(state: &FileDiffState, detail: &CommitDetailState, changes: usize) -> DiffContent {
    let line = |text: String| {
        if text.is_empty() {
            DiffContent::Empty
        } else {
            DiffContent::Placeholder(text.into())
        }
    };
    match state {
        FileDiffState::Empty => line(detail.empty_diff_placeholder(changes).to_owned()),
        FileDiffState::Loading(_) => line("Loading\u{2026}".to_owned()),
        FileDiffState::Loaded(change, diff) => {
            DiffContent::for_blob(diff, &change.path.to_str_lossy())
        }
        FileDiffState::Submodule(change) => line(rc_git_kit::models::submodule_summary(change)),
        FileDiffState::Failed(_, message) => line(message.clone()),
    }
}

impl Render for HistoryPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.displayed(cx).is_empty() {
            self.page_if_near_end(0, cx);
        }
        self.columns.fit_date(window, cx);
        div()
            .id("history-pane")
            .key_context("HistoryPane")
            .size_full()
            .child(self.render_splits(window, cx))
    }
}

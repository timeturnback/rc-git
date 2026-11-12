//! The Changes sidebar section: the sync row, the change list, the stash list and the commit
//! box. The list's rows are rebuilt only when the model says the list or its grouping
//! changed; a tick moves only redraw, and every visible row reads its tick box from the model
//! as it is drawn. Expansion and selection live by [`ChangeNode::key`], never by row index.

use std::collections::HashSet;

use gpui::{
    AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, UniformListScrollHandle, Window,
    div, prelude::FluentBuilder,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::{ActiveTheme, Disableable, Selectable, Sizable};
use rc_git_core::StatusEntry;
use rc_git_kit::change_tree::{ChangeGrouping, ChangeNode, visible_rows};
use rc_git_kit::changes::{ChangesEvent, ChangesListNotice, ChangesModel, CommitAuthorModel};
use rc_git_kit::changes_view::{stash_message, truncation_note};
use rc_git_kit::sync::{SyncModel, pull_title, push_title};

use crate::changes::{ChangesRequest, heading};
use crate::log;
use crate::style;

/// What the section tells its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeListEvent {
    /// A mutating request for the guard.
    Request(ChangesRequest),
}

/// One drawn row: the node without its children (a tick box reads only its path and kind),
/// how deep it sits, and whether a folder is open.
#[derive(Debug, Clone)]
pub(super) struct ListRow {
    pub(super) node: ChangeNode<StatusEntry>,
    pub(super) key: String,
    pub(super) depth: usize,
    pub(super) expanded: bool,
}

/// What the section's sync row and commit box read besides the change model.
pub struct Sources {
    /// Who the next commit or stash records.
    pub author: Option<Entity<CommitAuthorModel>>,
    /// The window's sync workflows, for the sync row.
    pub sync: Option<Entity<SyncModel>>,
    /// Whether the repository has a checkout, which Back to Default needs.
    pub has_work_dir: bool,
}

/// The Changes sidebar section of one window.
pub struct ChangeListView {
    pub(super) model: Entity<ChangesModel>,
    author: Option<Entity<CommitAuthorModel>>,
    sync: Option<Entity<SyncModel>>,
    has_work_dir: bool,
    /// The list's nodes, rebuilt on a status or grouping change only.
    pub(super) nodes: Vec<ChangeNode<StatusEntry>>,
    pub(super) rows: Vec<ListRow>,
    /// Folders the user closed; every other folder is open.
    pub(super) collapsed: HashSet<String>,
    pub(super) selected: HashSet<String>,
    pub(super) anchor: Option<usize>,
    pub(super) notice: ChangesListNotice,
    truncation: Option<String>,
    message: Entity<TextareaState>,
    pub(super) focus: FocusHandle,
    pub(super) scroll: UniformListScrollHandle,
    /// How many times the rows were rebuilt, for the tests that count them.
    rebuilds: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChangeListEvent> for ChangeListView {}

impl ChangeListView {
    /// The section over `model`, with the author line and sync row `sources` name.
    pub fn new(
        model: Entity<ChangesModel>,
        sources: Sources,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Sources {
            author,
            sync,
            has_work_dir,
        } = sources;
        let message = cx.new(|cx| TextareaState::new(window, cx).placeholder("Commit message"));
        let mut subscriptions = vec![
            cx.subscribe(&model, |this, _, event: &ChangesEvent, cx| match event {
                ChangesEvent::StatusChanged | ChangesEvent::GroupingChanged => {
                    this.rebuild(event == &ChangesEvent::GroupingChanged, cx);
                }
                ChangesEvent::TicksChanged | ChangesEvent::StashChanged => cx.notify(),
                ChangesEvent::DiffChanged | ChangesEvent::Error(_) => {}
            }),
            cx.subscribe(&message, |this, _, event: &InputEvent, cx| {
                if let InputEvent::PressEnter {
                    secondary: true, ..
                } = event
                {
                    this.commit(cx);
                }
            }),
        ];
        if let Some(author) = &author {
            subscriptions.push(cx.observe(author, |_, _, cx| cx.notify()));
        }
        if let Some(sync) = &sync {
            subscriptions.push(cx.observe(sync, |_, _, cx| cx.notify()));
        }
        let mut view = Self {
            model,
            author,
            sync,
            has_work_dir,
            nodes: Vec::new(),
            rows: Vec::new(),
            collapsed: HashSet::new(),
            selected: HashSet::new(),
            anchor: None,
            notice: rc_git_kit::changes::ChangesState::Idle.notice(false),
            truncation: None,
            message,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            rebuilds: 0,
            _subscriptions: subscriptions,
        };
        view.rebuild(false, cx);
        view
    }

    /// The labels of the rows on screen, in order, for tests.
    #[cfg(test)]
    pub(crate) fn labels(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.node.name.clone()).collect()
    }

    /// How many times the rows were rebuilt.
    #[cfg(test)]
    pub(crate) fn rebuilds(&self) -> usize {
        self.rebuilds
    }

    /// The commit box's text.
    #[must_use]
    pub fn message(&self, cx: &gpui::App) -> String {
        self.message.read(cx).value().to_string()
    }

    /// Replaces the commit box's text, as typing would leave it.
    pub fn set_message(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.message
            .update(cx, |message, cx| message.set_value(text, window, cx));
    }

    /// Empties the commit box after a commit or stash landed.
    pub fn clear_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_message("", window, cx);
    }

    /// Rebuilds the rows from the model: the list when the state carries one, the grouping
    /// always. A state that carries no list (a scan in flight) keeps the rows on screen.
    fn rebuild(&mut self, regrouped: bool, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let state = model.status_state();
        self.truncation = truncation_note(state);
        if regrouped {
            self.collapsed.clear();
        }
        if regrouped || state.listed_entries().is_some() {
            self.nodes = model.nodes();
            self.rebuilds += 1;
            self.relayout();
        }
        self.notice = state.notice(!self.nodes.is_empty());
        cx.notify();
    }

    /// Lays the rows out again for the current expansion, keeping the selection by key.
    pub(super) fn relayout(&mut self) {
        let rows: Vec<ListRow> = visible_rows(&self.nodes, &self.collapsed)
            .into_iter()
            .map(|row| ListRow {
                node: ChangeNode {
                    name: row.node.name.clone(),
                    path: row.node.path.clone(),
                    entry: row.node.entry.clone(),
                    children: Vec::new(),
                },
                key: row.node.key(),
                depth: row.depth,
                expanded: row.expanded,
            })
            .collect();
        let keys: HashSet<&String> = rows.iter().map(|row| &row.key).collect();
        self.selected.retain(|key| keys.contains(key));
        self.rows = rows;
        if self.anchor.is_some_and(|anchor| anchor >= self.rows.len()) {
            self.anchor = None;
        }
    }

    pub(super) fn request(request: ChangesRequest, cx: &mut Context<Self>) {
        cx.emit(ChangeListEvent::Request(request));
    }

    /// Commit, from the button or ⌘Return in the box.
    pub fn commit(&mut self, cx: &mut Context<Self>) {
        let message = self.message(cx);
        Self::request(ChangesRequest::Commit(message), cx);
    }

    fn stash(&mut self, cx: &mut Context<Self>) {
        let message = stash_message(&self.message(cx));
        Self::request(ChangesRequest::StashPush(message), cx);
    }

    fn set_grouping(&mut self, grouping: ChangeGrouping, cx: &mut Context<Self>) {
        let saved = self
            .model
            .update(cx, |model, cx| model.set_grouping(grouping, cx));
        if let Err(error) = saved {
            log::error(&format!("save the change grouping: {error}"));
        }
    }

    fn render_sync_row(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (availability, status) = match &self.sync {
            Some(sync) => {
                let sync = sync.read(cx);
                (
                    Some(sync.availability(self.has_work_dir)),
                    sync.remote_status().cloned(),
                )
            }
            None => (None, None),
        };
        let can = |pick: fn(&rc_git_kit::sync::SyncAvailability) -> bool| {
            availability.as_ref().is_some_and(pick)
        };
        let button = |id: &'static str, label: String, enabled: bool| {
            Button::new(id).small().label(label).disabled(!enabled)
        };
        div()
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .child(
                button("changes-fetch", "Fetch".to_owned(), can(|a| a.can_fetch))
                    .on_click(cx.listener(|_, _, _, cx| Self::request(ChangesRequest::Fetch, cx))),
            )
            .child(
                button(
                    "changes-pull",
                    pull_title(status.as_ref()),
                    can(|a| a.can_pull),
                )
                .on_click(cx.listener(|_, _, _, cx| Self::request(ChangesRequest::Pull, cx))),
            )
            .child(
                button(
                    "changes-push",
                    push_title(status.as_ref()),
                    can(|a| a.can_push),
                )
                .on_click(cx.listener(|_, _, _, cx| Self::request(ChangesRequest::Push, cx))),
            )
    }

    fn render_list_heading(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let grouping = self.model.read(cx).grouping();
        let mut toggle = div().flex().items_center();
        for (choice, label) in [
            (ChangeGrouping::Flat, "List"),
            (ChangeGrouping::Folders, "Tree"),
        ] {
            toggle = toggle.child(
                Button::new(label)
                    .xsmall()
                    .ghost()
                    .label(label)
                    .selected(grouping == choice)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_grouping(choice, cx))),
            );
        }
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(heading("Changes", cx))
            .child(toggle)
    }

    fn render_commit_box(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model = self.model.read(cx);
        let summary = model.tick_summary();
        let current = self.notice.is_current;
        let enabled = current && summary.has_selection;
        let author = self
            .author
            .as_ref()
            .and_then(|author| author.read(cx).author().cloned());
        let theme = cx.theme();
        let summary_color = if current {
            theme.muted_foreground
        } else {
            theme.muted_foreground.opacity(0.5)
        };
        div()
            .flex()
            .flex_col()
            .gap(style::ROW_GAP)
            .child(Textarea::new(&self.message).h(style::CHANGES_MESSAGE_HEIGHT))
            .when_some(author, |column, author| {
                column.child(
                    div()
                        .id("commit-author")
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(style::TEXT_CAPTION)
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(author.label())),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(style::ROW_GAP)
                    .child(
                        div()
                            .flex_1()
                            .text_size(style::TEXT_CAPTION)
                            .text_color(summary_color)
                            .child(SharedString::from(summary.text)),
                    )
                    .child(
                        Button::new("stash-push")
                            .small()
                            .label("Stash")
                            .disabled(!enabled)
                            .on_click(cx.listener(|this, _, _, cx| this.stash(cx))),
                    )
                    .child(
                        Button::new("commit")
                            .small()
                            .primary()
                            .label("Commit")
                            .disabled(!enabled)
                            .on_click(cx.listener(|this, _, _, cx| this.commit(cx))),
                    ),
            )
    }
}

impl Render for ChangeListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let truncation = self.truncation.clone();
        let muted = cx.theme().muted_foreground;
        div()
            .id("changes-section")
            .size_full()
            .flex()
            .flex_col()
            .gap(style::ROW_GAP)
            .px(style::MARGIN)
            .pb(style::MARGIN)
            .child(self.render_sync_row(cx))
            .child(self.render_list_heading(cx))
            .child(self.render_files(cx))
            .when_some(truncation, |column, note| {
                column.child(
                    div()
                        .text_size(style::TEXT_CAPTION)
                        .text_color(muted)
                        .child(SharedString::from(note)),
                )
            })
            .children(self.render_stashes(cx))
            .child(self.render_commit_box(cx))
    }
}

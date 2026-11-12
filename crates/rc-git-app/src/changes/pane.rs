//! The Changes content pane: the picked file's diff against `HEAD` in the shared
//! [`DiffView`], with a tick box drawn first in each hunk's header row. A press on a header
//! row ticks or unticks that hunk; the context menu discards the hunk of the row it was
//! opened on. Tick boxes read the model as they are drawn, so a tick move only redraws.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, ParentElement,
    Render, Styled, Subscription, WeakEntity, Window, div,
};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use rc_git_core::DiffHunk;
use rc_git_kit::changes::{ChangesEvent, ChangesModel, TickState, WorktreeDiffState};
use rc_git_kit::changes_view::{diff_path, diff_placeholder};
use rc_git_kit::diff_rows::DiffRow;
use rc_git_kit::guard_bridge::HunkStamp;

use crate::changes::list::ChangeListEvent;
use crate::changes::{ChangesRequest, tick_box};
use crate::diff::{DiffContent, DiffView, DiffViewEvent, RowContext, RowPrefix};
use crate::style;

/// The Changes content pane of one window.
pub struct WorktreeDiffPane {
    model: Entity<ChangesModel>,
    diff: Entity<DiffView>,
    path: Option<String>,
    hunks: Arc<[DiffHunk]>,
    /// The hunk of the row last pressed, which the context menu acts on.
    context_hunk: Option<usize>,
    /// Whether the window shows this pane across its whole width, for the path bar's toggle.
    expanded: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChangeListEvent> for WorktreeDiffPane {}

impl WorktreeDiffPane {
    /// The pane over `model`.
    pub fn new(model: Entity<ChangesModel>, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(DiffView::new);
        let prefix = Self::prefix(cx.entity().downgrade());
        diff.update(cx, |diff, cx| diff.set_prefix(Some(prefix), cx));
        let subscriptions = vec![
            cx.subscribe(&model, |this, _, event: &ChangesEvent, cx| match event {
                ChangesEvent::DiffChanged => this.show_diff(cx),
                ChangesEvent::TicksChanged => this.diff.update(cx, |_, cx| cx.notify()),
                ChangesEvent::StatusChanged
                | ChangesEvent::StashChanged
                | ChangesEvent::GroupingChanged
                | ChangesEvent::Error(_) => {}
            }),
            cx.subscribe(&diff, |this, _, event: &DiffViewEvent, cx| match *event {
                DiffViewEvent::HeaderClicked { hunk } => this.toggle_hunk(hunk, cx),
                DiffViewEvent::RowClicked { hunk, .. } => this.context_hunk = Some(hunk),
            }),
        ];
        let mut pane = Self {
            model,
            diff,
            path: None,
            hunks: Arc::new([]),
            context_hunk: None,
            expanded: false,
            _subscriptions: subscriptions,
        };
        pane.show_diff(cx);
        pane
    }

    /// Draws a hunk's tick box first in its header row and an empty box of the same width
    /// first in a line row, so the gutters stay aligned.
    fn prefix(pane: WeakEntity<Self>) -> RowPrefix {
        Rc::new(
            move |row: RowContext, _window: &mut Window, cx: &mut gpui::App| {
                let state = match row.info {
                    DiffRow::Header { hunk } => pane
                        .upgrade()
                        .and_then(|pane| pane.read(cx).hunk_tick(hunk, cx)),
                    DiffRow::Line { .. } => None,
                };
                let slot = div()
                    .flex_none()
                    .w(style::TICK_SIZE + style::ROW_GAP)
                    .pl(style::ROW_GAP)
                    .flex()
                    .items_center();
                match state {
                    Some(state) => slot.child(tick_box(state, true, cx)).into_any_element(),
                    None => slot.into_any_element(),
                }
            },
        )
    }

    fn hunk_tick(&self, hunk: usize, cx: &gpui::App) -> Option<TickState> {
        let path = self.path.as_deref()?;
        let stamp = HunkStamp::from(self.hunks.get(hunk)?);
        let ticked = self.model.read(cx).is_hunk_ticked(path, stamp);
        Some(if ticked {
            TickState::On
        } else {
            TickState::Off
        })
    }

    fn show_diff(&mut self, cx: &mut Context<Self>) {
        let state = self.model.read(cx).diff_state().clone();
        self.path = diff_path(&state).map(str::to_owned);
        let content = match (&state, diff_placeholder(&state)) {
            (_, Some(line)) => DiffContent::Placeholder(line.into()),
            (WorktreeDiffState::Loaded(path, diff), None) => DiffContent::for_blob(diff, path),
            (_, None) => DiffContent::Empty,
        };
        self.hunks = match &content {
            DiffContent::Hunks(hunks) => Arc::clone(hunks),
            DiffContent::Empty | DiffContent::Placeholder(_) => Arc::new([]),
        };
        self.context_hunk = None;
        self.diff
            .update(cx, |diff, cx| diff.set_content(content, cx));
        cx.notify();
    }

    /// Whether the window shows the pane across its whole width; the path bar's toggle shows it.
    pub(crate) fn set_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        if self.expanded != expanded {
            self.expanded = expanded;
            cx.notify();
        }
    }

    fn stamps(&self) -> Vec<HunkStamp> {
        self.hunks.iter().map(HunkStamp::from).collect()
    }

    /// Ticks or unticks hunk `hunk` of the file on show.
    pub(crate) fn toggle_hunk(&mut self, hunk: usize, cx: &mut Context<Self>) {
        let (Some(path), Some(stamp)) = (self.path.clone(), self.hunks.get(hunk)) else {
            return;
        };
        let stamp = HunkStamp::from(stamp);
        let all = self.stamps();
        self.model
            .update(cx, |model, cx| model.toggle_hunk(&path, stamp, &all, cx));
    }

    fn discard_hunk(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let Some(hunk) = self.context_hunk.and_then(|hunk| self.hunks.get(hunk)) else {
            return;
        };
        let stamp = HunkStamp::from(hunk);
        cx.emit(ChangeListEvent::Request(ChangesRequest::DiscardHunk {
            path,
            stamp,
        }));
    }
}

impl Render for WorktreeDiffPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let path = self.path.clone().map(gpui::SharedString::from);
        let bar = self.diff.read(cx).path_bar(path, self.expanded, cx);
        div()
            .id("worktree-diff")
            .size_full()
            .min_h(style::CHANGES_DIFF_FLOOR)
            .flex()
            .flex_col()
            .child(bar)
            .child(div().flex_1().min_h_0().child(self.diff.clone()))
            .context_menu(move |menu, _, cx| {
                let has_hunk = weak
                    .upgrade()
                    .is_some_and(|pane| pane.read(cx).context_hunk.is_some());
                let weak = weak.clone();
                menu.item(
                    PopupMenuItem::new("Discard This Hunk\u{2026}")
                        .disabled(!has_hunk)
                        .on_click(move |_, _, cx| {
                            weak.update(cx, Self::discard_hunk).ok();
                        }),
                )
            })
            .into_any_element()
    }
}

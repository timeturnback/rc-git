//! A pull request tab's Changes view: the note kit words for where the changes stand, the
//! button the state offers (Fetch Changes, Stop while fetching, Look Again), and once the files
//! are read, the changed-file list beside the shared [`DiffView`] of the picked file. Every
//! click goes to [`PullRequestsModel`]; nothing here reads the repository or the network, and
//! nothing starts a fetch except the Fetch Changes button.

use std::ops::Range;

use gpui::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, UniformListScrollHandle, div,
    prelude::FluentBuilder, relative, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use rc_git_core::TreeChange;
use rc_git_kit::diff_text::change_letter;
use rc_git_kit::layout_policy::PULL_REQUEST_FILES_SHARE;
use rc_git_kit::models::{FileDiffState, submodule_summary};
use rc_git_kit::pull_requests::changes::{ChangesPhase, ChangesState};
use rc_git_kit::pull_requests::copy::{
    FETCH_CHANGES, LOOK_AGAIN, PICK_A_FILE, STOP_FETCH, changed_file_label, changes_note,
};
use rc_git_kit::pull_requests::model::PullRequestsModel;

use crate::diff::{DiffContent, DiffView};
use crate::status::change_color;
use crate::style;

/// What a phase offers: the button and the model call it makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Offer {
    Fetch,
    Stop,
    LookAgain,
}

fn offer(phase: &ChangesPhase) -> Option<Offer> {
    match phase {
        ChangesPhase::NeedsFetch { .. } | ChangesPhase::Moved { .. } => Some(Offer::Fetch),
        ChangesPhase::Fetching => Some(Offer::Stop),
        ChangesPhase::NoBaseCopy { .. }
        | ChangesPhase::NoMergeBase { .. }
        | ChangesPhase::Ready { .. }
        | ChangesPhase::Failed(_) => Some(Offer::LookAgain),
        ChangesPhase::Reading => None,
    }
}

/// What the diff view shows for `state`.
pub(crate) fn diff_content(state: &FileDiffState) -> DiffContent {
    let line = |text: String| DiffContent::Placeholder(text.into());
    match state {
        FileDiffState::Empty => line(PICK_A_FILE.to_owned()),
        FileDiffState::Loading(_) => line("Loading\u{2026}".to_owned()),
        FileDiffState::Loaded(change, diff) => {
            DiffContent::for_blob(diff, &change.path.to_string())
        }
        FileDiffState::Submodule(change) => line(submodule_summary(change)),
        FileDiffState::Failed(_, message) => line(message.clone()),
    }
}

/// The Changes view's parts that live across renders.
pub(crate) struct ChangesView {
    pub(crate) diff: Entity<DiffView>,
    files_scroll: UniformListScrollHandle,
    /// The file diff the diff view was last handed, so an unrelated change keeps its scroll.
    shown: Option<FileDiffState>,
}

impl ChangesView {
    pub(crate) fn new<T: 'static>(cx: &mut Context<T>) -> Self {
        Self {
            diff: cx.new(DiffView::new),
            files_scroll: UniformListScrollHandle::new(),
            shown: None,
        }
    }

    /// Hands the diff view the selected file's diff when it changed.
    pub(crate) fn sync<T: 'static>(&mut self, changes: Option<&ChangesState>, cx: &mut Context<T>) {
        let state = changes.map_or(FileDiffState::Empty, |c| c.file_diff().clone());
        if self.shown.as_ref() == Some(&state) {
            return;
        }
        let content = diff_content(&state);
        self.shown = Some(state);
        self.diff
            .update(cx, |diff, cx| diff.set_content(content, cx));
    }
}

/// The Changes view of the selected tab, `remote` naming where its head comes from.
pub(crate) fn render<T: 'static>(
    view: &ChangesView,
    model: &Entity<PullRequestsModel>,
    remote: &str,
    cx: &mut Context<T>,
) -> AnyElement {
    let Some(changes) = model.read(cx).selected_changes().cloned() else {
        return note_bar(
            rc_git_kit::pull_requests::copy::DETAIL_LOADING.to_owned(),
            None,
            model,
            cx,
        )
        .into_any_element();
    };
    let note = changes_note(changes.phase(), changes.stamp(), remote);
    let bar = note_bar(note, offer(changes.phase()), model, cx);
    let ChangesPhase::Ready { files, .. } = changes.phase() else {
        return v_flex().size_full().child(bar).into_any_element();
    };
    let list = file_list(view, model, files, changes.selected(), cx);
    let path = changes_path(changes.file_diff());
    let diff = v_flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .child(view.diff.read(cx).path_bar(path, false, cx))
        .child(div().flex_1().min_h_0().child(view.diff.clone()));
    v_flex()
        .size_full()
        .child(bar)
        .child(
            h_flex()
                .flex_1()
                .min_h_0()
                .border_t(style::HAIRLINE)
                .border_color(cx.theme().border)
                .child(list)
                .child(diff),
        )
        .into_any_element()
}

/// The path the diff's bar names, once a file is picked.
fn changes_path(state: &FileDiffState) -> Option<SharedString> {
    match state {
        FileDiffState::Empty => None,
        FileDiffState::Loading(change)
        | FileDiffState::Loaded(change, _)
        | FileDiffState::Submodule(change)
        | FileDiffState::Failed(change, _) => Some(changed_file_label(change).into()),
    }
}

fn note_bar<T: 'static>(
    note: String,
    offer: Option<Offer>,
    model: &Entity<PullRequestsModel>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let muted = cx.theme().muted_foreground;
    let button = offer.map(|offer| {
        let (id, label) = match offer {
            Offer::Fetch => ("pull-request-fetch", FETCH_CHANGES),
            Offer::Stop => ("pull-request-stop-fetch", STOP_FETCH),
            Offer::LookAgain => ("pull-request-look-again", LOOK_AGAIN),
        };
        let model = model.clone();
        Button::new(id)
            .ghost()
            .label(label)
            .on_click(move |_, _, cx| {
                model.update(cx, |model, cx| match offer {
                    Offer::Fetch => model.fetch_changes(cx),
                    Offer::Stop => model.cancel_changes_fetch(cx),
                    Offer::LookAgain => model.reread_changes(cx),
                });
            })
    });
    h_flex()
        .w_full()
        .gap(style::ROW_GAP)
        .px(style::MARGIN)
        .py(style::BAR_PADDING)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(style::TEXT_CAPTION)
                .text_color(muted)
                .child(SharedString::from(note)),
        )
        .when_some(button, |bar, button| {
            bar.child(div().flex_none().child(button))
        })
}

fn file_list<T: 'static>(
    view: &ChangesView,
    model: &Entity<PullRequestsModel>,
    files: &[TreeChange],
    selected: Option<usize>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let rows: Vec<(String, TreeChange)> = files
        .iter()
        .map(|change| (changed_file_label(change), change.clone()))
        .collect();
    let count = rows.len();
    let model = model.clone();
    let border = cx.theme().border;
    div()
        .relative()
        .w(relative(PULL_REQUEST_FILES_SHARE))
        .h_full()
        .flex_none()
        .border_r(style::HAIRLINE)
        .border_color(border)
        .child(
            uniform_list(
                "pull-request-files",
                count,
                cx.processor(move |_, range: Range<usize>, _window, cx| {
                    let mut built = Vec::with_capacity(range.len());
                    for index in range {
                        if let Some((label, change)) = rows.get(index) {
                            let active = selected == Some(index);
                            built.push(file_row((index, active), label, change, &model, cx));
                        }
                    }
                    built
                }),
            )
            .track_scroll(&view.files_scroll)
            .size_full(),
        )
        .child(Scrollbar::vertical(&view.files_scroll))
}

/// Row `index` of the file list, highlighted when `active`.
fn file_row<T: 'static>(
    (index, active): (usize, bool),
    label: &str,
    change: &TreeChange,
    model: &Entity<PullRequestsModel>,
    cx: &mut Context<T>,
) -> AnyElement {
    let theme = cx.theme();
    let (foreground, list_active) = (theme.foreground, theme.list_active);
    let model = model.clone();
    h_flex()
        .id(("pull-request-file", index))
        .w_full()
        .h(style::LIST_ROW_HEIGHT)
        .px(style::MARGIN)
        .gap(style::ROW_GAP)
        .when(active, |row| row.bg(list_active))
        .cursor_pointer()
        .child(
            div()
                .w(style::ICON_SIZE)
                .flex_none()
                .text_size(style::TEXT_STATUS_MARK)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(change_color(change.kind, cx))
                .child(change_letter(change.kind)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(style::TEXT_BODY)
                .text_color(foreground)
                .child(SharedString::from(label.to_owned())),
        )
        .on_click(move |_, _, cx| {
            model.update(cx, |model, cx| model.select_changed_file(index, cx));
        })
        .into_any_element()
}

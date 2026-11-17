//! The ref list (`RefsViewController`): a filter field over the Working Tree row, the refs
//! grouped by namespace in natural order, and the refs whose target could not be read. History
//! shows it in the sidebar; Files shows it in the ⌘B picker under the banner. Grouping,
//! order, matching and the Return pick all come from [`RefList`].

use std::ops::Range;

use gpui::MouseButton;
use gpui::{
    AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, Subscription, UniformListScrollHandle, Window, div,
    prelude::FluentBuilder as _, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Icon, Sizable};
use rc_git_core::{BrokenRef, RefEntry};
use rc_git_kit::browser::{BrowserEvent, BrowserModel};
use rc_git_kit::picker;
use rc_git_kit::ref_filter::{RefList, UNREADABLE_TITLE, query, working_tree_matches};
use rc_git_kit::ref_menu::{RefMenu, RefMenuAction, ref_menu};

use crate::icons::{Glyph, icon, is_accented};
use crate::search::{PanelKey, keys};
use crate::style;

/// The label of the Working Tree row; also what its filter matches.
pub const WORKING_TREE: &str = "Working Tree";

/// What the list tells its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefListEvent {
    /// A ref was picked.
    Picked(RefEntry),
    /// The Working Tree row was picked.
    WorkingTree,
    /// Escape in the filter: the picker closes.
    Dismissed,
    /// The context menu asked for a mutating action on a ref.
    Action(RefMenuAction, RefEntry),
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    WorkingTree,
    Header(&'static str),
    Ref(RefEntry),
    Unreadable(BrokenRef),
}

/// The ref list of one window.
pub struct RefListView {
    model: Entity<BrowserModel>,
    filter: Entity<InputState>,
    rows: Vec<Row>,
    /// The ref the context menu was opened on.
    menu_target: Option<RefEntry>,
    /// The row the arrows moved to: in the picker it only highlights, and Return picks it; in
    /// the sidebar it is the row browsed.
    highlight: Option<usize>,
    /// Whether this list is the ⌘B picker rather than the History sidebar.
    picker: bool,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RefListEvent> for RefListView {}

impl Focusable for RefListView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.filter.focus_handle(cx)
    }
}

impl RefListView {
    /// A list over `model`'s refs.
    pub fn new(model: Entity<BrowserModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter refs"));
        let subscriptions = vec![
            cx.subscribe(&model, |this, _, event: &BrowserEvent, cx| {
                if *event == BrowserEvent::RefsChanged {
                    this.reload(cx);
                }
            }),
            cx.subscribe_in(&filter, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    this.reload(cx);
                }
            }),
        ];
        let mut view = Self {
            model,
            filter,
            rows: Vec::new(),
            menu_target: None,
            highlight: None,
            picker: false,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        view.reload(cx);
        view
    }

    /// The labels on screen, in order, for tests.
    #[cfg(test)]
    pub(crate) fn labels(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| match row {
                Row::WorkingTree => WORKING_TREE.to_owned(),
                Row::Header(title) => format!("# {title}"),
                Row::Ref(entry) => entry.short_name.clone(),
                Row::Unreadable(broken) => broken.full_name.clone(),
            })
            .collect()
    }

    /// Picks the row at `index`, as a click does.
    pub fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        match self.rows.get(index) {
            Some(Row::WorkingTree) => cx.emit(RefListEvent::WorkingTree),
            Some(Row::Ref(entry)) => cx.emit(RefListEvent::Picked(entry.clone())),
            Some(Row::Header(_) | Row::Unreadable(_)) | None => {}
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let filter = self.filter.read(cx).value().to_string();
        let model = self.model.read(cx);
        let list = RefList::new(model.refs().clone());
        let has_work_dir = model
            .repository()
            .is_some_and(|repository| repository.work_dir().is_some());
        let mut rows = Vec::new();
        if has_work_dir && working_tree_matches(&filter) {
            rows.push(Row::WorkingTree);
        }
        for section in list.sections(&filter) {
            rows.push(Row::Header(section.title));
            rows.extend(section.refs.into_iter().map(Row::Ref));
        }
        let unreadable = list.unreadable(&filter);
        if !unreadable.is_empty() {
            rows.push(Row::Header(UNREADABLE_TITLE));
            rows.extend(unreadable.into_iter().map(Row::Unreadable));
        }
        self.rows = rows;
        self.highlight = self.initial_highlight(&list, &filter, cx);
        if let Some(row) = self.highlight {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// Makes this list the ⌘B picker: the arrows only highlight, Return picks the highlighted
    /// row, and Escape closes it.
    pub fn set_picker(&mut self, cx: &mut Context<Self>) {
        self.picker = true;
        self.reload(cx);
    }

    /// The highlighted row.
    #[cfg(test)]
    pub(crate) fn highlight(&self) -> Option<usize> {
        self.highlight
    }

    /// Where the highlight starts: in the picker with a filter typed, the best match, or the
    /// Working Tree row when only it matches; otherwise the row browsed now.
    fn initial_highlight(&self, list: &RefList, filter: &str, cx: &Context<Self>) -> Option<usize> {
        if self.picker && query(filter).is_some() {
            let best = list.best_match(filter).and_then(|entry| {
                self.rows.iter().position(
                    |row| matches!(row, Row::Ref(found) if found.full_name == entry.full_name),
                )
            });
            return best.or_else(|| self.rows.iter().position(|row| *row == Row::WorkingTree));
        }
        self.rows.iter().position(|row| self.is_current(row, cx))
    }

    fn is_pickable(&self, index: usize) -> bool {
        match self.rows.get(index) {
            Some(Row::WorkingTree | Row::Ref(_)) => true,
            Some(Row::Header(_) | Row::Unreadable(_)) | None => false,
        }
    }

    /// The arrows, Return and Escape from the filter.
    fn key(&mut self, key: PanelKey, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            PanelKey::Up | PanelKey::Down => {
                let step = if key == PanelKey::Up { -1 } else { 1 };
                let next = picker::next_pickable(self.highlight, step, self.rows.len(), |row| {
                    self.is_pickable(row)
                });
                let Some(row) = next else {
                    return;
                };
                self.highlight = Some(row);
                self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
                if !self.picker {
                    self.pick(row, cx);
                }
                cx.notify();
            }
            PanelKey::Enter if self.picker => {
                let filtered = query(&self.filter.read(cx).value()).is_some();
                let first = (0..self.rows.len()).find(|row| self.is_pickable(*row));
                if let Some(row) = picker::ref_picker_return(self.highlight, filtered, first) {
                    self.pick_or_close(row, cx);
                }
            }
            PanelKey::Enter => self.pick_best(cx),
            PanelKey::Escape if self.picker => cx.emit(RefListEvent::Dismissed),
            PanelKey::Escape => {
                self.filter
                    .update(cx, |filter, cx| filter.set_value("", window, cx));
                self.reload(cx);
            }
        }
    }

    fn clicked(&mut self, index: usize, cx: &mut Context<Self>) {
        self.highlight = Some(index);
        if self.picker {
            self.pick_or_close(index, cx);
        } else {
            self.pick(index, cx);
        }
        cx.notify();
    }

    /// The picker's pick: the row browsed now closes it with nothing to reload, so the tree
    /// keeps its expanded folders.
    fn pick_or_close(&mut self, index: usize, cx: &mut Context<Self>) {
        let current = self
            .rows
            .get(index)
            .is_some_and(|row| self.is_current(row, cx));
        if current {
            cx.emit(RefListEvent::Dismissed);
        } else {
            self.pick(index, cx);
        }
    }

    /// Return in the sidebar's filter: the best match by [`RefList::best_match`], else the
    /// Working Tree row when it is the only one left.
    fn pick_best(&mut self, cx: &mut Context<Self>) {
        let filter = self.filter.read(cx).value().to_string();
        let list = RefList::new(self.model.read(cx).refs().clone());
        if let Some(entry) = list.best_match(&filter) {
            cx.emit(RefListEvent::Picked(entry));
        } else if self.rows.first() == Some(&Row::WorkingTree) {
            cx.emit(RefListEvent::WorkingTree);
        }
    }

    fn is_current(&self, row: &Row, cx: &Context<Self>) -> bool {
        let model = self.model.read(cx);
        match row {
            Row::WorkingTree => model.browsing_working_tree(),
            Row::Ref(entry) => {
                !model.browsing_working_tree()
                    && model
                        .selected_ref()
                        .is_some_and(|selected| selected.full_name == entry.full_name)
            }
            Row::Header(_) | Row::Unreadable(_) => false,
        }
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tertiary = crate::theme::tertiary(cx);
        let theme = cx.theme();
        let row = self.rows.get(index).cloned();
        let current = row.as_ref().is_some_and(|row| self.is_current(row, cx));
        let lit = self
            .highlight
            .map_or(current, |highlight| highlight == index);
        let (glyph, label, heading): (Option<Glyph>, String, bool) = match row {
            Some(Row::WorkingTree) => (Some(Glyph::WorkingTree), WORKING_TREE.to_owned(), false),
            Some(Row::Header(title)) => (None, title.to_owned(), true),
            Some(Row::Ref(entry)) => (Some(Glyph::Ref(entry.kind)), entry.short_name, false),
            Some(Row::Unreadable(broken)) => (Some(Glyph::Warning), broken.full_name, false),
            None => (None, String::new(), false),
        };
        let mut element = div()
            .id(index)
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .px(style::MARGIN)
            .rounded(style::RADIUS);
        element = if self.picker {
            element.h(style::PANEL_ROW_HEIGHT)
        } else {
            element.py(style::HAIRLINE)
        };
        if heading {
            element = element
                .text_size(style::TEXT_CAPTION)
                .font_weight(FontWeight::MEDIUM)
                .text_color(tertiary);
        } else {
            element = element
                .text_size(style::TEXT_BODY)
                .text_color(theme.foreground)
                .hover(|style| style.bg(theme.list_hover))
                .on_click(cx.listener(move |this, _, _, cx| this.clicked(index, cx)));
            element = element.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, _| {
                    this.menu_target = match this.rows.get(index) {
                        Some(Row::Ref(entry)) => Some(entry.clone()),
                        Some(Row::WorkingTree | Row::Header(_) | Row::Unreadable(_)) | None => None,
                    };
                }),
            );
        }
        if lit {
            element = element.bg(theme.list_active);
        }
        if let Some(glyph) = glyph {
            let tint = if is_accented(glyph) {
                theme.muted_foreground
            } else {
                theme.warning
            };
            element = element.child(
                Icon::new(icon(glyph))
                    .size(style::ICON_SIZE)
                    .text_color(tint),
            );
        }
        element.child(SharedString::from(label))
    }
}

impl Render for RefListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = if self.picker {
            crate::search::field(Input::new(&self.filter).appearance(false), cx)
        } else {
            div()
                .px(style::MARGIN)
                .pt(style::BAR_PADDING)
                .child(Input::new(&self.filter).small())
        };
        let list = if self.picker {
            crate::search::body()
        } else {
            div().flex_1().min_h_0()
        };
        let root = div()
            .id("ref-list")
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.menu_target = None))
            .size_full()
            .flex()
            .flex_col()
            .when(!self.picker, |root| root.gap(style::ROW_GAP))
            .child(filter)
            .child(
                list.relative()
                    .child(
                        uniform_list(
                            "refs",
                            self.rows.len(),
                            cx.processor(|this, range: Range<usize>, _window, cx| {
                                let mut rows = Vec::with_capacity(range.len());
                                for index in range {
                                    rows.push(this.render_row(index, cx));
                                }
                                rows
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    )
                    .child(Scrollbar::vertical(&self.scroll)),
            );
        keys(root, cx, Self::key).context_menu({
            let weak = cx.entity().downgrade();
            move |menu, _, cx| {
                let target = weak
                    .upgrade()
                    .and_then(|list| list.read(cx).menu_target.clone());
                ref_context_menu(menu, target, &weak)
            }
        })
    }
}

/// The ref list's context menu for `target`, as kit's [`ref_menu`] lays it out.
fn ref_context_menu(
    mut menu: gpui_component::menu::PopupMenu,
    target: Option<RefEntry>,
    list: &gpui::WeakEntity<RefListView>,
) -> gpui_component::menu::PopupMenu {
    let Some(entry) = target else {
        return menu;
    };
    match ref_menu(&entry) {
        RefMenu::NotExact(note) => menu.item(PopupMenuItem::new(note).disabled(true)),
        RefMenu::Items(items) => {
            for (separated, action, title) in items {
                if separated {
                    menu = menu.separator();
                }
                let (list, entry) = (list.clone(), entry.clone());
                menu = menu.item(PopupMenuItem::new(title).on_click(move |_, _, cx| {
                    list.update(cx, |_, cx| {
                        cx.emit(RefListEvent::Action(action, entry.clone()));
                    })
                    .ok();
                }));
            }
            menu
        }
    }
}

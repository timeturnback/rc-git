//! The ref list (`RefsViewController`): a filter field over the Working Tree row, the refs
//! grouped by namespace in natural order, and the refs whose target could not be read. History
//! shows it in the sidebar; Files shows it in the ⌘B picker under the banner. Grouping,
//! order, matching and the Return pick all come from [`RefList`].

use std::ops::Range;

use gpui::MouseButton;
use gpui::{
    AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{ActiveTheme, Icon, Sizable};
use rc_git_core::{BrokenRef, RefEntry};
use rc_git_kit::browser::{BrowserEvent, BrowserModel};
use rc_git_kit::ref_filter::{RefList, UNREADABLE_TITLE, working_tree_matches};
use rc_git_kit::ref_menu::{RefMenu, RefMenuAction, ref_menu};

use crate::icons::{Glyph, icon, is_accented};
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
            cx.subscribe_in(
                &filter,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.reload(cx),
                    InputEvent::PressEnter { .. } => this.pick_best(cx),
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            ),
        ];
        let mut view = Self {
            model,
            filter,
            rows: Vec::new(),
            menu_target: None,
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
        cx.notify();
    }

    /// Return in the filter: the best match by [`RefList::best_match`], else the Working Tree
    /// row when it is the only one left.
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
        let theme = cx.theme();
        let row = self.rows.get(index).cloned();
        let current = row.as_ref().is_some_and(|row| self.is_current(row, cx));
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
            .py(style::HAIRLINE)
            .rounded(style::RADIUS);
        if heading {
            element = element
                .text_size(style::TEXT_CAPTION)
                .text_color(theme.muted_foreground);
        } else {
            element = element
                .text_size(style::TEXT_BODY)
                .text_color(theme.foreground)
                .hover(|style| style.bg(theme.list_hover))
                .on_click(cx.listener(move |this, _, _, cx| this.pick(index, cx)));
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
        if current {
            element = element.bg(theme.list_active);
        }
        if let Some(glyph) = glyph {
            let tint = if is_accented(glyph) {
                theme.primary
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
        div()
            .id("ref-list")
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.menu_target = None))
            .size_full()
            .flex()
            .flex_col()
            .gap(style::ROW_GAP)
            .capture_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(RefListEvent::Dismissed);
                }
            }))
            .child(
                div()
                    .px(style::MARGIN)
                    .pt(style::BAR_PADDING)
                    .child(Input::new(&self.filter).small()),
            )
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
                .flex_1(),
            )
            .context_menu({
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

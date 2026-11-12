//! The Files tree (`TreeViewController`): the gpui-component tree over kit's [`FileTree`].
//! A folder's children are read on its first expand, one tree object (or one working-tree
//! directory) per folder; hidden rows stay read and are filtered when the items are built, so
//! the ⇧⌘. toggle re-filters without a read.

use std::rc::Rc;

use gpui::{
    AppContext, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, WeakEntity, Window, div,
};
use gpui_component::list::ListItem;
use gpui_component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_component::tree::{TreeEvent, TreeItem, TreeState, tree};
use gpui_component::{ActiveTheme, Icon};
use rc_git_core::{EntryKind, TreeEntry};
use rc_git_kit::browser::{BrowserEvent, BrowserModel};
use rc_git_kit::file_tree::{ChildLoad, FileTree, SelectionFollow, ViewKind, ViewNode};
use rc_git_kit::models::BrowseRow;
use rc_git_kit::repository::ReadResult;

use crate::files::fit::{self, RowExtent, WidthCache};
use crate::icons::{Glyph, icon, is_accented};
use crate::style;

/// How a row draws: its glyph, or none for a placeholder.
#[derive(Debug, Clone, Copy)]
struct RowLook {
    glyph: Option<Glyph>,
    dimmed: bool,
}

/// What a row's context menu asks of the window.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeAction {
    /// Show File History: scope History to this path.
    ShowHistory {
        /// The node's repository path.
        path: String,
        /// Whether that path names this node alone.
        path_is_exact: bool,
    },
    /// The width that shows every row on screen in full changed.
    ContentWidth(f64),
}

/// The Files tree of one window.
pub struct FileTreeView {
    model: Entity<BrowserModel>,
    tree: FileTree,
    state: Entity<TreeState>,
    /// Per node id, how its row draws; rebuilt with the items.
    looks: Rc<std::collections::HashMap<String, RowLook>>,
    /// Which root read is current; a slower read for an older root is dropped.
    root_generation: u64,
    /// The hidden-files setting the items were last built with.
    shows_hidden: bool,
    /// The node id opened last, so re-selecting it after a rebuild does not open it again.
    opened: Option<SharedString>,
    /// The rows on screen, for the sidebar fit; re-measured when `width_stale`.
    extents: Vec<RowExtent>,
    widths: WidthCache,
    width_stale: bool,
    /// The content width last reported.
    reported: Option<f64>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TreeAction> for FileTreeView {}

impl FileTreeView {
    /// A tree following `model`'s root.
    pub fn new(model: Entity<BrowserModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| TreeState::new(cx));
        let subscriptions = vec![
            cx.subscribe_in(&model, window, |this, _, event: &BrowserEvent, _, cx| {
                if *event == BrowserEvent::RootChanged {
                    this.load_root(cx);
                }
                if *event == BrowserEvent::FileChanged {
                    this.follow_open_file(cx);
                }
            }),
            cx.observe(&model, |this, model, cx| {
                let shows = model.read(cx).shows_hidden_files();
                if shows != this.shows_hidden {
                    this.shows_hidden = shows;
                    this.rebuild(cx);
                }
            }),
            cx.subscribe(&state, |this, _, event: &TreeEvent, cx| match event {
                TreeEvent::Expanded(id) => this.expand(id, cx),
                TreeEvent::Collapsed(id) => this.tree.collapse(id),
            }),
            cx.observe(&state, |this, _, cx| this.selection_changed(cx)),
        ];
        let shows_hidden = model.read(cx).shows_hidden_files();
        let mut view = Self {
            model,
            tree: FileTree::new(),
            state,
            looks: Rc::default(),
            root_generation: 0,
            shows_hidden,
            opened: None,
            extents: Vec::new(),
            widths: WidthCache::default(),
            width_stale: false,
            reported: None,
            _subscriptions: subscriptions,
        };
        view.load_root(cx);
        view
    }

    /// The tree's nodes, for tests.
    #[cfg(test)]
    pub(crate) fn file_tree(&self) -> &FileTree {
        &self.tree
    }

    /// The gpui-component state, for tests that click or expand rows.
    #[cfg(test)]
    pub(crate) fn tree_state(&self) -> &Entity<TreeState> {
        &self.state
    }

    /// Reads the root the model browses now; an empty tree when there is none.
    fn load_root(&mut self, cx: &mut Context<Self>) {
        self.root_generation += 1;
        let generation = self.root_generation;
        self.tree.clear();
        self.opened = None;
        self.rebuild(cx);
        let Some(task) = self.read_dir(None, "", cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let rows = task.await;
            this.update(cx, |this, cx| {
                if generation != this.root_generation {
                    return;
                }
                match rows {
                    Ok(rows) => this.tree.set_root(rows),
                    Err(error) => {
                        this.model.update(cx, |_, cx| {
                            cx.emit(BrowserEvent::Error(error.message().to_owned()));
                        });
                    }
                }
                this.rebuild(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The rows of one directory: a tree object, or a working-tree directory by path when the
    /// model browses the files on disk. `tree` is `None` for the root.
    fn read_dir(
        &mut self,
        tree: Option<rc_git_core::Oid>,
        path: &str,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Task<ReadResult<Vec<BrowseRow>>>> {
        let (on_disk, root) = {
            let model = self.model.read(cx);
            (model.browsing_working_tree(), model.root_tree())
        };
        let tree = tree.or(root)?;
        if on_disk {
            return Some(
                self.model
                    .update(cx, |model, cx| model.worktree_rows(path, cx)),
            );
        }
        let listing = self.model.update(cx, |model, cx| model.children(tree, cx));
        Some(cx.background_spawn(async move {
            listing.await.map(|entries| {
                entries
                    .iter()
                    .map(|entry: &TreeEntry| BrowseRow {
                        entry: entry.clone(),
                        status: None,
                        missing: false,
                    })
                    .collect()
            })
        }))
    }

    fn expand(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        let Some(load) = self.tree.expand(id) else {
            self.rebuild(cx);
            return;
        };
        self.rebuild(cx);
        self.run_load(load, cx);
    }

    fn run_load(&mut self, load: ChildLoad, cx: &mut Context<Self>) {
        let Some(task) = self.read_dir(Some(load.tree), &load.path, cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let rows = task.await.map_err(|error| error.message().to_owned());
            this.update(cx, |this, cx| {
                if this.tree.finish(&load, rows) {
                    this.rebuild(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Rebuilds the component's items from the tree, filtered by the model's hidden-files
    /// setting, and keeps the selected row selected.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let view = self.tree.view(&|row: &BrowseRow| model.is_visible(row));
        let mut looks = std::collections::HashMap::new();
        let items = items(&view, &mut looks);
        self.looks = Rc::new(looks);
        self.extents = fit::extents(&view);
        self.width_stale = true;
        let selected = self
            .state
            .read(cx)
            .selected_item()
            .map(|item| item.id.clone());
        self.state.update(cx, |state, cx| {
            state.set_items(items, cx);
            if let Some(selected) = &selected
                && let Some(index) = state.index_of(selected)
            {
                state.set_selected_index(Some(index), cx);
            }
        });
        cx.notify();
    }

    /// Measures the rows on screen once they changed and tells the window when the width that
    /// shows them all in full moved.
    fn report_width(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.width_stale) {
            return;
        }
        let width = self.widths.widest(&self.extents, window, cx);
        if self.reported != Some(width) {
            self.reported = Some(width);
            cx.emit(TreeAction::ContentWidth(width));
        }
    }

    /// The labels shaped so far, for tests that count measuring.
    #[cfg(test)]
    pub(crate) fn shaped_labels(&self) -> usize {
        self.widths.len()
    }

    /// Moves the selection to the file the pane opened some other way (a link, a search pick,
    /// a step back), or off a file row it no longer names, by [`FileTree::follow_open_file`].
    /// The row it selects is marked open, so selecting it opens nothing again.
    fn follow_open_file(&mut self, cx: &mut Context<Self>) {
        let (path, exact) = {
            let model = self.model.read(cx);
            (
                model.opened_file_path().map(str::to_owned),
                model.opened_path_is_exact(),
            )
        };
        let selected = self
            .state
            .read(cx)
            .selected_item()
            .map(|item| item.id.to_string());
        let follow = {
            let model = self.model.read(cx);
            self.tree.follow_open_file(
                selected.as_deref(),
                path.as_deref(),
                exact,
                &|row: &BrowseRow| model.is_visible(row),
            )
        };
        match follow {
            SelectionFollow::Keep => {}
            SelectionFollow::Select(id) => {
                let id = SharedString::from(id);
                self.opened = Some(id.clone());
                self.state.update(cx, |state, cx| {
                    let index = state.index_of(&id);
                    state.set_selected_index(index, cx);
                });
            }
            SelectionFollow::Clear => {
                self.opened = None;
                self.state
                    .update(cx, |state, cx| state.set_selected_index(None, cx));
            }
        }
    }

    /// Copy Path: the node's repository path, once the model allows it (an inexact one is
    /// refused there).
    pub fn copy_path(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(info) = self.tree.info(id) else {
            return;
        };
        let text = self.model.update(cx, |model, cx| {
            model.copyable_path(&info.path, info.path_is_exact, cx)
        });
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Show File History: hands the node's path to the window, which scopes History to it.
    pub fn show_history(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(info) = self.tree.info(id) {
            cx.emit(TreeAction::ShowHistory {
                path: info.path,
                path_is_exact: info.path_is_exact,
            });
        }
    }

    /// Opens the selected row when it is a file and not the one already open.
    fn selection_changed(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self
            .state
            .read(cx)
            .selected_item()
            .map(|item| item.id.clone())
        else {
            return;
        };
        if self.opened.as_ref() == Some(&id) {
            return;
        }
        let Some(info) = self.tree.info(&id) else {
            return;
        };
        if info.row.entry.kind == EntryKind::Tree {
            return;
        }
        self.opened = Some(id);
        self.model
            .update(cx, |model, cx| {
                model.open_tree_row(&info.row, &info.path, info.path_is_exact, cx)
            })
            .detach();
    }
}

/// A row's context menu (`TreeViewController.buildMenu`): Show File History and Copy Path.
/// A placeholder row (`id` ending in `/-`) gets none.
fn row_menu(popup: PopupMenu, weak: &WeakEntity<FileTreeView>, id: &str) -> PopupMenu {
    if id.ends_with("/-") {
        return popup;
    }
    let (history, copy) = ((weak.clone(), id.to_owned()), (weak.clone(), id.to_owned()));
    popup
        .item(
            PopupMenuItem::new("Show File History").on_click(move |_, _, cx| {
                let (weak, id) = &history;
                weak.update(cx, |this, cx| this.show_history(id, cx)).ok();
            }),
        )
        .item(PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
            let (weak, id) = &copy;
            weak.update(cx, |this, cx| this.copy_path(id, cx)).ok();
        }))
}

fn items(
    nodes: &[ViewNode],
    looks: &mut std::collections::HashMap<String, RowLook>,
) -> Vec<TreeItem> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        let item = TreeItem::new(node.id.clone(), node.label.clone());
        let item = match &node.kind {
            ViewKind::Folder { expanded, children } => {
                looks.insert(
                    node.id.clone(),
                    RowLook {
                        glyph: Some(Glyph::Entry(EntryKind::Tree)),
                        dimmed: false,
                    },
                );
                item.expanded(*expanded).children(items(children, looks))
            }
            ViewKind::Leaf(row) => {
                looks.insert(
                    node.id.clone(),
                    RowLook {
                        glyph: Some(Glyph::Entry(row.entry.kind)),
                        dimmed: row.missing,
                    },
                );
                item
            }
            ViewKind::Placeholder => {
                looks.insert(
                    node.id.clone(),
                    RowLook {
                        glyph: None,
                        dimmed: true,
                    },
                );
                item.disabled(true)
            }
        };
        out.push(item);
    }
    out
}

impl Render for FileTreeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.report_width(window, cx);
        let theme = cx.theme();
        let (accent, muted, foreground) = (theme.blue, theme.muted_foreground, theme.foreground);
        if self.model.read(cx).root_tree().is_none() && self.tree.node_count() == 0 {
            let text = self.model.read(cx).tree_placeholder();
            return div()
                .size_full()
                .p(style::MARGIN)
                .text_size(style::TEXT_BODY)
                .text_color(muted)
                .child(text)
                .into_any_element();
        }
        let looks = Rc::clone(&self.looks);
        let weak = cx.weak_entity();
        tree(&self.state, move |index, entry, _selected, _window, _cx| {
            let id = entry.item().id.to_string();
            let look = looks.get(&id).copied().unwrap_or(RowLook {
                glyph: None,
                dimmed: true,
            });
            let color = if look.dimmed { muted } else { foreground };
            let depth = f32::from(u16::try_from(entry.depth()).unwrap_or(u16::MAX));
            let disclosure = if entry.is_folder() {
                let glyph = if entry.is_expanded() {
                    Glyph::Expanded
                } else {
                    Glyph::Collapsed
                };
                Icon::new(icon(glyph))
                    .size(style::ICON_SIZE)
                    .text_color(muted)
                    .into_any_element()
            } else {
                div().w(style::ICON_SIZE).flex_none().into_any_element()
            };
            let mut row = div()
                .flex()
                .items_center()
                .gap(style::ROW_GAP)
                .pl(style::FOLDER_INDENT * depth)
                .text_size(style::TEXT_BODY)
                .text_color(color)
                .child(disclosure);
            if let Some(glyph) = look.glyph {
                let tint = if is_accented(glyph) { accent } else { muted };
                row = row.child(
                    Icon::new(icon(glyph))
                        .size(style::ICON_SIZE)
                        .text_color(tint),
                );
            }
            let menu = (weak.clone(), entry.item().id.to_string());
            ListItem::new(index).px(style::MARGIN).child(
                row.id(("tree-row", index))
                    .child(entry.item().label.clone())
                    .context_menu(move |popup, _, _| row_menu(popup, &menu.0, &menu.1)),
            )
        })
        .size_full()
        .into_any_element()
    }
}

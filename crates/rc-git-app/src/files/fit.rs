//! What the Files tree measures for the sidebar fit (`TreeViewController.reportContentWidth`):
//! the width that shows every row on screen in full, so the sidebar can hug the tree. Each
//! label is shaped once and cached by its text; the rows inside a collapsed folder do not count.

use std::collections::HashMap;

use gpui::{App, Pixels, SharedString, TextRun, Window, font};
use gpui_component::ActiveTheme;
use rc_git_kit::file_tree::{ViewKind, ViewNode};

use crate::style;

/// One row on screen, as far as its width goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowExtent {
    /// How many folders deep it sits.
    pub depth: usize,
    /// Its label.
    pub label: String,
    /// Whether it draws a glyph after the disclosure column (a placeholder does not).
    pub glyph: bool,
}

/// The rows on screen in `nodes`: every node, and the children of each expanded folder.
#[must_use]
pub fn extents(nodes: &[ViewNode]) -> Vec<RowExtent> {
    let mut out = Vec::new();
    collect(nodes, 0, &mut out);
    out
}

fn collect(nodes: &[ViewNode], depth: usize, out: &mut Vec<RowExtent>) {
    for node in nodes {
        let glyph = !matches!(node.kind, ViewKind::Placeholder);
        out.push(RowExtent {
            depth,
            label: node.label.clone(),
            glyph,
        });
        if let ViewKind::Folder {
            expanded: true,
            children,
        } = &node.kind
        {
            collect(children, depth + 1, out);
        }
    }
}

/// The width a row takes with its label `label` wide: the list inset either side, the indent,
/// the disclosure column, the glyph, and the gaps between them, as the tree row draws them.
#[must_use]
pub fn row_width(extent: &RowExtent, label: Pixels) -> Pixels {
    let depth = f32::from(u16::try_from(extent.depth).unwrap_or(u16::MAX));
    let glyph = if extent.glyph {
        style::ICON_SIZE + style::ROW_GAP
    } else {
        Pixels::ZERO
    };
    style::MARGIN * 2.
        + style::FOLDER_INDENT * depth
        + style::ICON_SIZE
        + style::ROW_GAP
        + glyph
        + label
        + style::MARGIN
}

/// Label widths shaped so far, by text.
#[derive(Debug, Default)]
pub struct WidthCache {
    widths: HashMap<String, Pixels>,
}

impl WidthCache {
    /// How many labels have been shaped.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.widths.len()
    }

    /// The widest of `rows`, shaping each label not seen before in the tree's font.
    pub fn widest(&mut self, rows: &[RowExtent], window: &Window, cx: &App) -> f64 {
        let family = cx.theme().font_family.clone();
        let mut widest = Pixels::ZERO;
        for row in rows {
            let label = if let Some(width) = self.widths.get(&row.label) {
                *width
            } else {
                let width = shaped_width(&row.label, &family, window, cx);
                self.widths.insert(row.label.clone(), width);
                width
            };
            widest = widest.max(row_width(row, label));
        }
        f64::from(f32::from(widest))
    }
}

/// `label`'s width in the tree's font and size.
fn shaped_width(label: &str, family: &SharedString, window: &Window, cx: &App) -> Pixels {
    let text = SharedString::from(label.to_owned());
    let run = TextRun {
        len: text.len(),
        font: font(family.clone()),
        color: cx.theme().foreground,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text, style::TEXT_BODY, &[run], None)
        .width
}

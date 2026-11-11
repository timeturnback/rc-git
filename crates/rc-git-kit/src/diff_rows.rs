//! The rows a diff pane draws (`DiffRendering.swift`): each hunk as its `@@` header row
//! followed by one row per line, with the old and new line numbers a gutter shows. The same
//! layout [`crate::diff_text::owning_hunk`] counts on, so a click on any row maps to the hunk
//! that owns it.
//!
//! Built once per diff, in one pass over its lines, so a pane draws any row without walking
//! the hunk above it: a regenerated lockfile can be one hunk of hundreds of thousands of lines.

use rc_git_core::{DiffHunk, DiffLineKind};

/// One row of a diff pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    /// A hunk's `@@ -a,b +c,d @@` header.
    Header {
        /// The hunk's index.
        hunk: usize,
    },
    /// One line of a hunk.
    Line {
        /// The hunk's index.
        hunk: usize,
        /// The line's index inside the hunk.
        line: usize,
        /// The line's number in the old blob; `None` for an added line.
        old: Option<u32>,
        /// The line's number in the new blob; `None` for a removed line.
        new: Option<u32>,
    },
}

impl DiffRow {
    /// The hunk the row belongs to.
    #[must_use]
    pub fn hunk(&self) -> usize {
        match self {
            Self::Header { hunk } | Self::Line { hunk, .. } => *hunk,
        }
    }
}

/// Every row of `hunks`, in pane order.
#[must_use]
pub fn rows(hunks: &[DiffHunk]) -> Vec<DiffRow> {
    let total = hunks.iter().map(|hunk| 1 + hunk.lines.len()).sum();
    let mut rows = Vec::with_capacity(total);
    for (index, hunk) in hunks.iter().enumerate() {
        rows.push(DiffRow::Header { hunk: index });
        let mut old = hunk.old_start;
        let mut new = hunk.new_start;
        for (line, diff_line) in hunk.lines.iter().enumerate() {
            let (old_number, new_number) = match diff_line.kind {
                DiffLineKind::Context => (Some(old), Some(new)),
                DiffLineKind::Removed => (Some(old), None),
                DiffLineKind::Added => (None, Some(new)),
            };
            if old_number.is_some() {
                old += 1;
            }
            if new_number.is_some() {
                new += 1;
            }
            rows.push(DiffRow::Line {
                hunk: index,
                line,
                old: old_number,
                new: new_number,
            });
        }
    }
    rows
}

/// The header text of `hunk`: `@@ -a,b +c,d @@`.
#[must_use]
pub fn header(hunk: &DiffHunk) -> String {
    format!(
        "@@ -{},{} +{},{} @@",
        hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
    )
}

/// The sign a line is drawn with: a space, `+` or `-`.
#[must_use]
pub fn sign(kind: DiffLineKind) -> char {
    match kind {
        DiffLineKind::Context => ' ',
        DiffLineKind::Added => '+',
        DiffLineKind::Removed => '-',
    }
}

#[cfg(test)]
mod tests {
    use rc_git_core::{DiffLine, LineEnd};

    use super::*;
    use crate::diff_text::owning_hunk;

    fn line(kind: DiffLineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            text: text.into(),
            eol: LineEnd::Lf,
        }
    }

    fn hunks() -> Vec<DiffHunk> {
        vec![
            DiffHunk {
                old_start: 3,
                old_lines: 3,
                new_start: 3,
                new_lines: 3,
                lines: vec![
                    line(DiffLineKind::Context, "a"),
                    line(DiffLineKind::Removed, "b"),
                    line(DiffLineKind::Added, "B"),
                    line(DiffLineKind::Context, "c"),
                ],
            },
            DiffHunk {
                old_start: 0,
                old_lines: 0,
                new_start: 20,
                new_lines: 1,
                lines: vec![line(DiffLineKind::Added, "z")],
            },
        ]
    }

    #[test]
    fn each_hunk_is_its_header_then_its_numbered_lines() {
        let rows = rows(&hunks());
        assert_eq!(
            rows,
            [
                DiffRow::Header { hunk: 0 },
                DiffRow::Line {
                    hunk: 0,
                    line: 0,
                    old: Some(3),
                    new: Some(3)
                },
                DiffRow::Line {
                    hunk: 0,
                    line: 1,
                    old: Some(4),
                    new: None
                },
                DiffRow::Line {
                    hunk: 0,
                    line: 2,
                    old: None,
                    new: Some(4)
                },
                DiffRow::Line {
                    hunk: 0,
                    line: 3,
                    old: Some(5),
                    new: Some(5)
                },
                DiffRow::Header { hunk: 1 },
                DiffRow::Line {
                    hunk: 1,
                    line: 0,
                    old: None,
                    new: Some(20)
                },
            ]
        );
    }

    #[test]
    fn every_row_belongs_to_the_hunk_owning_hunk_names() {
        let hunks = hunks();
        let rows = rows(&hunks);
        for (index, row) in rows.iter().enumerate() {
            let owner = owning_hunk(index, &hunks);
            assert_eq!(owner, hunks.get(row.hunk()), "row {index}");
        }
        assert_eq!(owning_hunk(rows.len(), &hunks), None);
    }

    #[test]
    fn no_hunks_is_no_rows() {
        assert!(rows(&[]).is_empty());
    }

    #[test]
    fn a_header_and_signs_read_like_git() {
        assert_eq!(header(&hunks()[1]), "@@ -0,0 +20,1 @@");
        assert_eq!(sign(DiffLineKind::Context), ' ');
        assert_eq!(sign(DiffLineKind::Added), '+');
        assert_eq!(sign(DiffLineKind::Removed), '-');
    }
}

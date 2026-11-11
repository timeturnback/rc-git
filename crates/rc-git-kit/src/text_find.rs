//! Find in file and the file viewer's selection, as byte offsets into
//! [`crate::text_document::TextDocument::text`]: what `NSTextView`'s find bar and selection
//! did for the `AppKit` file pane.
//!
//! The viewer draws each line with the breaks git does not count replaced by spaces of the
//! same byte length, so a byte offset into the document is also an offset into what is on
//! screen, and a copy reads the original bytes.

use std::ops::Range;

/// The most matches one search reports; a query that matches more stops there, so a one-letter
/// query in a large file does not hold the UI.
pub const MATCH_LIMIT: usize = 10_000;

/// Every place `query` occurs in `text`, ignoring case, in order and not overlapping, at most
/// [`MATCH_LIMIT`]. An empty query matches nothing.
#[must_use]
pub fn find_all(text: &str, query: &str) -> Vec<Range<usize>> {
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let mut matches = Vec::new();
    if needle.is_empty() {
        return matches;
    }
    let mut start = 0;
    while start < text.len() && matches.len() < MATCH_LIMIT {
        let Some(rest) = text.get(start..) else {
            break;
        };
        match match_at(rest, &needle) {
            Some(length) => {
                matches.push(start..start + length);
                start += length;
            }
            None => start += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    matches
}

/// The byte length of the match of `needle` (already lowercased) at the start of `text`.
fn match_at(text: &str, needle: &[char]) -> Option<usize> {
    let mut wanted = needle.iter();
    let mut length = 0;
    let mut pending = wanted.next();
    for character in text.chars() {
        if pending.is_none() {
            break;
        }
        for lower in character.to_lowercase() {
            if pending != Some(&lower) {
                return None;
            }
            pending = wanted.next();
        }
        length += character.len_utf8();
    }
    pending.is_none().then_some(length)
}

/// The match to show after `current` when stepping `forward` or back, wrapping at either end;
/// the first (or last) when nothing is current. `None` when there are no matches.
#[must_use]
pub fn step(count: usize, current: Option<usize>, forward: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match (current, forward) {
        (None, true) => 0,
        (None, false) => count - 1,
        (Some(index), true) => (index + 1) % count,
        (Some(index), false) => index.checked_sub(1).unwrap_or(count - 1),
    })
}

/// The first match at or after `offset`, wrapping to the first: where a new query starts.
#[must_use]
pub fn first_from(matches: &[Range<usize>], offset: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(
        matches
            .iter()
            .position(|range| range.start >= offset)
            .unwrap_or(0),
    )
}

/// The zero-based line holding byte `offset`, by `line_starts` (sorted ascending).
#[must_use]
pub fn line_of(line_starts: &[usize], offset: usize) -> usize {
    line_starts
        .partition_point(|&start| start <= offset)
        .saturating_sub(1)
}

/// A selection: where the drag started and where it is now, as document byte offsets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Selection {
    /// Where the selection started.
    pub anchor: usize,
    /// Where it ends now; before `anchor` when the drag went backwards.
    pub head: usize,
}

impl Selection {
    /// An empty selection at `offset`.
    #[must_use]
    pub fn at(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    /// The selected bytes, start before end.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    /// Whether nothing is selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The part of the selection inside `line` (document bytes), relative to the line's start;
    /// `None` when they do not meet. A selection that runs on past the line's end covers its
    /// break, which the caller may draw as one extra cell.
    #[must_use]
    pub fn within(&self, line: &Range<usize>) -> Option<Range<usize>> {
        let range = self.range();
        if range.is_empty() || range.end < line.start || range.start > line.end {
            return None;
        }
        let start = range.start.max(line.start) - line.start;
        let end = range.end.min(line.end) - line.start;
        (start < end || range.end > line.end).then_some(start..end)
    }
}

/// The text a copy puts on the pasteboard: the selected bytes of `text`, widened to character
/// boundaries; empty for an empty selection.
#[must_use]
pub fn selected_text<'a>(text: &'a str, selection: &Selection) -> &'a str {
    let range = selection.range();
    let mut start = range.start.min(text.len());
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = range.end.min(text.len());
    while !text.is_char_boundary(end) {
        end += 1;
    }
    text.get(start..end).unwrap_or_default()
}

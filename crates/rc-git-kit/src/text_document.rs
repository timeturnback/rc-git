//! Port of `TextDocument.swift` and `FileLimits.swift`: a text blob prepared for display, the
//! gutter's line numbering, byte counts for people, and the file pane's size limits.
//!
//! Every offset here is a UTF-8 byte offset into [`TextDocument::text`], which is what the
//! renderer takes. The Swift side's UTF-16 offsets are gone.

use std::ops::Range;

/// A text blob prepared for display. Building one costs the UTF-8 decode plus one pass over
/// the text for the line offsets, so construct it off the UI thread and hand the finished value
/// to the view.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextDocument {
    /// The content, decoded as UTF-8 with invalid sequences replaced by U+FFFD.
    pub text: String,
    /// Number of lines, following the core's rule: a trailing newline does not add a line.
    pub line_count: usize,
    /// Size of the original bytes.
    pub byte_count: usize,
    /// Whether any byte sequence had to be replaced during decoding. Comes from the core's
    /// one-time UTF-8 check; nothing here re-scans the bytes.
    pub is_lossy: bool,
    /// Byte offsets into `text` where lines start, for the line-number gutter. Same rule as
    /// the core's: empty text has none, a trailing newline does not start a line. For lossy
    /// text these are offsets into the decoded text, not the stored bytes, since each U+FFFD
    /// is three bytes standing in for one to three invalid ones.
    pub line_starts: Vec<usize>,
    /// Byte offsets into `text` of the characters other than LF that a text layout would start
    /// a new row at: a CR not followed by LF, VT, FF, NEL, U+2028 and U+2029. Git does not
    /// count them as line breaks, so the view draws them as visible markers to keep its rows
    /// on these lines.
    pub non_lf_breaks: Vec<usize>,
    /// What [`TextDocument::widest_line`] answers, found in the same walk as the breaks.
    widest_line: Option<usize>,
}

impl TextDocument {
    /// A document from a text blob's bytes, the core's line starts for it, and the core's
    /// verdict on whether the bytes are valid UTF-8.
    #[must_use]
    pub fn new(data: Vec<u8>, core_line_starts: &[usize], valid_utf8: bool) -> Self {
        let byte_count = data.len();
        let text = match String::from_utf8(data) {
            Ok(text) => text,
            Err(invalid) => String::from_utf8_lossy(invalid.as_bytes()).into_owned(),
        };
        let Walk {
            line_starts,
            non_lf_breaks,
            widest_line,
        } = breaks(&text);
        Self {
            text,
            line_count: core_line_starts.len(),
            byte_count,
            is_lossy: !valid_utf8,
            line_starts,
            non_lf_breaks,
            widest_line,
        }
    }

    /// The line with the most characters, line break left out: the one a viewer that does
    /// not wrap measures to know how far it scrolls sideways. The first of equals wins;
    /// `None` for an empty document.
    #[must_use]
    pub fn widest_line(&self) -> Option<usize> {
        self.widest_line
    }

    /// The byte range in `text` of a match the core found at byte `byte_column` of zero-based
    /// `line`, `byte_length` bytes long. `None` when the line is past the end. Lossy text maps
    /// to the start of the line, empty: a replacement character shifts every later offset, so
    /// the core's bytes no longer map. A range that runs past the end of the text or splits a
    /// character is empty at the line start too.
    #[must_use]
    pub fn match_range(
        &self,
        line: usize,
        byte_column: usize,
        byte_length: usize,
    ) -> Option<Range<usize>> {
        let line_start = *self.line_starts.get(line)?;
        let empty = line_start..line_start;
        if self.is_lossy {
            return Some(empty);
        }
        let Some(start) = line_start.checked_add(byte_column) else {
            return Some(empty);
        };
        let Some(end) = start.checked_add(byte_length) else {
            return Some(empty);
        };
        if end > self.text.len()
            || !self.text.is_char_boundary(start)
            || !self.text.is_char_boundary(end)
        {
            return Some(empty);
        }
        Some(start..end)
    }
}

/// What one walk over the decoded text finds.
struct Walk {
    line_starts: Vec<usize>,
    non_lf_breaks: Vec<usize>,
    widest_line: Option<usize>,
}

/// The widest line so far, by characters with the line's trailing CRs left out.
#[derive(Default)]
struct Widest {
    /// Characters on the line being walked, its CRs so far included.
    width: usize,
    /// How many CRs end the line being walked so far.
    trailing_cr: usize,
    line: usize,
    best: Option<(usize, usize)>,
}

impl Widest {
    fn byte(&mut self, byte: u8) {
        if byte & 0xC0 != 0x80 {
            self.width += 1;
        }
        if byte == b'\r' {
            self.trailing_cr += 1;
        } else {
            self.trailing_cr = 0;
        }
    }

    fn end_line(&mut self) {
        let width = self.width - self.trailing_cr;
        if self.best.is_none_or(|(_, most)| width > most) {
            self.best = Some((self.line, width));
        }
        self.line += 1;
        self.width = 0;
        self.trailing_cr = 0;
    }
}

/// One walk over the decoded text's bytes: line starts, the non-LF row breaks, and the widest
/// line.
fn breaks(text: &str) -> Walk {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return Walk {
            line_starts: Vec::new(),
            non_lf_breaks: Vec::new(),
            widest_line: None,
        };
    }
    let mut starts = vec![0];
    let mut non_lf = Vec::new();
    let mut widest = Widest::default();
    for (index, byte) in bytes.iter().enumerate() {
        let next = bytes.get(index + 1).copied();
        if *byte == b'\n' {
            widest.end_line();
        } else {
            widest.byte(*byte);
        }
        match *byte {
            b'\n' => starts.push(index + 1),
            b'\r' if next != Some(b'\n') => non_lf.push(index),
            0x0B | 0x0C => non_lf.push(index),
            0xC2 if next == Some(0x85) => non_lf.push(index),
            0xE2 if is_line_or_paragraph_separator(bytes, index) => non_lf.push(index),
            _ => {}
        }
    }
    if starts.len() > 1 && starts.last() == Some(&bytes.len()) {
        starts.pop();
    }
    if bytes.last() != Some(&b'\n') {
        widest.end_line();
    }
    Walk {
        line_starts: starts,
        non_lf_breaks: non_lf,
        widest_line: widest.best.map(|(line, _)| line),
    }
}

/// Whether the bytes at `index` (an `E2` lead) encode U+2028 (`E2 80 A8`) or U+2029
/// (`E2 80 A9`).
fn is_line_or_paragraph_separator(bytes: &[u8], index: usize) -> bool {
    bytes.get(index + 1) == Some(&0x80)
        && bytes
            .get(index + 2)
            .is_some_and(|third| *third == 0xA8 || *third == 0xA9)
}

/// Where the file pane scrolls once the file it was asked for has painted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FileReveal {
    /// A markdown heading, named the way a `#fragment` link names it.
    Fragment(String),
    /// A content-search match: zero-based `line`, the match's first byte `byte_column` bytes
    /// into it, `byte_length` bytes long.
    Match {
        /// Zero-based line.
        line: usize,
        /// Bytes from the line start to the match.
        byte_column: usize,
        /// Length of the match in bytes.
        byte_length: usize,
    },
}

impl FileReveal {
    /// Whether the file must paint as source even when the markdown preview is preferred: a
    /// match is selected in the text, which the preview does not show.
    #[must_use]
    pub fn needs_source(&self) -> bool {
        match self {
            Self::Fragment(_) => false,
            Self::Match { .. } => true,
        }
    }
}

/// The 1-based line whose start is exactly `offset`, or `None` when the row begins anywhere
/// else, so a row the layout broke off mid-line gets no number of its own. `line_starts` is
/// sorted ascending, as [`TextDocument::line_starts`] is.
#[must_use]
pub fn line_number_label(offset: usize, line_starts: &[usize]) -> Option<usize> {
    line_starts
        .binary_search(&offset)
        .ok()
        .map(|index| index + 1)
}

/// Picks the unit and the digit count from the value as it will print, so a value that rounds
/// up to 1000 carries to the next unit and one that rounds up to 10 drops its decimal. For
/// example `1.2 MB`.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    #[expect(
        clippy::cast_precision_loss,
        reason = "a display size; precision past 2^53 bytes does not show in one decimal"
    )]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value.round() >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    let unit = UNITS[unit];
    if (value * 10.0).round() / 10.0 < 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

/// A size and the limit it is over, worded for "is `size`, over the `limit` limit". Falls back
/// to exact byte counts when the short strings would read the same.
#[must_use]
pub fn format_over_limit(size: u64, limit: u64) -> (String, String) {
    let short = (format_bytes(size), format_bytes(limit));
    if short.0 != short.1 {
        return short;
    }
    (
        format!("{} bytes", grouped(size)),
        format!("{}-byte", grouped(limit)),
    )
}

/// Digits in groups of three separated by commas, independent of the user's locale.
pub(crate) fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Files above this size are never offered for display, even with "Load anyway".
pub const HARD_LIMIT: u64 = 512 * 1024 * 1024;

/// Whether a file of `size` bytes, rejected by the default limit, may be loaded anyway.
#[must_use]
pub fn can_force_load(size: u64) -> bool {
    size <= HARD_LIMIT
}

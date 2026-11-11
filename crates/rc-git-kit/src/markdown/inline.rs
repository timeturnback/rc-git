//! Turns one block's source into styled runs. Like the block parser it scans UTF-8 bytes, and
//! it emits runs as it goes rather than building a tree: a nested construct parses its own
//! contents recursively with the surrounding style folded in, so the result is already flat.

use std::collections::HashMap;

use super::ascii::{is_digit, is_escapable, is_letter, is_scheme_shaped, is_space};
use super::{
    MAXIMUM_INLINE_DEPTH, MarkdownDocument, MarkdownLink, MarkdownRun, MarkdownStyle, emphasis,
    text,
};

/// The prefixes a bare URL starts with, matched in either case.
const URL_PREFIXES: [&[u8]; 3] = [b"https://", b"http://", b"www."];

/// The runs of `source`, with `[label]` references looked up in `references`.
pub(super) fn runs(source: &str, references: &HashMap<String, MarkdownLink>) -> Vec<MarkdownRun> {
    if source.is_empty() {
        return Vec::new();
    }
    let mut scanner = Scanner::new(source.as_bytes(), references);
    let frame = Frame {
        end: source.len(),
        style: MarkdownStyle::PLAIN,
        link: None,
        depth: 0,
    };
    scanner.parse(0, &frame);
    scanner.output
}

/// Strips the `<…>` an author may wrap a destination in.
pub(super) fn unangled(destination: &str) -> &str {
    destination
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(destination)
}

/// The text inside a `"…"`, `'…'` or `(…)` title, or `None` when it is not quoted.
pub(super) fn unquoted(title: &str) -> Option<&str> {
    if title.chars().count() < 2 {
        return None;
    }
    for (open, close) in [('"', '"'), ('\'', '\''), ('(', ')')] {
        if let Some(inner) = title
            .strip_prefix(open)
            .and_then(|rest| rest.strip_suffix(close))
        {
            return Some(inner);
        }
    }
    None
}

/// What a construct inherits from the one around it: where its range ends, the style and link
/// its runs carry, and how deep the nesting already is.
struct Frame {
    end: usize,
    style: MarkdownStyle,
    link: Option<MarkdownLink>,
    depth: usize,
}

impl Frame {
    /// The frame for a construct nested inside this one.
    fn nested(&self, end: usize, style: MarkdownStyle, link: Option<MarkdownLink>) -> Self {
        Self {
            end,
            style,
            link,
            depth: self.depth + 1,
        }
    }
}

/// The inline scanner's state: the bytes, the runs emitted so far, and the plain text
/// accumulated since the last run was closed.
struct Scanner<'a> {
    bytes: &'a [u8],
    references: &'a HashMap<String, MarkdownLink>,
    /// The start of every maximal backtick run, ascending, keyed by the run's width. A code span
    /// closes on the next run of its own width, so each lookup is one binary search.
    backtick_runs: HashMap<usize, Vec<usize>>,
    /// For each index, the first `]` at or after it that closes past the bracket depth there.
    bracket_closers: Vec<Option<usize>>,
    /// For each index, the first `)` at or after it that closes past the paren depth there.
    paren_closers: Vec<Option<usize>>,
    /// Every emphasis the block's delimiter-stack pass matched, keyed by its first opening byte.
    emphasis_pairs: HashMap<usize, emphasis::EmphasisPair>,
    output: Vec<MarkdownRun>,
    buffer: Vec<u8>,
    /// The last scan for the byte ending an autolink: where it started and where it stopped.
    autolink_scan: Option<(usize, usize)>,
}

impl<'a> Scanner<'a> {
    fn new(bytes: &'a [u8], references: &'a HashMap<String, MarkdownLink>) -> Self {
        let backtick_runs = backtick_runs(bytes);
        let spans = code_spans(bytes, &backtick_runs);
        Self {
            bytes,
            references,
            emphasis_pairs: emphasis::pairs(bytes, &spans),
            bracket_closers: closers(bytes, b'[', b']', &spans),
            paren_closers: closers(bytes, b'(', b')', &HashMap::new()),
            backtick_runs,
            output: Vec::new(),
            buffer: Vec::new(),
            autolink_scan: None,
        }
    }

    fn byte(&self, index: usize) -> u8 {
        self.bytes.get(index).copied().unwrap_or(0)
    }

    fn slice(&self, start: usize, end: usize) -> &'a [u8] {
        self.bytes.get(start..end).unwrap_or_default()
    }

    fn text(&self, start: usize, end: usize) -> String {
        String::from_utf8_lossy(self.slice(start, end)).into_owned()
    }

    /// True when `index` falls before `end` and holds `byte`.
    fn has(&self, byte: u8, index: usize, end: usize) -> bool {
        index < end && self.byte(index) == byte
    }

    /// Parses `start..frame.end`, emitting every run with the frame's style and link.
    fn parse(&mut self, start: usize, frame: &Frame) {
        let mut cursor = start;
        while cursor < frame.end {
            if let Some(next) = self.special(cursor, frame) {
                cursor = next;
            } else {
                self.buffer.push(self.byte(cursor));
                cursor += 1;
            }
        }
        self.flush(frame);
    }

    /// Tries every construct that can start at `cursor`; `None` means the byte is literal text.
    fn special(&mut self, cursor: usize, frame: &Frame) -> Option<usize> {
        match self.byte(cursor) {
            b'\\' => self.escape(cursor, frame.end),
            b'`' => Some(self.code_span(cursor, frame)),
            b'*' | b'_' | b'~' => self.emphasis(cursor, frame),
            b'[' => self.bracket(cursor, frame, false),
            b'!' if self.has(b'[', cursor + 1, frame.end) => self.bracket(cursor + 1, frame, true),
            b'<' => self.angle(cursor, frame),
            b'\n' => Some(self.line_break(cursor, frame.end)),
            byte if byte | 0x20 == b'h' || byte | 0x20 == b'w' => self.bare_url(cursor, frame),
            _other => None,
        }
    }

    fn flush(&mut self, frame: &Frame) {
        if self.buffer.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&self.buffer).into_owned();
        self.buffer.clear();
        self.output.push(MarkdownRun {
            text,
            style: frame.style,
            link: frame.link.clone(),
            is_image: false,
        });
    }

    fn escape(&mut self, cursor: usize, end: usize) -> Option<usize> {
        if cursor + 1 >= end {
            return None;
        }
        let next = self.byte(cursor + 1);
        if next != b'\n' && !is_escapable(next) {
            return None;
        }
        self.buffer.push(next);
        Some(cursor + 2)
    }

    /// A backtick run closes only on a run of exactly the same length, which is what lets
    /// ``a ` b`` hold a backtick. A run with no closer is literal as a whole, never reopened
    /// one byte later as a narrower one.
    fn code_span(&mut self, cursor: usize, frame: &Frame) -> usize {
        let mut open = cursor;
        while self.has(b'`', open, frame.end) {
            open += 1;
        }
        let width = open - cursor;
        if let Some(close) = first_run(&self.backtick_runs, width, open)
            && close + width <= frame.end
        {
            self.emit_code(open, close, frame);
            return close + width;
        }
        self.buffer.extend_from_slice(self.slice(cursor, open));
        open
    }

    fn emit_code(&mut self, start: usize, end: usize, frame: &Frame) {
        self.flush(frame);
        let mut content = self.text(start, end).replace('\n', " ");
        if content.chars().count() > 2 && content.starts_with(' ') && content.ends_with(' ') {
            content.pop();
            content.remove(0);
        }
        if content.is_empty() {
            return;
        }
        self.output.push(MarkdownRun {
            text: content,
            style: frame.style.union(MarkdownStyle::CODE),
            link: frame.link.clone(),
            is_image: false,
        });
    }

    /// Opens the emphasis the pairing pass matched at `cursor`, when its closer falls inside
    /// this frame. Any other delimiter byte is literal text.
    fn emphasis(&mut self, cursor: usize, frame: &Frame) -> Option<usize> {
        if frame.depth >= MAXIMUM_INLINE_DEPTH {
            return None;
        }
        let pair = *self.emphasis_pairs.get(&cursor)?;
        if pair.close + pair.width > frame.end {
            return None;
        }
        self.flush(frame);
        let added = emphasis_style(self.byte(cursor), pair.width);
        let nested = frame.nested(pair.close, frame.style.union(added), frame.link.clone());
        self.parse(cursor + pair.width, &nested);
        Some(pair.close + pair.width)
    }
}

fn emphasis_style(marker: u8, width: usize) -> MarkdownStyle {
    if marker == b'~' {
        return MarkdownStyle::STRIKETHROUGH;
    }
    if width == 1 {
        MarkdownStyle::EMPHASIS
    } else {
        MarkdownStyle::STRONG
    }
}

fn backtick_runs(bytes: &[u8]) -> HashMap<usize, Vec<usize>> {
    let mut runs: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes.get(cursor) != Some(&b'`') {
            cursor += 1;
            continue;
        }
        let mut stop = cursor;
        while bytes.get(stop) == Some(&b'`') {
            stop += 1;
        }
        runs.entry(stop - cursor).or_default().push(cursor);
        cursor = stop;
    }
    runs
}

/// Pairs backtick runs by exact width, left to right, skipping escaped bytes outside spans.
fn code_spans(bytes: &[u8], runs: &HashMap<usize, Vec<usize>>) -> HashMap<usize, usize> {
    let mut spans = HashMap::new();
    let mut cursor = 0;
    while let Some(&byte) = bytes.get(cursor) {
        if byte == b'\\' {
            cursor += 2;
            continue;
        }
        if byte != b'`' {
            cursor += 1;
            continue;
        }
        let mut open = cursor;
        while bytes.get(open) == Some(&b'`') {
            open += 1;
        }
        let width = open - cursor;
        let Some(close) = first_run(runs, width, open) else {
            cursor = open;
            continue;
        };
        spans.insert(cursor, close + width);
        cursor = close + width;
    }
    spans
}

/// The start of the first backtick run of exactly `width` at or after `from`.
fn first_run(runs: &HashMap<usize, Vec<usize>>, width: usize, from: usize) -> Option<usize> {
    let starts = runs.get(&width)?;
    let index = starts.partition_point(|&start| start < from);
    starts.get(index).copied()
}

/// For every index `i`, the first `close` at or after `i` that takes the depth below the depth
/// at `i`. Escaped bytes and the spans in `skipping` count as neither. Pairing once per block
/// is what keeps a run of unmatched openers from rescanning to the end.
fn closers(
    bytes: &[u8],
    open: u8,
    close: u8,
    skipping: &HashMap<usize, usize>,
) -> Vec<Option<usize>> {
    if !bytes.contains(&open) {
        return Vec::new();
    }
    let depths = depths(bytes, open, close, skipping);
    let lowest = depths.iter().copied().min().unwrap_or(0);
    let highest = depths.iter().copied().max().unwrap_or(0);
    let levels = usize::try_from(highest - lowest).unwrap_or(0) + 1;
    let mut first_at: Vec<Option<usize>> = vec![None; levels];
    let mut result = vec![None; depths.len()];
    for (index, &depth) in depths.iter().enumerate().rev() {
        let level = usize::try_from(depth - lowest).unwrap_or(0);
        if let Some(below) = level.checked_sub(1)
            && let Some(Some(after)) = first_at.get(below).copied()
            && after > 0
            && let Some(slot) = result.get_mut(index)
        {
            *slot = Some(after - 1);
        }
        if let Some(slot) = first_at.get_mut(level) {
            *slot = Some(index);
        }
    }
    result
}

/// The `open` minus `close` balance before each index, and once more after the last byte.
fn depths(bytes: &[u8], open: u8, close: u8, skipping: &HashMap<usize, usize>) -> Vec<i64> {
    let mut depths = vec![0_i64; bytes.len() + 1];
    let mut level = 0_i64;
    let mut cursor = 0;
    while let Some(&byte) = bytes.get(cursor) {
        let skip_to = skip_end(bytes, cursor, skipping);
        if skip_to > cursor {
            for slot in depths.iter_mut().take(skip_to).skip(cursor) {
                *slot = level;
            }
            cursor = skip_to;
            continue;
        }
        if let Some(slot) = depths.get_mut(cursor) {
            *slot = level;
        }
        if byte == open {
            level += 1;
        } else if byte == close {
            level -= 1;
        }
        cursor += 1;
    }
    if let Some(last) = depths.last_mut() {
        *last = level;
    }
    depths
}

/// Where the bytes that count as neither opener nor closer end: past an escape pair, past a
/// skipped span, or `cursor` itself when neither starts there.
fn skip_end(bytes: &[u8], cursor: usize, skipping: &HashMap<usize, usize>) -> usize {
    if bytes.get(cursor) == Some(&b'\\') {
        return (cursor + 2).min(bytes.len());
    }
    skipping.get(&cursor).copied().unwrap_or(cursor)
}

/// The closer `table` records for `index`, when it falls before `end`.
fn closer(table: &[Option<usize>], index: usize, end: usize) -> Option<usize> {
    table
        .get(index)
        .copied()
        .flatten()
        .filter(|&found| found < end)
}

// Links, images, and footnotes

/// An inline or reference target and the index just past it.
struct Target {
    link: MarkdownLink,
    end: usize,
}

impl Scanner<'_> {
    /// `[text](target)`, `[text][label]`, `[label]`, `![alt](src)`, and `[^note]`. `cursor` is
    /// the opening bracket; for an image the `!` before it has already been skipped.
    fn bracket(&mut self, cursor: usize, frame: &Frame, image: bool) -> Option<usize> {
        if frame.depth >= MAXIMUM_INLINE_DEPTH {
            return None;
        }
        let close = self.matching_bracket(cursor, frame.end)?;
        let inner = (cursor + 1, close);
        if !image && close - inner.0 > 1 && self.byte(inner.0) == b'^' {
            self.emit_footnote_marker(inner, frame);
            return Some(close + 1);
        }
        let target = self.destination(inner, close + 1, frame.end)?;
        self.flush(frame);
        if image {
            self.emit_image(inner, target.link, frame);
        } else {
            let nested = frame.nested(inner.1, frame.style, Some(target.link));
            self.parse(inner.0, &nested);
        }
        Some(target.end)
    }

    fn emit_footnote_marker(&mut self, inner: (usize, usize), frame: &Frame) {
        self.flush(frame);
        let label = self.text(inner.0 + 1, inner.1);
        let destination = format!("#{}", MarkdownDocument::footnote_anchor(&label));
        self.output.push(MarkdownRun {
            text: label,
            style: frame.style.union(MarkdownStyle::FOOTNOTE_MARKER),
            link: Some(MarkdownLink::new(destination)),
            is_image: false,
        });
    }

    /// An image run: its alt text, or its own source when the alt is empty. Inside a link it
    /// follows the enclosing link, so the fallback text still names the image, not the link.
    fn emit_image(&mut self, inner: (usize, usize), source: MarkdownLink, frame: &Frame) {
        let alt = self.text(inner.0, inner.1);
        let text = if alt.is_empty() {
            source.destination.clone()
        } else {
            alt
        };
        self.output.push(MarkdownRun {
            text,
            style: frame.style,
            link: Some(frame.link.clone().unwrap_or(source)),
            is_image: true,
        });
    }

    /// The target that follows a bracket, whether written inline or as a reference.
    fn destination(&self, inner: (usize, usize), after: usize, end: usize) -> Option<Target> {
        if self.has(b'(', after, end)
            && let Some(inline) = self.inline_destination(after, end)
        {
            return Some(inline);
        }
        self.reference_destination(inner, after, end)
    }

    fn inline_destination(&self, cursor: usize, end: usize) -> Option<Target> {
        let mut position = cursor + 1;
        while position < end && is_space(self.byte(position)) {
            position += 1;
        }
        let (destination, target_end) = self.destination_text(position, end)?;
        let mut tail = target_end;
        while tail < end && (is_space(self.byte(tail)) || self.byte(tail) == b'\n') {
            tail += 1;
        }
        let title_end = closer(&self.paren_closers, tail, end)?;
        let raw = self.text(tail, title_end);
        let title = unquoted(raw.trim_matches(text::is_whitespace)).map(str::to_owned);
        Some(Target {
            link: MarkdownLink { destination, title },
            end: title_end + 1,
        })
    }

    /// The destination itself: either `<anything but a newline>` or a run without whitespace.
    fn destination_text(&self, cursor: usize, end: usize) -> Option<(String, usize)> {
        if !self.has(b'<', cursor, end) {
            return Some(self.bare_destination(cursor, end));
        }
        let mut stop = cursor + 1;
        while stop < end && self.byte(stop) != b'>' && self.byte(stop) != b'\n' {
            stop += 1;
        }
        if !self.has(b'>', stop, end) {
            return None;
        }
        Some((self.text(cursor + 1, stop), stop + 1))
    }

    /// A run without whitespace whose parentheses balance; an unopened `)` ends it.
    fn bare_destination(&self, cursor: usize, end: usize) -> (String, usize) {
        let mut stop = cursor;
        let mut depth = 0_usize;
        while stop < end && !is_space(self.byte(stop)) && self.byte(stop) != b'\n' {
            if self.byte(stop) == b'(' {
                depth += 1;
            } else if self.byte(stop) == b')' {
                let Some(outer) = depth.checked_sub(1) else {
                    break;
                };
                depth = outer;
            }
            stop += 1;
        }
        (self.text(cursor, stop), stop)
    }

    fn reference_destination(
        &self,
        inner: (usize, usize),
        after: usize,
        end: usize,
    ) -> Option<Target> {
        if self.has(b'[', after, end)
            && let Some(close) = self.matching_bracket(after, end)
        {
            let label = self.text(after + 1, close);
            let key = if label.is_empty() {
                self.text(inner.0, inner.1)
            } else {
                label
            };
            let link = self.references.get(&text::lowercased(&key))?.clone();
            return Some(Target {
                link,
                end: close + 1,
            });
        }
        let key = text::lowercased(&self.text(inner.0, inner.1));
        let link = self.references.get(&key)?.clone();
        Some(Target { link, end: after })
    }

    fn matching_bracket(&self, cursor: usize, end: usize) -> Option<usize> {
        closer(&self.bracket_closers, cursor + 1, end)
    }
}

// Angle brackets, breaks, and bare URLs

impl Scanner<'_> {
    /// `<https://…>` and `<user@host>` become links; a recognised HTML tag is dropped so its
    /// markup does not show as prose; anything else stays literal.
    fn angle(&mut self, cursor: usize, frame: &Frame) -> Option<usize> {
        let stop = self.autolink_stop(cursor + 1);
        if !self.has(b'>', stop, frame.end) {
            return self.html_tag(cursor, frame.end);
        }
        let content = self.text(cursor + 1, stop);
        if frame.link.is_some() || !is_autolink(&content) {
            return self.html_tag(cursor, frame.end);
        }
        self.flush(frame);
        let destination = if content.contains('@') && !content.contains(':') {
            format!("mailto:{content}")
        } else {
            content.clone()
        };
        self.output.push(MarkdownRun {
            text: content,
            style: frame.style,
            link: Some(MarkdownLink::new(destination)),
            is_image: false,
        });
        Some(stop + 1)
    }

    /// The first `>`, newline or space at or after `from`. Every `<` before that byte stops
    /// there too, so the last scan answers them all.
    fn autolink_stop(&mut self, from: usize) -> usize {
        if let Some((start, stop)) = self.autolink_scan
            && from >= start
            && from <= stop
        {
            return stop;
        }
        let mut stop = from;
        while stop < self.bytes.len() && !b"> \n".contains(&self.byte(stop)) {
            stop += 1;
        }
        self.autolink_scan = Some((from, stop));
        stop
    }

    /// A `CommonMark` open or closing tag: the name starts with a letter, an open tag carries only
    /// well-formed attributes, and anything else returns `None` so the `<` stays literal prose.
    fn html_tag(&mut self, cursor: usize, end: usize) -> Option<usize> {
        let mut position = cursor + 1;
        let closing = self.has(b'/', position, end);
        if closing {
            position += 1;
        }
        if position >= end || !is_letter(self.byte(position)) {
            return None;
        }
        let mut name_end = position + 1;
        while name_end < end && is_tag_name_byte(self.byte(name_end)) {
            name_end += 1;
        }
        let found = if closing {
            self.closing_tag_end(name_end, end)
        } else {
            self.open_tag_end(name_end, end)
        };
        let close = found?;
        if self.slice(position, name_end).eq_ignore_ascii_case(b"br") {
            self.buffer.push(b'\n');
        }
        Some(close + 1)
    }

    /// The index of the `>` ending a closing tag: only whitespace may follow its name.
    fn closing_tag_end(&self, start: usize, end: usize) -> Option<usize> {
        let close = self.skip_tag_space(start, end);
        self.has(b'>', close, end).then_some(close)
    }

    /// The index of the `>` ending an open tag: attributes, each after whitespace, then an
    /// optional `/`.
    fn open_tag_end(&self, start: usize, end: usize) -> Option<usize> {
        let mut position = start;
        loop {
            let next = self.skip_tag_space(position, end);
            if next >= end {
                return None;
            }
            if self.byte(next) == b'>' {
                return Some(next);
            }
            if self.byte(next) == b'/' {
                return self.has(b'>', next + 1, end).then_some(next + 1);
            }
            if next <= position {
                return None;
            }
            position = self.attribute_end(next, end)?;
        }
    }

    /// One attribute: a name `[A-Za-z_:][A-Za-z0-9_.:-]*`, optionally `=` and a value.
    fn attribute_end(&self, start: usize, end: usize) -> Option<usize> {
        if !is_attribute_name_start(self.byte(start)) {
            return None;
        }
        let mut name_end = start + 1;
        while name_end < end && is_attribute_name_byte(self.byte(name_end)) {
            name_end += 1;
        }
        let equals = self.skip_tag_space(name_end, end);
        if !self.has(b'=', equals, end) {
            return Some(name_end);
        }
        self.attribute_value_end(self.skip_tag_space(equals + 1, end), end)
    }

    /// A single-quoted, double-quoted or unquoted attribute value.
    fn attribute_value_end(&self, start: usize, end: usize) -> Option<usize> {
        if start >= end {
            return None;
        }
        let quote = self.byte(start);
        if quote == b'"' || quote == b'\'' {
            let mut stop = start + 1;
            while stop < end && self.byte(stop) != quote {
                stop += 1;
            }
            return (stop < end).then_some(stop + 1);
        }
        let mut stop = start;
        while stop < end && is_unquoted_value_byte(self.byte(stop)) {
            stop += 1;
        }
        (stop > start).then_some(stop)
    }

    fn skip_tag_space(&self, start: usize, end: usize) -> usize {
        let mut position = start;
        while position < end && is_tag_space(self.byte(position)) {
            position += 1;
        }
        position
    }

    /// A newline inside a block: two trailing spaces or a trailing backslash keep the break,
    /// anything else folds into a space so the preview can wrap the paragraph itself.
    fn line_break(&mut self, cursor: usize, end: usize) -> usize {
        let mut trailing = 0;
        while self.buffer.len() > trailing
            && self.buffer.get(self.buffer.len() - 1 - trailing) == Some(&b' ')
        {
            trailing += 1;
        }
        self.buffer.truncate(self.buffer.len() - trailing);
        let emitted = !self.buffer.is_empty() || !self.output.is_empty();
        if emitted && self.buffer.last() != Some(&b'\n') {
            self.buffer.push(if trailing >= 2 { b'\n' } else { b' ' });
        }
        let mut next = cursor + 1;
        while next < end && is_space(self.byte(next)) {
            next += 1;
        }
        next
    }

    /// GFM linkifies bare URLs. Only outside an existing link, and never mid-word.
    fn bare_url(&mut self, cursor: usize, frame: &Frame) -> Option<usize> {
        let end = frame.end;
        if frame.link.is_some() || (cursor > 0 && is_letter(self.byte(cursor - 1))) {
            return None;
        }
        let prefix = URL_PREFIXES
            .into_iter()
            .find(|prefix| self.matches(prefix, cursor, end))?;
        let mut stop = cursor + prefix.len();
        while stop < end && is_url_byte(self.byte(stop)) {
            stop += 1;
        }
        while stop > cursor
            && is_trailing_punctuation(self.byte(stop - 1))
            && !self.closes_paren(stop - 1, cursor)
        {
            stop -= 1;
        }
        if stop <= cursor + prefix.len() {
            return None;
        }
        let visible = self.text(cursor, stop);
        self.flush(frame);
        let destination = if visible
            .get(..4)
            .is_some_and(|start| start.eq_ignore_ascii_case("www."))
        {
            format!("https://{visible}")
        } else {
            visible.clone()
        };
        self.output.push(MarkdownRun {
            text: visible,
            style: frame.style,
            link: Some(MarkdownLink::new(destination)),
            is_image: false,
        });
        Some(stop)
    }

    /// GFM keeps a trailing `)` that closes a `(` earlier in the URL, as in Wikipedia links.
    fn closes_paren(&self, index: usize, start: usize) -> bool {
        if self.byte(index) != b')' {
            return false;
        }
        let mut depth = 0_i64;
        for &byte in self.slice(start, index) {
            if byte == b'(' {
                depth += 1;
            }
            if byte == b')' {
                depth -= 1;
            }
        }
        depth > 0
    }

    fn matches(&self, prefix: &[u8], cursor: usize, end: usize) -> bool {
        if cursor + prefix.len() > end {
            return false;
        }
        for (offset, &expected) in prefix.iter().enumerate() {
            if self.byte(cursor + offset) | 0x20 != expected {
                return false;
            }
        }
        true
    }
}

fn is_tag_space(byte: u8) -> bool {
    is_space(byte) || byte == b'\n' || byte == b'\r'
}

fn is_tag_name_byte(byte: u8) -> bool {
    is_letter(byte) || is_digit(byte) || byte == b'-'
}

fn is_attribute_name_start(byte: u8) -> bool {
    is_letter(byte) || byte == b'_' || byte == b':'
}

fn is_attribute_name_byte(byte: u8) -> bool {
    is_attribute_name_start(byte) || is_digit(byte) || byte == b'.' || byte == b'-'
}

fn is_unquoted_value_byte(byte: u8) -> bool {
    !is_tag_space(byte) && !b"\"'=<>`".contains(&byte)
}

fn is_url_byte(byte: u8) -> bool {
    byte > b' ' && !b"<>\"|\\".contains(&byte)
}

fn is_trailing_punctuation(byte: u8) -> bool {
    b".:,;!?)]'*_~".contains(&byte)
}

/// An autolink is a scheme followed by `:`, or something shaped like an email address.
fn is_autolink(content: &str) -> bool {
    if content.is_empty() || content.contains(' ') {
        return false;
    }
    if let Some(colon) = content.find(':') {
        let scheme = &content.as_bytes()[..colon];
        return (2..=32).contains(&scheme.len()) && is_scheme_shaped(scheme);
    }
    let Some(at) = content.find('@') else {
        return false;
    };
    at != 0 && content[at..].contains('.')
}

//! Splits a markdown file into blocks. Nesting is a stack of list content indents rather than a
//! tree, which is why the result is one flat list.

use std::collections::HashMap;

use super::ascii::{is_digit, is_letter, is_space};
use super::{
    Alignment, BlockKind, MAXIMUM_LIST_DEPTH, MarkdownBlock, MarkdownLink, MarkdownListMarker,
    MarkdownRun, MarkdownTable, inline, text,
};

/// A UTF-8 byte order mark. GitHub ignores one at the start of a file, so the parser does too.
const BYTE_ORDER_MARK: &[u8] = b"\xEF\xBB\xBF";

/// Block-level tags whose line starts raw HTML. Inline tags such as `<b>` are left to the
/// inline parser so prose around them keeps its formatting.
const HTML_BLOCK_TAGS: [&str; 46] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "div",
    "dl",
    "dd",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "iframe",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "picture",
    "pre",
    "script",
    "section",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "textarea",
    "tr",
    "ul",
    "video",
];

/// Tags whose block runs to the matching closing tag, blank lines included, as `CommonMark`'s
/// first kind of HTML block does.
const RAW_HTML_TAGS: [&str; 4] = ["pre", "script", "style", "textarea"];

/// A byte range of the source: `Range<usize>` without the move semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Span {
    pub(super) start: usize,
    pub(super) end: usize,
}

impl Span {
    fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    fn is_empty(self) -> bool {
        self.start >= self.end
    }

    fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

/// A paragraph or footnote body being accumulated line by line.
struct Pending {
    lines: Vec<String>,
    kind: BlockKind,
    quote_depth: usize,
    list_depth: usize,
    marker: Option<MarkdownListMarker>,
}

/// What a list marker introduced: how it looks, and where its content begins.
struct ListItem {
    marker: MarkdownListMarker,
    content_indent: usize,
    rest: Span,
}

/// The opening or closing line of a fenced code block.
struct Fence {
    marker: u8,
    count: usize,
    info: String,
}

/// An open fence while `collect_references` skips its body.
#[derive(Clone, Copy)]
struct OpenFence {
    marker: u8,
    count: usize,
    depth: usize,
}

/// What `collect_references` tracks between lines: the open paragraph, quote and list items.
#[derive(Default)]
struct ReferenceScan {
    in_paragraph: bool,
    quote_depth: usize,
    /// Content indents of the list items open, as the main pass keeps them.
    list_indents: Vec<usize>,
}

/// The block parser's state over one file.
pub(super) struct BlockParser<'a> {
    bytes: &'a [u8],
    lines: Vec<Span>,
    references: HashMap<String, MarkdownLink>,
    blocks: Vec<MarkdownBlock>,
    index: usize,
    /// Content indents of the list items currently open, outermost first.
    list_indents: Vec<usize>,
    quote_depth: usize,
    pending: Option<Pending>,
    /// Carried by the first block of a list item and cleared there, so one bullet is drawn.
    next_marker: Option<MarkdownListMarker>,
}

impl<'a> BlockParser<'a> {
    pub(super) fn new(source: &'a [u8]) -> Self {
        let bytes = source.strip_prefix(BYTE_ORDER_MARK).unwrap_or(source);
        Self {
            bytes,
            lines: line_spans(bytes),
            references: HashMap::new(),
            blocks: Vec::new(),
            index: 0,
            list_indents: Vec::new(),
            quote_depth: 0,
            pending: None,
            next_marker: None,
        }
    }

    pub(super) fn parse(mut self) -> Vec<MarkdownBlock> {
        self.collect_references();
        self.parse_front_matter();
        while self.index < self.lines.len() {
            self.consume_line();
        }
        self.flush_pending();
        self.blocks
    }

    fn byte(&self, position: usize) -> u8 {
        self.bytes.get(position).copied().unwrap_or(0)
    }

    fn line(&self, index: usize) -> Span {
        self.lines.get(index).copied().unwrap_or(Span::new(0, 0))
    }

    fn slice(&self, span: Span) -> &'a [u8] {
        self.bytes.get(span.start..span.end).unwrap_or_default()
    }

    fn text(&self, span: Span) -> String {
        String::from_utf8_lossy(self.slice(span)).into_owned()
    }

    /// Strips up to `limit` `>` markers, returning how many were found and what is left of the
    /// line. A fence body passes its own depth, so a `>` past it stays part of the code.
    fn stripping_quotes(&self, line: Span, limit: usize) -> (usize, Span) {
        let mut cursor = line.start;
        let mut depth = 0;
        while cursor < line.end && depth < limit {
            let mut probe = cursor;
            let mut spaces = 0;
            while probe < line.end && self.byte(probe) == b' ' && spaces < 3 {
                probe += 1;
                spaces += 1;
            }
            if probe >= line.end || self.byte(probe) != b'>' {
                break;
            }
            probe += 1;
            if probe < line.end && self.byte(probe) == b' ' {
                probe += 1;
            }
            depth += 1;
            cursor = probe;
        }
        (depth, Span::new(cursor, line.end))
    }

    /// Width of the leading whitespace, counting a tab as four columns.
    fn indent_width(&self, span: Span) -> usize {
        let mut width = 0;
        for &byte in self.slice(span) {
            if byte == b' ' {
                width += 1;
            } else if byte == b'\t' {
                width += 4 - (width % 4);
            } else {
                break;
            }
        }
        width
    }

    fn trimmed_start(&self, span: Span) -> Span {
        let mut cursor = span.start;
        while cursor < span.end && is_space(self.byte(cursor)) {
            cursor += 1;
        }
        Span::new(cursor, span.end)
    }

    fn trimmed_end(&self, span: Span) -> Span {
        let mut end = span.end;
        while end > span.start && is_space(self.byte(end - 1)) {
            end -= 1;
        }
        Span::new(span.start, end)
    }

    /// The line with up to `columns` of leading indentation removed, for code block bodies.
    fn text_dropping_indent(&self, span: Span, columns: usize) -> String {
        let mut cursor = span.start;
        let mut width = 0;
        while cursor < span.end && width < columns && is_space(self.byte(cursor)) {
            width += if self.byte(cursor) == b'\t' {
                4 - (width % 4)
            } else {
                1
            };
            cursor += 1;
        }
        self.text(Span::new(cursor, span.end))
    }

    // Main loop

    fn consume_line(&mut self) {
        let (depth, rest) = self.stripping_quotes(self.line(self.index), usize::MAX);
        let body = self.trimmed_start(rest);
        if body.is_empty() {
            self.flush_pending();
            self.index += 1;
            return;
        }
        if depth < self.quote_depth && self.continues_quoted_paragraph_lazily(body) {
            let line = self.text(body);
            if let Some(pending) = self.pending.as_mut() {
                pending.lines.push(line);
            }
            self.index += 1;
            return;
        }
        if depth != self.quote_depth {
            self.flush_pending();
            self.list_indents.clear();
            self.next_marker = None;
            self.quote_depth = depth;
        }
        let indent = self.indent_width(rest);
        if self.pending.is_some() && self.continue_pending(body, indent) {
            return;
        }
        self.flush_pending();
        self.pop_lists(indent);
        self.dispatch(body, indent);
    }

    fn pending_is_paragraph(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|open| open.kind == BlockKind::Paragraph)
    }

    /// Handles a line belonging to the paragraph already open: a setext underline that retitles
    /// it, a table delimiter that turns it into a table, or one more line of prose. An underline
    /// left of the open list item's content is no part of the item, as in `CommonMark`: `---`
    /// there ends the list as a thematic break. One indented to code depth is only more text.
    fn continue_pending(&mut self, body: Span, indent: usize) -> bool {
        let list_indent = self.list_indents.last().copied().unwrap_or(0);
        if let Some(level) = self.setext_level(body)
            && self.pending_is_paragraph()
            && indent >= list_indent
            && indent < self.code_indent()
        {
            self.retitle_pending(level);
            self.index += 1;
            return true;
        }
        if indent < self.code_indent() && self.starts_block(body) {
            return false;
        }
        if self.is_table_delimiter(body)
            && let Some(header) = self.single_pending_line()
        {
            self.pending = None;
            self.index += 1;
            self.consume_table(&header, body);
            return true;
        }
        let line = self.text(body);
        if let Some(pending) = self.pending.as_mut() {
            pending.lines.push(line);
        }
        self.index += 1;
        true
    }

    /// True when a line with fewer `>` markers than the open quote is a lazy continuation of the
    /// quoted paragraph, as `CommonMark` and GitHub read a wrapped quote line written without `>`.
    /// A setext underline cannot be lazy, so `===` there is text; `---` is a rule, which
    /// `starts_block` already refuses.
    fn continues_quoted_paragraph_lazily(&self, body: Span) -> bool {
        self.pending_is_paragraph() && !self.starts_block(body) && !self.is_table_delimiter(body)
    }

    /// The indent at which a line outside a paragraph is indented code, whatever it starts with.
    fn code_indent(&self) -> usize {
        self.list_indents.last().copied().unwrap_or(0) + 4
    }

    /// Reads one line outside a paragraph. A list marker opens its item and the rest of the line
    /// goes round again, so `- - - x` nests in a loop rather than by recursion.
    fn dispatch(&mut self, body: Span, indent: usize) {
        let mut line = Some((body, indent));
        while let Some((body, indent)) = line {
            line = self.dispatch_once(body, indent);
        }
    }

    /// Reads the line's first block; returns what follows a list marker when one opened an item.
    fn dispatch_once(&mut self, body: Span, indent: usize) -> Option<(Span, usize)> {
        if indent >= self.code_indent() {
            self.consume_indented_code(self.code_indent());
            return None;
        }
        if self.dispatch_leaf(body, indent) {
            return None;
        }
        if self.list_indents.len() < MAXIMUM_LIST_DEPTH
            && let Some(item) = self.list_item(body, indent)
        {
            return self.begin_list_item(&item);
        }
        if let Some((label, rest)) = self.footnote_definition(body) {
            let first = self.text(rest);
            self.begin_pending(BlockKind::Footnote { label }, first);
            self.index += 1;
        } else if self.reference_definition(body).is_some() {
            self.index += 1;
        } else if self.is_html_block_start(body) {
            self.consume_html_block(body, indent);
        } else {
            let first = self.text(body);
            self.begin_pending(BlockKind::Paragraph, first);
            self.index += 1;
        }
        None
    }

    /// Consumes a fence, an ATX heading or a thematic break; false when the line is none.
    fn dispatch_leaf(&mut self, body: Span, indent: usize) -> bool {
        if let Some(fence) = self.fence_info(body) {
            self.consume_fence(&fence, indent);
        } else if let Some(level) = self.atx_level(body) {
            let content = self.text(self.atx_content(body, level));
            let runs = self.inline_runs(&content);
            self.emit(BlockKind::Heading { level }, runs);
            self.index += 1;
        } else if self.is_thematic_break(body) {
            self.emit(BlockKind::ThematicBreak, Vec::new());
            self.index += 1;
        } else {
            return false;
        }
        true
    }

    /// True when the line opens a block of its own, so it cannot continue a paragraph.
    fn starts_block(&self, body: Span) -> bool {
        self.fence_info(body).is_some()
            || self.atx_level(body).is_some()
            || self.is_thematic_break(body)
            || self.list_item(body, 0).is_some()
            || self.footnote_definition(body).is_some()
            || self.is_html_block_start(body)
    }

    /// Closes every list item the indent leaves, and with it the marker of an item that never
    /// emitted a block, so the marker cannot land on a block outside its list.
    fn pop_lists(&mut self, indent: usize) {
        while let Some(&last) = self.list_indents.last() {
            if indent >= last {
                break;
            }
            self.list_indents.pop();
            self.next_marker = None;
        }
    }

    // Emitting

    fn emit(&mut self, kind: BlockKind, runs: Vec<MarkdownRun>) {
        let marker = self.next_marker.take();
        self.blocks.push(MarkdownBlock {
            kind,
            runs,
            quote_depth: self.quote_depth,
            list_depth: self.list_indents.len(),
            marker,
        });
    }

    fn begin_pending(&mut self, kind: BlockKind, first: String) {
        self.pending = Some(Pending {
            lines: vec![first],
            kind,
            quote_depth: self.quote_depth,
            list_depth: self.list_indents.len(),
            marker: self.next_marker.take(),
        });
    }

    fn flush_pending(&mut self) {
        let Some(open) = self.pending.take() else {
            return;
        };
        let joined = open.lines.join("\n");
        if !text::has_visible_text(&joined) {
            return;
        }
        let runs = self.inline_runs(&joined);
        self.blocks.push(MarkdownBlock {
            kind: open.kind,
            runs,
            quote_depth: open.quote_depth,
            list_depth: open.list_depth,
            marker: open.marker,
        });
    }

    fn retitle_pending(&mut self, level: u8) {
        let Some(open) = self.pending.as_mut() else {
            return;
        };
        open.kind = BlockKind::Heading { level };
        self.flush_pending();
    }

    /// The one prose line a table header must be: a plain paragraph nothing else has joined.
    fn single_pending_line(&self) -> Option<String> {
        let open = self.pending.as_ref()?;
        if open.kind != BlockKind::Paragraph || open.lines.len() != 1 {
            return None;
        }
        open.lines.first().cloned()
    }

    fn inline_runs(&self, source: &str) -> Vec<MarkdownRun> {
        inline::runs(source, &self.references)
    }
}

/// Every line's span, without its `\n` or a `\r` before it.
fn line_spans(bytes: &[u8]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (position, &byte) in bytes.iter().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let mut end = position;
        if end > start && bytes.get(end - 1) == Some(&b'\r') {
            end -= 1;
        }
        spans.push(Span::new(start, end));
        start = position + 1;
    }
    if start < bytes.len() {
        spans.push(Span::new(start, bytes.len()));
    }
    spans
}

// Detectors

impl BlockParser<'_> {
    /// The opening or closing line of a fenced code block, with its info string.
    fn fence_info(&self, body: Span) -> Option<Fence> {
        if body.is_empty() {
            return None;
        }
        let marker = self.byte(body.start);
        if marker != b'`' && marker != b'~' {
            return None;
        }
        let mut cursor = body.start;
        while cursor < body.end && self.byte(cursor) == marker {
            cursor += 1;
        }
        let count = cursor - body.start;
        if count < 3 {
            return None;
        }
        let rest = self.trimmed_end(self.trimmed_start(Span::new(cursor, body.end)));
        if marker == b'`' && self.slice(rest).contains(&b'`') {
            return None;
        }
        Some(Fence {
            marker,
            count,
            info: self.text(rest),
        })
    }

    fn atx_level(&self, body: Span) -> Option<u8> {
        let mut cursor = body.start;
        while cursor < body.end && self.byte(cursor) == b'#' {
            cursor += 1;
        }
        let level = cursor - body.start;
        if !(1..=6).contains(&level) {
            return None;
        }
        if cursor != body.end && !is_space(self.byte(cursor)) {
            return None;
        }
        u8::try_from(level).ok()
    }

    /// The heading text, without the opening hashes or a closing run of decoration hashes.
    fn atx_content(&self, body: Span, level: u8) -> Span {
        let start = body.start + usize::from(level);
        let span = self.trimmed_end(self.trimmed_start(Span::new(start, body.end)));
        let mut end = span.end;
        while end > span.start && self.byte(end - 1) == b'#' {
            end -= 1;
        }
        if end >= span.end {
            return span;
        }
        if end != span.start && !is_space(self.byte(end - 1)) {
            return span;
        }
        self.trimmed_end(Span::new(span.start, end))
    }

    fn is_thematic_break(&self, body: Span) -> bool {
        if body.is_empty() {
            return false;
        }
        let marker = self.byte(body.start);
        if marker != b'-' && marker != b'*' && marker != b'_' {
            return false;
        }
        let mut count = 0;
        for &byte in self.slice(body) {
            if byte == marker {
                count += 1;
            } else if !is_space(byte) {
                return false;
            }
        }
        count >= 3
    }

    fn setext_level(&self, body: Span) -> Option<u8> {
        if body.is_empty() {
            return None;
        }
        let marker = self.byte(body.start);
        if marker != b'=' && marker != b'-' {
            return None;
        }
        for &byte in self.slice(self.trimmed_end(body)) {
            if byte != marker {
                return None;
            }
        }
        Some(if marker == b'=' { 1 } else { 2 })
    }

    fn list_item(&self, body: Span, indent: usize) -> Option<ListItem> {
        if body.is_empty() {
            return None;
        }
        let (mut cursor, mut marker) = self.list_marker_end(body)?;
        if cursor != body.end && !is_space(self.byte(cursor)) {
            return None;
        }
        let marker_width = cursor - body.start;
        let mut spaces = 0;
        while cursor < body.end && is_space(self.byte(cursor)) && spaces < 4 {
            cursor += 1;
            spaces += 1;
        }
        let mut rest = Span::new(cursor, body.end);
        if marker == MarkdownListMarker::Bullet
            && let Some((done, after)) = self.task_marker(rest)
        {
            marker = MarkdownListMarker::Task { done };
            rest = after;
        }
        Some(ListItem {
            marker,
            content_indent: indent + marker_width + spaces.max(1),
            rest,
        })
    }

    /// Where a `-`, `*`, `+`, `1.` or `1)` marker ends, and what kind it is.
    fn list_marker_end(&self, body: Span) -> Option<(usize, MarkdownListMarker)> {
        let first = self.byte(body.start);
        if first == b'-' || first == b'+' || first == b'*' {
            return Some((body.start + 1, MarkdownListMarker::Bullet));
        }
        if !is_digit(first) {
            return None;
        }
        let mut cursor = body.start;
        let mut value: u32 = 0;
        let mut digits = 0;
        while cursor < body.end && is_digit(self.byte(cursor)) && digits < 9 {
            value = value * 10 + u32::from(self.byte(cursor) - b'0');
            cursor += 1;
            digits += 1;
        }
        let delimiter = self.byte(cursor);
        if cursor >= body.end || (delimiter != b'.' && delimiter != b')') {
            return None;
        }
        Some((cursor + 1, MarkdownListMarker::Ordered(value)))
    }

    fn task_marker(&self, rest: Span) -> Option<(bool, Span)> {
        if rest.len() < 3 || self.byte(rest.start) != b'[' || self.byte(rest.start + 2) != b']' {
            return None;
        }
        let mark = self.byte(rest.start + 1);
        let done = if mark == b' ' {
            false
        } else if mark | 0x20 == b'x' {
            true
        } else {
            return None;
        };
        let after = rest.start + 3;
        if after != rest.end && !is_space(self.byte(after)) {
            return None;
        }
        Some((done, self.trimmed_start(Span::new(after, rest.end))))
    }

    fn footnote_definition(&self, body: Span) -> Option<(String, Span)> {
        if body.len() <= 4 || self.byte(body.start) != b'[' || self.byte(body.start + 1) != b'^' {
            return None;
        }
        let mut cursor = body.start + 2;
        while cursor < body.end && self.byte(cursor) != b']' {
            cursor += 1;
        }
        if cursor + 1 >= body.end || self.byte(cursor + 1) != b':' {
            return None;
        }
        let label = self.text(Span::new(body.start + 2, cursor));
        if label.is_empty() {
            return None;
        }
        Some((label, self.trimmed_start(Span::new(cursor + 2, body.end))))
    }

    /// `[label]: destination "title"`, the out-of-line form of a link target.
    fn reference_definition(&self, body: Span) -> Option<(String, MarkdownLink)> {
        if body.len() <= 3 || self.byte(body.start) != b'[' || self.byte(body.start + 1) == b'^' {
            return None;
        }
        let mut cursor = body.start + 1;
        while cursor < body.end && self.byte(cursor) != b']' {
            cursor += 1;
        }
        if cursor + 1 >= body.end || self.byte(cursor) != b']' || self.byte(cursor + 1) != b':' {
            return None;
        }
        let label = text::lowercased(&self.text(Span::new(body.start + 1, cursor)));
        let after = self.trimmed_start(Span::new(cursor + 2, body.end));
        if label.is_empty() || after.is_empty() {
            return None;
        }
        let mut end = after.start;
        while end < after.end && !is_space(self.byte(end)) {
            end += 1;
        }
        let destination = inline::unangled(&self.text(Span::new(after.start, end))).to_owned();
        let trailing = self.trimmed_end(self.trimmed_start(Span::new(end, after.end)));
        let title = inline::unquoted(&self.text(trailing)).map(str::to_owned);
        if !trailing.is_empty() && title.is_none() {
            return None;
        }
        Some((label, MarkdownLink { destination, title }))
    }

    fn is_html_block_start(&self, body: Span) -> bool {
        if body.len() < 2 || self.byte(body.start) != b'<' {
            return false;
        }
        let mut cursor = body.start + 1;
        let next = self.byte(cursor);
        if next == b'!' || next == b'?' {
            return true;
        }
        if next == b'/' {
            cursor += 1;
        }
        let mut end = cursor;
        while end < body.end && (is_letter(self.byte(end)) || is_digit(self.byte(end))) {
            end += 1;
        }
        if end <= cursor {
            return false;
        }
        let name = self.text(Span::new(cursor, end)).to_ascii_lowercase();
        HTML_BLOCK_TAGS.contains(&name.as_str())
    }

    /// The text that ends the HTML block `body` opens, matched lowercased anywhere in a line, or
    /// `None` when the block ends at a blank line. Comments, processing instructions, CDATA,
    /// declarations and raw-text tags may hold blank lines, so they run to their closing marker.
    fn html_block_terminator(&self, body: Span) -> Option<String> {
        let line = text::lowercased(&self.text(body));
        if line.starts_with("<!--") {
            return Some("-->".to_owned());
        }
        if line.starts_with("<?") {
            return Some("?>".to_owned());
        }
        if line.starts_with("<![cdata[") {
            return Some("]]>".to_owned());
        }
        if line.starts_with("<!") && line.as_bytes().get(2).is_some_and(|&byte| is_letter(byte)) {
            return Some(">".to_owned());
        }
        for tag in RAW_HTML_TAGS {
            let Some(after_tag) = line
                .strip_prefix('<')
                .and_then(|rest| rest.strip_prefix(tag))
            else {
                continue;
            };
            let after = after_tag.as_bytes().first().copied();
            if after.is_none_or(|byte| is_space(byte) || byte == b'>') {
                return Some(format!("</{tag}>"));
            }
        }
        None
    }

    fn is_table_delimiter(&self, body: Span) -> bool {
        let mut saw_pipe = false;
        let mut saw_dash = false;
        for &byte in self.slice(body) {
            if byte == b'|' {
                saw_pipe = true;
            } else if byte == b'-' {
                saw_dash = true;
            } else if byte != b':' && !is_space(byte) {
                return false;
            }
        }
        saw_pipe && saw_dash
    }
}

// Multi-line blocks

impl BlockParser<'_> {
    fn parse_front_matter(&mut self) {
        if self.index != 0 || self.lines.len() <= 1 {
            return;
        }
        if self.slice(self.trimmed_end(self.line(0))) != b"---" {
            return;
        }
        let mut cursor = 1;
        while cursor < self.lines.len() {
            let line = self.slice(self.trimmed_end(self.line(cursor)));
            if line == b"---" || line == b"..." {
                break;
            }
            cursor += 1;
        }
        if cursor >= self.lines.len() {
            return;
        }
        let mut body = Vec::new();
        for index in 1..cursor {
            body.push(self.text(self.line(index)));
        }
        self.blocks.push(MarkdownBlock::new(BlockKind::FrontMatter {
            text: body.join("\n"),
        }));
        self.index = cursor + 1;
    }

    /// Collects `[label]: target` definitions before the main pass, because a reference may be
    /// used above the line that defines it. Fenced and indented code is skipped so a sample
    /// cannot define one, and so is a line that continues a paragraph, which the main pass
    /// shows as text. List items are followed as the main pass follows them, so a definition
    /// indented under an item counts and code depth is measured from the item's content.
    fn collect_references(&mut self) {
        let mut fence: Option<OpenFence> = None;
        let mut scan = ReferenceScan::default();
        for index in 0..self.lines.len() {
            let line = self.line(index);
            if let Some(open) = fence
                && let Some(rest) = self.fence_body_line(line, open.depth)
            {
                if self.closes_fence(self.trimmed_start(rest), open.marker, open.count) {
                    fence = None;
                }
                continue;
            }
            fence = None;
            let (depth, rest) = self.stripping_quotes(line, usize::MAX);
            let Some(body) = self.reference_scan_body(rest, depth, &mut scan) else {
                continue;
            };
            if let Some(opening) = self.fence_info(body) {
                fence = Some(OpenFence {
                    marker: opening.marker,
                    count: opening.count,
                    depth,
                });
                scan.in_paragraph = false;
                continue;
            }
            if !scan.in_paragraph
                && let Some((label, link)) = self.reference_definition(body)
            {
                self.references.entry(label).or_insert(link);
                continue;
            }
            scan.in_paragraph = self.atx_level(body).is_none() && !self.is_thematic_break(body);
        }
    }

    /// The line's content for `collect_references`, past the list markers that open items, or
    /// `None` when nothing on it can define a reference: a blank line, indented code, or a bare
    /// marker.
    fn reference_scan_body(
        &self,
        rest: Span,
        depth: usize,
        scan: &mut ReferenceScan,
    ) -> Option<Span> {
        let body = self.trimmed_start(rest);
        if body.is_empty() {
            scan.in_paragraph = false;
            return None;
        }
        let indent = self.indent_width(rest);
        let code_indent = scan.list_indents.last().copied().unwrap_or(0) + 4;
        if scan.in_paragraph
            && depth <= scan.quote_depth
            && (indent >= code_indent || !self.starts_block(body))
        {
            return Some(body);
        }
        scan.in_paragraph = false;
        if depth != scan.quote_depth {
            scan.list_indents.clear();
            scan.quote_depth = depth;
        }
        while scan.list_indents.last().is_some_and(|&last| indent < last) {
            scan.list_indents.pop();
        }
        self.reading_list_markers(body, indent, &mut scan.list_indents)
    }

    /// Opens an item for each list marker leading `body`, as `dispatch` does, and returns what
    /// follows them; `None` when the rest is indented code or empty.
    fn reading_list_markers(
        &self,
        body: Span,
        indent: usize,
        list_indents: &mut Vec<usize>,
    ) -> Option<Span> {
        let mut body = body;
        let mut indent = indent;
        while indent < list_indents.last().copied().unwrap_or(0) + 4 {
            if self.is_thematic_break(body) || list_indents.len() >= MAXIMUM_LIST_DEPTH {
                return Some(body);
            }
            let Some(item) = self.list_item(body, indent) else {
                return Some(body);
            };
            list_indents.push(item.content_indent);
            body = self.trimmed_start(item.rest);
            indent = item.content_indent;
            if body.is_empty() {
                return None;
            }
        }
        None
    }

    /// The line with the fence's `depth` quote markers removed, or `None` when it has fewer: the
    /// quote the fence opened in has ended, and the fence with it.
    fn fence_body_line(&self, line: Span, depth: usize) -> Option<Span> {
        let (found, rest) = self.stripping_quotes(line, depth);
        (found == depth).then_some(rest)
    }

    /// True when the line closes the fence: the same marker, at least as many of it, no info.
    fn closes_fence(&self, body: Span, marker: u8, count: usize) -> bool {
        let Some(closing) = self.fence_info(body) else {
            return false;
        };
        closing.marker == marker && closing.count >= count && closing.info.is_empty()
    }

    /// Opens the item and returns the rest of its line, or `None` when the marker ends the line.
    fn begin_list_item(&mut self, item: &ListItem) -> Option<(Span, usize)> {
        self.list_indents.push(item.content_indent);
        self.next_marker = Some(item.marker);
        let rest = self.trimmed_start(item.rest);
        if rest.is_empty() {
            self.index += 1;
            return None;
        }
        Some((rest, item.content_indent))
    }

    fn consume_fence(&mut self, fence: &Fence, indent: usize) {
        let mut language = None;
        for word in fence.info.split([' ', ',']) {
            if !word.is_empty() {
                language = Some(text::lowercased(word));
                break;
            }
        }
        self.index += 1;
        let mut body = Vec::new();
        while self.index < self.lines.len() {
            let Some(rest) = self.fence_body_line(self.line(self.index), self.quote_depth) else {
                break;
            };
            if self.closes_fence(self.trimmed_start(rest), fence.marker, fence.count) {
                self.index += 1;
                break;
            }
            body.push(self.text_dropping_indent(rest, indent));
            self.index += 1;
        }
        self.emit(
            BlockKind::Code {
                language,
                text: body.join("\n"),
            },
            Vec::new(),
        );
    }

    /// Four-space code. Trailing blank lines belong to whatever follows, so they are only
    /// committed once another indented line proves the block continues.
    fn consume_indented_code(&mut self, base_indent: usize) {
        let mut body: Vec<String> = Vec::new();
        let mut blanks = 0;
        while self.index < self.lines.len() {
            let (depth, rest) = self.stripping_quotes(self.line(self.index), usize::MAX);
            if self.trimmed_start(rest).is_empty() {
                blanks += 1;
                self.index += 1;
                continue;
            }
            if depth != self.quote_depth || self.indent_width(rest) < base_indent {
                break;
            }
            for _ in 0..blanks {
                body.push(String::new());
            }
            blanks = 0;
            body.push(self.text_dropping_indent(rest, base_indent));
            self.index += 1;
        }
        self.index -= blanks;
        self.emit(
            BlockKind::Code {
                language: None,
                text: body.join("\n"),
            },
            Vec::new(),
        );
    }

    /// Raw HTML up to the line holding its closing marker, or to the next blank line for a block
    /// with none (see `html_block_terminator`). The first line is `first`, the body already read
    /// past any list marker, so an item's marker never lands in the block's text.
    fn consume_html_block(&mut self, first: Span, indent: usize) {
        let terminator = self.html_block_terminator(first);
        let first_text = self.text(first);
        let mut ended = terminator
            .as_deref()
            .is_some_and(|end| text::lowercased(&first_text).contains(end));
        let mut body = vec![first_text];
        self.index += 1;
        while self.index < self.lines.len() && !ended {
            let (_, rest) = self.stripping_quotes(self.line(self.index), usize::MAX);
            if let Some(end) = terminator.as_deref() {
                ended = text::lowercased(&self.text(rest)).contains(end);
            } else if self.trimmed_start(rest).is_empty() {
                break;
            }
            body.push(self.text_dropping_indent(rest, indent));
            self.index += 1;
        }
        self.emit(
            BlockKind::Html {
                text: body.join("\n"),
            },
            Vec::new(),
        );
    }

    /// Builds the table whose header row is `header` and whose alignments come from
    /// `delimiter`, then eats the body rows that follow.
    fn consume_table(&mut self, header: &str, delimiter: Span) {
        let mut alignments = Vec::new();
        for cell in split_cells(&self.text(delimiter)) {
            alignments.push(alignment(&cell));
        }
        let header_cells = self.cells(header, alignments.len());
        let mut rows = Vec::new();
        while self.index < self.lines.len() {
            let (depth, rest) = self.stripping_quotes(self.line(self.index), usize::MAX);
            let body = self.trimmed_start(rest);
            if depth != self.quote_depth || body.is_empty() || self.starts_block(body) {
                break;
            }
            rows.push(self.cells(&self.text(body), alignments.len()));
            self.index += 1;
        }
        let table = MarkdownTable {
            alignments,
            header: header_cells,
            rows,
        };
        self.emit(BlockKind::Table(table), Vec::new());
    }

    /// One row's cells, parsed as inline content and padded to the table's width so a renderer
    /// never has to check a row's length.
    fn cells(&self, line: &str, columns: usize) -> Vec<Vec<MarkdownRun>> {
        let mut parsed = Vec::with_capacity(columns);
        for cell in split_cells(line).into_iter().take(columns) {
            parsed.push(self.inline_runs(&cell));
        }
        parsed.resize_with(columns, Vec::new);
        parsed
    }
}

/// Splits a table row on unescaped pipes, dropping the empty cells that the optional outer
/// pipes produce.
fn split_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim_matches(text::is_whitespace);
    let mut cells = split_on_unescaped_pipes(trimmed);
    if trimmed.starts_with('|') && !cells.is_empty() {
        cells.remove(0);
    }
    if trimmed.ends_with('|') {
        cells.pop();
    }
    let mut out = Vec::with_capacity(cells.len());
    for cell in cells {
        out.push(cell.trim_matches(text::is_whitespace).to_owned());
    }
    out
}

/// Every cell between unescaped pipes, outer empty ones included. A backslash before a pipe
/// is dropped; before anything else it stays.
fn split_on_unescaped_pipes(row: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in row.chars() {
        if escaped {
            if ch != '|' {
                current.push('\\');
            }
            current.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '|' {
            cells.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    if escaped {
        current.push('\\');
    }
    cells.push(current);
    cells
}

fn alignment(cell: &str) -> Alignment {
    match (cell.starts_with(':'), cell.ends_with(':')) {
        (true, true) => Alignment::Center,
        (true, false) => Alignment::Leading,
        (false, true) => Alignment::Trailing,
        (false, false) => Alignment::Natural,
    }
}

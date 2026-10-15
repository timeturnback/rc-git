//! Tree-sitter syntax highlighting for the file pane.
//!
//! Pure: bytes in, spans out. The app applies spans after first paint, so plain text never
//! waits on a parse, and caches them by blob id, so a file is parsed once. Languages are
//! detected from the file name only; unknown names simply produce no spans.
//!
//! [`import_links`] reads the same bytes for the regions that name another module, which is
//! what lets the file pane follow an import.

mod imports;

use std::borrow::Cow;
use std::ops::{ControlFlow, Range};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

pub use imports::{ImportLink, import_links};

use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

/// The languages the highlighter knows. Each pairs a grammar with its highlight query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// Shell scripts: `.sh`, `.bash`, `.zsh`.
    Bash,
    /// C: `.c`, `.h`.
    C,
    /// JavaScript: `.js`, `.mjs`, `.cjs`, `.jsx`.
    JavaScript,
    /// JSON: `.json`.
    Json,
    /// Markdown: `.md`, `.markdown`. Fenced code blocks are highlighted in their own
    /// language when it is one the highlighter knows.
    Markdown,
    /// Python: `.py`.
    Python,
    /// Rust: `.rs`.
    Rust,
    /// Swift: `.swift`.
    Swift,
    /// TOML: `.toml`.
    Toml,
    /// TypeScript with JSX: `.tsx`. A grammar of its own, because JSX makes `<T>expr` casts
    /// and some generic arrow functions mean something else.
    Tsx,
    /// TypeScript: `.ts`, `.mts`, `.cts`.
    TypeScript,
    /// YAML: `.yml`, `.yaml`.
    Yaml,
}

impl Language {
    /// Picks a language from a file name by its extension, case-insensitively.
    #[must_use]
    pub fn from_file_name(name: &str) -> Option<Self> {
        let (_, extension) = name.rsplit_once('.')?;
        match extension.to_ascii_lowercase().as_str() {
            "sh" | "bash" | "zsh" => Some(Self::Bash),
            "c" | "h" => Some(Self::C),
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "json" => Some(Self::Json),
            "md" | "markdown" => Some(Self::Markdown),
            "py" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            "swift" => Some(Self::Swift),
            "toml" => Some(Self::Toml),
            "tsx" => Some(Self::Tsx),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "yml" | "yaml" => Some(Self::Yaml),
            _ => None,
        }
    }

    /// Stable name, matching the extension family.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::C => "c",
            Self::JavaScript => "javascript",
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Swift => "swift",
            Self::Toml => "toml",
            Self::Tsx => "tsx",
            Self::TypeScript => "typescript",
            Self::Yaml => "yaml",
        }
    }

    /// Picks a language from a fence's info string, as written after a markdown code fence.
    /// Accepts the common aliases fences use, case-insensitively. This is also how the
    /// rendered markdown preview asks for a fenced block's language, which it has as a name
    /// like `rust` rather than as a file name.
    #[must_use]
    pub fn from_fence(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "bash" | "sh" | "shell" | "zsh" => Some(Self::Bash),
            "c" | "h" => Some(Self::C),
            "javascript" | "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "json" => Some(Self::Json),
            "markdown" | "md" => Some(Self::Markdown),
            "python" | "py" => Some(Self::Python),
            "rust" | "rs" => Some(Self::Rust),
            "swift" => Some(Self::Swift),
            "toml" => Some(Self::Toml),
            "tsx" => Some(Self::Tsx),
            "typescript" | "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "yaml" | "yml" => Some(Self::Yaml),
            _ => None,
        }
    }

    /// Every language, in declaration order, so `ALL[language as usize] == language`.
    const ALL: [Self; 12] = [
        Self::Bash,
        Self::C,
        Self::JavaScript,
        Self::Json,
        Self::Markdown,
        Self::Python,
        Self::Rust,
        Self::Swift,
        Self::Toml,
        Self::Tsx,
        Self::TypeScript,
        Self::Yaml,
    ];

    /// The language's position in `ALL`. The match is exhaustive, so a new variant needs an
    /// arm, and each arm's `const` block fails the build when `ALL` does not list it at its
    /// discriminant.
    const fn index(self) -> usize {
        match self {
            Self::Bash => const { Self::position_in_all(Self::Bash) },
            Self::C => const { Self::position_in_all(Self::C) },
            Self::JavaScript => const { Self::position_in_all(Self::JavaScript) },
            Self::Json => const { Self::position_in_all(Self::Json) },
            Self::Markdown => const { Self::position_in_all(Self::Markdown) },
            Self::Python => const { Self::position_in_all(Self::Python) },
            Self::Rust => const { Self::position_in_all(Self::Rust) },
            Self::Swift => const { Self::position_in_all(Self::Swift) },
            Self::Toml => const { Self::position_in_all(Self::Toml) },
            Self::Tsx => const { Self::position_in_all(Self::Tsx) },
            Self::TypeScript => const { Self::position_in_all(Self::TypeScript) },
            Self::Yaml => const { Self::position_in_all(Self::Yaml) },
        }
    }

    /// Finds `language` in `ALL`. Evaluated only in `const` blocks, where a failed assertion
    /// is a compile error rather than a panic.
    const fn position_in_all(language: Self) -> usize {
        let mut position = 0;
        while position < Self::ALL.len() && Self::ALL[position] as usize != language as usize {
            position += 1;
        }
        assert!(
            position < Self::ALL.len(),
            "Language::ALL does not list a variant"
        );
        assert!(
            position == language as usize,
            "Language::ALL is not in declaration order"
        );
        position
    }

    fn configuration(self) -> Result<&'static HighlightConfiguration, Error> {
        static CONFIGS: [OnceLock<Loaded>; LANGUAGE_COUNT] =
            [const { OnceLock::new() }; LANGUAGE_COUNT];
        CONFIGS[self.index()]
            .get_or_init(|| build_configuration(self))
            .as_ref()
            .map_err(|detail| Error::Grammar {
                language: self,
                detail: detail.clone(),
            })
    }
}

const LANGUAGE_COUNT: usize = Language::ALL.len();

/// A grammar's configuration as first loaded, or why it failed to: the query error with its
/// row and column, or the ABI version mismatch. Cached either way, so the cause is kept.
type Loaded = Result<HighlightConfiguration, String>;

/// The markdown inline grammar, reachable only through markdown's injections. Configured with
/// the same capture names as every other grammar, so classes mean the same thing everywhere.
fn markdown_inline_configuration() -> Result<&'static HighlightConfiguration, Error> {
    static CONFIG: OnceLock<Loaded> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            load(
                tree_sitter_md::INLINE_LANGUAGE.into(),
                "markdown_inline",
                tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
            )
        })
        .as_ref()
        .map_err(|detail| Error::Grammar {
            language: Language::Markdown,
            detail: detail.clone(),
        })
}

/// Builds one grammar's configuration with the shared capture names.
fn load(grammar: tree_sitter::Language, name: &str, query: &str) -> Loaded {
    let mut config = HighlightConfiguration::new(grammar, name, query, "", "")
        .map_err(|error| format!("{name} query: {error}"))?;
    config.configure(&CAPTURE_NAMES);
    Ok(config)
}

/// What a span means, for the view to color. One flat set for every language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// `@attribute`, decorators, derives.
    Attribute,
    /// Comments, doc comments.
    Comment,
    /// Named constants and enum cases.
    Constant,
    /// Emphasized prose, `*like this*` in markdown.
    Emphasis,
    /// Function and method names.
    Function,
    /// Markdown headings.
    Heading,
    /// Language keywords.
    Keyword,
    /// Link destinations and references.
    Link,
    /// Numeric literals.
    Number,
    /// Operators.
    Operator,
    /// Object keys and struct fields.
    Property,
    /// Brackets and delimiters.
    Punctuation,
    /// String and character literals, escapes included.
    String,
    /// Strongly emphasized prose, `**like this**` in markdown.
    Strong,
    /// Type names.
    Type,
    /// Variables and parameters worth tinting.
    Variable,
}

/// Every capture name the queries are matched against, paired with the class it colors as.
/// tree-sitter-highlight matches a capture to an entry whose dot-parts all appear in the
/// capture; the entry with the most parts wins, and the earlier entry breaks ties.
/// A `None` class leaves the node plain, on purpose.
const CAPTURES: [(&str, Option<Class>); 27] = [
    ("attribute", Some(Class::Attribute)),
    ("boolean", Some(Class::Constant)),
    ("character", Some(Class::String)),
    ("comment", Some(Class::Comment)),
    ("constant", Some(Class::Constant)),
    ("constructor", Some(Class::Type)),
    ("delimiter", Some(Class::Punctuation)),
    ("embedded", None),
    ("escape", Some(Class::String)),
    ("function", Some(Class::Function)),
    ("keyword", Some(Class::Keyword)),
    ("keyword.function", Some(Class::Keyword)),
    ("label", Some(Class::Constant)),
    ("none", None),
    ("number", Some(Class::Number)),
    ("operator", Some(Class::Operator)),
    ("property", Some(Class::Property)),
    ("punctuation", Some(Class::Punctuation)),
    ("string", Some(Class::String)),
    ("text.emphasis", Some(Class::Emphasis)),
    ("text.literal", Some(Class::String)),
    ("text.reference", Some(Class::Link)),
    ("text.strong", Some(Class::Strong)),
    ("text.title", Some(Class::Heading)),
    ("text.uri", Some(Class::Link)),
    ("type", Some(Class::Type)),
    ("variable", Some(Class::Variable)),
];

/// The names half of [`CAPTURES`], in the same order, which is what `configure` takes: the
/// highlight index it hands back is a position in this array and so in `CAPTURES`.
const CAPTURE_NAMES: [&str; CAPTURES.len()] = {
    let mut names = [""; CAPTURES.len()];
    let mut index = 0;
    while index < CAPTURES.len() {
        names[index] = CAPTURES[index].0;
        index += 1;
    }
    names
};

/// One highlighted region: byte offsets into the input, half-open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// First byte of the region.
    pub start: u32,
    /// One past the last byte.
    pub end: u32,
    /// What to color it as.
    pub class: Class,
}

/// Why highlighting failed. Callers treat any failure as "show plain text".
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The grammar or its highlight query failed to load; a packaging problem, not an input one.
    #[error("highlighting {}: the grammar failed to load: {detail}", language.name())]
    Grammar {
        /// The language whose grammar failed.
        language: Language,
        /// Why: the query error with its row and column, or the grammar's ABI version
        /// against the range this tree-sitter accepts.
        detail: String,
    },
    /// The parse failed: the caller raised the cancellation flag (`Cancelled`), or the
    /// grammar's ABI was rejected when the parse started (`InvalidLanguage`).
    #[error("highlighting {}: {source}", language.name())]
    Parse {
        /// The language being highlighted.
        language: Language,
        /// The underlying cause.
        source: tree_sitter_highlight::Error,
    },
    /// The parser returned no tree while looking for import links.
    #[error("finding imports in {}: the parser returned no tree", language.name())]
    Imports {
        /// The language being read.
        language: Language,
    },
}

/// Highlights `data`, returning non-overlapping spans ordered by start. Nested captures
/// flatten to the innermost one, which is what a single-color-per-range text view needs.
/// Markdown additionally highlights its inline prose and fenced code blocks whose language
/// it knows; an unknown fence language stays plain.
///
/// `cancel` stops the work once it holds a nonzero value; the parse and the event walk poll
/// it, so another thread can abandon a highlight of a file the user has already left.
///
/// # Errors
/// [`Error::Grammar`] when the language cannot be loaded, [`Error::Parse`] when parsing
/// fails outright, wrapping `Cancelled` when `cancel` was raised. An unparseable region
/// inside an otherwise fine file is not an error; tree-sitter recovers and the region simply
/// gets no spans.
pub fn highlight(
    data: &[u8],
    language: Language,
    cancel: Option<&AtomicUsize>,
) -> Result<Vec<Span>, Error> {
    match language {
        Language::Markdown => highlight_markdown(data, Nesting { depth: 0, cancel }),
        Language::Bash
        | Language::C
        | Language::JavaScript
        | Language::Json
        | Language::Python
        | Language::Rust
        | Language::Swift
        | Language::Toml
        | Language::Tsx
        | Language::TypeScript
        | Language::Yaml => single_layer(language.configuration()?, data, language, cancel),
    }
}

/// Runs one grammar over `data` and flattens the events to innermost-wins spans.
///
/// Injections are resolved by hand in [`highlight_markdown`] instead of through
/// `tree_sitter_highlight`'s own layer merge: with injections the merged event stream
/// interleaves `HighlightStart`/`HighlightEnd` across layers in the wrong order at equal
/// offsets, which mis-attributes source runs.
fn single_layer(
    config: &HighlightConfiguration,
    data: &[u8],
    language: Language,
    cancel: Option<&AtomicUsize>,
) -> Result<Vec<Span>, Error> {
    check_cancel(cancel, language)?;
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(config, data, None, cancel, |_| None)
        .map_err(|source| Error::Parse { language, source })?;
    let mut spans = Vec::new();
    let mut stack: Vec<Option<Class>> = Vec::new();
    for event in events {
        let event = event.map_err(|source| Error::Parse { language, source })?;
        match event {
            HighlightEvent::HighlightStart(highlight) => {
                stack.push(CAPTURES[highlight.0].1);
            }
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let Some(Some(class)) = stack.last() else {
                    continue;
                };
                if start >= end {
                    continue;
                }
                spans.push(Span {
                    start: offset(start),
                    end: offset(end),
                    class: *class,
                });
            }
        }
    }
    Ok(spans)
}

/// A byte index as a span offset, saturating at `u32::MAX` for an input past 4 GiB.
pub(crate) fn offset(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// Markdown fences may nest more markdown; beyond this depth an inner markdown fence
/// stays plain instead of recursing further.
const MARKDOWN_FENCE_DEPTH: u8 = 3;

/// Where a markdown pass sits: how deep in nested markdown fences, and the caller's flag.
#[derive(Clone, Copy)]
struct Nesting<'a> {
    depth: u8,
    cancel: Option<&'a AtomicUsize>,
}

fn is_raised(cancel: Option<&AtomicUsize>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed) != 0)
}

/// Fails with `Cancelled` when the flag is already raised. tree-sitter polls the flag only
/// every so many parse steps and events, so a short input would otherwise finish anyway.
fn check_cancel(cancel: Option<&AtomicUsize>, language: Language) -> Result<(), Error> {
    if is_raised(cancel) {
        return Err(Error::Parse {
            language,
            source: tree_sitter_highlight::Error::Cancelled,
        });
    }
    Ok(())
}

/// Highlights markdown: the block grammar for structure, the inline grammar for prose inside
/// every `inline` node and table cell, and the named language for every fenced code block and
/// metadata section. Inner spans win over the block layer's, so emphasis inside a heading and
/// keywords inside a fence keep their own class.
///
/// The document is parsed once: the block spans and the injected regions both read that tree.
fn highlight_markdown(data: &[u8], nesting: Nesting) -> Result<Vec<Span>, Error> {
    let config = Language::Markdown.configuration()?;
    let tree = parse_markdown(data, nesting.cancel)?;
    let base = block_spans(config, &tree, data, nesting.cancel)?;
    let overlays = markdown_overlays(&tree, data, nesting)?;
    Ok(merge_overlays(base, overlays))
}

/// Parses `data` with the markdown block grammar, polling `cancel` while it runs.
///
/// # Errors
/// [`Error::Grammar`] when the grammar is rejected, and [`Error::Parse`] wrapping
/// `Cancelled` when the caller's flag is raised or the parser returns no tree, as
/// `tree_sitter_highlight` reports it.
fn parse_markdown(data: &[u8], cancel: Option<&AtomicUsize>) -> Result<tree_sitter::Tree, Error> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .map_err(|error| Error::Grammar {
            language: Language::Markdown,
            detail: error.to_string(),
        })?;
    check_cancel(cancel, Language::Markdown)?;
    let mut progress = |_: &tree_sitter::ParseState| {
        if is_raised(cancel) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = tree_sitter::ParseOptions::new().progress_callback(&mut progress);
    let mut read = |offset: usize, _| data.get(offset..).unwrap_or_default();
    parser
        .parse_with_options(&mut read, None, Some(options))
        .ok_or(Error::Parse {
            language: Language::Markdown,
            source: tree_sitter_highlight::Error::Cancelled,
        })
}

/// The block grammar's spans from an already parsed `tree`, flattened innermost-wins exactly
/// as [`single_layer`] flattens `tree_sitter_highlight`'s events: when several patterns
/// capture one node the last one wins, and a capture with no configured name leaves the node
/// uncolored without hiding its children. The block query has no predicates and no locals,
/// so nothing else of the highlighter's walk applies.
///
/// # Errors
/// [`Error::Parse`] wrapping `Cancelled` when the caller's flag is raised.
fn block_spans(
    config: &HighlightConfiguration,
    tree: &tree_sitter::Tree,
    data: &[u8],
    cancel: Option<&AtomicUsize>,
) -> Result<Vec<Span>, Error> {
    use tree_sitter::StreamingIterator;

    check_cancel(cancel, Language::Markdown)?;
    let highlights = capture_highlights(config.query.capture_names());
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut captures = cursor.captures(&config.query, tree.root_node(), data);
    let mut flattener = Flattener::default();
    let mut pending: Option<(tree_sitter::Node, usize)> = None;
    while let Some((found, index)) = captures.next() {
        check_cancel(cancel, Language::Markdown)?;
        let Some(capture) = found.captures().get(*index) else {
            continue;
        };
        let current = (capture.node, capture.index as usize);
        match pending.replace(current) {
            Some((node, _)) if node == capture.node => {}
            Some((node, capture_index)) => {
                flattener.open(
                    node.byte_range(),
                    highlights.get(capture_index).copied().flatten(),
                );
            }
            None => {}
        }
    }
    if let Some((node, capture_index)) = pending {
        flattener.open(
            node.byte_range(),
            highlights.get(capture_index).copied().flatten(),
        );
    }
    Ok(flattener.finish())
}

/// Each query capture name's position in [`CAPTURES`], by `HighlightConfiguration::configure`'s
/// rule, whose result the configuration keeps private: every dot-part of the entry appears in
/// the capture name, the entry with the most parts wins, and the earlier entry breaks a tie.
/// `None` is a name no entry resolves; such a capture opens no highlight at all, unlike an
/// entry whose class is `None`.
fn capture_highlights(names: &[&str]) -> Vec<Option<Highlight>> {
    let mut highlights = Vec::with_capacity(names.len());
    for name in names {
        let parts: Vec<&str> = name.split('.').collect();
        let mut best = None;
        let mut best_len = 0;
        for (index, configured) in CAPTURE_NAMES.iter().enumerate() {
            let len = configured.split('.').count();
            if len > best_len && configured.split('.').all(|part| parts.contains(&part)) {
                best = Some(Highlight(index));
                best_len = len;
            }
        }
        highlights.push(best);
    }
    highlights
}

/// Turns nested highlight ranges, opened in document order, into innermost-wins spans, the
/// way [`single_layer`] turns `HighlightStart`/`HighlightEnd`/`Source` events into them.
#[derive(Default)]
struct Flattener {
    spans: Vec<Span>,
    open: Vec<(usize, Option<Class>)>,
    at: usize,
}

impl Flattener {
    /// Closes every open range that ends at or before `range.start`, then opens `range` when
    /// its capture resolved to a [`CAPTURES`] entry.
    fn open(&mut self, range: Range<usize>, highlight: Option<Highlight>) {
        while let Some(&(end, _)) = self.open.last()
            && end <= range.start
        {
            self.advance(end);
            self.open.pop();
        }
        if let Some(highlight) = highlight {
            self.advance(range.start);
            self.open.push((
                range.end,
                CAPTURES.get(highlight.0).and_then(|entry| entry.1),
            ));
        }
    }

    /// Colors the text up to `to` in the innermost open range's class.
    fn advance(&mut self, to: usize) {
        if self.at >= to {
            return;
        }
        if let Some(&(_, Some(class))) = self.open.last() {
            self.spans.push(Span {
                start: offset(self.at),
                end: offset(to),
                class,
            });
        }
        self.at = to;
    }

    fn finish(mut self) -> Vec<Span> {
        while let Some(&(end, _)) = self.open.last() {
            self.advance(end);
            self.open.pop();
        }
        self.spans
    }
}

/// Collects spans from every injected region of a parsed markdown document. A region that
/// fails to parse simply contributes nothing; the file still shows its block-level color.
///
/// # Errors
/// [`Error::Grammar`] when a region's grammar fails to load, so a packaging problem reaches
/// the caller instead of quietly dropping the prose color. [`Error::Parse`] wrapping
/// `Cancelled` when the caller's flag is raised.
fn markdown_overlays(
    tree: &tree_sitter::Tree,
    data: &[u8],
    nesting: Nesting,
) -> Result<Vec<Span>, Error> {
    let mut overlays = Vec::new();
    let mut cursor = tree.walk();
    'walk: loop {
        let descend = overlay_node(&mut overlays, data, cursor.node(), nesting)?;
        if descend && cursor.goto_first_child() {
            continue 'walk;
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'walk;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    overlays.sort_unstable_by_key(|span| span.start);
    Ok(overlays)
}

/// Appends the spans of `node` when it is an injected region, and answers whether the walk
/// should descend into it: a region's children are already covered by its own grammar.
///
/// # Errors
/// As [`append_region`].
fn overlay_node(
    overlays: &mut Vec<Span>,
    data: &[u8],
    node: tree_sitter::Node,
    nesting: Nesting,
) -> Result<bool, Error> {
    match node.kind() {
        "inline" | "pipe_table_cell" => {
            append_region(overlays, data, node, Region::Inline, nesting)?;
        }
        "fenced_code_block" => append_fence(overlays, data, node, nesting)?,
        "minus_metadata" => {
            append_region(overlays, data, node, Region::Fence(Language::Yaml), nesting)?;
        }
        "plus_metadata" => {
            append_region(overlays, data, node, Region::Fence(Language::Toml), nesting)?;
        }
        _ => return Ok(true),
    }
    Ok(false)
}

/// What fills an injected markdown region.
#[derive(Clone, Copy)]
enum Region {
    /// Prose: emphasis, links, code spans, via the inline grammar.
    Inline,
    /// A fenced code block or metadata section in a known language.
    Fence(Language),
}

/// Reads a fence's info string and highlights its content in that language, when known.
fn append_fence(
    overlays: &mut Vec<Span>,
    data: &[u8],
    node: tree_sitter::Node,
    nesting: Nesting,
) -> Result<(), Error> {
    let mut language = None;
    let mut content = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "info_string" => {
                let mut info_cursor = child.walk();
                for part in child.named_children(&mut info_cursor) {
                    if part.kind() == "language" {
                        language = std::str::from_utf8(&data[part.byte_range()])
                            .ok()
                            .and_then(Language::from_fence);
                    }
                }
            }
            "code_fence_content" => content = Some(child),
            _ => {}
        }
    }
    let (Some(language), Some(content)) = (language, content) else {
        return Ok(());
    };
    append_region(overlays, data, content, Region::Fence(language), nesting)
}

/// The byte ranges of `node` an injected grammar reads: the node minus its direct
/// `block_continuation` children, which are the list indent or `> ` quote marker that
/// continue the enclosing block on each line. tree-sitter-highlight subtracts child ranges
/// the same way when a query does not set `injection.include-children`, which markdown's
/// does not.
fn included_ranges(node: tree_sitter::Node) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = node.start_byte();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "block_continuation" {
            continue;
        }
        if child.start_byte() > start {
            ranges.push(start..child.start_byte());
        }
        start = start.max(child.end_byte());
    }
    if node.end_byte() > start {
        ranges.push(start..node.end_byte());
    }
    ranges
}

/// One included range of a region: where it starts in the concatenated buffer the injected
/// grammar reads, and where in the document.
struct Segment {
    buffer: usize,
    document: usize,
    len: usize,
}

/// Highlights one region, its continuation markers left out, and appends its spans mapped
/// back to document offsets. A span that crosses a left-out marker splits around it.
///
/// # Errors
/// [`Error::Grammar`] when the region's grammar fails to load, and [`Error::Parse`] wrapping
/// `Cancelled` when the caller's flag is raised. A region whose parse fails otherwise
/// contributes no spans and is not an error.
fn append_region(
    overlays: &mut Vec<Span>,
    data: &[u8],
    node: tree_sitter::Node,
    region: Region,
    nesting: Nesting,
) -> Result<(), Error> {
    let mut buffer = Vec::new();
    let mut segments = Vec::new();
    for range in included_ranges(node) {
        let Some(slice) = data.get(range.clone()) else {
            return Ok(());
        };
        segments.push(Segment {
            buffer: buffer.len(),
            document: range.start,
            len: slice.len(),
        });
        buffer.extend_from_slice(slice);
    }
    let spans = highlight_region(&buffer, region, nesting)?;
    for span in spans {
        push_mapped(overlays, &segments, span);
    }
    Ok(())
}

/// Appends `span`, given in buffer offsets, as one document span per segment it overlaps.
fn push_mapped(overlays: &mut Vec<Span>, segments: &[Segment], span: Span) {
    let (start, end) = (span.start as usize, span.end as usize);
    let first = segments.partition_point(|segment| segment.buffer + segment.len <= start);
    for segment in segments.iter().skip(first) {
        if segment.buffer >= end {
            break;
        }
        let from = start.max(segment.buffer) - segment.buffer + segment.document;
        let to = end.min(segment.buffer + segment.len) - segment.buffer + segment.document;
        overlays.push(Span {
            start: offset(from),
            end: offset(to),
            class: span.class,
        });
    }
}

/// Highlights a region's concatenated text in the region's grammar, in buffer offsets.
///
/// # Errors
/// As [`append_region`]; a parse failure other than a cancel yields no spans.
fn highlight_region(buffer: &[u8], region: Region, nesting: Nesting) -> Result<Vec<Span>, Error> {
    let spans = match region {
        Region::Inline => markdown_inline_configuration()
            .and_then(|config| single_layer(config, buffer, Language::Markdown, nesting.cancel)),
        Region::Fence(Language::Markdown) => {
            if nesting.depth >= MARKDOWN_FENCE_DEPTH {
                return Ok(Vec::new());
            }
            let inner = Nesting {
                depth: nesting.depth + 1,
                cancel: nesting.cancel,
            };
            highlight_markdown(buffer, inner)
        }
        Region::Fence(language) => language
            .configuration()
            .and_then(|config| single_layer(config, buffer, language, nesting.cancel)),
    };
    match spans {
        Ok(spans) => Ok(spans),
        Err(
            error @ (Error::Grammar { .. }
            | Error::Parse {
                source: tree_sitter_highlight::Error::Cancelled,
                ..
            }),
        ) => Err(error),
        // A region tree-sitter cannot parse leaves its text plain; the rest of the file keeps
        // its color, as an unparseable stretch of any other file does.
        Err(Error::Parse { .. } | Error::Imports { .. }) => Ok(Vec::new()),
    }
}

/// Merges overlay spans into base spans, both sorted and internally non-overlapping. An
/// overlay is deeper than the base, so base spans are clipped around every overlay they touch
/// and the result stays sorted and non-overlapping.
fn merge_overlays(base: Vec<Span>, overlays: Vec<Span>) -> Vec<Span> {
    if overlays.is_empty() {
        return base;
    }
    let clipped = clip_around(&base, &overlays);
    merge_sorted(clipped, overlays)
}

/// Cuts every base span around the overlays it touches, keeping only the uncovered pieces.
/// Both inputs are sorted and internally non-overlapping, and so is the result.
fn clip_around(base: &[Span], overlays: &[Span]) -> Vec<Span> {
    let mut clipped = Vec::with_capacity(base.len());
    let mut first_relevant = 0;
    for span in base {
        first_relevant += overlays[first_relevant..].partition_point(|cut| cut.end <= span.start);
        let mut cursor = span.start;
        let mut index = first_relevant;
        while index < overlays.len() && overlays[index].start < span.end {
            let cut = overlays[index];
            if cut.start > cursor {
                clipped.push(Span {
                    start: cursor,
                    end: cut.start,
                    class: span.class,
                });
            }
            cursor = cursor.max(cut.end);
            if cut.end >= span.end {
                break;
            }
            index += 1;
        }
        if cursor < span.end {
            clipped.push(Span {
                start: cursor,
                end: span.end,
                class: span.class,
            });
        }
    }
    clipped
}

/// Merges two lists sorted by start into one; on a tie the left span comes first.
fn merge_sorted(left: Vec<Span>, right: Vec<Span>) -> Vec<Span> {
    let mut merged = Vec::with_capacity(left.len() + right.len());
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    loop {
        match (left.peek(), right.peek()) {
            (Some(a), Some(b)) => {
                if a.start <= b.start {
                    merged.extend(left.next());
                } else {
                    merged.extend(right.next());
                }
            }
            (Some(_), None) => merged.extend(left.next()),
            (None, Some(_)) => merged.extend(right.next()),
            (None, None) => break,
        }
    }
    merged
}

fn build_configuration(language: Language) -> Loaded {
    let (grammar, query): (tree_sitter::Language, Cow<'static, str>) = match language {
        Language::Bash => (
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY.into(),
        ),
        Language::C => (
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::HIGHLIGHT_QUERY.into(),
        ),
        Language::JavaScript => (
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY.into(),
        ),
        Language::Json => (
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY.into(),
        ),
        Language::Markdown => (
            tree_sitter_md::LANGUAGE.into(),
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.into(),
        ),
        Language::Python => (
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY.into(),
        ),
        Language::Rust => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY.into(),
        ),
        Language::Swift => (tree_sitter_swift::LANGUAGE.into(), swift_query().into()),
        Language::Toml => (
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY.into(),
        ),
        Language::Tsx => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            typescript_query().into(),
        ),
        Language::TypeScript => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            typescript_query().into(),
        ),
        Language::Yaml => (
            tree_sitter_yaml::LANGUAGE.into(),
            tree_sitter_yaml::HIGHLIGHTS_QUERY.into(),
        ),
    };
    load(grammar, language.name(), &query)
}

/// The TypeScript grammars extend JavaScript's, and their highlight query only covers what
/// they add, so it is read together with JavaScript's. Later patterns win on a node both
/// capture, which is why the TypeScript half goes first: JavaScript's `@constant` for
/// `MAX_SIZE` must beat TypeScript's broader "capitalized identifier is a type".
fn typescript_query() -> String {
    [
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
        tree_sitter_javascript::HIGHLIGHT_QUERY,
    ]
    .join("\n")
}

/// Swift's query tags comments `@comment @spell`. The later capture wins on a node and
/// `spell` is no color, so left in it would leave every `//` and `/* */` comment plain.
fn swift_query() -> String {
    tree_sitter_swift::HIGHLIGHTS_QUERY.replace(" @spell", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes_at(spans: &[Span], data: &[u8], needle: &str) -> Vec<Class> {
        let start = data
            .windows(needle.len())
            .position(|window| window == needle.as_bytes())
            .unwrap_or_else(|| panic!("'{needle}' not found"));
        let end = start + needle.len();
        spans
            .iter()
            .filter(|span| (span.start as usize) < end && start < span.end as usize)
            .map(|span| span.class)
            .collect()
    }

    #[test]
    fn detects_languages_from_file_names() {
        assert_eq!(Language::from_file_name("main.rs"), Some(Language::Rust));
        assert_eq!(Language::from_file_name("App.SWIFT"), Some(Language::Swift));
        assert_eq!(Language::from_file_name("a.tar.yml"), Some(Language::Yaml));
        assert_eq!(Language::from_file_name("script.sh"), Some(Language::Bash));
        assert_eq!(
            Language::from_file_name("README.md"),
            Some(Language::Markdown)
        );
        assert_eq!(
            Language::from_file_name("notes.MARKDOWN"),
            Some(Language::Markdown)
        );
        assert_eq!(
            Language::from_file_name("api.ts"),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::from_file_name("types.d.ts"),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::from_file_name("vite.config.mts"),
            Some(Language::TypeScript)
        );
        assert_eq!(Language::from_file_name("App.tsx"), Some(Language::Tsx));
        assert_eq!(Language::from_fence("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_fence("TSX"), Some(Language::Tsx));
        assert_eq!(Language::from_fence("rust"), Some(Language::Rust));
        assert_eq!(Language::from_fence("SH"), Some(Language::Bash));
        assert_eq!(Language::from_fence("mermaid"), None);
        assert_eq!(Language::from_fence("h"), Some(Language::C));
        assert_eq!(Language::from_fence("mjs"), Some(Language::JavaScript));
        assert_eq!(Language::from_fence("cts"), Some(Language::TypeScript));
        assert_eq!(Language::from_file_name("noext"), None);
        for name in ["c", "sh", "json", "rs", "md"] {
            assert_eq!(Language::from_file_name(name), None, "{name}");
        }
        assert_eq!(Language::from_file_name("photo.png"), None);
        assert_eq!(Language::from_file_name(""), None);
    }

    #[test]
    fn rust_keywords_strings_and_comments_are_classified() {
        let data = b"// note\nfn main() {\n    let x = \"hi\";\n}\n";
        let spans = highlight(data, Language::Rust, None).unwrap();
        assert_ne!(spans.len(), 0);
        assert!(classes_at(&spans, data, "// note").contains(&Class::Comment));
        assert!(classes_at(&spans, data, "fn").contains(&Class::Keyword));
        assert!(classes_at(&spans, data, "main").contains(&Class::Function));
        assert!(classes_at(&spans, data, "\"hi\"").contains(&Class::String));
        for window in spans.windows(2) {
            assert!(window[0].start <= window[1].start, "spans are ordered");
        }
    }

    /// tree-sitter-highlight's `configure` rule: a name resolves to a configured name whose
    /// dot-separated parts all appear among its own.
    fn resolves(name: &str) -> bool {
        let parts: Vec<&str> = name.split('.').collect();
        CAPTURE_NAMES
            .iter()
            .any(|configured| configured.split('.').all(|part| parts.contains(&part)))
    }

    #[test]
    fn every_query_capture_name_resolves() {
        let mut configs = Vec::new();
        for language in Language::ALL {
            let config = language
                .configuration()
                .unwrap_or_else(|err| panic!("{}: {err}", language.name()));
            configs.push((language.name(), config));
        }
        let inline =
            markdown_inline_configuration().unwrap_or_else(|err| panic!("markdown_inline: {err}"));
        configs.push(("markdown_inline", inline));
        for (language, config) in configs {
            for name in config.names() {
                assert!(
                    name.starts_with('_') || resolves(name),
                    "{language} captures @{name}, which no CAPTURES entry resolves"
                );
            }
        }
    }

    #[test]
    fn swift_func_is_keyword_and_booleans_are_constants() {
        let swift = b"func f() {}\nlet x = true\n";
        let spans = highlight(swift, Language::Swift, None).unwrap();
        assert_eq!(classes_at(&spans, swift, "func"), vec![Class::Keyword]);
        assert_eq!(classes_at(&spans, swift, "true"), vec![Class::Constant]);

        let yaml = b"a: true\n";
        let spans = highlight(yaml, Language::Yaml, None).unwrap();
        assert_eq!(classes_at(&spans, yaml, "true"), vec![Class::Constant]);

        let toml = b"a = true\n";
        let spans = highlight(toml, Language::Toml, None).unwrap();
        assert_eq!(classes_at(&spans, toml, "true"), vec![Class::Constant]);
    }

    #[test]
    fn capture_names_pair_with_their_classes() {
        assert_eq!(CAPTURE_NAMES.len(), CAPTURES.len());
        for (name, (paired, _)) in CAPTURE_NAMES.iter().zip(CAPTURES.iter()) {
            assert_eq!(name, paired);
        }
        let class_of = |name: &str| {
            CAPTURES
                .iter()
                .find(|(configured, _)| *configured == name)
                .map_or_else(|| panic!("{name} is not configured"), |(_, class)| *class)
        };
        assert_eq!(class_of("escape"), Some(Class::String));
        assert_eq!(class_of("text.uri"), Some(Class::Link));
        assert_eq!(class_of("variable"), Some(Class::Variable));
        assert_eq!(class_of("delimiter"), Some(Class::Punctuation));
        assert_eq!(class_of("character"), Some(Class::String));
    }

    #[test]
    fn swift_line_block_and_doc_comments_are_classified() {
        let data = b"// line\n/* block */\n/// doc\nlet x = 1\n";
        let spans = highlight(data, Language::Swift, None).unwrap();
        assert_eq!(classes_at(&spans, data, "// line"), vec![Class::Comment]);
        assert_eq!(
            classes_at(&spans, data, "/* block */"),
            vec![Class::Comment]
        );
        assert_eq!(classes_at(&spans, data, "/// doc"), vec![Class::Comment]);
    }

    #[test]
    fn c_delimiters_and_swift_wildcards_are_classified() {
        let c = b"int x = s.y;\n";
        let spans = highlight(c, Language::C, None).unwrap();
        assert_eq!(classes_at(&spans, c, ";"), vec![Class::Punctuation]);
        assert_eq!(classes_at(&spans, c, "."), vec![Class::Punctuation]);

        let swift = b"let _ = f()\n";
        let spans = highlight(swift, Language::Swift, None).unwrap();
        assert_eq!(classes_at(&spans, swift, "_"), vec![Class::String]);
    }

    #[test]
    fn every_language_grammar_loads_and_highlights() {
        for language in Language::ALL {
            let data: &[u8] = match language {
                Language::Bash => b"echo \"hi\" # done\n",
                Language::C => b"int main(void) { return 0; }\n",
                Language::JavaScript => b"const x = 1; // c\n",
                Language::Json => b"{\"key\": [1, true]}\n",
                Language::Markdown => b"# Title\n\nSome *prose*.\n",
                Language::Python => b"def f():\n    return 'x'\n",
                Language::Rust => b"pub fn f() -> u32 { 1 }\n",
                Language::Swift => b"func f() -> Int { return 1 }\n",
                Language::Toml => b"[table]\nkey = \"value\"\n",
                Language::Tsx => b"const a = <Tag prop={1} />;\n",
                Language::TypeScript => b"const x: number = 1; // c\n",
                Language::Yaml => b"key: value\nlist:\n  - 1\n",
            };
            let spans = highlight(data, language, None)
                .unwrap_or_else(|err| panic!("{}: {err}", language.name()));
            assert_ne!(spans.len(), 0, "{} produced no spans", language.name());
        }
    }

    #[test]
    fn typescript_types_keywords_and_constants_are_classified() {
        let data = b"interface Shape { readonly side: number }\nconst MAX_SIZE = 4;\nexport function area(shape: Shape): number { return shape.side ** 2; }\n";
        let spans = highlight(data, Language::TypeScript, None).unwrap();
        assert!(classes_at(&spans, data, "interface").contains(&Class::Keyword));
        assert!(classes_at(&spans, data, "readonly").contains(&Class::Keyword));
        assert!(classes_at(&spans, data, "Shape {").contains(&Class::Type));
        assert!(classes_at(&spans, data, "number }").contains(&Class::Type));
        assert!(classes_at(&spans, data, "area").contains(&Class::Function));
        assert_eq!(
            classes_at(&spans, data, "MAX_SIZE"),
            vec![Class::Constant],
            "an all-caps name is a constant, not a type"
        );
    }

    #[test]
    fn typescript_casts_parse_in_ts_and_jsx_parses_in_tsx() {
        let cast = b"const n = <number>value;\nconst f = <T>(x: T) => x;\n";
        let spans = highlight(cast, Language::TypeScript, None).unwrap();
        assert!(classes_at(&spans, cast, "number").contains(&Class::Type));

        let jsx = b"const view = <Panel title=\"a\">{count}</Panel>;\n";
        let spans = highlight(jsx, Language::Tsx, None).unwrap();
        assert!(classes_at(&spans, jsx, "Panel title").contains(&Class::Type));
        assert!(classes_at(&spans, jsx, "\"a\"").contains(&Class::String));
        assert!(classes_at(&spans, jsx, "title").contains(&Class::Property));
    }

    #[test]
    fn markdown_fences_highlight_typescript() {
        let data = b"```ts\nconst x: number = 1;\n```\n";
        let spans = highlight(data, Language::Markdown, None).unwrap();
        assert!(classes_at(&spans, data, "const").contains(&Class::Keyword));
        assert!(classes_at(&spans, data, "number").contains(&Class::Type));
    }

    #[test]
    fn the_imports_error_names_the_operation_and_the_language() {
        let error = Error::Imports {
            language: Language::Tsx,
        };
        assert_eq!(
            error.to_string(),
            "finding imports in tsx: the parser returned no tree"
        );
    }

    #[test]
    fn the_grammar_error_carries_its_cause() {
        let error = Error::Grammar {
            language: Language::Swift,
            detail: "swift query: Query error at 3:7. Invalid node type foo".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "highlighting swift: the grammar failed to load: \
             swift query: Query error at 3:7. Invalid node type foo"
        );
    }

    #[test]
    fn a_broken_query_names_its_row_and_column() {
        let query = "(identifier) @variable\n(no_such_node) @keyword\n";
        let Err(detail) = load(tree_sitter_rust::LANGUAGE.into(), "rust", query) else {
            panic!("a query naming an unknown node type loaded");
        };
        assert!(
            detail.starts_with("rust query: Query error at 2:2."),
            "{detail}"
        );
        assert!(detail.contains("no_such_node"), "{detail}");
    }

    #[test]
    fn markdown_prose_is_classified() {
        let data = b"# Title\n\nSome *soft* and **hard** emphasis, a `span`,\nand a [link](https://example.com).\n";
        let spans = highlight(data, Language::Markdown, None).unwrap();
        assert!(classes_at(&spans, data, "Title").contains(&Class::Heading));
        assert!(classes_at(&spans, data, "soft").contains(&Class::Emphasis));
        assert!(classes_at(&spans, data, "hard").contains(&Class::Strong));
        assert!(classes_at(&spans, data, "`span`").contains(&Class::String));
        assert!(classes_at(&spans, data, "https://example.com").contains(&Class::Link));
    }

    #[test]
    fn markdown_table_cells_highlight_inline_prose() {
        let data =
            b"| name | note |\n|---|---|\n| *em* | `cell` |\n| x | [link](https://example.com) |\n";
        let spans = highlight(data, Language::Markdown, None).unwrap();
        assert!(classes_at(&spans, data, "em*").contains(&Class::Emphasis));
        assert!(classes_at(&spans, data, "`cell`").contains(&Class::String));
        assert!(classes_at(&spans, data, "https://example.com").contains(&Class::Link));
    }

    #[test]
    fn markdown_code_fences_highlight_in_their_own_language() {
        let data = b"# Doc\n\n```rust\nfn fenced() -> u32 { 1 }\n```\n\n```nosuchlang\nfn ignored() {}\n```\n";
        let spans = highlight(data, Language::Markdown, None).unwrap();
        assert!(classes_at(&spans, data, "fn fenced").contains(&Class::Keyword));
        assert!(classes_at(&spans, data, "fenced").contains(&Class::Function));
        assert!(
            !classes_at(&spans, data, "ignored").contains(&Class::Function),
            "an unknown fence language stays plain"
        );
    }

    #[test]
    fn markdown_fences_nested_past_the_cap_stay_plain() {
        let mut doc = String::from("# L0\n\n");
        for (level, run) in (1..=5).zip((4..=8).rev()) {
            doc.push_str(&"`".repeat(run));
            doc.push_str("md\n# L");
            doc.push_str(&level.to_string());
            doc.push_str("\n\n");
        }
        doc.push_str("```rust\nfn innermost() {}\n```\n");
        for run in 4..=8 {
            doc.push_str(&"`".repeat(run));
            doc.push('\n');
        }
        let data = doc.as_bytes();
        let spans = highlight(data, Language::Markdown, None).unwrap();
        for level in 0..=3 {
            let heading = format!("L{level}");
            assert!(
                classes_at(&spans, data, &heading).contains(&Class::Heading),
                "{heading} sits within the depth cap and highlights"
            );
        }
        for level in 4..=5 {
            let heading = format!("L{level}");
            assert_eq!(
                classes_at(&spans, data, &heading),
                Vec::new(),
                "{heading} sits past the depth cap and stays plain"
            );
        }
        assert!(
            !classes_at(&spans, data, "innermost").contains(&Class::Function),
            "a code fence past the depth cap stays plain"
        );
    }

    #[test]
    fn markdown_merged_spans_stay_ordered_and_disjoint() {
        let data = b"---\nkey: value\n---\n\n# A *bold* title\n\nProse with `code` and *emphasis*.\n\n```toml\n[table]\nkey = \"v\"\n```\n";
        let spans = highlight(data, Language::Markdown, None).unwrap();
        assert_ne!(spans.len(), 0);
        for window in spans.windows(2) {
            assert!(
                window[0].end <= window[1].start,
                "spans overlap: {:?} then {:?}",
                window[0],
                window[1]
            );
        }
        assert!(classes_at(&spans, data, "bold").contains(&Class::Emphasis));
        assert!(classes_at(&spans, data, "title").contains(&Class::Heading));
        assert!(classes_at(&spans, data, "key").contains(&Class::Property));
    }

    #[test]
    fn markdown_block_spans_match_the_highlighters() {
        let data = b"---\nkey: value\n---\n\n# Title *em*\n\nSetext\n======\n\n\
            - item\n- [x] done\n  1. nested\n  2) paren\n\n* star\n+ plus\n\n***\n\n\
            > quoted \\* escape\n> - in quote\n>\n> ```rust\n> fn f() {}\n> ```\n\n\
            ```md\n# inner\n```\n\n~~~\nplain\n~~~\n\n    indented code\n\n\
            [ref]: https://example.com \"Title\"\n[other]: <dest> 'single'\n\n\
            | a | b |\n|---|:-:|\n| \\| | x |\n\n<div>html</div>\n";
        let config = Language::Markdown.configuration().unwrap();
        let tree = parse_markdown(data, None).unwrap();
        let expected = single_layer(config, data, Language::Markdown, None).unwrap();
        assert!(
            expected.len() > 20,
            "the sample exercises the block query: {expected:?}"
        );
        assert_eq!(block_spans(config, &tree, data, None).unwrap(), expected);
    }

    #[test]
    fn block_spans_let_the_later_pattern_win_on_a_shared_node() {
        let query = "(atx_heading) @text.title\n(atx_heading) @keyword\n(inline) @spell\n\
            (atx_h1_marker) @none\n(paragraph) @string\n(paragraph) @spell\n\
            (list_item) @string\n(list_marker_minus) @punctuation\n";
        let config = load(tree_sitter_md::LANGUAGE.into(), "markdown", query).unwrap();
        let data = b"# Heading\n\nProse.\n\n- one\n- two\n";
        let tree = parse_markdown(data, None).unwrap();
        let expected = single_layer(&config, data, Language::Markdown, None).unwrap();
        assert_eq!(classes_at(&expected, data, "Heading"), vec![Class::Keyword]);
        assert_eq!(classes_at(&expected, data, "Prose"), Vec::new());
        assert_eq!(block_spans(&config, &tree, data, None).unwrap(), expected);
    }

    #[test]
    fn capture_highlights_follow_configures_rule() {
        let names: Vec<Option<&str>> = capture_highlights(&[
            "string.escape",
            "keyword.function",
            "punctuation.special",
            "text.title",
            "none",
            "spell",
        ])
        .into_iter()
        .map(|highlight| highlight.map(|highlight| CAPTURE_NAMES[highlight.0]))
        .collect();
        assert_eq!(
            names,
            vec![
                Some("escape"),
                Some("keyword.function"),
                Some("punctuation"),
                Some("text.title"),
                Some("none"),
                None,
            ]
        );
    }

    #[test]
    fn merge_overlays_clips_base_around_overlays_without_empty_pieces() {
        let span = |start, end, class| Span { start, end, class };
        let base = vec![
            span(0, 10, Class::Heading),
            span(12, 14, Class::Comment),
            span(20, 30, Class::Heading),
        ];
        let overlays = vec![
            span(0, 4, Class::Emphasis),
            span(4, 6, Class::Strong),
            span(8, 12, Class::Link),
            span(25, 30, Class::String),
        ];
        let merged = merge_overlays(base, overlays);
        assert_eq!(
            merged,
            vec![
                span(0, 4, Class::Emphasis),
                span(4, 6, Class::Strong),
                span(6, 8, Class::Heading),
                span(8, 12, Class::Link),
                span(12, 14, Class::Comment),
                span(20, 25, Class::Heading),
                span(25, 30, Class::String),
            ]
        );
    }

    #[test]
    fn markdown_front_matter_highlights_as_yaml_and_toml() {
        let documents: [&[u8]; 2] = [
            b"---\ntitle: 42\n---\n# H\n",
            b"+++\ntitle = 42\n+++\n# H\n",
        ];
        for data in documents {
            let spans = highlight(data, Language::Markdown, None).unwrap();
            let text = String::from_utf8_lossy(data);
            assert!(
                classes_at(&spans, data, "title").contains(&Class::Property),
                "front matter key is a Property in {text:?}"
            );
            assert!(
                classes_at(&spans, data, "42").contains(&Class::Number),
                "front matter value is a Number in {text:?}"
            );
            assert!(
                classes_at(&spans, data, "H").contains(&Class::Heading),
                "heading after front matter stays a Heading in {text:?}"
            );
        }
    }

    #[test]
    fn markdown_fence_inside_a_list_item_drops_the_indent() {
        let data = b"- step\n\n  ```yaml\n  key:\n    nested: 1\n  other: 2\n  ```\n";
        let spans = highlight(data, Language::Markdown, None).unwrap();
        assert!(classes_at(&spans, data, "key").contains(&Class::Property));
        assert!(classes_at(&spans, data, "other").contains(&Class::Property));
        assert!(classes_at(&spans, data, "2\n").contains(&Class::Number));
    }

    #[test]
    fn markdown_quote_markers_stay_punctuation_inside_a_fence() {
        let data = b"> ```python\n> def f():\n>     return 1\n> x = 2\n> ```\n";
        let spans = highlight(data, Language::Markdown, None).unwrap();
        assert!(classes_at(&spans, data, "def").contains(&Class::Keyword));
        for (index, byte) in data.iter().enumerate() {
            let line_start = index == 0 || data[index - 1] == b'\n';
            if *byte != b'>' || !line_start {
                continue;
            }
            let covering: Vec<Class> = spans
                .iter()
                .filter(|span| span.start as usize <= index && index < span.end as usize)
                .map(|span| span.class)
                .collect();
            assert!(
                !covering.contains(&Class::Operator),
                "quote marker at {index} is an Operator: {covering:?}"
            );
        }
    }

    #[test]
    fn markdown_emphasis_across_quoted_lines_leaves_the_marker() {
        let data = b"> Some *soft\n> text* here.\n";
        let spans = highlight(data, Language::Markdown, None).unwrap();
        assert!(classes_at(&spans, data, "soft").contains(&Class::Emphasis));
        assert!(classes_at(&spans, data, "text").contains(&Class::Emphasis));
        let marker = data.windows(3).position(|w| w == b"\n> ").unwrap() + 1;
        let emphasized = spans.iter().any(|span| {
            span.class == Class::Emphasis
                && (span.start as usize) < marker + 2
                && marker < span.end as usize
        });
        assert!(!emphasized, "the quote marker is emphasized: {spans:?}");
    }

    #[test]
    fn broken_input_still_highlights_the_intact_parts() {
        let data = b"fn ok() {}\n%%%%\nfn also_ok() {}\n";
        let spans = highlight(data, Language::Rust, None).unwrap();
        assert!(classes_at(&spans, data, "also_ok").contains(&Class::Function));
    }

    #[test]
    fn empty_input_yields_no_spans() {
        assert_eq!(highlight(b"", Language::Json, None).unwrap(), Vec::new());
    }

    fn assert_cancelled(result: Result<Vec<Span>, Error>, message: &str) {
        let error = result.unwrap_err();
        assert!(
            matches!(
                error,
                Error::Parse {
                    source: tree_sitter_highlight::Error::Cancelled,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(error.to_string(), message);
    }

    #[test]
    fn a_raised_cancel_flag_stops_highlighting() {
        let raised = AtomicUsize::new(1);
        let rust = b"fn main() {}\n";
        assert_cancelled(
            highlight(rust, Language::Rust, Some(&raised)),
            "highlighting rust: Cancelled",
        );
        let markdown = b"# Title\n\n```rust\nfn main() {}\n```\n";
        assert_cancelled(
            highlight(markdown, Language::Markdown, Some(&raised)),
            "highlighting markdown: Cancelled",
        );
    }

    #[test]
    fn a_lowered_cancel_flag_changes_nothing() {
        let lowered = AtomicUsize::new(0);
        let data = b"# Title\n\n```rust\nfn main() {}\n```\n";
        assert_eq!(
            highlight(data, Language::Markdown, Some(&lowered)).unwrap(),
            highlight(data, Language::Markdown, None).unwrap()
        );
    }

    #[test]
    fn markdown_regions_pass_a_cancel_through_instead_of_going_plain() {
        let raised = AtomicUsize::new(1);
        let nesting = Nesting {
            depth: 0,
            cancel: Some(&raised),
        };
        let data = b"some *prose* here\n";
        let tree = parse_markdown(data, None).unwrap();
        assert_cancelled(
            markdown_overlays(&tree, data, nesting),
            "highlighting markdown: Cancelled",
        );
        let config = Language::Markdown.configuration().unwrap();
        assert_cancelled(
            block_spans(config, &tree, data, Some(&raised)),
            "highlighting markdown: Cancelled",
        );
        assert_cancelled(
            highlight_region(data, Region::Inline, nesting),
            "highlighting markdown: Cancelled",
        );
    }
}

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
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

pub use imports::{ImportLink, import_links};

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

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

    fn configuration(self) -> Result<&'static HighlightConfiguration, Error> {
        static CONFIGS: [OnceLock<Loaded>; LANGUAGE_COUNT] =
            [const { OnceLock::new() }; LANGUAGE_COUNT];
        let Some(slot) = CONFIGS.get(self as usize) else {
            return Err(Error::Grammar {
                language: self,
                detail: "no configuration slot: Language::ALL does not list it".to_owned(),
            });
        };
        slot.get_or_init(|| build_configuration(self))
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
                    start: u32::try_from(start).unwrap_or(u32::MAX),
                    end: u32::try_from(end).unwrap_or(u32::MAX),
                    class: *class,
                });
            }
        }
    }
    Ok(spans)
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
/// every `inline` node, and the named language for every fenced code block and metadata
/// section. Inner spans win over the block layer's, so emphasis inside a heading and keywords
/// inside a fence keep their own class.
fn highlight_markdown(data: &[u8], nesting: Nesting) -> Result<Vec<Span>, Error> {
    let config = Language::Markdown.configuration()?;
    let base = single_layer(config, data, Language::Markdown, nesting.cancel)?;
    let overlays = markdown_overlays(data, nesting)?;
    Ok(merge_overlays(base, overlays))
}

/// Collects spans from every injected region of a markdown document. A region that fails to
/// parse simply contributes nothing; the file still shows its block-level color.
///
/// # Errors
/// [`Error::Grammar`] when the block grammar or a region's grammar fails to load, so a
/// packaging problem reaches the caller instead of quietly dropping the prose color.
/// [`Error::Parse`] wrapping `Cancelled` when the caller's flag is raised.
fn markdown_overlays(data: &[u8], nesting: Nesting) -> Result<Vec<Span>, Error> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .map_err(|error| Error::Grammar {
            language: Language::Markdown,
            detail: error.to_string(),
        })?;
    check_cancel(nesting.cancel, Language::Markdown)?;
    let mut progress = |_: &tree_sitter::ParseState| {
        if is_raised(nesting.cancel) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = tree_sitter::ParseOptions::new().progress_callback(&mut progress);
    let mut read = |offset: usize, _| data.get(offset..).unwrap_or_default();
    let Some(tree) = parser.parse_with_options(&mut read, None, Some(options)) else {
        check_cancel(nesting.cancel, Language::Markdown)?;
        return Ok(Vec::new());
    };
    let mut overlays = Vec::new();
    let mut cursor = tree.walk();
    'walk: loop {
        let node = cursor.node();
        let mut descend = true;
        match node.kind() {
            "inline" => {
                append_region(
                    &mut overlays,
                    data,
                    node.byte_range(),
                    Region::Inline,
                    nesting,
                )?;
                descend = false;
            }
            "fenced_code_block" => {
                append_fence(&mut overlays, data, node, nesting)?;
                descend = false;
            }
            "minus_metadata" => {
                let region = Region::Fence(Language::Yaml);
                append_region(&mut overlays, data, node.byte_range(), region, nesting)?;
                descend = false;
            }
            "plus_metadata" => {
                let region = Region::Fence(Language::Toml);
                append_region(&mut overlays, data, node.byte_range(), region, nesting)?;
                descend = false;
            }
            _ => {}
        }
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
            "code_fence_content" => content = Some(child.byte_range()),
            _ => {}
        }
    }
    let (Some(language), Some(content)) = (language, content) else {
        return Ok(());
    };
    append_region(overlays, data, content, Region::Fence(language), nesting)
}

/// Highlights one region's slice and appends its spans shifted back to document offsets.
///
/// # Errors
/// [`Error::Grammar`] when the region's grammar fails to load, and [`Error::Parse`] wrapping
/// `Cancelled` when the caller's flag is raised. A region whose parse fails otherwise
/// contributes no spans and is not an error.
fn append_region(
    overlays: &mut Vec<Span>,
    data: &[u8],
    range: std::ops::Range<usize>,
    region: Region,
    nesting: Nesting,
) -> Result<(), Error> {
    let Some(slice) = data.get(range.clone()) else {
        return Ok(());
    };
    let spans = match region {
        Region::Inline => markdown_inline_configuration()
            .and_then(|config| single_layer(config, slice, Language::Markdown, nesting.cancel)),
        Region::Fence(Language::Markdown) => {
            if nesting.depth >= MARKDOWN_FENCE_DEPTH {
                return Ok(());
            }
            let inner = Nesting {
                depth: nesting.depth + 1,
                cancel: nesting.cancel,
            };
            highlight_markdown(slice, inner)
        }
        Region::Fence(language) => language
            .configuration()
            .and_then(|config| single_layer(config, slice, language, nesting.cancel)),
    };
    let spans = match spans {
        Ok(spans) => spans,
        Err(
            error @ (Error::Grammar { .. }
            | Error::Parse {
                source: tree_sitter_highlight::Error::Cancelled,
                ..
            }),
        ) => return Err(error),
        // A region tree-sitter cannot parse leaves its text plain; the rest of the file keeps
        // its color, as an unparseable stretch of any other file does.
        Err(Error::Parse { .. } | Error::Imports { .. }) => return Ok(()),
    };
    let offset = u32::try_from(range.start).unwrap_or(u32::MAX);
    for span in spans {
        overlays.push(Span {
            start: span.start.saturating_add(offset),
            end: span.end.saturating_add(offset),
            class: span.class,
        });
    }
    Ok(())
}

/// Merges overlay spans into base spans, both sorted and internally non-overlapping. An
/// overlay is deeper than the base, so base spans are clipped around every overlay they touch
/// and the result stays sorted and non-overlapping.
fn merge_overlays(base: Vec<Span>, overlays: Vec<Span>) -> Vec<Span> {
    if overlays.is_empty() {
        return base;
    }
    let mut clipped = Vec::with_capacity(base.len());
    let mut first_relevant = 0;
    for span in &base {
        while first_relevant < overlays.len() && overlays[first_relevant].end <= span.start {
            first_relevant += 1;
        }
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
    let mut merged = Vec::with_capacity(clipped.len() + overlays.len());
    let mut left = clipped.into_iter().peekable();
    let mut right = overlays.into_iter().peekable();
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
    fn all_lists_every_language_at_its_discriminant() {
        for (index, language) in Language::ALL.into_iter().enumerate() {
            assert_eq!(
                language as usize,
                index,
                "{} is out of order",
                language.name()
            );
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
    fn markdown_merged_spans_stay_ordered_and_disjoint() {
        let data = b"# A *bold* title\n\n---\nkey: value\n---\n\nProse with `code` and *emphasis*.\n\n```toml\n[table]\nkey = \"v\"\n```\n";
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
        assert_cancelled(
            markdown_overlays(data, nesting),
            "highlighting markdown: Cancelled",
        );
        let mut overlays = Vec::new();
        let error = append_region(&mut overlays, data, 0..data.len(), Region::Inline, nesting);
        assert_cancelled(error.map(|()| overlays), "highlighting markdown: Cancelled");
    }
}

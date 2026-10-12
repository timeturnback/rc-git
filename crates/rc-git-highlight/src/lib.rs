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
use std::sync::OnceLock;

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
        let extension = name.rsplit('.').next()?;
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
            "c" => Some(Self::C),
            "javascript" | "js" | "jsx" => Some(Self::JavaScript),
            "json" => Some(Self::Json),
            "markdown" | "md" => Some(Self::Markdown),
            "python" | "py" => Some(Self::Python),
            "rust" | "rs" => Some(Self::Rust),
            "swift" => Some(Self::Swift),
            "toml" => Some(Self::Toml),
            "tsx" => Some(Self::Tsx),
            "typescript" | "ts" => Some(Self::TypeScript),
            "yaml" | "yml" => Some(Self::Yaml),
            _ => None,
        }
    }

    fn configuration(self) -> Result<&'static HighlightConfiguration, Error> {
        static CONFIGS: [OnceLock<Option<HighlightConfiguration>>; LANGUAGE_COUNT] =
            [const { OnceLock::new() }; LANGUAGE_COUNT];
        CONFIGS[self as usize]
            .get_or_init(|| build_configuration(self).ok())
            .as_ref()
            .ok_or(Error::Grammar { language: self })
    }
}

const LANGUAGE_COUNT: usize = 12;

/// The markdown inline grammar, reachable only through markdown's injections. Configured with
/// the same capture names as every other grammar, so classes mean the same thing everywhere.
fn markdown_inline_configuration() -> Option<&'static HighlightConfiguration> {
    static CONFIG: OnceLock<Option<HighlightConfiguration>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut config = HighlightConfiguration::new(
                tree_sitter_md::INLINE_LANGUAGE.into(),
                "markdown_inline",
                tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
                "",
                "",
            )
            .ok()?;
            config.configure(&CAPTURE_NAMES);
            Some(config)
        })
        .as_ref()
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
/// Dotted capture names fall back to their longest prefix here, per tree-sitter's rules.
/// A `None` class leaves the node plain, on purpose.
const CAPTURES: [(&str, Option<Class>); 25] = [
    ("attribute", Some(Class::Attribute)),
    ("character", Some(Class::String)),
    ("comment", Some(Class::Comment)),
    ("constant", Some(Class::Constant)),
    ("constructor", Some(Class::Type)),
    ("delimiter", Some(Class::Punctuation)),
    ("embedded", None),
    ("escape", Some(Class::String)),
    ("function", Some(Class::Function)),
    ("keyword", Some(Class::Keyword)),
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
    #[error("highlighting {}: the grammar failed to load", language.name())]
    Grammar {
        /// The language whose grammar failed.
        language: Language,
    },
    /// The parse itself failed, for instance on a cancelled or oversized input.
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
/// # Errors
/// [`Error::Grammar`] when the language cannot be loaded, [`Error::Parse`] when parsing
/// fails outright. An unparseable region inside an otherwise fine file is not an error;
/// tree-sitter recovers and the region simply gets no spans.
pub fn highlight(data: &[u8], language: Language) -> Result<Vec<Span>, Error> {
    match language {
        Language::Markdown => highlight_markdown(data, 0),
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
        | Language::Yaml => single_layer(language.configuration()?, data, language),
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
) -> Result<Vec<Span>, Error> {
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(config, data, None, None, |_| None)
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

/// Highlights markdown: the block grammar for structure, the inline grammar for prose inside
/// every `inline` node, and the named language for every fenced code block and metadata
/// section. Inner spans win over the block layer's, so emphasis inside a heading and keywords
/// inside a fence keep their own class.
fn highlight_markdown(data: &[u8], depth: u8) -> Result<Vec<Span>, Error> {
    let config = Language::Markdown.configuration()?;
    let base = single_layer(config, data, Language::Markdown)?;
    let overlays = markdown_overlays(data, depth);
    Ok(merge_overlays(base, overlays))
}

/// Collects spans from every injected region of a markdown document. A region that fails to
/// parse simply contributes nothing; the file still shows its block-level color.
fn markdown_overlays(data: &[u8], depth: u8) -> Vec<Span> {
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(data, None) else {
        return Vec::new();
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
                    depth,
                );
                descend = false;
            }
            "fenced_code_block" => {
                append_fence(&mut overlays, data, node, depth);
                descend = false;
            }
            "minus_metadata" => {
                let region = Region::Fence(Language::Yaml);
                append_region(&mut overlays, data, node.byte_range(), region, depth);
                descend = false;
            }
            "plus_metadata" => {
                let region = Region::Fence(Language::Toml);
                append_region(&mut overlays, data, node.byte_range(), region, depth);
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
    overlays
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
fn append_fence(overlays: &mut Vec<Span>, data: &[u8], node: tree_sitter::Node, depth: u8) {
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
        return;
    };
    append_region(overlays, data, content, Region::Fence(language), depth);
}

/// Highlights one region's slice and appends its spans shifted back to document offsets.
fn append_region(
    overlays: &mut Vec<Span>,
    data: &[u8],
    range: std::ops::Range<usize>,
    region: Region,
    depth: u8,
) {
    let Some(slice) = data.get(range.clone()) else {
        return;
    };
    let spans = match region {
        Region::Inline => markdown_inline_configuration()
            .ok_or(Error::Grammar {
                language: Language::Markdown,
            })
            .and_then(|config| single_layer(config, slice, Language::Markdown)),
        Region::Fence(Language::Markdown) => {
            if depth >= MARKDOWN_FENCE_DEPTH {
                return;
            }
            highlight_markdown(slice, depth + 1)
        }
        Region::Fence(language) => language
            .configuration()
            .and_then(|config| single_layer(config, slice, language)),
    };
    let Ok(spans) = spans else { return };
    let offset = u32::try_from(range.start).unwrap_or(u32::MAX);
    for span in spans {
        overlays.push(Span {
            start: span.start.saturating_add(offset),
            end: span.end.saturating_add(offset),
            class: span.class,
        });
    }
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

fn build_configuration(
    language: Language,
) -> Result<HighlightConfiguration, Box<dyn std::error::Error>> {
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
    let mut config = HighlightConfiguration::new(grammar, language.name(), &query, "", "")?;
    config.configure(&CAPTURE_NAMES);
    Ok(config)
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
        assert_eq!(Language::from_file_name("noext"), None);
        assert_eq!(Language::from_file_name("photo.png"), None);
        assert_eq!(Language::from_file_name(""), None);
    }

    #[test]
    fn rust_keywords_strings_and_comments_are_classified() {
        let data = b"// note\nfn main() {\n    let x = \"hi\";\n}\n";
        let spans = highlight(data, Language::Rust).unwrap();
        assert_ne!(spans.len(), 0);
        assert!(classes_at(&spans, data, "// note").contains(&Class::Comment));
        assert!(classes_at(&spans, data, "fn").contains(&Class::Keyword));
        assert!(classes_at(&spans, data, "main").contains(&Class::Function));
        assert!(classes_at(&spans, data, "\"hi\"").contains(&Class::String));
        for window in spans.windows(2) {
            assert!(window[0].start <= window[1].start, "spans are ordered");
        }
    }

    const ALL_LANGUAGES: [Language; LANGUAGE_COUNT] = [
        Language::Bash,
        Language::C,
        Language::JavaScript,
        Language::Json,
        Language::Markdown,
        Language::Python,
        Language::Rust,
        Language::Swift,
        Language::Toml,
        Language::Tsx,
        Language::TypeScript,
        Language::Yaml,
    ];

    /// Capture names a pinned query uses that no `CAPTURES` entry resolves yet. Each one is
    /// a node that loses its color; P10-02 maps `boolean` and empties this list.
    const KNOWN_UNRESOLVED: [&str; 1] = ["boolean"];

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
        for language in ALL_LANGUAGES {
            let config = language
                .configuration()
                .unwrap_or_else(|err| panic!("{}: {err}", language.name()));
            configs.push((language.name(), config));
        }
        let inline = markdown_inline_configuration().expect("markdown_inline loads");
        configs.push(("markdown_inline", inline));
        let mut unresolved = Vec::new();
        for (language, config) in configs {
            for name in config.names() {
                if name.starts_with('_') || resolves(name) {
                    continue;
                }
                if !unresolved.contains(name) {
                    unresolved.push(*name);
                }
                assert!(
                    KNOWN_UNRESOLVED.contains(name),
                    "{language} captures @{name}, which no CAPTURES entry resolves"
                );
            }
        }
        for name in KNOWN_UNRESOLVED {
            assert!(
                unresolved.contains(&name),
                "@{name} resolves now; drop it from KNOWN_UNRESOLVED"
            );
        }
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
        let spans = highlight(data, Language::Swift).unwrap();
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
        let spans = highlight(c, Language::C).unwrap();
        assert_eq!(classes_at(&spans, c, ";"), vec![Class::Punctuation]);
        assert_eq!(classes_at(&spans, c, "."), vec![Class::Punctuation]);

        let swift = b"let _ = f()\n";
        let spans = highlight(swift, Language::Swift).unwrap();
        assert_eq!(classes_at(&spans, swift, "_"), vec![Class::String]);
    }

    #[test]
    fn every_language_grammar_loads_and_highlights() {
        let samples: [(Language, &[u8]); LANGUAGE_COUNT] = [
            (Language::Bash, b"echo \"hi\" # done\n"),
            (Language::C, b"int main(void) { return 0; }\n"),
            (Language::JavaScript, b"const x = 1; // c\n"),
            (Language::Json, b"{\"key\": [1, true]}\n"),
            (Language::Markdown, b"# Title\n\nSome *prose*.\n"),
            (Language::Python, b"def f():\n    return 'x'\n"),
            (Language::Rust, b"pub fn f() -> u32 { 1 }\n"),
            (Language::Swift, b"func f() -> Int { return 1 }\n"),
            (Language::Toml, b"[table]\nkey = \"value\"\n"),
            (Language::Tsx, b"const a = <Tag prop={1} />;\n"),
            (Language::TypeScript, b"const x: number = 1; // c\n"),
            (Language::Yaml, b"key: value\nlist:\n  - 1\n"),
        ];
        for (language, data) in samples {
            let spans = highlight(data, language)
                .unwrap_or_else(|err| panic!("{}: {err}", language.name()));
            assert_ne!(spans.len(), 0, "{} produced no spans", language.name());
        }
    }

    #[test]
    fn typescript_types_keywords_and_constants_are_classified() {
        let data = b"interface Shape { readonly side: number }\nconst MAX_SIZE = 4;\nexport function area(shape: Shape): number { return shape.side ** 2; }\n";
        let spans = highlight(data, Language::TypeScript).unwrap();
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
        let spans = highlight(cast, Language::TypeScript).unwrap();
        assert!(classes_at(&spans, cast, "number").contains(&Class::Type));

        let jsx = b"const view = <Panel title=\"a\">{count}</Panel>;\n";
        let spans = highlight(jsx, Language::Tsx).unwrap();
        assert!(classes_at(&spans, jsx, "Panel title").contains(&Class::Type));
        assert!(classes_at(&spans, jsx, "\"a\"").contains(&Class::String));
        assert!(classes_at(&spans, jsx, "title").contains(&Class::Property));
    }

    #[test]
    fn markdown_fences_highlight_typescript() {
        let data = b"```ts\nconst x: number = 1;\n```\n";
        let spans = highlight(data, Language::Markdown).unwrap();
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
    fn markdown_prose_is_classified() {
        let data = b"# Title\n\nSome *soft* and **hard** emphasis, a `span`,\nand a [link](https://example.com).\n";
        let spans = highlight(data, Language::Markdown).unwrap();
        assert!(classes_at(&spans, data, "Title").contains(&Class::Heading));
        assert!(classes_at(&spans, data, "soft").contains(&Class::Emphasis));
        assert!(classes_at(&spans, data, "hard").contains(&Class::Strong));
        assert!(classes_at(&spans, data, "`span`").contains(&Class::String));
        assert!(classes_at(&spans, data, "https://example.com").contains(&Class::Link));
    }

    #[test]
    fn markdown_code_fences_highlight_in_their_own_language() {
        let data = b"# Doc\n\n```rust\nfn fenced() -> u32 { 1 }\n```\n\n```nosuchlang\nfn ignored() {}\n```\n";
        let spans = highlight(data, Language::Markdown).unwrap();
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
        let spans = highlight(data, Language::Markdown).unwrap();
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
        let spans = highlight(data, Language::Rust).unwrap();
        assert!(classes_at(&spans, data, "also_ok").contains(&Class::Function));
    }

    #[test]
    fn empty_input_yields_no_spans() {
        assert_eq!(highlight(b"", Language::Json).unwrap(), Vec::new());
    }
}

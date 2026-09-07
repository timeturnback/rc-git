//! Tree-sitter syntax highlighting for the file pane.
//!
//! Pure: bytes in, spans out. The app applies spans after first paint, so plain text never
//! waits on a parse, and caches them by blob id, so a file is parsed once. Languages are
//! detected from the file name only; unknown names simply produce no spans.

use std::sync::OnceLock;

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
    /// Python: `.py`.
    Python,
    /// Rust: `.rs`.
    Rust,
    /// Swift: `.swift`.
    Swift,
    /// TOML: `.toml`.
    Toml,
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
            "py" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            "swift" => Some(Self::Swift),
            "toml" => Some(Self::Toml),
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
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Swift => "swift",
            Self::Toml => "toml",
            Self::Yaml => "yaml",
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

const LANGUAGE_COUNT: usize = 9;

/// What a span means, for the view to color. One flat set for every language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// `@attribute`, decorators, derives.
    Attribute,
    /// Comments, doc comments.
    Comment,
    /// Named constants and enum cases.
    Constant,
    /// Function and method names.
    Function,
    /// Language keywords.
    Keyword,
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
    /// Type names.
    Type,
    /// Variables and parameters worth tinting.
    Variable,
}

/// The capture names the queries are matched against, in the order `CLASS_FOR_NAME` maps.
/// Dotted capture names fall back to their longest prefix here, per tree-sitter's rules.
const CAPTURE_NAMES: [&str; 16] = [
    "attribute",
    "comment",
    "constant",
    "constructor",
    "embedded",
    "escape",
    "function",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "string",
    "type",
    "variable",
];

const CLASS_FOR_NAME: [Option<Class>; 16] = [
    Some(Class::Attribute),
    Some(Class::Comment),
    Some(Class::Constant),
    Some(Class::Type),
    None,
    Some(Class::String),
    Some(Class::Function),
    Some(Class::Keyword),
    Some(Class::Constant),
    Some(Class::Number),
    Some(Class::Operator),
    Some(Class::Property),
    Some(Class::Punctuation),
    Some(Class::String),
    Some(Class::Type),
    Some(Class::Variable),
];

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
}

/// Highlights `data`, returning spans ordered by start. Nested captures flatten to the
/// innermost one, which is what a single-color-per-range text view needs.
///
/// # Errors
/// [`Error::Grammar`] when the language cannot be loaded, [`Error::Parse`] when parsing
/// fails outright. An unparseable region inside an otherwise fine file is not an error;
/// tree-sitter recovers and the region simply gets no spans.
pub fn highlight(data: &[u8], language: Language) -> Result<Vec<Span>, Error> {
    let config = language.configuration()?;
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
                stack.push(CLASS_FOR_NAME[highlight.0]);
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

fn build_configuration(
    language: Language,
) -> Result<HighlightConfiguration, Box<dyn std::error::Error>> {
    let (grammar, query): (tree_sitter::Language, &str) = match language {
        Language::Bash => (
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY,
        ),
        Language::C => (
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::HIGHLIGHT_QUERY,
        ),
        Language::JavaScript => (
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY,
        ),
        Language::Json => (
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY,
        ),
        Language::Python => (
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY,
        ),
        Language::Rust => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        ),
        Language::Swift => (
            tree_sitter_swift::LANGUAGE.into(),
            tree_sitter_swift::HIGHLIGHTS_QUERY,
        ),
        Language::Toml => (
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        ),
        Language::Yaml => (
            tree_sitter_yaml::LANGUAGE.into(),
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
        ),
    };
    let mut config = HighlightConfiguration::new(grammar, language.name(), query, "", "")?;
    config.configure(&CAPTURE_NAMES);
    Ok(config)
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
        assert_eq!(Language::from_file_name("noext"), None);
        assert_eq!(Language::from_file_name("photo.png"), None);
        assert_eq!(Language::from_file_name(""), None);
    }

    #[test]
    fn rust_keywords_strings_and_comments_are_classified() {
        let data = b"// note\nfn main() {\n    let x = \"hi\";\n}\n";
        let spans = highlight(data, Language::Rust).unwrap();
        assert!(!spans.is_empty());
        assert!(classes_at(&spans, data, "// note").contains(&Class::Comment));
        assert!(classes_at(&spans, data, "fn").contains(&Class::Keyword));
        assert!(classes_at(&spans, data, "main").contains(&Class::Function));
        assert!(classes_at(&spans, data, "\"hi\"").contains(&Class::String));
        for window in spans.windows(2) {
            assert!(window[0].start <= window[1].start, "spans are ordered");
        }
    }

    #[test]
    fn every_language_grammar_loads_and_highlights() {
        let samples: [(Language, &[u8]); 9] = [
            (Language::Bash, b"echo \"hi\" # done\n"),
            (Language::C, b"int main(void) { return 0; }\n"),
            (Language::JavaScript, b"const x = 1; // c\n"),
            (Language::Json, b"{\"key\": [1, true]}\n"),
            (Language::Python, b"def f():\n    return 'x'\n"),
            (Language::Rust, b"pub fn f() -> u32 { 1 }\n"),
            (Language::Swift, b"func f() -> Int { return 1 }\n"),
            (Language::Toml, b"[table]\nkey = \"value\"\n"),
            (Language::Yaml, b"key: value\nlist:\n  - 1\n"),
        ];
        for (language, data) in samples {
            let spans = highlight(data, language)
                .unwrap_or_else(|err| panic!("{}: {err}", language.name()));
            assert!(!spans.is_empty(), "{} produced no spans", language.name());
        }
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

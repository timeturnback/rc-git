//! Port of `Markdown.swift`, `MarkdownBlockParser.swift`, `MarkdownInlineParser.swift` and
//! `MarkdownLinkTarget.swift`: markdown source in, flat blocks of styled runs out.
//!
//! The parser reads UTF-8 bytes: every delimiter markdown uses is ASCII, and a continuation
//! byte can never be mistaken for one. It never fails; anything it does not recognise stays
//! literal text.

mod ascii;
mod block;
mod emphasis;
mod inline;
mod link_target;
mod text;

use std::collections::HashMap;

use rc_git_highlight::Language;

pub use link_target::MarkdownLinkTarget;

/// The deepest list nesting the parser opens. A marker past it reads as text, so a crafted
/// line of markers cannot push the renderer's indent off the page.
pub const MAXIMUM_LIST_DEPTH: usize = 32;

/// How deep nested emphasis and links may go before the inline parser stops recursing and
/// treats the delimiter as literal text. Bounds the stack on pathological input.
pub const MAXIMUM_INLINE_DEPTH: usize = 12;

/// Inline appearance flags. Several can apply to one run, so they combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MarkdownStyle(u8);

impl MarkdownStyle {
    /// No styling.
    pub const PLAIN: Self = Self(0);
    /// Emphasis, `*like this*`.
    pub const EMPHASIS: Self = Self(1);
    /// Strong emphasis, `**like this**`.
    pub const STRONG: Self = Self(1 << 1);
    /// A code span.
    pub const CODE: Self = Self(1 << 2);
    /// `~~struck~~` text.
    pub const STRIKETHROUGH: Self = Self(1 << 3);
    /// A footnote reference marker, drawn raised and small.
    pub const FOOTNOTE_MARKER: Self = Self(1 << 4);

    /// Whether every flag in `other` is set here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The flags of both.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether no flag is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Where an inline link points. `destination` is the raw markdown target, unresolved: it may
/// be a URL, a repository-relative path, or a bare fragment. Resolve it only through
/// [`MarkdownLinkTarget::resolve`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MarkdownLink {
    /// The target as written.
    pub destination: String,
    /// The quoted title after the target, without its quotes.
    pub title: Option<String>,
}

impl MarkdownLink {
    /// A link with no title.
    #[must_use]
    pub fn new(destination: impl Into<String>) -> Self {
        Self {
            destination: destination.into(),
            title: None,
        }
    }
}

/// One stretch of inline text that shares a single appearance. A paragraph is a list of these,
/// in order, and concatenating their text reproduces the visible line.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MarkdownRun {
    /// The visible text.
    pub text: String,
    /// How it looks.
    pub style: MarkdownStyle,
    /// Where it links, if anywhere.
    pub link: Option<MarkdownLink>,
    /// True when the run stands in for an image: `text` is the alt text and `link` the source.
    pub is_image: bool,
}

impl MarkdownRun {
    /// A plain run of `text`.
    #[must_use]
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: MarkdownStyle::PLAIN,
            link: None,
            is_image: false,
        }
    }
}

/// How a table column aligns its cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Alignment {
    /// No colon in the delimiter: the writing direction's default.
    Natural,
    /// `:--`
    Leading,
    /// `:-:`
    Center,
    /// `--:`
    Trailing,
}

/// A GFM table: one alignment per column, a header row, and the body rows. Every row is padded
/// to `alignments.len()` cells by the parser, so a renderer never has to check lengths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MarkdownTable {
    /// One per column.
    pub alignments: Vec<Alignment>,
    /// The header cells, each a list of runs.
    pub header: Vec<Vec<MarkdownRun>>,
    /// The body rows, each a list of cells.
    pub rows: Vec<Vec<Vec<MarkdownRun>>>,
}

impl MarkdownTable {
    /// How many columns every row has.
    #[must_use]
    pub fn column_count(&self) -> usize {
        self.alignments.len()
    }
}

/// What starts one item of a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkdownListMarker {
    /// `-`, `*` or `+`.
    Bullet,
    /// `1.` or `1)`, with its number.
    Ordered(u32),
    /// A GFM task item: `- [ ]` or `- [x]`.
    Task {
        /// Whether the box is checked.
        done: bool,
    },
}

/// What one block is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// Prose.
    Paragraph,
    /// An ATX or setext heading, level 1 to 6.
    Heading {
        /// 1 for `#`, 6 for `######`.
        level: u8,
    },
    /// A fenced or indented code block.
    Code {
        /// The fence's info string up to the first space or comma, lowercased.
        language: Option<String>,
        /// The body, lines joined with `\n`.
        text: String,
    },
    /// A GFM table.
    Table(MarkdownTable),
    /// `---`, `***` or `___`.
    ThematicBreak,
    /// The YAML block delimited by `---` at the very start of a file.
    FrontMatter {
        /// The lines between the delimiters.
        text: String,
    },
    /// A raw HTML block, shown as-is rather than interpreted.
    Html {
        /// The block's lines.
        text: String,
    },
    /// The body of `[^label]: …`, shown under the prose that references it.
    Footnote {
        /// The label as written.
        label: String,
    },
}

/// One block of a parsed document. Nesting is flat: `quote_depth` and `list_depth` say how deep
/// the block sits, which is all a renderer needs to indent it, and lets the whole document be
/// one list instead of a tree to walk.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MarkdownBlock {
    /// What the block is.
    pub kind: BlockKind,
    /// The inline content. Empty for code, tables, and thematic breaks.
    pub runs: Vec<MarkdownRun>,
    /// How many `>` markers enclose it.
    pub quote_depth: usize,
    /// 0 outside a list; 1 for a top-level item, 2 for one nested inside it.
    pub list_depth: usize,
    /// Set only on the first block of a list item, so a renderer draws one bullet per item.
    pub marker: Option<MarkdownListMarker>,
}

impl MarkdownBlock {
    /// A top-level block of `kind` with no runs.
    #[must_use]
    pub fn new(kind: BlockKind) -> Self {
        Self {
            kind,
            runs: Vec::new(),
            quote_depth: 0,
            list_depth: 0,
            marker: None,
        }
    }
}

/// A parsed markdown file, ready to render.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MarkdownDocument {
    /// Every block, in document order.
    pub blocks: Vec<MarkdownBlock>,
}

impl MarkdownDocument {
    /// Markdown above this many bytes opens in source mode. The parse is quick; the limit is
    /// what keeps laying out the rendered result quick.
    pub const PREVIEW_LIMIT: usize = 8 * 1024 * 1024;

    /// A preview above this many bytes takes long enough to build that the pane clears the
    /// previous file at once instead of leaving it on screen until the build lands.
    pub const BUILDING_PLACEHOLDER_THRESHOLD: usize = 512 * 1024;

    /// Whether the file `name`, `byte_count` bytes long, offers the rendered preview: it is
    /// markdown by the highlighter's own extension lookup and no larger than `PREVIEW_LIMIT`.
    #[must_use]
    pub fn is_previewable(name: &str, byte_count: usize) -> bool {
        byte_count <= Self::PREVIEW_LIMIT
            && Language::from_file_name(name).is_some_and(|language| language == Language::Markdown)
    }

    /// What the pane shows while the preview of `name`, `byte_count` bytes long, builds: a
    /// placeholder above `BUILDING_PLACEHOLDER_THRESHOLD`, or `None` to keep the previous text
    /// up for a build quick enough that a placeholder would only flash.
    #[must_use]
    pub fn building_placeholder(name: &str, byte_count: usize) -> Option<String> {
        (byte_count > Self::BUILDING_PLACEHOLDER_THRESHOLD)
            .then(|| format!("Building the preview of {name}…"))
    }

    /// Parses `source`. Never fails: anything it does not recognise stays literal text.
    #[must_use]
    pub fn parse(source: &str) -> Self {
        Self {
            blocks: block::BlockParser::new(source.as_bytes()).parse(),
        }
    }

    /// The fragment a heading answers to, by the rule GitHub uses: lowercased, spaces joined
    /// with hyphens, and everything but letters, digits, `-` and `_` dropped. It is what makes
    /// a table-of-contents link in a README land on its section.
    ///
    /// A letter is an `Alphabetic` scalar and a digit a numeric one, judged on the first scalar
    /// of each user-perceived character, whose combining marks stay with it.
    #[must_use]
    pub fn anchor(heading: &str) -> String {
        let lower = text::lowercased(heading);
        let mut slug = String::with_capacity(lower.len());
        for cluster in text::clusters(&lower) {
            let Some(first) = cluster.chars().next() else {
                continue;
            };
            if first.is_alphabetic() || first.is_numeric() || cluster == "-" || cluster == "_" {
                slug.push_str(cluster);
            } else if cluster == " " {
                slug.push('-');
            }
        }
        slug
    }

    /// The fragment the footnote `label` answers to: `fn-` plus the label, slugged by
    /// [`anchor`](Self::anchor). The marker links to it and the definition records it, so a
    /// label with uppercase or punctuation still matches, case-insensitively as GFM does.
    #[must_use]
    pub fn footnote_anchor(label: &str) -> String {
        Self::anchor(&format!("fn-{label}"))
    }

    /// The fragment each heading in `blocks` answers to, one per heading block in document
    /// order. A repeated slug takes the first free `-1`, `-2`, ... suffix, the way GitHub
    /// de-duplicates heading ids, so the second "Fixed" is `fixed-1`. Footnote anchors count
    /// as taken, so a heading never claims a footnote's id. A heading with no slug gets an
    /// empty string and claims nothing.
    #[must_use]
    pub fn heading_anchors(blocks: &[MarkdownBlock]) -> Vec<String> {
        let mut occurrences: HashMap<String, usize> = HashMap::new();
        for block in blocks {
            if let BlockKind::Footnote { label } = &block.kind {
                occurrences.insert(Self::footnote_anchor(label), 0);
            }
        }
        let mut anchors = Vec::new();
        for block in blocks {
            let BlockKind::Heading { .. } = block.kind else {
                continue;
            };
            let mut heading = String::new();
            for run in &block.runs {
                heading.push_str(&run.text);
            }
            let original = Self::anchor(&heading);
            if original.is_empty() {
                anchors.push(String::new());
                continue;
            }
            let mut slug = original.clone();
            while occurrences.contains_key(&slug) {
                let count = occurrences.get(&original).copied().unwrap_or(0) + 1;
                occurrences.insert(original.clone(), count);
                slug = format!("{original}-{count}");
            }
            occurrences.insert(slug.clone(), 0);
            anchors.push(slug);
        }
        anchors
    }
}

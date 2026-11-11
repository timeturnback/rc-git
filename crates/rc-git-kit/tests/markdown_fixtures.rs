//! Golden fixtures: every `.md.in` under `tests/fixtures/markdown` was parsed by the Swift
//! `MarkdownDocument` the Rust port replaces, and its document dumped to the `.json` beside it.
//! The inputs are every tracked `.md` file of the repository (`repo__<path>`) and every
//! markdown literal from `MarkdownTests.swift` and `MarkdownLinkTargetTests.swift`
//! (`test__<name>`). This test serialises the Rust parse to the same JSON and compares, so a
//! port that narrows or changes any construct fails here.

#![expect(
    clippy::unwrap_used,
    reason = "the JSON writer appends to a String, which cannot fail"
)]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use rc_git_kit::markdown::{
    Alignment, BlockKind, MarkdownBlock, MarkdownDocument, MarkdownLink, MarkdownLinkTarget,
    MarkdownListMarker, MarkdownRun, MarkdownStyle,
};

/// The repository path a fixture input stands for, which its links resolve against: the
/// original path for a repository file, a nested docs path for a test literal.
fn source_path(name: &str) -> String {
    match name.strip_prefix("repo__") {
        Some(rest) => rest.trim_end_matches(".in").replace("__", "/"),
        None => "docs/guide/index.md".to_owned(),
    }
}

fn string(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            ch if u32::from(ch) < 0x20 => write!(out, "\\u{:04x}", u32::from(ch)).unwrap(),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn optional(text: Option<&str>) -> String {
    text.map_or_else(|| "null".to_owned(), string)
}

fn style(style: MarkdownStyle) -> String {
    let flags = [
        (MarkdownStyle::EMPHASIS, "emphasis"),
        (MarkdownStyle::STRONG, "strong"),
        (MarkdownStyle::CODE, "code"),
        (MarkdownStyle::STRIKETHROUGH, "strikethrough"),
        (MarkdownStyle::FOOTNOTE_MARKER, "footnoteMarker"),
    ];
    let mut names = Vec::new();
    for (flag, name) in flags {
        if style.contains(flag) {
            names.push(string(name));
        }
    }
    format!("[{}]", names.join(","))
}

fn link(link: Option<&MarkdownLink>) -> String {
    let Some(link) = link else {
        return "null".to_owned();
    };
    format!(
        "{{\"destination\":{},\"title\":{}}}",
        string(&link.destination),
        optional(link.title.as_deref())
    )
}

fn run(run: &MarkdownRun) -> String {
    format!(
        "{{\"text\":{},\"style\":{},\"link\":{},\"image\":{}}}",
        string(&run.text),
        style(run.style),
        link(run.link.as_ref()),
        run.is_image
    )
}

fn runs(runs: &[MarkdownRun]) -> String {
    let mut out = Vec::new();
    for item in runs {
        out.push(run(item));
    }
    format!("[{}]", out.join(","))
}

fn alignment(alignment: Alignment) -> &'static str {
    match alignment {
        Alignment::Natural => "\"natural\"",
        Alignment::Leading => "\"leading\"",
        Alignment::Center => "\"center\"",
        Alignment::Trailing => "\"trailing\"",
    }
}

fn cells(cells: &[Vec<MarkdownRun>]) -> String {
    let mut out = Vec::new();
    for cell in cells {
        out.push(runs(cell));
    }
    format!("[{}]", out.join(","))
}

fn kind(kind: &BlockKind) -> String {
    match kind {
        BlockKind::Paragraph => "{\"type\":\"paragraph\"}".to_owned(),
        BlockKind::Heading { level } => format!("{{\"type\":\"heading\",\"level\":{level}}}"),
        BlockKind::Code { language, text } => format!(
            "{{\"type\":\"code\",\"language\":{},\"text\":{}}}",
            optional(language.as_deref()),
            string(text)
        ),
        BlockKind::Table(table) => {
            let mut aligned = Vec::new();
            for item in &table.alignments {
                aligned.push(alignment(*item));
            }
            let mut rows = Vec::new();
            for row in &table.rows {
                rows.push(cells(row));
            }
            format!(
                "{{\"type\":\"table\",\"alignments\":[{}],\"header\":{},\"rows\":[{}]}}",
                aligned.join(","),
                cells(&table.header),
                rows.join(",")
            )
        }
        BlockKind::ThematicBreak => "{\"type\":\"thematicBreak\"}".to_owned(),
        BlockKind::FrontMatter { text } => {
            format!("{{\"type\":\"frontMatter\",\"text\":{}}}", string(text))
        }
        BlockKind::Html { text } => format!("{{\"type\":\"html\",\"text\":{}}}", string(text)),
        BlockKind::Footnote { label } => {
            format!("{{\"type\":\"footnote\",\"label\":{}}}", string(label))
        }
    }
}

fn marker(marker: Option<MarkdownListMarker>) -> String {
    match marker {
        None => "null".to_owned(),
        Some(MarkdownListMarker::Bullet) => "\"bullet\"".to_owned(),
        Some(MarkdownListMarker::Ordered(number)) => format!("{{\"ordered\":{number}}}"),
        Some(MarkdownListMarker::Task { done }) => format!("{{\"task\":{done}}}"),
    }
}

fn block(block: &MarkdownBlock) -> String {
    format!(
        "{{\"kind\":{},\"runs\":{},\"quoteDepth\":{},\"listDepth\":{},\"marker\":{}}}",
        kind(&block.kind),
        runs(&block.runs),
        block.quote_depth,
        block.list_depth,
        marker(block.marker)
    )
}

fn target(target: Option<&MarkdownLinkTarget>) -> String {
    match target {
        None => "null".to_owned(),
        Some(MarkdownLinkTarget::Web(url)) => format!("{{\"web\":{}}}", string(url)),
        Some(MarkdownLinkTarget::OtherScheme(url)) => {
            format!("{{\"otherScheme\":{}}}", string(url))
        }
        Some(MarkdownLinkTarget::RepositoryPath { path, fragment }) => format!(
            "{{\"repositoryPath\":{},\"fragment\":{}}}",
            string(path),
            optional(fragment.as_deref())
        ),
        Some(MarkdownLinkTarget::Fragment(fragment)) => {
            format!("{{\"fragment\":{}}}", string(fragment))
        }
    }
}

/// Every run of the document, table cells included, in the order the Swift dump walked them.
fn all_runs(blocks: &[MarkdownBlock]) -> Vec<&MarkdownRun> {
    let mut out = Vec::new();
    for item in blocks {
        out.extend(&item.runs);
        if let BlockKind::Table(table) = &item.kind {
            for cell in &table.header {
                out.extend(cell);
            }
            for row in &table.rows {
                for cell in row {
                    out.extend(cell);
                }
            }
        }
    }
    out
}

fn document(source: &str, path: &str) -> String {
    let blocks = MarkdownDocument::parse(source).blocks;
    let mut lines = vec!["{".to_owned(), "\"blocks\": [".to_owned()];
    let mut rendered = Vec::new();
    for item in &blocks {
        rendered.push(block(item));
    }
    lines.push(rendered.join(",\n"));
    lines.push("],".to_owned());
    let mut anchors = Vec::new();
    for anchor in MarkdownDocument::heading_anchors(&blocks) {
        anchors.push(string(&anchor));
    }
    lines.push(format!("\"headingAnchors\": [{}],", anchors.join(",")));
    lines.push("\"links\": [".to_owned());
    let mut links = Vec::new();
    for item in all_runs(&blocks) {
        let Some(found) = &item.link else { continue };
        let resolved = MarkdownLinkTarget::resolve(&found.destination, Some(path));
        links.push(format!(
            "{{\"destination\":{},\"target\":{}}}",
            string(&found.destination),
            target(resolved.as_ref())
        ));
    }
    lines.push(links.join(",\n"));
    lines.push("]".to_owned());
    lines.push("}".to_owned());
    lines.join("\n") + "\n"
}

/// The first line where `actual` and `expected` differ, for a readable failure.
fn first_difference(actual: &str, expected: &str) -> String {
    for (number, (left, right)) in actual.lines().zip(expected.lines()).enumerate() {
        if left != right {
            return format!("line {}:\n  rust:  {left}\n  swift: {right}", number + 1);
        }
    }
    format!(
        "line counts differ: rust {}, swift {}",
        actual.lines().count(),
        expected.lines().count()
    )
}

#[test]
fn every_fixture_parses_as_the_swift_parser_did() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/markdown");
    let mut checked = 0;
    let mut failures = Vec::new();
    for entry in fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".md.in") else {
            continue;
        };
        let source = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
        let expected = fs::read_to_string(directory.join(format!("{stem}.json"))).unwrap();
        let actual = document(&source, &source_path(&name));
        checked += 1;
        if actual != expected {
            failures.push(format!("{stem}: {}", first_difference(&actual, &expected)));
        }
    }
    assert!(
        checked >= 190,
        "only {checked} fixtures found in {}",
        directory.display()
    );
    assert!(
        failures.is_empty(),
        "{} of {checked} fixtures differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

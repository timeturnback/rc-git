//! Ported from `MarkdownTests.swift`: blocks, anchors, lists, tables and inline runs.

use std::time::{Duration, Instant};

use rc_git_kit::markdown::{
    Alignment, BlockKind, MAXIMUM_INLINE_DEPTH, MAXIMUM_LIST_DEPTH, MarkdownBlock,
    MarkdownDocument, MarkdownLink, MarkdownListMarker, MarkdownRun, MarkdownStyle, MarkdownTable,
};

const PLAIN: MarkdownStyle = MarkdownStyle::PLAIN;
const EMPHASIS: MarkdownStyle = MarkdownStyle::EMPHASIS;
const STRONG: MarkdownStyle = MarkdownStyle::STRONG;
const CODE: MarkdownStyle = MarkdownStyle::CODE;

fn parse(source: &str) -> Vec<MarkdownBlock> {
    MarkdownDocument::parse(source).blocks
}

fn plain(runs: &[MarkdownRun]) -> String {
    let mut out = String::new();
    for run in runs {
        out.push_str(&run.text);
    }
    out
}

fn kinds(blocks: &[MarkdownBlock]) -> Vec<BlockKind> {
    blocks.iter().map(|block| block.kind.clone()).collect()
}

fn texts(runs: &[MarkdownRun]) -> Vec<&str> {
    runs.iter().map(|run| run.text.as_str()).collect()
}

fn styles(runs: &[MarkdownRun]) -> Vec<MarkdownStyle> {
    runs.iter().map(|run| run.style).collect()
}

fn destinations(runs: &[MarkdownRun]) -> Vec<&str> {
    runs.iter()
        .filter_map(|run| run.link.as_ref().map(|link| link.destination.as_str()))
        .collect()
}

fn first_run(
    blocks: &[MarkdownBlock],
    matches: impl Fn(&MarkdownRun) -> bool,
) -> Option<&MarkdownRun> {
    blocks
        .iter()
        .flat_map(|block| &block.runs)
        .find(|run| matches(run))
}

fn code(language: Option<&str>, text: &str) -> BlockKind {
    BlockKind::Code {
        language: language.map(str::to_owned),
        text: text.to_owned(),
    }
}

fn has_link_to(blocks: &[MarkdownBlock], destination: &str) -> bool {
    blocks
        .last()
        .is_some_and(|block| destinations(&block.runs).contains(&destination))
}

fn has_no_link(runs: &[MarkdownRun]) -> bool {
    runs.iter().all(|run| run.link.is_none())
}

mod blocks {
    use super::*;

    #[test]
    fn atx_headings_carry_their_level() {
        let blocks = parse("# One\n\n### Three ###\n");
        assert_eq!(
            kinds(&blocks),
            [
                BlockKind::Heading { level: 1 },
                BlockKind::Heading { level: 3 }
            ]
        );
        assert_eq!(plain(&blocks[0].runs), "One");
        assert_eq!(plain(&blocks[1].runs), "Three");
    }

    #[test]
    fn setext_underlines_retitle_the_paragraph() {
        let blocks = parse("Title\n=====\n\nOther\n-----\n");
        assert_eq!(
            kinds(&blocks),
            [
                BlockKind::Heading { level: 1 },
                BlockKind::Heading { level: 2 }
            ]
        );
        assert_eq!(plain(&blocks[0].runs), "Title");
    }

    #[test]
    fn an_underline_left_of_a_list_item_is_a_rule_not_a_heading() {
        let blocks = parse("- Foo\n---\n");
        assert_eq!(
            kinds(&blocks),
            [BlockKind::Paragraph, BlockKind::ThematicBreak]
        );
        assert_eq!(
            blocks.iter().map(|b| b.list_depth).collect::<Vec<_>>(),
            [1, 0]
        );
        assert_eq!(plain(&blocks[0].runs), "Foo");
    }

    #[test]
    fn an_underline_inside_a_list_item_still_retitles_it() {
        let blocks = parse("- Foo\n  ---\n");
        assert_eq!(kinds(&blocks), [BlockKind::Heading { level: 2 }]);
        assert_eq!(blocks[0].list_depth, 1);
    }

    #[test]
    fn an_underline_indented_to_code_depth_is_text() {
        for underline in ["---", "==="] {
            let blocks = parse(&format!("a\n    {underline}\n"));
            assert_eq!(kinds(&blocks), [BlockKind::Paragraph], "{underline}");
            assert_eq!(
                plain(&blocks[0].runs),
                format!("a {underline}"),
                "{underline}"
            );
        }
    }

    #[test]
    fn soft_breaks_fold_and_trailing_spaces_keep_the_break() {
        let blocks = parse("one\ntwo\n\nthree  \nfour\n");
        assert_eq!(blocks.len(), 2);
        assert_eq!(plain(&blocks[0].runs), "one two");
        assert_eq!(plain(&blocks[1].runs), "three\nfour");
    }

    #[test]
    fn a_soft_break_after_an_inline_construct_keeps_the_space() {
        let source = "Run `make`\nthen read [the docs](docs/a.md)\nnext *line*\nend";
        assert_eq!(
            plain(&parse(source)[0].runs),
            "Run make then read the docs next line end"
        );
    }

    #[test]
    fn a_hard_break_after_a_link_keeps_the_break() {
        assert_eq!(plain(&parse("a [x](y)  \nb")[0].runs), "a x\nb");
    }

    #[test]
    fn a_break_tag_at_the_end_of_a_line_gains_no_space() {
        assert_eq!(plain(&parse("a<br>\nb")[0].runs), "a\nb");
    }

    #[test]
    fn thematic_breaks_are_not_list_items() {
        let blocks = parse("a\n\n---\n\n* * *\n");
        assert_eq!(
            kinds(&blocks)[1..],
            [BlockKind::ThematicBreak, BlockKind::ThematicBreak]
        );
    }

    #[test]
    fn fenced_code_keeps_its_language_and_body() {
        assert_eq!(
            parse("```rust\nfn main() {}\n```\n"),
            [MarkdownBlock::new(code(Some("rust"), "fn main() {}"))]
        );
    }

    #[test]
    fn fenced_code_is_not_read_as_markdown() {
        let blocks = parse("```\n# not a heading\n- not a list\n```\n");
        assert_eq!(
            kinds(&blocks),
            [code(None, "# not a heading\n- not a list")]
        );
    }

    #[test]
    fn a_shorter_fence_does_not_close_a_longer_one() {
        let blocks = parse("````md\n```js\nx\n```\n````\n\nafter\n");
        let mut after = MarkdownBlock::new(BlockKind::Paragraph);
        after.runs = vec![MarkdownRun::plain("after")];
        assert_eq!(
            blocks,
            [MarkdownBlock::new(code(Some("md"), "```js\nx\n```")), after]
        );
    }

    #[test]
    fn a_fence_line_with_an_info_string_does_not_close() {
        assert_eq!(
            parse("```\n```js\nx\n```\n"),
            [MarkdownBlock::new(code(None, "```js\nx"))]
        );
    }

    #[test]
    fn tilde_fences_open_code() {
        assert_eq!(
            parse("~~~sh\nls\n~~~\n"),
            [MarkdownBlock::new(code(Some("sh"), "ls"))]
        );
    }

    #[test]
    fn fenced_code_keeps_leading_greater_than_signs() {
        let repl = parse("```python\n>>> import os\n```\n");
        assert_eq!(kinds(&repl), [code(Some("python"), ">>> import os")]);
        let quoted = parse("```text\n> quoted line\n```\n");
        assert_eq!(kinds(&quoted), [code(Some("text"), "> quoted line")]);
    }

    #[test]
    fn a_quoted_fence_keeps_markers_past_its_own_depth() {
        let blocks = parse("> ```\n> >>> x\n> ```\n");
        assert_eq!(kinds(&blocks), [code(None, ">>> x")]);
        assert_eq!(blocks[0].quote_depth, 1);
    }

    #[test]
    fn a_quoted_fence_ends_with_the_quote() {
        let blocks = parse("> ```\n> code\n\noutside\n");
        assert_eq!(kinds(&blocks), [code(None, "code"), BlockKind::Paragraph]);
        assert_eq!(
            blocks.iter().map(|b| b.quote_depth).collect::<Vec<_>>(),
            [1, 0]
        );
        assert_eq!(plain(&blocks[1].runs), "outside");
    }

    #[test]
    fn a_definition_after_an_unclosed_quoted_fence_still_defines() {
        let blocks = parse("> ```\n> code\n\n[a]: /real\n\nSee [a].\n");
        assert!(has_link_to(&blocks, "/real"));
    }

    #[test]
    fn indented_code_stops_at_the_next_paragraph() {
        let blocks = parse("intro\n\n    code line\n\nafter\n");
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[1].kind, code(None, "code line"));
        assert_eq!(plain(&blocks[2].runs), "after");
    }

    #[test]
    fn indented_code_is_code_whatever_it_starts_with() {
        for first in ["# x", "- x", "[a]: https://example.com"] {
            let blocks = parse(&format!("Text\n\n    {first}\n    more\n"));
            assert_eq!(
                kinds(&blocks),
                [BlockKind::Paragraph, code(None, &format!("{first}\nmore"))]
            );
        }
    }

    #[test]
    fn an_indented_block_start_continues_the_paragraph() {
        let blocks = parse("Text\n    # not heading\n");
        assert_eq!(kinds(&blocks), [BlockKind::Paragraph]);
        assert_eq!(plain(&blocks[0].runs), "Text # not heading");
    }

    #[test]
    fn block_quotes_carry_their_depth() {
        let blocks = parse("> one\n\n>> two\n");
        assert_eq!(
            blocks.iter().map(|b| b.quote_depth).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(plain(&blocks[1].runs), "two");
    }

    #[test]
    fn an_unmarked_line_continues_the_quoted_paragraph() {
        let blocks = parse("> quote\ncontinued\n");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].quote_depth, 1);
        assert_eq!(plain(&blocks[0].runs), "quote continued");
    }

    #[test]
    fn a_lazy_equals_line_is_text_not_an_underline() {
        let blocks = parse("> foo\n===\n");
        assert_eq!(kinds(&blocks), [BlockKind::Paragraph]);
        assert_eq!(blocks[0].quote_depth, 1);
        assert_eq!(plain(&blocks[0].runs), "foo ===");
    }

    #[test]
    fn an_unmarked_block_start_leaves_the_quote() {
        let blocks = parse("> quote\n# Title\n");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].quote_depth, 0);
        assert_eq!(blocks[1].kind, BlockKind::Heading { level: 1 });
    }

    fn html(text: &str) -> BlockKind {
        BlockKind::Html {
            text: text.to_owned(),
        }
    }

    #[test]
    fn html_blocks_stay_raw() {
        let blocks = parse("<div class=\"x\">\ntext\n</div>\n");
        assert_eq!(kinds(&blocks), [html("<div class=\"x\">\ntext\n</div>")]);
    }

    #[test]
    fn an_html_comment_runs_past_a_blank_line_to_its_closer() {
        let blocks = parse("<!-- note\n\n# hidden -->\nafter\n");
        assert_eq!(
            kinds(&blocks),
            [html("<!-- note\n\n# hidden -->"), BlockKind::Paragraph]
        );
    }

    #[test]
    fn a_one_line_html_comment_ends_on_its_own_line() {
        let blocks = parse("<!-- note -->\n# Title\n");
        assert_eq!(
            kinds(&blocks),
            [html("<!-- note -->"), BlockKind::Heading { level: 1 }]
        );
    }

    #[test]
    fn a_pre_block_runs_past_a_blank_line_to_its_closing_tag() {
        let blocks = parse("<PRE>\nline\n\nmore\n</Pre>\n\ntext\n");
        assert_eq!(
            kinds(&blocks),
            [html("<PRE>\nline\n\nmore\n</Pre>"), BlockKind::Paragraph]
        );
    }

    #[test]
    fn a_plain_block_tag_still_ends_at_a_blank_line() {
        let blocks = parse("<div>\nx\n\n# Title\n");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].kind, BlockKind::Heading { level: 1 });
    }

    #[test]
    fn an_html_block_in_a_list_item_leaves_the_marker_out_of_its_text() {
        let blocks = parse("- <details>\n  <summary>More</summary>\n");
        assert_eq!(kinds(&blocks), [html("<details>\n<summary>More</summary>")]);
        assert_eq!(blocks[0].marker, Some(MarkdownListMarker::Bullet));
    }

    #[test]
    fn front_matter_is_its_own_block() {
        let blocks = parse("---\nmodule: core\n---\n\n# Title\n");
        assert_eq!(
            kinds(&blocks),
            [
                BlockKind::FrontMatter {
                    text: "module: core".to_owned()
                },
                BlockKind::Heading { level: 1 }
            ]
        );
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_a_leading_heading() {
        let blocks = parse("\u{feff}# Title\n");
        assert_eq!(kinds(&blocks), [BlockKind::Heading { level: 1 }]);
        assert_eq!(plain(&blocks[0].runs), "Title");
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_front_matter() {
        let blocks = parse("\u{feff}---\ntitle: x\n---\n");
        assert_eq!(
            kinds(&blocks),
            [BlockKind::FrontMatter {
                text: "title: x".to_owned()
            }]
        );
    }

    #[test]
    fn a_leading_rule_without_a_closer_is_not_front_matter() {
        let blocks = parse("---\n\ntext\n");
        assert_eq!(
            kinds(&blocks),
            [BlockKind::ThematicBreak, BlockKind::Paragraph]
        );
    }

    #[test]
    fn footnotes_split_into_marker_and_definition() {
        let blocks = parse("See it[^a].\n\n[^a]: Because.\n");
        let marker = first_run(&blocks, |run| {
            run.style.contains(MarkdownStyle::FOOTNOTE_MARKER)
        });
        assert_eq!(marker.map(|run| run.text.as_str()), Some("a"));
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[1].kind,
            BlockKind::Footnote {
                label: "a".to_owned()
            }
        );
        assert_eq!(plain(&blocks[1].runs), "Because.");
    }

    #[test]
    fn footnote_anchor_matches_marker() {
        let blocks = parse("See[^Note].\n\n[^Note]: text\n");
        let marker = first_run(&blocks, |run| {
            run.style.contains(MarkdownStyle::FOOTNOTE_MARKER)
        })
        .unwrap();
        let destination = &marker.link.as_ref().unwrap().destination;
        assert_eq!(destination, "#fn-note");
        let BlockKind::Footnote { label } = &blocks.last().unwrap().kind else {
            panic!("expected a footnote block, got {:?}", blocks.last());
        };
        assert_eq!(
            MarkdownDocument::anchor(&destination[1..]),
            MarkdownDocument::footnote_anchor(label)
        );
        assert_eq!(
            MarkdownDocument::anchor("fn-note"),
            MarkdownDocument::footnote_anchor("Note")
        );
    }

    #[test]
    fn prose_after_a_bracketed_label_stays_a_paragraph() {
        let blocks = parse("[Note]: this is not done\n\nSee [Note].\n");
        assert_eq!(blocks.len(), 2);
        assert_eq!(plain(&blocks[0].runs), "[Note]: this is not done");
        assert!(has_no_link(&blocks[1].runs));
    }

    #[test]
    fn a_fence_line_with_info_does_not_close_the_fence() {
        let blocks = parse("```\n```js\n[a]: https://example.com\n```\n\nSee [a].\n");
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0].kind,
            code(None, "```js\n[a]: https://example.com")
        );
        assert!(has_no_link(&blocks[1].runs));
    }

    #[test]
    fn a_definition_after_a_fence_with_an_info_line_still_defines() {
        let blocks = parse("```\n```js\n```\n\n[a]: /real\n\nSee [a].\n");
        assert_eq!(blocks.len(), 2);
        assert!(has_link_to(&blocks, "/real"));
    }

    #[test]
    fn indented_and_continuation_definitions_define_nothing() {
        let indented = parse("    [a]: /x\n\nSee [a].\n");
        assert!(has_no_link(&indented.last().unwrap().runs));
        let continued = parse("Text\n[a]: /x\n\nSee [a].\n");
        assert_eq!(plain(&continued[0].runs), "Text [a]: /x");
        assert!(has_no_link(&continued.last().unwrap().runs));
    }

    #[test]
    fn crlf_lines_parse_like_lf() {
        let lf = "---\nmodule: core\n---\n\n### Three ###\n\nthree  \nfour\n\n\
                  ```rust\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        assert_eq!(
            MarkdownDocument::parse(&lf.replace('\n', "\r\n")),
            MarkdownDocument::parse(lf)
        );
    }
}

mod anchors {
    use super::*;

    fn heading_anchors(source: &str) -> Vec<String> {
        MarkdownDocument::heading_anchors(&parse(source))
    }

    #[test]
    fn headings_slug_like_github() {
        assert_eq!(
            MarkdownDocument::anchor("Working agreement"),
            "working-agreement"
        );
        assert_eq!(
            MarkdownDocument::anchor("`Oid` keys, and why"),
            "oid-keys-and-why"
        );
        assert_eq!(MarkdownDocument::anchor("Step 1: open"), "step-1-open");
    }

    #[test]
    fn a_fragment_and_its_heading_agree() {
        assert_eq!(
            MarkdownDocument::anchor("Invariants"),
            MarkdownDocument::anchor("invariants")
        );
    }

    /// Swift lowercases scalar by scalar and keeps a letter's combining marks with it; the
    /// port must slug non-ASCII headings the same way or a copied GitHub link stops landing.
    #[test]
    fn non_ascii_headings_slug_like_swift() {
        assert_eq!(MarkdownDocument::anchor("Tiếng Việt"), "tiếng-việt");
        assert_eq!(MarkdownDocument::anchor("ΣΑΣ"), "σασ");
        assert_eq!(
            MarkdownDocument::anchor("e\u{301}te\u{301}!"),
            "e\u{301}te\u{301}"
        );
        assert_eq!(MarkdownDocument::anchor("日本語 ٣ ½"), "日本語-٣-½");
    }

    #[test]
    fn repeated_headings_take_numbered_suffixes() {
        assert_eq!(
            heading_anchors("## Fixed\n\n## Fixed\n\n## Fixed\n"),
            ["fixed", "fixed-1", "fixed-2"]
        );
    }

    #[test]
    fn a_suffix_already_taken_is_skipped() {
        assert_eq!(
            heading_anchors("# Foo\n\n# Foo-1\n\n# Foo\n"),
            ["foo", "foo-1", "foo-2"]
        );
        assert_eq!(
            heading_anchors("# Usage\n\n# Usage\n\n# Usage\n\n# Usage 1\n"),
            ["usage", "usage-1", "usage-2", "usage-1-1"]
        );
    }

    #[test]
    fn a_heading_never_takes_a_footnotes_anchor() {
        assert_eq!(
            heading_anchors("# fn-a\n\nSee[^a].\n\n[^a]: Note.\n"),
            ["fn-a-1"]
        );
    }

    #[test]
    fn a_heading_with_no_slug_claims_nothing() {
        assert_eq!(
            heading_anchors("# !!!\n\n# Foo\n\n# !!!\n"),
            ["", "foo", ""]
        );
    }
}

mod preview {
    use super::*;

    #[test]
    fn only_markdown_within_the_limit_is_previewable() {
        let limit = MarkdownDocument::PREVIEW_LIMIT;
        assert!(MarkdownDocument::is_previewable("README.md", limit));
        assert!(MarkdownDocument::is_previewable("notes.markdown", 0));
        assert!(!MarkdownDocument::is_previewable("README.md", limit + 1));
        assert!(!MarkdownDocument::is_previewable("main.rs", 10));
    }

    #[test]
    fn a_placeholder_shows_only_above_the_threshold() {
        let threshold = MarkdownDocument::BUILDING_PLACEHOLDER_THRESHOLD;
        assert_eq!(
            MarkdownDocument::building_placeholder("a.md", threshold),
            None
        );
        assert_eq!(
            MarkdownDocument::building_placeholder("a.md", threshold + 1).as_deref(),
            Some("Building the preview of a.md…")
        );
    }
}

mod lists {
    use super::*;

    fn markers(blocks: &[MarkdownBlock]) -> Vec<Option<MarkdownListMarker>> {
        blocks.iter().map(|block| block.marker).collect()
    }

    #[test]
    fn bullets_and_ordinals_carry_one_marker_per_item() {
        let blocks = parse("- one\n- two\n\n1. first\n2. second\n");
        let bullet = Some(MarkdownListMarker::Bullet);
        assert_eq!(
            markers(&blocks),
            [
                bullet,
                bullet,
                Some(MarkdownListMarker::Ordered(1)),
                Some(MarkdownListMarker::Ordered(2))
            ]
        );
        assert!(blocks.iter().all(|block| block.list_depth == 1));
    }

    #[test]
    fn indentation_nests_and_unnests() {
        let blocks = parse("- one\n  - inner\n- two\n");
        assert_eq!(
            blocks.iter().map(|b| b.list_depth).collect::<Vec<_>>(),
            [1, 2, 1]
        );
    }

    #[test]
    fn a_line_of_markers_nests_no_deeper_than_the_cap() {
        let blocks = parse(&("+ ".repeat(10_000) + "x"));
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].list_depth, MAXIMUM_LIST_DEPTH);
        assert!(plain(&blocks[0].runs).ends_with("+ + x"));
    }

    #[test]
    fn a_second_paragraph_stays_inside_the_item() {
        let blocks = parse("- one\n\n  more\n\nafter\n");
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[1].list_depth, 1);
        assert_eq!(blocks[1].marker, None);
        assert_eq!(blocks[2].list_depth, 0);
    }

    #[test]
    fn task_items_record_whether_they_are_done() {
        let blocks = parse("- [ ] open\n- [x] done\n");
        assert_eq!(
            markers(&blocks),
            [
                Some(MarkdownListMarker::Task { done: false }),
                Some(MarkdownListMarker::Task { done: true })
            ]
        );
        assert_eq!(plain(&blocks[1].runs), "done");
    }

    #[test]
    fn an_empty_item_leaves_no_marker_on_the_next_block() {
        for source in [
            "Intro\n\n*\n\n# Heading\n",
            "- [ ] \n\n## Next\n",
            "-\n\nparagraph after\n",
            "> -\n\nafter the quote\n",
        ] {
            let blocks = parse(source);
            let last = blocks.last().unwrap();
            assert_eq!(last.list_depth, 0, "{source:?}");
            assert_eq!(last.marker, None, "{source:?}");
        }
    }

    #[test]
    fn an_empty_item_keeps_its_marker_for_its_indented_content() {
        let blocks = parse("-\n  inside\n");
        assert_eq!(markers(&blocks), [Some(MarkdownListMarker::Bullet)]);
        assert_eq!(blocks[0].list_depth, 1);
    }

    #[test]
    fn code_inside_an_item_keeps_the_bullet() {
        let blocks = parse("- item\n\n  ```sh\n  ls\n  ```\n");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].kind, code(Some("sh"), "ls"));
        assert_eq!(blocks[1].list_depth, 1);
    }
}

mod tables {
    use super::*;

    fn table(source: &str) -> MarkdownTable {
        for block in parse(source) {
            if let BlockKind::Table(table) = block.kind {
                return table;
            }
        }
        unreachable!("no table in {source:?}");
    }

    fn row_texts(row: &[Vec<MarkdownRun>]) -> Vec<String> {
        row.iter().map(|cell| plain(cell)).collect()
    }

    #[test]
    fn header_delimiter_and_rows_become_a_table() {
        let parsed = table("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n");
        assert_eq!(parsed.column_count(), 2);
        assert_eq!(row_texts(&parsed.header), ["a", "b"]);
        let rows: Vec<_> = parsed.rows.iter().map(|row| row_texts(row)).collect();
        assert_eq!(rows, [["1", "2"], ["3", "4"]]);
    }

    #[test]
    fn colons_set_the_alignment() {
        let parsed = table("| a | b | c | d |\n|:--|--:|:-:|---|\n| 1 | 2 | 3 | 4 |\n");
        assert_eq!(
            parsed.alignments,
            [
                Alignment::Leading,
                Alignment::Trailing,
                Alignment::Center,
                Alignment::Natural
            ]
        );
    }

    #[test]
    fn the_shortest_delimiter_makes_a_one_column_table() {
        let parsed = table("a\n|-\n");
        assert_eq!(parsed.column_count(), 1);
        assert_eq!(row_texts(&parsed.header), ["a"]);
        assert_eq!(parsed.alignments, [Alignment::Natural]);
    }

    #[test]
    fn short_rows_are_padded_to_the_column_count() {
        let parsed = table("a | b | c\n--- | --- | ---\n1\n");
        assert_eq!(row_texts(&parsed.rows[0]), ["1", "", ""]);
    }

    #[test]
    fn an_escaped_pipe_stays_inside_its_cell() {
        let parsed = table("| a | b |\n|---|---|\n| x \\| y | z |\n");
        assert_eq!(row_texts(&parsed.rows[0]), ["x | y", "z"]);
    }

    #[test]
    fn cells_keep_their_inline_formatting() {
        let parsed = table("| a |\n|---|\n| [doc](x.md) |\n");
        assert_eq!(destinations(&parsed.rows[0][0]), ["x.md"]);
    }

    #[test]
    fn a_table_ends_at_the_next_block() {
        let blocks = parse("| a |\n|---|\n| 1 |\n\n# After\n");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].kind, BlockKind::Heading { level: 1 });
    }
}

mod inline {
    use super::*;

    fn runs(source: &str) -> Vec<MarkdownRun> {
        parse(source)
            .into_iter()
            .next()
            .map(|block| block.runs)
            .unwrap_or_default()
    }

    fn first_style(source: &str) -> Option<MarkdownStyle> {
        runs(source).first().map(|run| run.style)
    }

    #[test]
    fn emphasis_strong_and_both_combine() {
        assert_eq!(first_style("*a*"), Some(EMPHASIS));
        assert_eq!(first_style("**a**"), Some(STRONG));
        assert_eq!(first_style("***a***"), Some(STRONG.union(EMPHASIS)));
        assert_eq!(first_style("~~a~~"), Some(MarkdownStyle::STRIKETHROUGH));
    }

    #[test]
    fn nested_emphasis_keeps_both_styles() {
        let parsed = runs("**bold _and italic_**");
        assert_eq!(plain(&parsed), "bold and italic");
        assert_eq!(parsed.last().unwrap().style, STRONG.union(EMPHASIS));
    }

    #[test]
    fn strong_inside_emphasis_keeps_both() {
        for source in [
            "*This is **important** text*",
            "_This is __important__ text_",
        ] {
            let parsed = runs(source);
            assert_eq!(plain(&parsed), "This is important text", "{source}");
            let important = parsed.iter().find(|run| run.text == "important").unwrap();
            assert_eq!(important.style, EMPHASIS.union(STRONG), "{source}");
            assert!(
                parsed.iter().all(|run| run.style.contains(EMPHASIS)),
                "{source}"
            );
        }
    }

    #[test]
    fn nesting_past_the_depth_limit_stays_literal() {
        let depth = 20;
        let links = runs(&("[".repeat(depth) + "x" + &"](u)".repeat(depth)));
        let literal = depth - MAXIMUM_INLINE_DEPTH;
        assert!(links[0].text.starts_with(&("[".repeat(literal) + "x")));
        assert!(
            links
                .iter()
                .all(|run| run.link.as_ref().is_some_and(|l| l.destination == "u"))
        );

        let markers: Vec<&str> = (0..depth)
            .map(|i| if i % 2 == 0 { "*" } else { "_" })
            .collect();
        let opening: String = markers.iter().map(|m| [*m, "a "].concat()).collect();
        let closing: String = markers.iter().rev().map(|m| [" a", *m].concat()).collect();
        let emphasized = runs(&(opening + "x" + &closing));
        let innermost: String = markers[depth - literal..]
            .iter()
            .map(|m| [*m, "a "].concat())
            .collect::<String>()
            + "x";
        assert!(
            emphasized
                .iter()
                .any(|run| run.text.starts_with(&format!("a {innermost}")))
        );
        assert!(!emphasized.is_empty() && emphasized.iter().all(|run| run.style == EMPHASIS));
    }

    /// Parses `unit` repeated to 64 KiB and to 256 KiB and compares the two. Four times the
    /// input takes four times as long when linear and sixteen when quadratic; the bound sits at
    /// eight plus a floor for noise. Each size keeps its fastest of three parses, so a load
    /// spike that preempts one parse cannot flip the comparison.
    fn assert_linear_parse(unit: &str) {
        let parse_time = |kibibytes: usize| {
            let text = unit.repeat(kibibytes * 1024 / unit.len());
            let mut best = Duration::MAX;
            for _ in 0..3 {
                let start = Instant::now();
                let _ = MarkdownDocument::parse(&text);
                best = best.min(start.elapsed());
            }
            best
        };
        let small = parse_time(64);
        let large = parse_time(256);
        assert!(
            large < small * 8 + Duration::from_millis(50),
            "{unit:?}: 64 KiB took {small:?}, 256 KiB took {large:?}"
        );
    }

    /// Each unit leaves an opener unmatched; a closer search per opener that rescans to the end
    /// of the block is quadratic.
    #[test]
    fn unmatched_openers_parse_in_linear_time() {
        for unit in [
            "x [y ",
            "a *b ",
            "[a](b ",
            "x<y ",
            "x<y",
            "*Note: x\n",
            "x [y](z(w ",
        ] {
            assert_linear_parse(unit);
        }
    }

    /// Each unit closes one emphasis and leaves one opener unmatched.
    #[test]
    fn paragraphs_of_emphasis_units_parse_in_linear_time() {
        assert_linear_parse("*x *y* ");
    }

    #[test]
    fn a_run_inside_a_word_does_not_close_the_outer_emphasis_early() {
        let parsed = runs("*a**b**c*");
        assert_eq!(texts(&parsed), ["a", "b", "c"]);
        assert_eq!(
            styles(&parsed),
            [EMPHASIS, EMPHASIS.union(STRONG), EMPHASIS]
        );
    }

    #[test]
    fn a_closer_pairs_with_the_nearest_opener() {
        let parsed = runs("*a **b c*");
        assert_eq!(texts(&parsed), ["*a *", "b c"]);
        assert_eq!(styles(&parsed), [PLAIN, EMPHASIS]);
    }

    #[test]
    fn leftover_delimiters_stay_literal_beside_the_matched_ones() {
        let strong = runs("**a*");
        assert_eq!(texts(&strong), ["*", "a"]);
        assert_eq!(styles(&strong), [PLAIN, EMPHASIS]);
        let nested = runs("***a** b*");
        assert_eq!(texts(&nested), ["a", " b"]);
        assert_eq!(styles(&nested), [EMPHASIS.union(STRONG), EMPHASIS]);
    }

    #[test]
    fn underscores_follow_their_flanking_rules() {
        let parsed = runs("_a_b_ and __x__");
        assert_eq!(texts(&parsed), ["a_b", " and ", "x"]);
        assert_eq!(styles(&parsed), [EMPHASIS, PLAIN, STRONG]);
        assert_eq!(plain(&runs("a _b_c")), "a _b_c");
    }

    #[test]
    fn underscores_inside_a_word_are_literal() {
        let parsed = runs("snake_case_name");
        assert_eq!(plain(&parsed), "snake_case_name");
        assert!(parsed.iter().all(|run| run.style.is_empty()));
    }

    #[test]
    fn an_unclosed_delimiter_stays_literal() {
        assert_eq!(plain(&runs("2 * 3 = 6")), "2 * 3 = 6");
        assert_eq!(plain(&runs("**never closed")), "**never closed");
    }

    #[test]
    fn code_spans_swallow_their_delimiters() {
        assert_eq!(first_style("`a * b`"), Some(CODE));
        assert_eq!(runs("`a * b`")[0].text, "a * b");
        assert_eq!(runs("`` a ` b ``")[0].text, "a ` b");
    }

    #[test]
    fn an_unclosed_backtick_run_stays_literal_as_a_whole() {
        let parsed = runs("x ``` y `` z");
        assert_eq!(plain(&parsed), "x ``` y `` z");
        assert!(parsed.iter().all(|run| !run.style.contains(CODE)));
    }

    #[test]
    fn emphasis_does_not_close_inside_a_code_span() {
        let parsed = runs("*a `x*y` b*");
        assert_eq!(texts(&parsed), ["a ", "x*y", " b"]);
        assert_eq!(styles(&parsed), [EMPHASIS, EMPHASIS.union(CODE), EMPHASIS]);
        let underscores = runs("_see `__init__` here_");
        assert_eq!(texts(&underscores), ["see ", "__init__", " here"]);
        assert!(underscores.iter().all(|run| run.style.contains(EMPHASIS)));
    }

    #[test]
    fn link_text_does_not_close_inside_a_code_span() {
        let parsed = runs("[see `]` here](x.md)");
        assert_eq!(plain(&parsed), "see ] here");
        assert_eq!(destinations(&parsed), ["x.md"; 3]);
    }

    #[test]
    fn an_escaped_backtick_opens_no_code_span_for_the_closer_search() {
        let parsed = runs("*a \\` b `c* d`");
        assert_eq!(texts(&parsed), ["*a ` b ", "c* d"]);
        assert_eq!(styles(&parsed), [PLAIN, CODE]);
    }

    #[test]
    fn escapes_produce_the_literal_character() {
        assert_eq!(plain(&runs("\\*not emphasis\\*")), "*not emphasis*");
    }

    #[test]
    fn inline_links_carry_destination_and_title() {
        let link = runs("[docs](docs/x.md \"Title\")")[0].link.clone();
        let expected = MarkdownLink {
            destination: "docs/x.md".to_owned(),
            title: Some("Title".to_owned()),
        };
        assert_eq!(link, Some(expected));
    }

    #[test]
    fn inline_and_reference_titles_agree_for_every_quoting() {
        for (title, expected) in [("\"T\"", "T"), ("'T'", "T"), ("(T)", "T"), ("\"\"", "")] {
            let inline = runs(&format!("[a](x {title})"))[0].link.clone();
            let reference = runs(&format!("[a][r]\n\n[r]: x {title}\n"))[0].link.clone();
            let link = MarkdownLink {
                destination: "x".to_owned(),
                title: Some(expected.to_owned()),
            };
            assert_eq!(inline, Some(link), "{title}");
            assert_eq!(reference, inline, "{title}");
        }
    }

    #[test]
    fn reference_links_resolve_in_either_order() {
        assert_eq!(
            destinations(&runs("[a][ref]\n\n[ref]: https://example.com\n")),
            ["https://example.com"]
        );
        let collapsed = parse("[ref]: https://example.com\n\n[ref][]\n");
        assert_eq!(destinations(&collapsed[0].runs), ["https://example.com"]);
    }

    #[test]
    fn a_definition_inside_a_fence_defines_nothing() {
        let plain_fence = parse("```\n[ref]: https://x\n```\n\n[a][ref]\n");
        assert!(has_no_link(&plain_fence.last().unwrap().runs));
        let nested = parse("```md\n```js\n[ref]: https://sample.example\n```\n\n[docs][ref]\n");
        assert!(has_no_link(&nested.last().unwrap().runs));
    }

    #[test]
    fn a_definition_inside_a_list_item_resolves() {
        assert!(has_link_to(&parse("- item\n\n    [a]: /x\n\n[a]\n"), "/x"));
        assert!(has_link_to(&parse("- [b]: /y\n\n[b]\n"), "/y"));
        let code = parse("- item\n\n        [c]: /z\n\n[c]\n");
        assert!(has_no_link(&code.last().unwrap().runs));
    }

    #[test]
    fn the_first_definition_of_a_label_wins() {
        let parsed = runs("[a][ref]\n\n[ref]: https://one\n[ref]: https://two\n");
        assert_eq!(destinations(&parsed), ["https://one"]);
    }

    #[test]
    fn labels_match_ignoring_case() {
        assert_eq!(
            destinations(&runs("[a][REF]\n\n[ref]: https://example.com\n")),
            ["https://example.com"]
        );
    }

    #[test]
    fn autolinks_and_bare_urls_become_links() {
        assert_eq!(
            destinations(&runs("<https://example.com>")),
            ["https://example.com"]
        );
        assert_eq!(destinations(&runs("<a@b.com>")), ["mailto:a@b.com"]);
        let bare = runs("see https://example.com/x. done");
        assert_eq!(destinations(&bare), ["https://example.com/x"]);
        assert_eq!(plain(&bare), "see https://example.com/x. done");
    }

    #[test]
    fn autolink_scheme_follows_the_common_mark_rule() {
        let digits = runs("get <s3://bucket/key> now");
        assert_eq!(destinations(&digits), ["s3://bucket/key"]);
        assert_eq!(plain(&digits), "get s3://bucket/key now");
        assert_eq!(destinations(&runs("<a.b-c+d://x>")), ["a.b-c+d://x"]);
        assert!(has_no_link(&runs("<3d://x>")));
        assert!(has_no_link(&runs("<é1://x>")));
        let longest = "a".repeat(32);
        assert_eq!(
            destinations(&runs(&format!("<{longest}://x>"))),
            [format!("{longest}://x")]
        );
        assert!(has_no_link(&runs(&format!("<a{longest}://x>"))));
    }

    #[test]
    fn bare_www_url_gets_https_whatever_its_case() {
        let upper = runs("Visit WWW.Example.com today");
        assert_eq!(destinations(&upper), ["https://WWW.Example.com"]);
        assert_eq!(plain(&upper), "Visit WWW.Example.com today");
        assert_eq!(
            destinations(&runs("Visit www.example.com today")),
            ["https://www.example.com"]
        );
    }

    #[test]
    fn bare_url_prefixes_match_in_either_case() {
        for url in ["HTTPS://X.COM", "Http://x.com", "https://x.com"] {
            assert_eq!(
                destinations(&runs(&format!("go {url} now"))),
                [url],
                "{url}"
            );
        }
        assert!(has_no_link(&runs("go ftp://x.com now")));
    }

    #[test]
    fn bare_url_keeps_a_balanced_closing_parenthesis() {
        let wiki = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        let balanced = runs(&format!("See {wiki} now"));
        assert_eq!(destinations(&balanced), [wiki]);
        assert_eq!(plain(&balanced), format!("See {wiki} now"));
        let wrapped = runs(&format!("(see {wiki})"));
        assert_eq!(destinations(&wrapped), [wiki]);
        assert_eq!(plain(&wrapped), format!("(see {wiki})"));
        assert_eq!(
            destinations(&runs("(see https://example.com/x)")),
            ["https://example.com/x"]
        );
    }

    #[test]
    fn images_keep_their_alt_text_and_source() {
        let image = &runs("![logo](assets/logo.png)")[0];
        assert!(image.is_image);
        assert_eq!(image.text, "logo");
        assert_eq!(
            destinations(std::slice::from_ref(image)),
            ["assets/logo.png"]
        );
    }

    #[test]
    fn image_inside_a_link_follows_the_link() {
        let parsed = runs("[![CI](https://ci.example/badge.svg)](https://ci.example/actions)");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].is_image);
        assert_eq!(parsed[0].text, "CI");
        assert_eq!(destinations(&parsed), ["https://ci.example/actions"]);
    }

    #[test]
    fn image_without_alt_inside_a_link_shows_its_own_source() {
        let parsed = runs("[![](a.png)](https://x)");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].is_image);
        assert_eq!(parsed[0].text, "a.png");
        assert_eq!(destinations(&parsed), ["https://x"]);
    }

    #[test]
    fn inline_tags_are_dropped_and_breaks_survive() {
        assert_eq!(plain(&runs("a <span>b</span> c")), "a b c");
        assert_eq!(plain(&runs("a<br>b")), "a\nb");
        assert_eq!(
            plain(&runs("a <img src=\"x.png\" alt='y' width=10 hidden/> b")),
            "a  b"
        );
    }

    #[test]
    fn prose_between_angle_brackets_stays_literal() {
        for prose in [
            "Returns Result<T, E> on error",
            "if a<b && c>d then",
            "Use it when n<10 and m>5 holds.",
            "a<1>b",
            "a</b c>d",
        ] {
            assert_eq!(plain(&runs(prose)), prose);
        }
    }

    #[test]
    fn link_text_keeps_its_own_formatting() {
        let parsed = runs("[**bold** plain](x.md)");
        assert_eq!(destinations(&parsed), ["x.md"; 2]);
        assert_eq!(parsed[0].style, STRONG);
        assert_eq!(plain(&parsed), "bold plain");
    }
}

import Testing

@testable import RcGitKit

private func plain(_ runs: [MarkdownRun]) -> String { runs.map(\.text).joined() }

private func firstRun(_ blocks: [MarkdownBlock], where match: (MarkdownRun) -> Bool)
    -> MarkdownRun?
{
    for block in blocks {
        if let run = block.runs.first(where: match) { return run }
    }
    return nil
}

@Suite struct MarkdownBlockTests {
    @Test func atxHeadingsCarryTheirLevel() {
        let blocks = MarkdownDocument.parse("# One\n\n### Three ###\n").blocks
        #expect(blocks.count == 2)
        #expect(blocks[0].kind == .heading(level: 1))
        #expect(plain(blocks[0].runs) == "One")
        #expect(blocks[1].kind == .heading(level: 3))
        #expect(plain(blocks[1].runs) == "Three")
    }

    @Test func setextUnderlinesRetitleTheParagraph() {
        let blocks = MarkdownDocument.parse("Title\n=====\n\nOther\n-----\n").blocks
        #expect(blocks[0].kind == .heading(level: 1))
        #expect(plain(blocks[0].runs) == "Title")
        #expect(blocks[1].kind == .heading(level: 2))
    }

    @Test func softBreaksFoldAndTrailingSpacesKeepTheBreak() {
        let blocks = MarkdownDocument.parse("one\ntwo\n\nthree  \nfour\n").blocks
        #expect(plain(blocks[0].runs) == "one two")
        #expect(plain(blocks[1].runs) == "three\nfour")
    }

    @Test func thematicBreaksAreNotListItems() {
        let blocks = MarkdownDocument.parse("a\n\n---\n\n* * *\n").blocks
        #expect(blocks[1].kind == .thematicBreak)
        #expect(blocks[2].kind == .thematicBreak)
    }

    @Test func fencedCodeKeepsItsLanguageAndBody() {
        let blocks = MarkdownDocument.parse("```rust\nfn main() {}\n```\n").blocks
        #expect(blocks == [MarkdownBlock(kind: .code(language: "rust", text: "fn main() {}"))])
    }

    @Test func fencedCodeIsNotReadAsMarkdown() {
        let blocks = MarkdownDocument.parse("```\n# not a heading\n- not a list\n```\n").blocks
        #expect(blocks.count == 1)
        #expect(blocks[0].kind == .code(language: nil, text: "# not a heading\n- not a list"))
    }

    @Test func indentedCodeStopsAtTheNextParagraph() {
        let blocks = MarkdownDocument.parse("intro\n\n    code line\n\nafter\n").blocks
        #expect(blocks[1].kind == .code(language: nil, text: "code line"))
        #expect(plain(blocks[2].runs) == "after")
    }

    @Test func blockQuotesCarryTheirDepth() {
        let blocks = MarkdownDocument.parse("> one\n\n>> two\n").blocks
        #expect(blocks[0].quoteDepth == 1)
        #expect(blocks[1].quoteDepth == 2)
        #expect(plain(blocks[1].runs) == "two")
    }

    @Test func htmlBlocksStayRaw() {
        let blocks = MarkdownDocument.parse("<div class=\"x\">\ntext\n</div>\n").blocks
        #expect(blocks.count == 1)
        #expect(blocks[0].kind == .html(text: "<div class=\"x\">\ntext\n</div>"))
    }

    @Test func frontMatterIsItsOwnBlock() {
        let blocks = MarkdownDocument.parse("---\nmodule: core\n---\n\n# Title\n").blocks
        #expect(blocks[0].kind == .frontMatter(text: "module: core"))
        #expect(blocks[1].kind == .heading(level: 1))
    }

    @Test func aByteOrderMarkDoesNotHideALeadingHeading() {
        let blocks = MarkdownDocument.parse("\u{FEFF}# Title\n").blocks
        #expect(blocks.count == 1)
        #expect(blocks[0].kind == .heading(level: 1))
        #expect(plain(blocks[0].runs) == "Title")
    }

    @Test func aByteOrderMarkDoesNotHideFrontMatter() {
        let blocks = MarkdownDocument.parse("\u{FEFF}---\ntitle: x\n---\n").blocks
        #expect(blocks.count == 1)
        #expect(blocks[0].kind == .frontMatter(text: "title: x"))
    }

    @Test func aLeadingRuleWithoutACloserIsNotFrontMatter() {
        let blocks = MarkdownDocument.parse("---\n\ntext\n").blocks
        #expect(blocks[0].kind == .thematicBreak)
    }

    @Test func footnotesSplitIntoMarkerAndDefinition() {
        let blocks = MarkdownDocument.parse("See it[^a].\n\n[^a]: Because.\n").blocks
        let marker = firstRun(blocks) { $0.style.contains(.footnoteMarker) }
        #expect(marker?.text == "a")
        #expect(blocks[1].kind == .footnote(label: "a"))
        #expect(plain(blocks[1].runs) == "Because.")
    }
}

@Suite struct MarkdownAnchorTests {
    @Test func headingsSlugLikeGitHub() {
        #expect(MarkdownDocument.anchor(for: "Working agreement") == "working-agreement")
        #expect(MarkdownDocument.anchor(for: "`Oid` keys, and why") == "oid-keys-and-why")
        #expect(MarkdownDocument.anchor(for: "Step 1: open") == "step-1-open")
    }

    @Test func aFragmentAndItsHeadingAgree() {
        #expect(
            MarkdownDocument.anchor(for: "Invariants") == MarkdownDocument.anchor(for: "invariants")
        )
    }

    private func headingAnchors(_ source: String) -> [String] {
        MarkdownDocument.headingAnchors(for: MarkdownDocument.parse(source).blocks)
    }

    @Test func repeatedHeadingsTakeNumberedSuffixes() {
        #expect(
            headingAnchors("## Fixed\n\n## Fixed\n\n## Fixed\n") == ["fixed", "fixed-1", "fixed-2"])
    }

    @Test func aSuffixAlreadyTakenIsSkipped() {
        #expect(headingAnchors("# Foo\n\n# Foo-1\n\n# Foo\n") == ["foo", "foo-1", "foo-2"])
        #expect(
            headingAnchors("# Usage\n\n# Usage\n\n# Usage\n\n# Usage 1\n")
                == ["usage", "usage-1", "usage-2", "usage-1-1"])
    }

    @Test func aHeadingNeverTakesAFootnotesAnchor() {
        #expect(headingAnchors("# fn-a\n\nSee[^a].\n\n[^a]: Note.\n") == ["fn-a-1"])
    }

    @Test func aHeadingWithNoSlugClaimsNothing() {
        #expect(headingAnchors("# !!!\n\n# Foo\n\n# !!!\n") == ["", "foo", ""])
    }
}

@Suite struct MarkdownListTests {
    @Test func bulletsAndOrdinalsCarryOneMarkerPerItem() {
        let blocks = MarkdownDocument.parse("- one\n- two\n\n1. first\n2. second\n").blocks
        #expect(blocks.map(\.marker) == [.bullet, .bullet, .ordered(1), .ordered(2)])
        #expect(blocks.allSatisfy { $0.listDepth == 1 })
    }

    @Test func indentationNestsAndUnnests() {
        let blocks = MarkdownDocument.parse("- one\n  - inner\n- two\n").blocks
        #expect(blocks.map(\.listDepth) == [1, 2, 1])
    }

    @Test func aSecondParagraphStaysInsideTheItem() {
        let blocks = MarkdownDocument.parse("- one\n\n  more\n\nafter\n").blocks
        #expect(blocks[1].listDepth == 1)
        #expect(blocks[1].marker == nil)
        #expect(blocks[2].listDepth == 0)
    }

    @Test func taskItemsRecordWhetherTheyAreDone() {
        let blocks = MarkdownDocument.parse("- [ ] open\n- [x] done\n").blocks
        #expect(blocks.map(\.marker) == [.task(done: false), .task(done: true)])
        #expect(plain(blocks[1].runs) == "done")
    }

    @Test func codeInsideAnItemKeepsTheBullet() {
        let blocks = MarkdownDocument.parse("- item\n\n  ```sh\n  ls\n  ```\n").blocks
        #expect(blocks[1].kind == .code(language: "sh", text: "ls"))
        #expect(blocks[1].listDepth == 1)
    }
}

@Suite struct MarkdownTableTests {
    private func table(_ source: String) -> MarkdownTable? {
        for block in MarkdownDocument.parse(source).blocks {
            if case let .table(value) = block.kind { return value }
        }
        return nil
    }

    @Test func headerDelimiterAndRowsBecomeATable() {
        let parsed = table("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n")
        #expect(parsed?.columnCount == 2)
        #expect(parsed?.header.map(plain) == ["a", "b"])
        #expect(parsed?.rows.map { $0.map(plain) } == [["1", "2"], ["3", "4"]])
    }

    @Test func colonsSetTheAlignment() {
        let parsed = table("| a | b | c | d |\n|:--|--:|:-:|---|\n| 1 | 2 | 3 | 4 |\n")
        #expect(parsed?.alignments == [.leading, .trailing, .center, .natural])
    }

    @Test func shortRowsArePaddedToTheColumnCount() {
        let parsed = table("a | b | c\n--- | --- | ---\n1\n")
        #expect(parsed?.rows.first?.count == 3)
        #expect(parsed?.rows.first.map { $0.map(plain) } == ["1", "", ""])
    }

    @Test func anEscapedPipeStaysInsideItsCell() {
        let parsed = table("| a | b |\n|---|---|\n| x \\| y | z |\n")
        #expect(parsed?.rows.first.map { $0.map(plain) } == ["x | y", "z"])
    }

    @Test func cellsKeepTheirInlineFormatting() {
        let parsed = table("| a |\n|---|\n| [doc](x.md) |\n")
        #expect(parsed?.rows.first?.first?.first?.link?.destination == "x.md")
    }

    @Test func aTableEndsAtTheNextBlock() {
        let blocks = MarkdownDocument.parse("| a |\n|---|\n| 1 |\n\n# After\n").blocks
        #expect(blocks.count == 2)
        #expect(blocks[1].kind == .heading(level: 1))
    }
}

@Suite struct MarkdownInlineTests {
    private func runs(_ source: String) -> [MarkdownRun] {
        MarkdownDocument.parse(source).blocks.first?.runs ?? []
    }

    @Test func emphasisStrongAndBothCombine() {
        #expect(runs("*a*").first?.style == .emphasis)
        #expect(runs("**a**").first?.style == .strong)
        #expect(runs("***a***").first?.style == [.strong, .emphasis])
        #expect(runs("~~a~~").first?.style == .strikethrough)
    }

    @Test func nestedEmphasisKeepsBothStyles() {
        let parsed = runs("**bold _and italic_**")
        #expect(plain(parsed) == "bold and italic")
        #expect(parsed.last?.style == [.strong, .emphasis])
    }

    @Test func underscoresInsideAWordAreLiteral() {
        #expect(plain(runs("snake_case_name")) == "snake_case_name")
        #expect(runs("snake_case_name").allSatisfy { $0.style.isEmpty })
    }

    @Test func anUnclosedDelimiterStaysLiteral() {
        #expect(plain(runs("2 * 3 = 6")) == "2 * 3 = 6")
        #expect(plain(runs("**never closed")) == "**never closed")
    }

    @Test func codeSpansSwallowTheirDelimiters() {
        #expect(runs("`a * b`").first?.style == .code)
        #expect(runs("`a * b`").first?.text == "a * b")
        #expect(runs("`` a ` b ``").first?.text == "a ` b")
    }

    @Test func escapesProduceTheLiteralCharacter() {
        #expect(plain(runs("\\*not emphasis\\*")) == "*not emphasis*")
    }

    @Test func inlineLinksCarryDestinationAndTitle() {
        let link = runs("[docs](docs/x.md \"Title\")").first?.link
        #expect(link == MarkdownLink(destination: "docs/x.md", title: "Title"))
    }

    @Test func referenceLinksResolveInEitherOrder() {
        let before = runs("[a][ref]\n\n[ref]: https://example.com\n")
        #expect(before.first?.link?.destination == "https://example.com")
        let collapsed = MarkdownDocument.parse("[ref]: https://example.com\n\n[ref][]\n")
        #expect(collapsed.blocks.first?.runs.first?.link?.destination == "https://example.com")
    }

    @Test func autolinksAndBareURLsBecomeLinks() {
        #expect(runs("<https://example.com>").first?.link?.destination == "https://example.com")
        #expect(runs("<a@b.com>").first?.link?.destination == "mailto:a@b.com")
        let bare = runs("see https://example.com/x. done")
        #expect(bare.contains { $0.link?.destination == "https://example.com/x" })
        #expect(plain(bare) == "see https://example.com/x. done")
    }

    @Test func imagesKeepTheirAltTextAndSource() {
        let image = runs("![logo](assets/logo.png)").first
        #expect(image?.isImage == true)
        #expect(image?.text == "logo")
        #expect(image?.link?.destination == "assets/logo.png")
    }

    @Test func inlineTagsAreDroppedAndBreaksSurvive() {
        #expect(plain(runs("a <span>b</span> c")) == "a b c")
        #expect(plain(runs("a<br>b")) == "a\nb")
    }

    @Test func linkTextKeepsItsOwnFormatting() {
        let parsed = runs("[**bold** plain](x.md)")
        #expect(parsed.allSatisfy { $0.link?.destination == "x.md" })
        #expect(parsed.first?.style == .strong)
        #expect(plain(parsed) == "bold plain")
    }
}

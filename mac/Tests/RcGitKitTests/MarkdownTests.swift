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
    @Test func atxHeadingsCarryTheirLevel() throws {
        let blocks = MarkdownDocument.parse("# One\n\n### Three ###\n").blocks
        try #require(blocks.count == 2)
        #expect(blocks[0].kind == .heading(level: 1))
        #expect(plain(blocks[0].runs) == "One")
        #expect(blocks[1].kind == .heading(level: 3))
        #expect(plain(blocks[1].runs) == "Three")
    }

    @Test func setextUnderlinesRetitleTheParagraph() throws {
        let blocks = MarkdownDocument.parse("Title\n=====\n\nOther\n-----\n").blocks
        try #require(blocks.count == 2)
        #expect(blocks[0].kind == .heading(level: 1))
        #expect(plain(blocks[0].runs) == "Title")
        #expect(blocks[1].kind == .heading(level: 2))
    }

    @Test func anUnderlineLeftOfAListItemIsARuleNotAHeading() throws {
        let blocks = MarkdownDocument.parse("- Foo\n---\n").blocks
        #expect(blocks.map(\.kind) == [.paragraph, .thematicBreak])
        #expect(blocks.map(\.listDepth) == [1, 0])
        #expect(plain(try #require(blocks.first).runs) == "Foo")
    }

    @Test func anUnderlineInsideAListItemStillRetitlesIt() {
        let blocks = MarkdownDocument.parse("- Foo\n  ---\n").blocks
        #expect(blocks.map(\.kind) == [.heading(level: 2)])
        #expect(blocks.map(\.listDepth) == [1])
    }

    @Test func softBreaksFoldAndTrailingSpacesKeepTheBreak() throws {
        let blocks = MarkdownDocument.parse("one\ntwo\n\nthree  \nfour\n").blocks
        try #require(blocks.count == 2)
        #expect(plain(blocks[0].runs) == "one two")
        #expect(plain(blocks[1].runs) == "three\nfour")
    }

    @Test func aSoftBreakAfterAnInlineConstructKeepsTheSpace() throws {
        let source = "Run `make`\nthen read [the docs](docs/a.md)\nnext *line*\nend"
        let text = plain(try #require(MarkdownDocument.parse(source).blocks.first).runs)
        #expect(text == "Run make then read the docs next line end")
    }

    @Test func aHardBreakAfterALinkKeepsTheBreak() throws {
        let block = try #require(MarkdownDocument.parse("a [x](y)  \nb").blocks.first)
        #expect(plain(block.runs) == "a x\nb")
    }

    @Test func aBreakTagAtTheEndOfALineGainsNoSpace() throws {
        let block = try #require(MarkdownDocument.parse("a<br>\nb").blocks.first)
        #expect(plain(block.runs) == "a\nb")
    }

    @Test func thematicBreaksAreNotListItems() throws {
        let blocks = MarkdownDocument.parse("a\n\n---\n\n* * *\n").blocks
        try #require(blocks.count == 3)
        #expect(blocks[1].kind == .thematicBreak)
        #expect(blocks[2].kind == .thematicBreak)
    }

    @Test func fencedCodeKeepsItsLanguageAndBody() {
        let blocks = MarkdownDocument.parse("```rust\nfn main() {}\n```\n").blocks
        #expect(blocks == [MarkdownBlock(kind: .code(language: "rust", text: "fn main() {}"))])
    }

    @Test func fencedCodeIsNotReadAsMarkdown() throws {
        let blocks = MarkdownDocument.parse("```\n# not a heading\n- not a list\n```\n").blocks
        try #require(blocks.count == 1)
        #expect(blocks[0].kind == .code(language: nil, text: "# not a heading\n- not a list"))
    }

    @Test func aShorterFenceDoesNotCloseALongerOne() {
        let blocks = MarkdownDocument.parse("````md\n```js\nx\n```\n````\n\nafter\n").blocks
        #expect(
            blocks == [
                MarkdownBlock(kind: .code(language: "md", text: "```js\nx\n```")),
                MarkdownBlock(kind: .paragraph, runs: [MarkdownRun(text: "after")]),
            ])
    }

    @Test func aFenceLineWithAnInfoStringDoesNotClose() {
        let blocks = MarkdownDocument.parse("```\n```js\nx\n```\n").blocks
        #expect(blocks == [MarkdownBlock(kind: .code(language: nil, text: "```js\nx"))])
    }

    @Test func tildeFencesOpenCode() {
        let blocks = MarkdownDocument.parse("~~~sh\nls\n~~~\n").blocks
        #expect(blocks == [MarkdownBlock(kind: .code(language: "sh", text: "ls"))])
    }

    @Test func fencedCodeKeepsLeadingGreaterThanSigns() {
        let repl = MarkdownDocument.parse("```python\n>>> import os\n```\n").blocks
        #expect(repl.map(\.kind) == [.code(language: "python", text: ">>> import os")])
        let quoted = MarkdownDocument.parse("```text\n> quoted line\n```\n").blocks
        #expect(quoted.map(\.kind) == [.code(language: "text", text: "> quoted line")])
    }

    @Test func aQuotedFenceKeepsMarkersPastItsOwnDepth() {
        let blocks = MarkdownDocument.parse("> ```\n> >>> x\n> ```\n").blocks
        #expect(blocks.map(\.kind) == [.code(language: nil, text: ">>> x")])
        #expect(blocks.map(\.quoteDepth) == [1])
    }

    @Test func aQuotedFenceEndsWithTheQuote() {
        let blocks = MarkdownDocument.parse("> ```\n> code\n\noutside\n").blocks
        #expect(blocks.map(\.kind) == [.code(language: nil, text: "code"), .paragraph])
        #expect(blocks.map(\.quoteDepth) == [1, 0])
        #expect(blocks.last.map { plain($0.runs) } == "outside")
    }

    @Test func aDefinitionAfterAnUnclosedQuotedFenceStillDefines() {
        let source = "> ```\n> code\n\n[a]: /real\n\nSee [a].\n"
        let blocks = MarkdownDocument.parse(source).blocks
        #expect(blocks.last?.runs.contains { $0.link?.destination == "/real" } == true)
    }

    @Test func indentedCodeStopsAtTheNextParagraph() throws {
        let blocks = MarkdownDocument.parse("intro\n\n    code line\n\nafter\n").blocks
        try #require(blocks.count == 3)
        #expect(blocks[1].kind == .code(language: nil, text: "code line"))
        #expect(plain(blocks[2].runs) == "after")
    }

    @Test(arguments: ["# x", "- x", "[a]: https://example.com"])
    func indentedCodeIsCodeWhateverItStartsWith(first: String) {
        let blocks = MarkdownDocument.parse("Text\n\n    \(first)\n    more\n").blocks
        #expect(blocks.map(\.kind) == [.paragraph, .code(language: nil, text: "\(first)\nmore")])
    }

    @Test func anIndentedBlockStartContinuesTheParagraph() throws {
        let blocks = MarkdownDocument.parse("Text\n    # not heading\n").blocks
        #expect(blocks.map(\.kind) == [.paragraph])
        #expect(plain(try #require(blocks.first).runs) == "Text # not heading")
    }

    @Test func blockQuotesCarryTheirDepth() throws {
        let blocks = MarkdownDocument.parse("> one\n\n>> two\n").blocks
        try #require(blocks.count == 2)
        #expect(blocks[0].quoteDepth == 1)
        #expect(blocks[1].quoteDepth == 2)
        #expect(plain(blocks[1].runs) == "two")
    }

    @Test func anUnmarkedLineContinuesTheQuotedParagraph() {
        let blocks = MarkdownDocument.parse("> quote\ncontinued\n").blocks
        #expect(blocks.count == 1)
        #expect(blocks.first?.quoteDepth == 1)
        #expect(blocks.first.map { plain($0.runs) } == "quote continued")
    }

    @Test func anUnmarkedBlockStartLeavesTheQuote() {
        let blocks = MarkdownDocument.parse("> quote\n# Title\n").blocks
        #expect(blocks.count == 2)
        #expect(blocks.last?.quoteDepth == 0)
        #expect(blocks.last?.kind == .heading(level: 1))
    }

    @Test func htmlBlocksStayRaw() throws {
        let blocks = MarkdownDocument.parse("<div class=\"x\">\ntext\n</div>\n").blocks
        try #require(blocks.count == 1)
        #expect(blocks[0].kind == .html(text: "<div class=\"x\">\ntext\n</div>"))
    }

    @Test func anHTMLCommentRunsPastABlankLineToItsCloser() {
        let blocks = MarkdownDocument.parse("<!-- note\n\n# hidden -->\nafter\n").blocks
        #expect(blocks.count == 2)
        #expect(blocks.first?.kind == .html(text: "<!-- note\n\n# hidden -->"))
        #expect(blocks.last?.kind == .paragraph)
    }

    @Test func aOneLineHTMLCommentEndsOnItsOwnLine() {
        let blocks = MarkdownDocument.parse("<!-- note -->\n# Title\n").blocks
        #expect(blocks.count == 2)
        #expect(blocks.first?.kind == .html(text: "<!-- note -->"))
        #expect(blocks.last?.kind == .heading(level: 1))
    }

    @Test func aPreBlockRunsPastABlankLineToItsClosingTag() {
        let blocks = MarkdownDocument.parse("<PRE>\nline\n\nmore\n</Pre>\n\ntext\n").blocks
        #expect(blocks.count == 2)
        #expect(blocks.first?.kind == .html(text: "<PRE>\nline\n\nmore\n</Pre>"))
        #expect(blocks.last?.kind == .paragraph)
    }

    @Test func aPlainBlockTagStillEndsAtABlankLine() {
        let blocks = MarkdownDocument.parse("<div>\nx\n\n# Title\n").blocks
        #expect(blocks.count == 2)
        #expect(blocks.last?.kind == .heading(level: 1))
    }

    @Test func anHTMLBlockInAListItemLeavesTheMarkerOutOfItsText() {
        let blocks = MarkdownDocument.parse("- <details>\n  <summary>More</summary>\n").blocks
        #expect(blocks.count == 1)
        #expect(blocks.first?.kind == .html(text: "<details>\n<summary>More</summary>"))
        #expect(blocks.first?.marker == .bullet)
    }

    @Test func frontMatterIsItsOwnBlock() throws {
        let blocks = MarkdownDocument.parse("---\nmodule: core\n---\n\n# Title\n").blocks
        try #require(blocks.count == 2)
        #expect(blocks[0].kind == .frontMatter(text: "module: core"))
        #expect(blocks[1].kind == .heading(level: 1))
    }

    @Test func aByteOrderMarkDoesNotHideALeadingHeading() throws {
        let blocks = MarkdownDocument.parse("\u{FEFF}# Title\n").blocks
        try #require(blocks.count == 1)
        #expect(blocks[0].kind == .heading(level: 1))
        #expect(plain(blocks[0].runs) == "Title")
    }

    @Test func aByteOrderMarkDoesNotHideFrontMatter() throws {
        let blocks = MarkdownDocument.parse("\u{FEFF}---\ntitle: x\n---\n").blocks
        try #require(blocks.count == 1)
        #expect(blocks[0].kind == .frontMatter(text: "title: x"))
    }

    @Test func aLeadingRuleWithoutACloserIsNotFrontMatter() throws {
        let blocks = MarkdownDocument.parse("---\n\ntext\n").blocks
        try #require(blocks.count == 2)
        #expect(blocks[0].kind == .thematicBreak)
    }

    @Test func footnotesSplitIntoMarkerAndDefinition() throws {
        let blocks = MarkdownDocument.parse("See it[^a].\n\n[^a]: Because.\n").blocks
        let marker = firstRun(blocks) { $0.style.contains(.footnoteMarker) }
        #expect(marker?.text == "a")
        try #require(blocks.count == 2)
        #expect(blocks[1].kind == .footnote(label: "a"))
        #expect(plain(blocks[1].runs) == "Because.")
    }

    @Test func footnoteAnchorMatchesMarker() throws {
        let blocks = MarkdownDocument.parse("See[^Note].\n\n[^Note]: text\n").blocks
        let marker = try #require(firstRun(blocks) { $0.style.contains(.footnoteMarker) })
        let destination = try #require(marker.link?.destination)
        #expect(destination == "#fn-note")
        let footnote = try #require(blocks.last)
        guard case let .footnote(label) = footnote.kind else {
            Issue.record("expected a footnote block, got \(footnote.kind)")
            return
        }
        #expect(
            MarkdownDocument.anchor(for: String(destination.dropFirst()))
                == MarkdownDocument.footnoteAnchor(for: label))
        #expect(
            MarkdownDocument.anchor(for: "fn-note")
                == MarkdownDocument.footnoteAnchor(for: "Note"))
    }

    @Test func proseAfterABracketedLabelStaysAParagraph() throws {
        let blocks = MarkdownDocument.parse("[Note]: this is not done\n\nSee [Note].\n").blocks
        try #require(blocks.count == 2)
        #expect(plain(blocks[0].runs) == "[Note]: this is not done")
        #expect(blocks[1].runs.allSatisfy { $0.link == nil })
    }

    @Test func aFenceLineWithInfoDoesNotCloseTheFence() throws {
        let source = "```\n```js\n[a]: https://example.com\n```\n\nSee [a].\n"
        let blocks = MarkdownDocument.parse(source).blocks
        try #require(blocks.count == 2)
        #expect(blocks[0].kind == .code(language: nil, text: "```js\n[a]: https://example.com"))
        #expect(blocks[1].runs.allSatisfy { $0.link == nil })
    }

    @Test func aDefinitionAfterAFenceWithAnInfoLineStillDefines() throws {
        let source = "```\n```js\n```\n\n[a]: /real\n\nSee [a].\n"
        let blocks = MarkdownDocument.parse(source).blocks
        try #require(blocks.count == 2)
        #expect(blocks[1].runs.contains { $0.link?.destination == "/real" })
    }

    @Test func indentedAndContinuationDefinitionsDefineNothing() {
        let indented = MarkdownDocument.parse("    [a]: /x\n\nSee [a].\n").blocks
        #expect(indented.last?.runs.allSatisfy { $0.link == nil } == true)
        let continued = MarkdownDocument.parse("Text\n[a]: /x\n\nSee [a].\n").blocks
        #expect(plain(continued[0].runs) == "Text [a]: /x")
        #expect(continued.last?.runs.allSatisfy { $0.link == nil } == true)
    }

    @Test func crlfLinesParseLikeLf() {
        let lf =
            "---\nmodule: core\n---\n\n### Three ###\n\nthree  \nfour\n\n"
            + "```rust\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n"
        let crlf = lf.replacingOccurrences(of: "\n", with: "\r\n")
        #expect(MarkdownDocument.parse(crlf) == MarkdownDocument.parse(lf))
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

    @Test func aLineOfMarkersNestsNoDeeperThanTheCap() throws {
        let source = String(repeating: "+ ", count: 10_000) + "x"
        let blocks = MarkdownDocument.parse(source).blocks
        let block = try #require(blocks.first)
        #expect(blocks.count == 1)
        #expect(block.listDepth == MarkdownBlockParser.maximumListDepth)
        #expect(plain(block.runs).hasSuffix("+ + x"))
    }

    @Test func aSecondParagraphStaysInsideTheItem() throws {
        let blocks = MarkdownDocument.parse("- one\n\n  more\n\nafter\n").blocks
        try #require(blocks.count == 3)
        #expect(blocks[1].listDepth == 1)
        #expect(blocks[1].marker == nil)
        #expect(blocks[2].listDepth == 0)
    }

    @Test func taskItemsRecordWhetherTheyAreDone() throws {
        let blocks = MarkdownDocument.parse("- [ ] open\n- [x] done\n").blocks
        try #require(blocks.count == 2)
        #expect(blocks.map(\.marker) == [.task(done: false), .task(done: true)])
        #expect(plain(blocks[1].runs) == "done")
    }

    @Test func anEmptyItemLeavesNoMarkerOnTheNextBlock() {
        for source in [
            "Intro\n\n*\n\n# Heading\n", "- [ ] \n\n## Next\n", "-\n\nparagraph after\n",
            "> -\n\nafter the quote\n",
        ] {
            let last = MarkdownDocument.parse(source).blocks.last
            #expect(last?.listDepth == 0, "\(source)")
            #expect(last?.marker == nil, "\(source)")
        }
    }

    @Test func anEmptyItemKeepsItsMarkerForItsIndentedContent() {
        let blocks = MarkdownDocument.parse("-\n  inside\n").blocks
        #expect(blocks.map(\.marker) == [.bullet])
        #expect(blocks.first?.listDepth == 1)
    }

    @Test func codeInsideAnItemKeepsTheBullet() throws {
        let blocks = MarkdownDocument.parse("- item\n\n  ```sh\n  ls\n  ```\n").blocks
        try #require(blocks.count == 2)
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

    @Test func theShortestDelimiterMakesAOneColumnTable() {
        let parsed = table("a\n|-\n")
        #expect(parsed?.columnCount == 1)
        #expect(parsed?.header.map(plain) == ["a"])
        #expect(parsed?.alignments == [.natural])
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

    @Test func aTableEndsAtTheNextBlock() throws {
        let blocks = MarkdownDocument.parse("| a |\n|---|\n| 1 |\n\n# After\n").blocks
        try #require(blocks.count == 2)
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

    @Test func strongInsideEmphasisKeepsBoth() {
        for source in ["*This is **important** text*", "_This is __important__ text_"] {
            let parsed = runs(source)
            #expect(plain(parsed) == "This is important text", "\(source)")
            let important = parsed.first { $0.text == "important" }
            #expect(important?.style == [.emphasis, .strong], "\(source)")
            #expect(parsed.allSatisfy { $0.style.contains(.emphasis) }, "\(source)")
        }
    }

    @Test func nestingPastTheDepthLimitStaysLiteral() {
        let depth = 20
        let links = runs(
            String(repeating: "[", count: depth) + "x" + String(repeating: "](u)", count: depth))
        let literal = depth - MarkdownInlineParser.maximumDepth
        #expect(links.first?.text.hasPrefix(String(repeating: "[", count: literal) + "x") == true)
        #expect(!links.isEmpty && links.allSatisfy { $0.link?.destination == "u" })

        let markers = (0..<depth).map { $0.isMultiple(of: 2) ? "*" : "_" }
        let source =
            markers.map { "\($0)a " }.joined() + "x" + markers.reversed().map { " a\($0)" }.joined()
        let emphasized = runs(source)
        let innermost = markers.suffix(literal).map { "\($0)a " }.joined() + "x"
        #expect(emphasized.contains { $0.text.hasPrefix("a " + innermost) })
        #expect(!emphasized.isEmpty && emphasized.allSatisfy { $0.style == .emphasis })
    }

    /// Each unit leaves an opener unmatched; a closer search per opener that rescans to the end
    /// of the block took 15-30 s here before the closers were paired once per block.
    @Test func unmatchedOpenersParseInLinearTime() {
        let clock = ContinuousClock()
        for unit in ["x [y ", "a *b ", "[a](b ", "x<y ", "x<y", "*Note: x\n", "x [y](z(w "] {
            let text = String(repeating: unit, count: 256 * 1024 / unit.utf8.count)
            let elapsed = clock.measure { _ = MarkdownDocument.parse(text) }
            #expect(elapsed < .seconds(2), "\(unit.debugDescription) took \(elapsed)")
        }
    }

    @Test func anInnerOpenerWithNoCloserStaysLiteral() {
        let parsed = runs("*a **b c*")
        #expect(plain(parsed) == "a **b c")
        #expect(parsed.allSatisfy { $0.style == .emphasis })
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

    @Test func anUnclosedBacktickRunStaysLiteralAsAWhole() {
        let parsed = runs("x ``` y `` z")
        #expect(plain(parsed) == "x ``` y `` z")
        #expect(parsed.allSatisfy { !$0.style.contains(.code) })
    }

    @Test func emphasisDoesNotCloseInsideACodeSpan() {
        let parsed = runs("*a `x*y` b*")
        #expect(parsed.map(\.text) == ["a ", "x*y", " b"])
        #expect(parsed.map(\.style) == [.emphasis, [.emphasis, .code], .emphasis])
        let underscores = runs("_see `__init__` here_")
        #expect(underscores.map(\.text) == ["see ", "__init__", " here"])
        #expect(underscores.allSatisfy { $0.style.contains(.emphasis) })
    }

    @Test func linkTextDoesNotCloseInsideACodeSpan() {
        let parsed = runs("[see `]` here](x.md)")
        #expect(plain(parsed) == "see ] here")
        #expect(parsed.allSatisfy { $0.link?.destination == "x.md" })
    }

    @Test func anEscapedBacktickOpensNoCodeSpanForTheCloserSearch() {
        let parsed = runs("*a \\` b `c* d`")
        #expect(parsed.map(\.text) == ["*a ` b ", "c* d"])
        #expect(parsed.map(\.style) == [[], .code])
    }

    @Test func escapesProduceTheLiteralCharacter() {
        #expect(plain(runs("\\*not emphasis\\*")) == "*not emphasis*")
    }

    @Test func inlineLinksCarryDestinationAndTitle() {
        let link = runs("[docs](docs/x.md \"Title\")").first?.link
        #expect(link == MarkdownLink(destination: "docs/x.md", title: "Title"))
    }

    @Test func inlineAndReferenceTitlesAgreeForEveryQuoting() {
        for (title, expected) in [("\"T\"", "T"), ("'T'", "T"), ("(T)", "T"), ("\"\"", "")] {
            let inline = runs("[a](x \(title))").first?.link
            let reference = runs("[a][r]\n\n[r]: x \(title)\n").first?.link
            #expect(inline == MarkdownLink(destination: "x", title: expected), "\(title)")
            #expect(reference == inline, "\(title)")
        }
    }

    @Test func referenceLinksResolveInEitherOrder() {
        let before = runs("[a][ref]\n\n[ref]: https://example.com\n")
        #expect(before.first?.link?.destination == "https://example.com")
        let collapsed = MarkdownDocument.parse("[ref]: https://example.com\n\n[ref][]\n")
        #expect(collapsed.blocks.first?.runs.first?.link?.destination == "https://example.com")
    }

    @Test func aDefinitionInsideAFenceDefinesNothing() {
        let plainFence = MarkdownDocument.parse("```\n[ref]: https://x\n```\n\n[a][ref]\n")
        #expect(plainFence.blocks.last?.runs.first?.link == nil)
        let source = "```md\n```js\n[ref]: https://sample.example\n```\n\n[docs][ref]\n"
        let nested = MarkdownDocument.parse(source)
        #expect(nested.blocks.last?.runs.first?.link == nil)
    }

    @Test func theFirstDefinitionOfALabelWins() {
        let parsed = runs("[a][ref]\n\n[ref]: https://one\n[ref]: https://two\n")
        #expect(parsed.first?.link?.destination == "https://one")
    }

    @Test func labelsMatchIgnoringCase() {
        let parsed = runs("[a][REF]\n\n[ref]: https://example.com\n")
        #expect(parsed.first?.link?.destination == "https://example.com")
    }

    @Test func autolinksAndBareURLsBecomeLinks() {
        #expect(runs("<https://example.com>").first?.link?.destination == "https://example.com")
        #expect(runs("<a@b.com>").first?.link?.destination == "mailto:a@b.com")
        let bare = runs("see https://example.com/x. done")
        #expect(bare.contains { $0.link?.destination == "https://example.com/x" })
        #expect(plain(bare) == "see https://example.com/x. done")
    }

    @Test func autolinkSchemeFollowsTheCommonMarkRule() {
        let digits = runs("get <s3://bucket/key> now")
        #expect(digits.compactMap { $0.link?.destination } == ["s3://bucket/key"])
        #expect(plain(digits) == "get s3://bucket/key now")
        #expect(runs("<a.b-c+d://x>").first?.link?.destination == "a.b-c+d://x")
        #expect(runs("<3d://x>").allSatisfy { $0.link == nil })
        #expect(runs("<é1://x>").allSatisfy { $0.link == nil })
        let longest = String(repeating: "a", count: 32)
        #expect(runs("<\(longest)://x>").first?.link?.destination == "\(longest)://x")
        #expect(runs("<a\(longest)://x>").allSatisfy { $0.link == nil })
    }

    @Test func bareWWWURLGetsHTTPSWhateverItsCase() {
        let upper = runs("Visit WWW.Example.com today")
        #expect(upper.compactMap { $0.link?.destination } == ["https://WWW.Example.com"])
        #expect(plain(upper) == "Visit WWW.Example.com today")
        let lower = runs("Visit www.example.com today")
        #expect(lower.compactMap { $0.link?.destination } == ["https://www.example.com"])
    }

    @Test func bareURLPrefixesMatchInEitherCase() {
        for url in ["HTTPS://X.COM", "Http://x.com", "https://x.com"] {
            #expect(runs("go \(url) now").compactMap { $0.link?.destination } == [url], "\(url)")
        }
        #expect(runs("go ftp://x.com now").allSatisfy { $0.link == nil })
    }

    @Test func bareURLKeepsABalancedClosingParenthesis() {
        let wiki = "https://en.wikipedia.org/wiki/Rust_(programming_language)"
        let balanced = runs("See \(wiki) now")
        #expect(balanced.compactMap { $0.link?.destination } == [wiki])
        #expect(plain(balanced) == "See \(wiki) now")
        let wrapped = runs("(see \(wiki))")
        #expect(wrapped.compactMap { $0.link?.destination } == [wiki])
        #expect(plain(wrapped) == "(see \(wiki))")
        let unbalanced = runs("(see https://example.com/x)")
        #expect(unbalanced.compactMap { $0.link?.destination } == ["https://example.com/x"])
    }

    @Test func imagesKeepTheirAltTextAndSource() {
        let image = runs("![logo](assets/logo.png)").first
        #expect(image?.isImage == true)
        #expect(image?.text == "logo")
        #expect(image?.link?.destination == "assets/logo.png")
    }

    @Test func imageInsideALinkFollowsTheLink() {
        let parsed = runs("[![CI](https://ci.example/badge.svg)](https://ci.example/actions)")
        #expect(parsed.count == 1)
        #expect(parsed.first?.isImage == true)
        #expect(parsed.first?.text == "CI")
        #expect(parsed.first?.link?.destination == "https://ci.example/actions")
    }

    @Test func inlineTagsAreDroppedAndBreaksSurvive() {
        #expect(plain(runs("a <span>b</span> c")) == "a b c")
        #expect(plain(runs("a<br>b")) == "a\nb")
        #expect(plain(runs("a <img src=\"x.png\" alt='y' width=10 hidden/> b")) == "a  b")
    }

    @Test func proseBetweenAngleBracketsStaysLiteral() {
        #expect(plain(runs("Returns Result<T, E> on error")) == "Returns Result<T, E> on error")
        #expect(plain(runs("if a<b && c>d then")) == "if a<b && c>d then")
        #expect(plain(runs("Use it when n<10 and m>5 holds.")) == "Use it when n<10 and m>5 holds.")
        #expect(plain(runs("a<1>b")) == "a<1>b")
        #expect(plain(runs("a</b c>d")) == "a</b c>d")
    }

    @Test func linkTextKeepsItsOwnFormatting() {
        let parsed = runs("[**bold** plain](x.md)")
        #expect(parsed.allSatisfy { $0.link?.destination == "x.md" })
        #expect(parsed.first?.style == .strong)
        #expect(plain(parsed) == "bold plain")
    }
}

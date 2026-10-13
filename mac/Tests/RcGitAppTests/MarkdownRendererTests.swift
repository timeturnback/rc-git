import AppKit
import Testing

@testable import RcGitApp

/// Lays `text` out in a container `width` points wide and returns each line fragment's width
/// with the text it holds; see
/// `docs/solutions/ui-bugs/nstextblock-without-content-width-collapses.md`. It reads the
/// fragment rect, not the used rect: a short line in a correct block uses little of a wide
/// fragment, while a collapsed block's fragments are themselves one glyph wide.
private func fragments(of text: NSAttributedString, width: CGFloat) -> [(CGFloat, String)] {
    let storage = NSTextStorage(attributedString: text)
    let layout = NSLayoutManager()
    let container = NSTextContainer(size: NSSize(width: width, height: .greatestFiniteMagnitude))
    layout.addTextContainer(container)
    storage.addLayoutManager(layout)
    layout.ensureLayout(for: container)
    var result: [(CGFloat, String)] = []
    var glyph = 0
    while glyph < layout.numberOfGlyphs {
        var range = NSRange()
        let used = layout.lineFragmentRect(forGlyphAt: glyph, effectiveRange: &range)
        let characters = layout.characterRange(forGlyphRange: range, actualGlyphRange: nil)
        result.append((used.width, (text.string as NSString).substring(with: characters)))
        glyph = NSMaxRange(range)
    }
    return result
}

/// Every foreground color set anywhere in `range` of `text`.
private func foregroundColors(in text: NSAttributedString, range: NSRange) -> [NSColor] {
    var colors: [NSColor] = []
    text.enumerateAttribute(.foregroundColor, in: range) { value, _, _ in
        if let color = value as? NSColor { colors.append(color) }
    }
    return colors
}

@Suite struct MarkdownRendererTests {
    @Test func standaloneBlocksFillThePane() throws {
        let source = """
            ---
            title: Preview
            ---

            ```swift
            let x = 1
            ```

            > outer
            > > inner

            ---

            <div>
            html
            </div>
            """
        let text = MarkdownRenderer.render(source).text
        let lines = fragments(of: text, width: 600)
        try #require(lines.count >= 6)
        for (width, line) in lines {
            #expect(width > 100, "collapsed fragment \(line.debugDescription): \(width) pt")
        }
    }

    @Test func sourceThatRendersNothingComesBackUnchanged() {
        let source = "   \n\n  \n"
        let rendered = MarkdownRenderer.render(source)
        #expect(rendered.text.string == source)
        #expect(rendered.anchors.isEmpty)
    }

    @Test func aCodeBlockOverTheHighlightLimitStaysPlain() throws {
        let line = "let value = \"text\" // comment\n"
        let count = MarkdownRenderer.Theme.highlightLimit / line.utf8.count + 1
        let body = String(repeating: line, count: count)
        let text = MarkdownRenderer.render("```swift\n" + body + "```\n").text
        let range = (text.string as NSString).range(of: body)
        try #require(range.location != NSNotFound)
        let colors = foregroundColors(in: text, range: range)
        #expect(!colors.isEmpty)
        #expect(colors.allSatisfy { $0 == NSColor.textColor })
    }

    @Test func aCodeBlockUnderTheHighlightLimitIsColored() throws {
        let body = "let value = \"text\" // comment\n"
        let text = MarkdownRenderer.render("```swift\n" + body + "```\n").text
        let range = (text.string as NSString).range(of: body)
        try #require(range.location != NSNotFound)
        let colors = foregroundColors(in: text, range: range)
        #expect(colors.contains { $0 != NSColor.textColor })
    }

    @Test func aFootnoteMarkerLinksToItsDefinition() throws {
        let rendered = MarkdownRenderer.render("See[^Note].\n\n[^Note]: text\n")
        var destination: String?
        rendered.text.enumerateAttribute(
            .link, in: NSRange(location: 0, length: rendered.text.length)
        ) { value, _, _ in
            if let link = value as? String { destination = link }
        }
        let fragment = String(try #require(destination).dropFirst())
        let offset = try #require(rendered.anchors[fragment])
        let definition = (rendered.text.string as NSString).range(of: "Note. text")
        #expect(offset == definition.location)
    }

    @Test func repeatedHeadingsGetDistinctAnchors() throws {
        let rendered = MarkdownRenderer.render("## Fixed\n\none\n\n## Fixed\n")
        let first = try #require(rendered.anchors["fixed"])
        let second = try #require(rendered.anchors["fixed-1"])
        let string = rendered.text.string as NSString
        #expect(first == 0)
        #expect(second == string.range(of: "Fixed", options: .backwards).location)
    }
}

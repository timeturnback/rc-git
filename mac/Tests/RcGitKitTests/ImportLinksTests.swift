import Foundation
import Testing

@testable import RcGitKit

/// The import-link bridge and the byte-to-UTF-16 hit map.
@Suite struct ImportLinksTests {
    private func clicked(_ link: ImportLink, in text: String) -> String {
        String(decoding: Array(text.utf8)[Int(link.start)..<Int(link.end)], as: UTF8.self)
    }

    @Test func typescriptImportsAndComponentsCrossTheBridge() throws {
        let text = "import Panel from './Panel';\nconst view = <Panel />;\n"
        let links = try #require(ImportLinks.find(name: "App.tsx", text: text))
        #expect(links.map { clicked($0, in: text) } == ["Panel", "./Panel", "Panel"])
        #expect(links.allSatisfy { $0.specifierEnd - $0.specifierStart == 7 })
    }

    @Test func typescriptGetsHighlightSpans() {
        let data = Array("interface Shape { side: number }\n".utf8)
        let spans = Highlighter.highlight(name: "shape.ts", data: data)
        #expect(spans?.contains { $0.class == .keyword } == true)
        #expect(spans?.contains { $0.class == .type } == true)
        #expect(Highlighter.highlight(name: "App.tsx", data: data)?.isEmpty == false)
    }

    @Test func otherLanguagesHaveNoLinksAndUnknownOnesHaveNoAnswer() {
        #expect(ImportLinks.find(name: "main.rs", text: "use std::fmt;\n") == [])
        #expect(ImportLinks.find(name: "plain.ts", text: "const x = 1;\n") == [])
        #expect(ImportLinks.find(name: "photo.png", text: "import a from './a';\n") == nil)
    }

    @Test func oversizedTextIsNotParsed() {
        let big = String(repeating: "a", count: Highlighter.byteLimit + 1)
        #expect(ImportLinks.find(name: "big.ts", text: big) == nil)
    }

    @Test func textExactlyAtTheLimitIsParsed() throws {
        let head = "import a from './a';\n"
        let text = head + String(repeating: " ", count: Highlighter.byteLimit - head.utf8.count)
        #expect(text.utf8.count == Highlighter.byteLimit)
        let links = try #require(ImportLinks.find(name: "big.ts", text: text))
        #expect(links.count == 2)
    }

    @Test func theMapAnswersInsideALinkAndNowhereElse() {
        let text = "import a from './mod';\n"
        let links = [
            ImportLink(start: 7, end: 8, specifierStart: 15, specifierEnd: 20),
            ImportLink(start: 15, end: 20, specifierStart: 15, specifierEnd: 20),
        ]
        let map = ImportLinkMap(links: links, text: text)
        #expect(!map.isEmpty)
        #expect(map.hit(at: 6) == nil)
        #expect(
            map.hit(at: 7)
                == ImportLinkMap.Hit(range: NSRange(location: 7, length: 1), specifier: "./mod"))
        #expect(map.hit(at: 8) == nil, "ranges are half-open")
        #expect(map.hit(at: 19)?.range == NSRange(location: 15, length: 5))
        #expect(map.hit(at: 20) == nil)
        #expect(map.hit(at: 10_000) == nil)
        #expect(ImportLinkMap(links: [], text: text).hit(at: 0) == nil)
    }

    @Test func theMapCountsInUTF16WhenTextBeforeALinkIsMultibyte() throws {
        // "é" is 2 bytes and 1 UTF-16 unit; "🙂" is 4 bytes and 2 units.
        let text = "// é🙂\nimport a from './é';\n"
        let links = try #require(ImportLinks.find(name: "a.ts", text: text))
        #expect(links.count == 2)
        let map = ImportLinkMap(links: links, text: text)
        let name = (text as NSString).range(of: "a from").location
        #expect(
            map.hit(at: name)
                == ImportLinkMap.Hit(range: NSRange(location: name, length: 1), specifier: "./é"))
        let quote = (text as NSString).range(of: "'").location
        #expect(map.hit(at: quote) == nil, "the quote is not part of the link")
        #expect(map.hit(at: quote + 1)?.range == NSRange(location: quote + 1, length: 3))
        #expect(map.hit(at: quote + 4) == nil)
    }

    @Test func aSpecifierThatCutsACharacterIsNoHit() {
        let text = "é = 1"
        let links = [ImportLink(start: 3, end: 4, specifierStart: 1, specifierEnd: 2)]
        #expect(ImportLinkMap(links: links, text: text).hit(at: 2) == nil)
    }
}

import Foundation

/// The ASCII bytes markdown uses as delimiters.
enum Ascii {
    static let tab: UInt8 = 0x09
    static let newline: UInt8 = 0x0A
    static let carriageReturn: UInt8 = 0x0D
    static let space: UInt8 = 0x20
    static let exclamation: UInt8 = 0x21
    static let quote: UInt8 = 0x22
    static let hash: UInt8 = 0x23
    static let apostrophe: UInt8 = 0x27
    static let leftParen: UInt8 = 0x28
    static let rightParen: UInt8 = 0x29
    static let asterisk: UInt8 = 0x2A
    static let plus: UInt8 = 0x2B
    static let hyphen: UInt8 = 0x2D
    static let dot: UInt8 = 0x2E
    static let slash: UInt8 = 0x2F
    static let zero: UInt8 = 0x30
    static let nine: UInt8 = 0x39
    static let colon: UInt8 = 0x3A
    static let lessThan: UInt8 = 0x3C
    static let equals: UInt8 = 0x3D
    static let greaterThan: UInt8 = 0x3E
    static let question: UInt8 = 0x3F
    static let leftBracket: UInt8 = 0x5B
    static let backslash: UInt8 = 0x5C
    static let rightBracket: UInt8 = 0x5D
    static let caret: UInt8 = 0x5E
    static let underscore: UInt8 = 0x5F
    static let backtick: UInt8 = 0x60
    static let tilde: UInt8 = 0x7E
    static let pipe: UInt8 = 0x7C

    static func isDigit(_ byte: UInt8) -> Bool { byte >= zero && byte <= nine }

    static func isLetter(_ byte: UInt8) -> Bool {
        (byte | 0x20) >= 0x61 && (byte | 0x20) <= 0x7A
    }

    static func isSpace(_ byte: UInt8) -> Bool { byte == space || byte == tab }
}

/// Splits a markdown file into blocks. It works on the UTF-8 bytes: every delimiter markdown
/// uses is ASCII, and a UTF-8 continuation byte can never be mistaken for one, so a byte scan
/// is both correct and far cheaper than walking grapheme clusters.
///
/// Nesting is a stack of list content indents rather than a tree, which is why the result is
/// one flat array. Anything unrecognised stays literal text, so there is no failure case.
struct MarkdownBlockParser {
    private let bytes: [UInt8]
    private let lines: [Range<Int>]
    private var references: [String: MarkdownLink] = [:]
    private var blocks: [MarkdownBlock] = []
    private var index = 0
    /// Content indents of the list items currently open, outermost first.
    private var listIndents: [Int] = []
    /// The deepest list nesting opened. A marker past it reads as text, so a crafted line of
    /// markers cannot push the renderer's indent off the page.
    static let maximumListDepth = 32
    private var quoteDepth = 0
    private var pending: Pending?
    /// Carried by the first block of a list item and cleared there, so one bullet is drawn.
    private var nextMarker: MarkdownListMarker?

    /// A paragraph or footnote body being accumulated line by line.
    private struct Pending {
        var lines: [String]
        var kind: MarkdownBlock.Kind
        var quoteDepth: Int
        var listDepth: Int
        var marker: MarkdownListMarker?
    }

    /// What a list marker introduced: how it looks, and where its content begins.
    private struct ListItem {
        let marker: MarkdownListMarker
        let contentIndent: Int
        let rest: Range<Int>
    }

    /// A UTF-8 byte order mark. GitHub ignores one at the start of a file, so the parser does too.
    private static let byteOrderMark: [UInt8] = [0xEF, 0xBB, 0xBF]

    init(source: String) {
        let utf8 = Array(source.utf8)
        bytes =
            utf8.starts(with: Self.byteOrderMark)
            ? Array(utf8.dropFirst(Self.byteOrderMark.count)) : utf8
        lines = Self.lineRanges(of: bytes)
    }

    mutating func parse() -> [MarkdownBlock] {
        collectReferences()
        parseFrontMatter()
        while index < lines.count { consumeLine() }
        flushPending()
        return blocks
    }

    // MARK: Lines

    private static func lineRanges(of bytes: [UInt8]) -> [Range<Int>] {
        var ranges: [Range<Int>] = []
        var start = 0
        for position in bytes.indices where bytes[position] == Ascii.newline {
            var end = position
            if end > start, bytes[end - 1] == Ascii.carriageReturn { end -= 1 }
            ranges.append(start..<end)
            start = position + 1
        }
        if start < bytes.count { ranges.append(start..<bytes.count) }
        return ranges
    }

    /// Strips up to `limit` `>` markers, returning how many were found and what is left of the
    /// line. A fence body passes its own depth, so a `>` past it stays part of the code.
    private func strippingQuotes(_ line: Range<Int>, limit: Int = .max) -> (
        depth: Int, rest: Range<Int>
    ) {
        var cursor = line.lowerBound
        var depth = 0
        while cursor < line.upperBound, depth < limit {
            var probe = cursor
            var spaces = 0
            while probe < line.upperBound, bytes[probe] == Ascii.space, spaces < 3 {
                probe += 1
                spaces += 1
            }
            guard probe < line.upperBound, bytes[probe] == Ascii.greaterThan else { break }
            probe += 1
            if probe < line.upperBound, bytes[probe] == Ascii.space { probe += 1 }
            depth += 1
            cursor = probe
        }
        return (depth, cursor..<line.upperBound)
    }

    /// Width of the leading whitespace, counting a tab as four columns.
    private func indentWidth(of range: Range<Int>) -> Int {
        var width = 0
        for position in range {
            if bytes[position] == Ascii.space {
                width += 1
            } else if bytes[position] == Ascii.tab {
                width += 4 - (width % 4)
            } else {
                break
            }
        }
        return width
    }

    private func trimmedStart(of range: Range<Int>) -> Range<Int> {
        var cursor = range.lowerBound
        while cursor < range.upperBound, Ascii.isSpace(bytes[cursor]) { cursor += 1 }
        return cursor..<range.upperBound
    }

    private func trimmedEnd(of range: Range<Int>) -> Range<Int> {
        var end = range.upperBound
        while end > range.lowerBound, Ascii.isSpace(bytes[end - 1]) { end -= 1 }
        return range.lowerBound..<end
    }

    private func text(_ range: Range<Int>) -> String {
        String(decoding: bytes[range], as: UTF8.self)
    }

    /// The line with up to `columns` of leading indentation removed, for code block bodies.
    private func text(_ range: Range<Int>, droppingIndent columns: Int) -> String {
        var cursor = range.lowerBound
        var width = 0
        while cursor < range.upperBound, width < columns, Ascii.isSpace(bytes[cursor]) {
            width += bytes[cursor] == Ascii.tab ? 4 - (width % 4) : 1
            cursor += 1
        }
        return text(cursor..<range.upperBound)
    }

    // MARK: Main loop

    private mutating func consumeLine() {
        let (depth, rest) = strippingQuotes(lines[index])
        let body = trimmedStart(of: rest)
        if body.isEmpty {
            flushPending()
            index += 1
            return
        }
        if depth < quoteDepth, continuesQuotedParagraphLazily(body) {
            let line = text(body)
            pending?.lines.append(line)
            index += 1
            return
        }
        if depth != quoteDepth {
            flushPending()
            listIndents.removeAll()
            nextMarker = nil
            quoteDepth = depth
        }
        let indent = indentWidth(of: rest)
        if pending != nil, continuePending(body: body, indent: indent) { return }
        flushPending()
        popLists(to: indent)
        dispatch(body: body, indent: indent)
    }

    /// Handles a line belonging to the paragraph already open: a setext underline that retitles
    /// it, a table delimiter that turns it into a table, or one more line of prose. An underline
    /// left of the open list item's content is no part of the item, as in CommonMark: `---`
    /// there ends the list as a thematic break.
    private mutating func continuePending(body: Range<Int>, indent: Int) -> Bool {
        if let level = setextLevel(body), pending?.kind == .paragraph,
            indent >= (listIndents.last ?? 0)
        {
            retitlePending(level: level)
            index += 1
            return true
        }
        if indent < codeIndent, startsBlock(body) { return false }
        if isTableDelimiter(body), let header = singlePendingLine() {
            pending = nil
            index += 1
            consumeTable(header: header, delimiter: body)
            return true
        }
        let line = text(body)
        pending?.lines.append(line)
        index += 1
        return true
    }

    /// True when a line with fewer `>` markers than the open quote is a lazy continuation of the
    /// quoted paragraph, as CommonMark and GitHub read a wrapped quote line written without `>`.
    private func continuesQuotedParagraphLazily(_ body: Range<Int>) -> Bool {
        guard pending?.kind == .paragraph else { return false }
        return !startsBlock(body) && setextLevel(body) == nil && !isTableDelimiter(body)
    }

    /// The indent at which a line outside a paragraph is indented code, whatever it starts with.
    private var codeIndent: Int { (listIndents.last ?? 0) + 4 }

    /// Reads one line outside a paragraph. A list marker opens its item and the rest of the line
    /// goes round again, so `- - - x` nests in a loop rather than by recursion.
    private mutating func dispatch(body: Range<Int>, indent: Int) {
        var line: (body: Range<Int>, indent: Int)? = (body, indent)
        while let (body, indent) = line {
            line = dispatchOnce(body: body, indent: indent)
        }
    }

    /// Reads the line's first block; returns what follows a list marker when one opened an item.
    private mutating func dispatchOnce(body: Range<Int>, indent: Int)
        -> (body: Range<Int>, indent: Int)?
    {
        if indent >= codeIndent {
            consumeIndentedCode(baseIndent: codeIndent)
        } else if let fence = fenceInfo(body) {
            consumeFence(fence, indent: indent)
        } else if let level = atxLevel(body) {
            emit(.heading(level: level), runs: inlineRuns(text(atxContent(body, level: level))))
            index += 1
        } else if isThematicBreak(body) {
            emit(.thematicBreak)
            index += 1
        } else if listIndents.count < Self.maximumListDepth,
            let item = listItem(body, indent: indent)
        {
            return beginListItem(item)
        } else if let footnote = footnoteDefinition(body) {
            beginPending(kind: .footnote(label: footnote.label), first: text(footnote.rest))
            index += 1
        } else if isReferenceDefinition(body) {
            index += 1
        } else if isHTMLBlockStart(body) {
            consumeHTMLBlock(indent: indent)
        } else {
            beginPending(kind: .paragraph, first: text(body))
            index += 1
        }
        return nil
    }

    /// True when the line opens a block of its own, so it cannot continue a paragraph.
    private func startsBlock(_ body: Range<Int>) -> Bool {
        fenceInfo(body) != nil || atxLevel(body) != nil || isThematicBreak(body)
            || listItem(body, indent: 0) != nil || footnoteDefinition(body) != nil
            || isHTMLBlockStart(body)
    }

    /// Closes every list item the indent leaves, and with it the marker of an item that never
    /// emitted a block, so the marker cannot land on a block outside its list.
    private mutating func popLists(to indent: Int) {
        while let last = listIndents.last, indent < last {
            listIndents.removeLast()
            nextMarker = nil
        }
    }

    // MARK: Emitting

    private mutating func emit(_ kind: MarkdownBlock.Kind, runs: [MarkdownRun] = []) {
        blocks.append(
            MarkdownBlock(
                kind: kind, runs: runs, quoteDepth: quoteDepth, listDepth: listIndents.count,
                marker: takeMarker()))
    }

    private mutating func takeMarker() -> MarkdownListMarker? {
        defer { nextMarker = nil }
        return nextMarker
    }

    private mutating func beginPending(kind: MarkdownBlock.Kind, first line: String) {
        pending = Pending(
            lines: [line], kind: kind, quoteDepth: quoteDepth, listDepth: listIndents.count,
            marker: takeMarker())
    }

    private mutating func flushPending() {
        guard let open = pending else { return }
        pending = nil
        let joined = open.lines.joined(separator: "\n")
        guard joined.contains(where: { !$0.isWhitespace }) else { return }
        blocks.append(
            MarkdownBlock(
                kind: open.kind, runs: inlineRuns(joined), quoteDepth: open.quoteDepth,
                listDepth: open.listDepth, marker: open.marker))
    }

    private mutating func retitlePending(level: Int) {
        guard var open = pending else { return }
        open.kind = .heading(level: level)
        pending = open
        flushPending()
    }

    /// The one prose line a table header must be: a plain paragraph nothing else has joined.
    private func singlePendingLine() -> String? {
        guard let open = pending, open.kind == .paragraph, open.lines.count == 1 else {
            return nil
        }
        return open.lines[0]
    }

    private func inlineRuns(_ source: String) -> [MarkdownRun] {
        MarkdownInlineParser.runs(of: source, references: references)
    }
}

// MARK: - Detectors

extension MarkdownBlockParser {
    /// The opening or closing line of a fenced code block, with its info string.
    private func fenceInfo(_ body: Range<Int>) -> (marker: UInt8, count: Int, info: String)? {
        guard !body.isEmpty else { return nil }
        let marker = bytes[body.lowerBound]
        guard marker == Ascii.backtick || marker == Ascii.tilde else { return nil }
        var cursor = body.lowerBound
        while cursor < body.upperBound, bytes[cursor] == marker { cursor += 1 }
        guard cursor - body.lowerBound >= 3 else { return nil }
        let rest = trimmedEnd(of: trimmedStart(of: cursor..<body.upperBound))
        if marker == Ascii.backtick, bytes[rest].contains(Ascii.backtick) { return nil }
        return (marker, cursor - body.lowerBound, text(rest))
    }

    private func atxLevel(_ body: Range<Int>) -> Int? {
        var cursor = body.lowerBound
        while cursor < body.upperBound, bytes[cursor] == Ascii.hash { cursor += 1 }
        let level = cursor - body.lowerBound
        guard level >= 1, level <= 6 else { return nil }
        guard cursor == body.upperBound || Ascii.isSpace(bytes[cursor]) else { return nil }
        return level
    }

    /// The heading text, without the opening hashes or a closing run of decoration hashes.
    private func atxContent(_ body: Range<Int>, level: Int) -> Range<Int> {
        let range = trimmedEnd(
            of: trimmedStart(of: (body.lowerBound + level)..<body.upperBound))
        var end = range.upperBound
        while end > range.lowerBound, bytes[end - 1] == Ascii.hash { end -= 1 }
        guard end < range.upperBound else { return range }
        guard end == range.lowerBound || Ascii.isSpace(bytes[end - 1]) else { return range }
        return trimmedEnd(of: range.lowerBound..<end)
    }

    private func isThematicBreak(_ body: Range<Int>) -> Bool {
        guard !body.isEmpty else { return false }
        let marker = bytes[body.lowerBound]
        guard marker == Ascii.hyphen || marker == Ascii.asterisk || marker == Ascii.underscore
        else { return false }
        var count = 0
        for position in body {
            if bytes[position] == marker {
                count += 1
            } else if !Ascii.isSpace(bytes[position]) {
                return false
            }
        }
        return count >= 3
    }

    private func setextLevel(_ body: Range<Int>) -> Int? {
        guard !body.isEmpty else { return nil }
        let marker = bytes[body.lowerBound]
        guard marker == Ascii.equals || marker == Ascii.hyphen else { return nil }
        for position in trimmedEnd(of: body) where bytes[position] != marker { return nil }
        return marker == Ascii.equals ? 1 : 2
    }

    private func listItem(_ body: Range<Int>, indent: Int) -> ListItem? {
        guard !body.isEmpty, let start = listMarkerEnd(body) else { return nil }
        var (cursor, marker) = start
        guard cursor == body.upperBound || Ascii.isSpace(bytes[cursor]) else { return nil }
        let markerWidth = cursor - body.lowerBound
        var spaces = 0
        while cursor < body.upperBound, Ascii.isSpace(bytes[cursor]), spaces < 4 {
            cursor += 1
            spaces += 1
        }
        var rest = cursor..<body.upperBound
        if marker == .bullet, let task = taskMarker(rest) {
            marker = .task(done: task.done)
            rest = task.rest
        }
        return ListItem(
            marker: marker, contentIndent: indent + markerWidth + max(spaces, 1), rest: rest)
    }

    /// Where a `-`, `*`, `+`, `1.` or `1)` marker ends, and what kind it is.
    private func listMarkerEnd(_ body: Range<Int>) -> (Int, MarkdownListMarker)? {
        let first = bytes[body.lowerBound]
        if first == Ascii.hyphen || first == Ascii.plus || first == Ascii.asterisk {
            return (body.lowerBound + 1, .bullet)
        }
        guard Ascii.isDigit(first) else { return nil }
        var cursor = body.lowerBound
        var value = 0
        var digits = 0
        while cursor < body.upperBound, Ascii.isDigit(bytes[cursor]), digits < 9 {
            value = value * 10 + Int(bytes[cursor] - Ascii.zero)
            cursor += 1
            digits += 1
        }
        guard cursor < body.upperBound,
            bytes[cursor] == Ascii.dot || bytes[cursor] == Ascii.rightParen
        else { return nil }
        return (cursor + 1, .ordered(value))
    }

    private func taskMarker(_ rest: Range<Int>) -> (done: Bool, rest: Range<Int>)? {
        guard rest.count >= 3, bytes[rest.lowerBound] == Ascii.leftBracket,
            bytes[rest.lowerBound + 2] == Ascii.rightBracket
        else { return nil }
        let mark = bytes[rest.lowerBound + 1]
        let done: Bool
        if mark == Ascii.space {
            done = false
        } else if mark | 0x20 == 0x78 {
            done = true
        } else {
            return nil
        }
        let after = rest.lowerBound + 3
        guard after == rest.upperBound || Ascii.isSpace(bytes[after]) else { return nil }
        return (done, trimmedStart(of: after..<rest.upperBound))
    }

    private func footnoteDefinition(_ body: Range<Int>) -> (label: String, rest: Range<Int>)? {
        guard body.count > 4, bytes[body.lowerBound] == Ascii.leftBracket,
            bytes[body.lowerBound + 1] == Ascii.caret
        else { return nil }
        var cursor = body.lowerBound + 2
        while cursor < body.upperBound, bytes[cursor] != Ascii.rightBracket { cursor += 1 }
        guard cursor + 1 < body.upperBound, bytes[cursor + 1] == Ascii.colon else { return nil }
        let label = text((body.lowerBound + 2)..<cursor)
        guard !label.isEmpty else { return nil }
        return (label, trimmedStart(of: (cursor + 2)..<body.upperBound))
    }

    /// `[label]: destination "title"`, the out-of-line form of a link target.
    private func referenceDefinition(_ body: Range<Int>) -> (label: String, link: MarkdownLink)? {
        guard body.count > 3, bytes[body.lowerBound] == Ascii.leftBracket,
            bytes[body.lowerBound + 1] != Ascii.caret
        else { return nil }
        var cursor = body.lowerBound + 1
        while cursor < body.upperBound, bytes[cursor] != Ascii.rightBracket { cursor += 1 }
        guard cursor + 1 < body.upperBound, bytes[cursor] == Ascii.rightBracket,
            bytes[cursor + 1] == Ascii.colon
        else { return nil }
        let label = text((body.lowerBound + 1)..<cursor).lowercased()
        let after = trimmedStart(of: (cursor + 2)..<body.upperBound)
        guard !label.isEmpty, !after.isEmpty else { return nil }
        var end = after.lowerBound
        while end < after.upperBound, !Ascii.isSpace(bytes[end]) { end += 1 }
        let destination = MarkdownInlineParser.unangled(text(after.lowerBound..<end))
        let trailing = trimmedEnd(of: trimmedStart(of: end..<after.upperBound))
        let title = quotedTitle(trailing)
        guard trailing.isEmpty || title != nil else { return nil }
        return (label, MarkdownLink(destination: destination, title: title))
    }

    private func isReferenceDefinition(_ body: Range<Int>) -> Bool {
        referenceDefinition(body) != nil
    }

    private func quotedTitle(_ range: Range<Int>) -> String? {
        guard range.count >= 2 else { return nil }
        let open = bytes[range.lowerBound]
        let close = bytes[range.upperBound - 1]
        let matches =
            (open == Ascii.quote && close == Ascii.quote)
            || (open == Ascii.apostrophe && close == Ascii.apostrophe)
            || (open == Ascii.leftParen && close == Ascii.rightParen)
        guard matches else { return nil }
        return text((range.lowerBound + 1)..<(range.upperBound - 1))
    }

    /// Block-level tags whose line starts raw HTML. Inline tags such as `<b>` are left to the
    /// inline parser so prose around them keeps its formatting.
    private static let htmlBlockTags: Set<String> = [
        "address", "article", "aside", "blockquote", "center", "details", "dialog", "div", "dl",
        "dd", "dt", "fieldset", "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4",
        "h5", "h6", "header", "hr", "iframe", "li", "main", "nav", "ol", "p", "picture", "pre",
        "script", "section", "style", "summary", "table", "tbody", "td", "tfoot", "th", "thead",
        "tr", "ul", "video",
    ]

    private func isHTMLBlockStart(_ body: Range<Int>) -> Bool {
        guard body.count >= 2, bytes[body.lowerBound] == Ascii.lessThan else { return false }
        var cursor = body.lowerBound + 1
        if bytes[cursor] == Ascii.exclamation || bytes[cursor] == Ascii.question { return true }
        if bytes[cursor] == Ascii.slash { cursor += 1 }
        var end = cursor
        while end < body.upperBound, Ascii.isLetter(bytes[end]) || Ascii.isDigit(bytes[end]) {
            end += 1
        }
        guard end > cursor else { return false }
        return Self.htmlBlockTags.contains(text(cursor..<end).lowercased())
    }

    private func isTableDelimiter(_ body: Range<Int>) -> Bool {
        var sawPipe = false
        var sawDash = false
        for position in body {
            switch bytes[position] {
            case Ascii.pipe: sawPipe = true
            case Ascii.hyphen: sawDash = true
            case Ascii.colon, Ascii.space, Ascii.tab: continue
            default: return false
            }
        }
        return sawPipe && sawDash
    }
}

// MARK: - Multi-line blocks

extension MarkdownBlockParser {
    private mutating func parseFrontMatter() {
        guard index == 0, lines.count > 1, text(trimmedEnd(of: lines[0])) == "---" else { return }
        var cursor = 1
        while cursor < lines.count {
            let line = text(trimmedEnd(of: lines[cursor]))
            if line == "---" || line == "..." { break }
            cursor += 1
        }
        guard cursor < lines.count else { return }
        let body = (1..<cursor).map { text(lines[$0]) }.joined(separator: "\n")
        blocks.append(MarkdownBlock(kind: .frontMatter(text: body)))
        index = cursor + 1
    }

    /// Collects `[label]: target` definitions before the main pass, because a reference may be
    /// used above the line that defines it. Fenced and indented code is skipped so a sample
    /// cannot define one, and so is a line that continues a paragraph, which the main pass
    /// shows as text.
    private mutating func collectReferences() {
        var fence: (marker: UInt8, count: Int, depth: Int)?
        var inParagraph = false
        for line in lines {
            if let open = fence, let rest = fenceBodyLine(line, depth: open.depth) {
                if closesFence(trimmedStart(of: rest), opening: (open.marker, open.count)) {
                    fence = nil
                }
                continue
            }
            fence = nil
            let (depth, rest) = strippingQuotes(line)
            let body = trimmedStart(of: rest)
            if body.isEmpty {
                inParagraph = false
                continue
            }
            if indentWidth(of: rest) >= 4 { continue }
            if let opening = fenceInfo(body) {
                fence = (opening.marker, opening.count, depth)
                inParagraph = false
                continue
            }
            if !inParagraph, let definition = referenceDefinition(body) {
                if references[definition.label] == nil {
                    references[definition.label] = definition.link
                }
                continue
            }
            inParagraph = atxLevel(body) == nil && !isThematicBreak(body)
        }
    }

    /// The line with the fence's `depth` quote markers removed, or nil when it has fewer: the
    /// quote the fence opened in has ended, and the fence with it.
    private func fenceBodyLine(_ line: Range<Int>, depth: Int) -> Range<Int>? {
        let (found, rest) = strippingQuotes(line, limit: depth)
        return found == depth ? rest : nil
    }

    /// True when the line closes `opening`: the same marker, at least as many of it, no info.
    private func closesFence(_ body: Range<Int>, opening: (marker: UInt8, count: Int)) -> Bool {
        guard let closing = fenceInfo(body) else { return false }
        return closing.marker == opening.marker && closing.count >= opening.count
            && closing.info.isEmpty
    }

    /// Opens the item and returns the rest of its line, or nil when the marker ends the line.
    private mutating func beginListItem(_ item: ListItem) -> (body: Range<Int>, indent: Int)? {
        listIndents.append(item.contentIndent)
        nextMarker = item.marker
        let rest = trimmedStart(of: item.rest)
        guard !rest.isEmpty else {
            index += 1
            return nil
        }
        return (rest, item.contentIndent)
    }

    private mutating func consumeFence(
        _ fence: (marker: UInt8, count: Int, info: String), indent: Int
    ) {
        let language = fence.info.split(whereSeparator: { $0 == " " || $0 == "," }).first
            .map { String($0).lowercased() }
        index += 1
        var body: [String] = []
        while index < lines.count {
            guard let rest = fenceBodyLine(lines[index], depth: quoteDepth) else { break }
            let trimmed = trimmedStart(of: rest)
            if closesFence(trimmed, opening: (fence.marker, fence.count)) {
                index += 1
                break
            }
            body.append(text(rest, droppingIndent: indent))
            index += 1
        }
        emit(.code(language: language, text: body.joined(separator: "\n")))
    }

    /// Four-space code. Trailing blank lines belong to whatever follows, so they are only
    /// committed once another indented line proves the block continues.
    private mutating func consumeIndentedCode(baseIndent: Int) {
        var body: [String] = []
        var blanks: [String] = []
        while index < lines.count {
            let (depth, rest) = strippingQuotes(lines[index])
            if trimmedStart(of: rest).isEmpty {
                blanks.append("")
                index += 1
                continue
            }
            guard depth == quoteDepth, indentWidth(of: rest) >= baseIndent else { break }
            body.append(contentsOf: blanks)
            blanks.removeAll()
            body.append(text(rest, droppingIndent: baseIndent))
            index += 1
        }
        index -= blanks.count
        emit(.code(language: nil, text: body.joined(separator: "\n")))
    }

    private mutating func consumeHTMLBlock(indent: Int) {
        var body: [String] = []
        while index < lines.count {
            let rest = strippingQuotes(lines[index]).rest
            if trimmedStart(of: rest).isEmpty { break }
            body.append(text(rest, droppingIndent: indent))
            index += 1
        }
        emit(.html(text: body.joined(separator: "\n")))
    }

    /// Builds the table whose header row is `header` and whose alignments come from
    /// `delimiter`, then eats the body rows that follow.
    private mutating func consumeTable(header: String, delimiter: Range<Int>) {
        let alignments = Self.splitCells(text(delimiter)).map(Self.alignment(of:))
        guard !alignments.isEmpty else {
            beginPending(kind: .paragraph, first: header)
            return
        }
        let headerCells = cells(of: header, columns: alignments.count)
        var rows: [[[MarkdownRun]]] = []
        while index < lines.count {
            let (depth, rest) = strippingQuotes(lines[index])
            let body = trimmedStart(of: rest)
            guard depth == quoteDepth, !body.isEmpty, !startsBlock(body) else { break }
            rows.append(cells(of: text(body), columns: alignments.count))
            index += 1
        }
        emit(.table(MarkdownTable(alignments: alignments, header: headerCells, rows: rows)))
    }

    /// One row's cells, parsed as inline content and padded to the table's width so a renderer
    /// never has to check a row's length.
    private func cells(of line: String, columns: Int) -> [[MarkdownRun]] {
        var parsed = Self.splitCells(line).prefix(columns).map { inlineRuns($0) }
        while parsed.count < columns { parsed.append([]) }
        return Array(parsed)
    }

    /// Splits a table row on unescaped pipes, dropping the empty cells that the optional outer
    /// pipes produce.
    fileprivate static func splitCells(_ line: String) -> [String] {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        var cells: [String] = []
        var current = ""
        var escaped = false
        for character in trimmed {
            if escaped {
                if character != "|" { current.append("\\") }
                current.append(character)
                escaped = false
            } else if character == "\\" {
                escaped = true
            } else if character == "|" {
                cells.append(current)
                current = ""
            } else {
                current.append(character)
            }
        }
        if escaped { current.append("\\") }
        cells.append(current)
        if trimmed.hasPrefix("|"), !cells.isEmpty { cells.removeFirst() }
        if trimmed.hasSuffix("|"), !cells.isEmpty { cells.removeLast() }
        return cells.map { $0.trimmingCharacters(in: .whitespaces) }
    }

    fileprivate static func alignment(of cell: String) -> MarkdownTable.Alignment {
        switch (cell.hasPrefix(":"), cell.hasSuffix(":")) {
        case (true, true): return .center
        case (true, false): return .leading
        case (false, true): return .trailing
        case (false, false): return .natural
        }
    }
}

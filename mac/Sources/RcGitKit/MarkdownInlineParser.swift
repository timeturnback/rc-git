import Foundation

/// Turns one block's source into styled runs. Like the block parser it scans UTF-8 bytes, and
/// it emits runs as it goes rather than building a tree: a nested construct parses its own
/// contents recursively with the surrounding style folded in, so the result is already flat.
enum MarkdownInlineParser {
    /// How deep nested emphasis and links may go before the parser stops recursing and treats
    /// the delimiter as literal text. Bounds the stack on pathological input.
    static let maximumDepth = 12

    static func runs(of source: String, references: [String: MarkdownLink]) -> [MarkdownRun] {
        guard !source.isEmpty else { return [] }
        var scanner = InlineScanner(bytes: Array(source.utf8), references: references)
        scanner.parse(0..<scanner.bytes.count, style: [], link: nil, depth: 0)
        return scanner.output
    }

    /// Strips the `<…>` an author may wrap a destination in.
    static func unangled(_ destination: String) -> String {
        guard destination.hasPrefix("<"), destination.hasSuffix(">"), destination.count >= 2
        else { return destination }
        return String(destination.dropFirst().dropLast())
    }

    static func unquoted(_ title: String) -> String? {
        guard title.count >= 2 else { return nil }
        let pairs = [("\"", "\""), ("'", "'"), ("(", ")")]
        for (open, close) in pairs where title.hasPrefix(open) && title.hasSuffix(close) {
            return String(title.dropFirst().dropLast())
        }
        return nil
    }
}

/// The inline scanner's mutable state: the bytes, the runs emitted so far, and the plain text
/// accumulated since the last run was closed.
struct InlineScanner {
    let bytes: [UInt8]
    let references: [String: MarkdownLink]
    /// Where each code span that opens at a backtick ends (one past its closing run). Code spans
    /// bind tighter than emphasis and brackets, so the closer searches jump over them.
    let codeSpanEnds: [Int: Int]
    var output: [MarkdownRun] = []
    private var buffer: [UInt8] = []

    init(bytes: [UInt8], references: [String: MarkdownLink]) {
        self.bytes = bytes
        self.references = references
        self.codeSpanEnds = Self.codeSpans(in: bytes)
    }

    /// Pairs backtick runs by exact width, left to right, skipping escaped bytes outside spans.
    private static func codeSpans(in bytes: [UInt8]) -> [Int: Int] {
        var spans: [Int: Int] = [:]
        var cursor = 0
        while cursor < bytes.count {
            switch bytes[cursor] {
            case Ascii.backslash:
                cursor += 2
            case Ascii.backtick:
                var open = cursor
                while open < bytes.count, bytes[open] == Ascii.backtick { open += 1 }
                guard let close = closingBackticks(in: bytes, width: open - cursor, from: open)
                else {
                    cursor = open
                    continue
                }
                spans[cursor] = close
                cursor = close
            default:
                cursor += 1
            }
        }
        return spans
    }

    /// One past the first backtick run of exactly `width` at or after `from`.
    private static func closingBackticks(in bytes: [UInt8], width: Int, from: Int) -> Int? {
        var search = from
        while search < bytes.count {
            guard bytes[search] == Ascii.backtick else {
                search += 1
                continue
            }
            var close = search
            while close < bytes.count, bytes[close] == Ascii.backtick { close += 1 }
            if close - search == width { return close }
            search = close
        }
        return nil
    }

    mutating func parse(
        _ range: Range<Int>, style: MarkdownStyle, link: MarkdownLink?, depth: Int
    ) {
        var cursor = range.lowerBound
        while cursor < range.upperBound {
            if let next = special(
                at: cursor, end: range.upperBound, style: style, link: link, depth: depth)
            {
                cursor = next
            } else {
                buffer.append(bytes[cursor])
                cursor += 1
            }
        }
        flush(style: style, link: link)
    }

    /// Tries every construct that can start at `cursor`; `nil` means the byte is literal text.
    private mutating func special(
        at cursor: Int, end: Int, style: MarkdownStyle, link: MarkdownLink?, depth: Int
    ) -> Int? {
        switch bytes[cursor] {
        case Ascii.backslash:
            return escape(at: cursor, end: end)
        case Ascii.backtick:
            return codeSpan(at: cursor, end: end, style: style, link: link)
        case Ascii.asterisk, Ascii.underscore, Ascii.tilde:
            return emphasis(at: cursor, end: end, style: style, link: link, depth: depth)
        case Ascii.leftBracket:
            return bracket(
                at: cursor, end: end, style: style, link: link, depth: depth, image: false)
        case Ascii.exclamation:
            guard cursor + 1 < end, bytes[cursor + 1] == Ascii.leftBracket else { return nil }
            return bracket(
                at: cursor + 1, end: end, style: style, link: link, depth: depth, image: true)
        case Ascii.lessThan:
            return angle(at: cursor, end: end, style: style, link: link)
        case Ascii.newline:
            return lineBreak(at: cursor, end: end)
        case 0x68, 0x48, 0x77, 0x57:
            return bareURL(at: cursor, end: end, style: style, link: link)
        default:
            return nil
        }
    }

    private mutating func flush(style: MarkdownStyle, link: MarkdownLink?) {
        guard !buffer.isEmpty else { return }
        output.append(
            MarkdownRun(text: String(decoding: buffer, as: UTF8.self), style: style, link: link))
        buffer.removeAll(keepingCapacity: true)
    }

    // MARK: Constructs

    private mutating func escape(at cursor: Int, end: Int) -> Int? {
        guard cursor + 1 < end else { return nil }
        let next = bytes[cursor + 1]
        if next == Ascii.newline {
            buffer.append(Ascii.newline)
            return cursor + 2
        }
        guard Self.isEscapable(next) else { return nil }
        buffer.append(next)
        return cursor + 2
    }

    /// A backtick run closes only on a run of exactly the same length, which is what lets
    /// ``a ` b`` hold a backtick. A run with no closer is literal as a whole, never reopened
    /// one byte later as a narrower one.
    private mutating func codeSpan(
        at cursor: Int, end: Int, style: MarkdownStyle, link: MarkdownLink?
    ) -> Int? {
        var open = cursor
        while open < end, bytes[open] == Ascii.backtick { open += 1 }
        let width = open - cursor
        var search = open
        while search < end {
            guard bytes[search] == Ascii.backtick else {
                search += 1
                continue
            }
            var close = search
            while close < end, bytes[close] == Ascii.backtick { close += 1 }
            if close - search == width {
                emitCode(open..<search, style: style, link: link)
                return close
            }
            search = close
        }
        buffer.append(contentsOf: bytes[cursor..<open])
        return open
    }

    private mutating func emitCode(
        _ range: Range<Int>, style: MarkdownStyle, link: MarkdownLink?
    ) {
        flush(style: style, link: link)
        var content = String(decoding: bytes[range], as: UTF8.self)
            .replacingOccurrences(of: "\n", with: " ")
        if content.count > 2, content.hasPrefix(" "), content.hasSuffix(" ") {
            content = String(content.dropFirst().dropLast())
        }
        guard !content.isEmpty else { return }
        output.append(MarkdownRun(text: content, style: style.union(.code), link: link))
    }

    private mutating func emphasis(
        at cursor: Int, end: Int, style: MarkdownStyle, link: MarkdownLink?, depth: Int
    ) -> Int? {
        guard depth < MarkdownInlineParser.maximumDepth else { return nil }
        let marker = bytes[cursor]
        var run = cursor
        while run < end, bytes[run] == marker { run += 1 }
        guard let (width, added) = Self.delimiter(marker: marker, runLength: run - cursor)
        else { return nil }
        guard canOpen(marker: marker, at: cursor, width: width, end: end) else { return nil }
        guard let close = closingRun(marker: marker, width: width, from: cursor + width, end: end)
        else { return nil }
        flush(style: style, link: link)
        parse((cursor + width)..<close, style: style.union(added), link: link, depth: depth + 1)
        return close + width
    }

    /// How many delimiter bytes to consume, and what that run means.
    private static func delimiter(marker: UInt8, runLength: Int) -> (Int, MarkdownStyle)? {
        if marker == Ascii.tilde {
            return runLength == 2 ? (2, .strikethrough) : nil
        }
        switch runLength {
        case 0: return nil
        case 1: return (1, .emphasis)
        case 2: return (2, .strong)
        default: return (3, [.strong, .emphasis])
        }
    }

    private func canOpen(marker: UInt8, at cursor: Int, width: Int, end: Int) -> Bool {
        let after = cursor + width
        guard after < end, !Ascii.isSpace(bytes[after]), bytes[after] != Ascii.newline else {
            return false
        }
        guard marker == Ascii.underscore, cursor > 0 else { return true }
        return !Ascii.isLetter(bytes[cursor - 1]) && !Ascii.isDigit(bytes[cursor - 1])
    }

    /// Where the matching closing run starts, or `nil` when the delimiter never closes. A
    /// same-marker run that opens on the way is pushed, and a later run closes those inner
    /// openers first, innermost out, so `*a **b** c*` keeps its strong run inside the
    /// emphasis. An inner opener the closing run cannot fit stays literal.
    private func closingRun(marker: UInt8, width: Int, from: Int, end: Int) -> Int? {
        var openers: [Int] = []
        var cursor = from
        while cursor < end {
            if bytes[cursor] == Ascii.backslash {
                cursor += 2
                continue
            }
            if let spanEnd = codeSpanEnds[cursor], spanEnd <= end {
                cursor = spanEnd
                continue
            }
            guard bytes[cursor] == marker else {
                cursor += 1
                continue
            }
            var stop = cursor
            while stop < end, bytes[stop] == marker { stop += 1 }
            if canClose(marker: marker, run: cursor..<stop, from: from, end: end) {
                var remaining = stop - cursor
                while let inner = openers.last, inner <= remaining {
                    remaining -= inner
                    openers.removeLast()
                }
                if remaining >= width { return stop - remaining }
            } else if opensInside(run: cursor..<stop, from: from, end: end) {
                openers.append(stop - cursor)
            }
            cursor = stop
        }
        return nil
    }

    private func canClose(marker: UInt8, run: Range<Int>, from: Int, end: Int) -> Bool {
        guard run.lowerBound > from else { return false }
        let before = bytes[run.lowerBound - 1]
        guard !Ascii.isSpace(before), before != Ascii.newline else { return false }
        return closes(marker: marker, after: run.upperBound, end: end)
    }

    /// A run inside a delimiter's span that opens a nested one of the same marker.
    private func opensInside(run: Range<Int>, from: Int, end: Int) -> Bool {
        guard run.upperBound < end else { return false }
        let after = bytes[run.upperBound]
        guard !Ascii.isSpace(after), after != Ascii.newline else { return false }
        guard run.lowerBound > from else { return true }
        let before = bytes[run.lowerBound - 1]
        return Ascii.isSpace(before) || before == Ascii.newline
    }

    /// `_` does not close inside a word, which is what keeps `snake_case_name` intact.
    private func closes(marker: UInt8, after stop: Int, end: Int) -> Bool {
        guard marker == Ascii.underscore, stop < end else { return true }
        return !Ascii.isLetter(bytes[stop]) && !Ascii.isDigit(bytes[stop])
    }
}

// MARK: - Links, images, and footnotes

extension InlineScanner {
    /// `[text](target)`, `[text][label]`, `[label]`, `![alt](src)`, and `[^note]`. `cursor` is
    /// the opening bracket; for an image the `!` before it has already been skipped.
    fileprivate mutating func bracket(
        at cursor: Int, end: Int, style: MarkdownStyle, link: MarkdownLink?, depth: Int,
        image: Bool
    ) -> Int? {
        guard depth < MarkdownInlineParser.maximumDepth,
            let close = matchingBracket(from: cursor, end: end)
        else { return nil }
        let inner = (cursor + 1)..<close
        if !image, inner.count > 1, bytes[inner.lowerBound] == Ascii.caret {
            emitFootnoteMarker(inner, style: style, link: link)
            return close + 1
        }
        guard let target = destination(inner: inner, after: close + 1, end: end) else {
            return nil
        }
        flush(style: style, link: link)
        if image {
            emitImage(inner, target: link ?? target.link, style: style)
        } else {
            parse(inner, style: style, link: target.link, depth: depth + 1)
        }
        return target.end
    }

    private mutating func emitFootnoteMarker(
        _ inner: Range<Int>, style: MarkdownStyle, link: MarkdownLink?
    ) {
        flush(style: style, link: link)
        let label = String(
            decoding: bytes[(inner.lowerBound + 1)..<inner.upperBound], as: UTF8.self)
        output.append(
            MarkdownRun(
                text: label, style: style.union(.footnoteMarker),
                link: MarkdownLink(destination: "#fn-\(label)")))
    }

    private mutating func emitImage(
        _ inner: Range<Int>, target: MarkdownLink, style: MarkdownStyle
    ) {
        let alt = String(decoding: bytes[inner], as: UTF8.self)
        output.append(
            MarkdownRun(
                text: alt.isEmpty ? target.destination : alt, style: style, link: target,
                isImage: true))
    }

    /// The target that follows a bracket, whether written inline or as a reference.
    private func destination(inner: Range<Int>, after: Int, end: Int)
        -> (link: MarkdownLink, end: Int)?
    {
        if after < end, bytes[after] == Ascii.leftParen,
            let inline = inlineDestination(from: after, end: end)
        {
            return inline
        }
        return referenceDestination(inner: inner, after: after, end: end)
    }

    private func inlineDestination(from cursor: Int, end: Int) -> (link: MarkdownLink, end: Int)? {
        var position = cursor + 1
        while position < end, Ascii.isSpace(bytes[position]) { position += 1 }
        guard let target = destinationText(from: position, end: end) else { return nil }
        var tail = target.end
        while tail < end, Ascii.isSpace(bytes[tail]) || bytes[tail] == Ascii.newline { tail += 1 }
        var titleEnd = tail
        var depth = 1
        while titleEnd < end {
            if bytes[titleEnd] == Ascii.backslash {
                titleEnd += 2
                continue
            }
            if bytes[titleEnd] == Ascii.leftParen { depth += 1 }
            if bytes[titleEnd] == Ascii.rightParen {
                depth -= 1
                if depth == 0 { break }
            }
            titleEnd += 1
        }
        guard titleEnd < end, depth == 0 else { return nil }
        let raw = String(decoding: bytes[tail..<titleEnd], as: UTF8.self)
            .trimmingCharacters(in: .whitespaces)
        let title = MarkdownInlineParser.unquoted(raw)
        return (MarkdownLink(destination: target.text, title: title), titleEnd + 1)
    }

    /// The destination itself: either `<anything but a newline>` or a run without whitespace.
    private func destinationText(from cursor: Int, end: Int) -> (text: String, end: Int)? {
        if cursor < end, bytes[cursor] == Ascii.lessThan {
            var stop = cursor + 1
            while stop < end, bytes[stop] != Ascii.greaterThan, bytes[stop] != Ascii.newline {
                stop += 1
            }
            guard stop < end, bytes[stop] == Ascii.greaterThan else { return nil }
            return (String(decoding: bytes[(cursor + 1)..<stop], as: UTF8.self), stop + 1)
        }
        var stop = cursor
        var depth = 0
        while stop < end {
            let byte = bytes[stop]
            if Ascii.isSpace(byte) || byte == Ascii.newline { break }
            if byte == Ascii.leftParen { depth += 1 }
            if byte == Ascii.rightParen {
                if depth == 0 { break }
                depth -= 1
            }
            stop += 1
        }
        return (String(decoding: bytes[cursor..<stop], as: UTF8.self), stop)
    }

    private func referenceDestination(inner: Range<Int>, after: Int, end: Int)
        -> (link: MarkdownLink, end: Int)?
    {
        if after < end, bytes[after] == Ascii.leftBracket,
            let close = matchingBracket(from: after, end: end)
        {
            let label = String(decoding: bytes[(after + 1)..<close], as: UTF8.self)
            let key = label.isEmpty ? String(decoding: bytes[inner], as: UTF8.self) : label
            guard let link = references[key.lowercased()] else { return nil }
            return (link, close + 1)
        }
        let key = String(decoding: bytes[inner], as: UTF8.self).lowercased()
        guard let link = references[key] else { return nil }
        return (link, after)
    }

    private func matchingBracket(from cursor: Int, end: Int) -> Int? {
        var depth = 0
        var position = cursor
        while position < end {
            switch bytes[position] {
            case Ascii.backslash:
                position += 1
            case Ascii.backtick:
                if let spanEnd = codeSpanEnds[position], spanEnd <= end { position = spanEnd - 1 }
            case Ascii.leftBracket:
                depth += 1
            case Ascii.rightBracket:
                depth -= 1
                if depth == 0 { return position }
            default:
                break
            }
            position += 1
        }
        return nil
    }
}

// MARK: - Angle brackets, breaks, and bare URLs

extension InlineScanner {
    /// `<https://…>` and `<user@host>` become links; a recognised HTML tag is dropped so its
    /// markup does not show as prose; anything else stays literal.
    fileprivate mutating func angle(
        at cursor: Int, end: Int, style: MarkdownStyle, link: MarkdownLink?
    ) -> Int? {
        var stop = cursor + 1
        while stop < end, bytes[stop] != Ascii.greaterThan, bytes[stop] != Ascii.newline,
            bytes[stop] != Ascii.space
        {
            stop += 1
        }
        guard stop < end, bytes[stop] == Ascii.greaterThan else {
            return htmlTag(at: cursor, end: end)
        }
        let content = String(decoding: bytes[(cursor + 1)..<stop], as: UTF8.self)
        guard link == nil, Self.isAutolink(content) else { return htmlTag(at: cursor, end: end) }
        flush(style: style, link: link)
        let destination =
            content.contains("@") && !content.contains(":")
            ? "mailto:\(content)" : content
        output.append(
            MarkdownRun(
                text: content, style: style, link: MarkdownLink(destination: destination)))
        return stop + 1
    }

    /// A CommonMark open or closing tag: the name starts with a letter, an open tag carries only
    /// well-formed attributes, and anything else returns nil so the `<` stays literal prose.
    private mutating func htmlTag(at cursor: Int, end: Int) -> Int? {
        var position = cursor + 1
        let closing = position < end && bytes[position] == Ascii.slash
        if closing { position += 1 }
        guard position < end, Ascii.isLetter(bytes[position]) else { return nil }
        var nameEnd = position + 1
        while nameEnd < end, Self.isTagNameByte(bytes[nameEnd]) { nameEnd += 1 }
        let found =
            closing
            ? closingTagEnd(from: nameEnd, end: end) : openTagEnd(from: nameEnd, end: end)
        guard let close = found else { return nil }
        if String(decoding: bytes[position..<nameEnd], as: UTF8.self).lowercased() == "br" {
            buffer.append(Ascii.newline)
        }
        return close + 1
    }

    /// The index of the `>` ending a closing tag: only whitespace may follow its name.
    private func closingTagEnd(from start: Int, end: Int) -> Int? {
        let close = skipTagSpace(from: start, end: end)
        return close < end && bytes[close] == Ascii.greaterThan ? close : nil
    }

    /// The index of the `>` ending an open tag: attributes, each after whitespace, then an
    /// optional `/`.
    private func openTagEnd(from start: Int, end: Int) -> Int? {
        var position = start
        while true {
            let next = skipTagSpace(from: position, end: end)
            guard next < end else { return nil }
            if bytes[next] == Ascii.greaterThan { return next }
            if bytes[next] == Ascii.slash {
                let close = next + 1
                return close < end && bytes[close] == Ascii.greaterThan ? close : nil
            }
            guard next > position, let attributeEnd = attributeEnd(from: next, end: end) else {
                return nil
            }
            position = attributeEnd
        }
    }

    /// One attribute: a name `[A-Za-z_:][A-Za-z0-9_.:-]*`, optionally `=` and a value.
    private func attributeEnd(from start: Int, end: Int) -> Int? {
        guard Self.isAttributeNameStart(bytes[start]) else { return nil }
        var nameEnd = start + 1
        while nameEnd < end, Self.isAttributeNameByte(bytes[nameEnd]) { nameEnd += 1 }
        let equals = skipTagSpace(from: nameEnd, end: end)
        guard equals < end, bytes[equals] == Ascii.equals else { return nameEnd }
        return attributeValueEnd(from: skipTagSpace(from: equals + 1, end: end), end: end)
    }

    /// A single-quoted, double-quoted or unquoted attribute value.
    private func attributeValueEnd(from start: Int, end: Int) -> Int? {
        guard start < end else { return nil }
        let quote = bytes[start]
        if quote == Ascii.quote || quote == Ascii.apostrophe {
            var stop = start + 1
            while stop < end, bytes[stop] != quote { stop += 1 }
            return stop < end ? stop + 1 : nil
        }
        var stop = start
        while stop < end, Self.isUnquotedValueByte(bytes[stop]) { stop += 1 }
        return stop > start ? stop : nil
    }

    private func skipTagSpace(from start: Int, end: Int) -> Int {
        var position = start
        while position < end, Self.isTagSpace(bytes[position]) { position += 1 }
        return position
    }

    private static func isTagSpace(_ byte: UInt8) -> Bool {
        Ascii.isSpace(byte) || byte == Ascii.newline || byte == Ascii.carriageReturn
    }

    private static func isTagNameByte(_ byte: UInt8) -> Bool {
        Ascii.isLetter(byte) || Ascii.isDigit(byte) || byte == Ascii.hyphen
    }

    private static func isAttributeNameStart(_ byte: UInt8) -> Bool {
        Ascii.isLetter(byte) || byte == Ascii.underscore || byte == Ascii.colon
    }

    private static func isAttributeNameByte(_ byte: UInt8) -> Bool {
        isAttributeNameStart(byte) || Ascii.isDigit(byte) || byte == Ascii.dot
            || byte == Ascii.hyphen
    }

    private static func isUnquotedValueByte(_ byte: UInt8) -> Bool {
        !isTagSpace(byte) && byte != Ascii.quote && byte != Ascii.apostrophe
            && byte != Ascii.equals && byte != Ascii.lessThan && byte != Ascii.greaterThan
            && byte != Ascii.backtick
    }

    /// A newline inside a block: two trailing spaces or a trailing backslash keep the break,
    /// anything else folds into a space so the preview can wrap the paragraph itself.
    fileprivate mutating func lineBreak(at cursor: Int, end: Int) -> Int? {
        var trailing = 0
        while buffer.count > trailing, buffer[buffer.count - 1 - trailing] == Ascii.space {
            trailing += 1
        }
        buffer.removeLast(trailing)
        let emitted = !buffer.isEmpty || !output.isEmpty
        if emitted, buffer.last != Ascii.newline {
            buffer.append(trailing >= 2 ? Ascii.newline : Ascii.space)
        }
        var next = cursor + 1
        while next < end, Ascii.isSpace(bytes[next]) { next += 1 }
        return next
    }

    /// GFM linkifies bare URLs. Only outside an existing link, and never mid-word.
    fileprivate mutating func bareURL(
        at cursor: Int, end: Int, style: MarkdownStyle, link: MarkdownLink?
    ) -> Int? {
        guard link == nil, cursor == 0 || !Ascii.isLetter(bytes[cursor - 1]) else { return nil }
        guard let prefix = Self.urlPrefixes.first(where: { matches($0, at: cursor, end: end) })
        else { return nil }
        var stop = cursor + prefix.count
        while stop < end, Self.isURLByte(bytes[stop]) { stop += 1 }
        while stop > cursor, Self.isTrailingPunctuation(bytes[stop - 1]),
            !closesParen(at: stop - 1, from: cursor)
        {
            stop -= 1
        }
        guard stop > cursor + prefix.count else { return nil }
        let visible = String(decoding: bytes[cursor..<stop], as: UTF8.self)
        flush(style: style, link: link)
        let destination = visible.lowercased().hasPrefix("www.") ? "https://\(visible)" : visible
        output.append(
            MarkdownRun(
                text: visible, style: style, link: MarkdownLink(destination: destination)))
        return stop
    }

    /// GFM keeps a trailing `)` that closes a `(` earlier in the URL, as in Wikipedia links.
    private func closesParen(at index: Int, from start: Int) -> Bool {
        guard bytes[index] == Ascii.rightParen else { return false }
        var depth = 0
        for byte in bytes[start..<index] {
            if byte == Ascii.leftParen { depth += 1 }
            if byte == Ascii.rightParen { depth -= 1 }
        }
        return depth > 0
    }

    private func matches(_ prefix: [UInt8], at cursor: Int, end: Int) -> Bool {
        guard cursor + prefix.count <= end else { return false }
        for offset in prefix.indices where bytes[cursor + offset] | 0x20 != prefix[offset] {
            return false
        }
        return true
    }

    private static let urlPrefixes: [[UInt8]] = [
        Array("https://".utf8), Array("http://".utf8), Array("www.".utf8),
    ]

    private static func isURLByte(_ byte: UInt8) -> Bool {
        byte > Ascii.space && byte != Ascii.lessThan && byte != Ascii.greaterThan
            && byte != Ascii.quote && byte != Ascii.pipe && byte != Ascii.backslash
    }

    private static func isTrailingPunctuation(_ byte: UInt8) -> Bool {
        switch byte {
        case Ascii.dot, Ascii.colon, 0x2C, 0x3B, Ascii.exclamation, Ascii.question,
            Ascii.rightParen, Ascii.rightBracket, Ascii.apostrophe, Ascii.asterisk,
            Ascii.underscore, Ascii.tilde:
            return true
        default:
            return false
        }
    }

    static func isEscapable(_ byte: UInt8) -> Bool {
        switch byte {
        case 0x21...0x2F, 0x3A...0x40, 0x5B...0x60, 0x7B...0x7E:
            return true
        default:
            return false
        }
    }

    /// An autolink is a scheme followed by `:`, or something shaped like an email address.
    static func isAutolink(_ content: String) -> Bool {
        guard !content.isEmpty, !content.contains(" ") else { return false }
        if let colon = content.firstIndex(of: ":") {
            let scheme = content[content.startIndex..<colon]
            return scheme.count >= 2 && scheme.allSatisfy { $0.isLetter || $0 == "+" || $0 == "-" }
        }
        guard let at = content.firstIndex(of: "@") else { return false }
        return at != content.startIndex && content[at...].contains(".")
    }
}

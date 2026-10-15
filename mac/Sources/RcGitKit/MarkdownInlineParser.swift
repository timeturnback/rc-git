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
        scanner.parse(
            from: 0,
            in: InlineScanner.Frame(end: scanner.bytes.count, style: [], link: nil, depth: 0))
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
    /// What a construct inherits from the one around it: where its range ends, the style and
    /// link its runs carry, and how deep the nesting already is.
    struct Frame {
        let end: Int
        let style: MarkdownStyle
        let link: MarkdownLink?
        let depth: Int

        /// The frame for a construct nested inside this one.
        func nested(end: Int, style: MarkdownStyle, link: MarkdownLink?) -> Frame {
            Frame(end: end, style: style, link: link, depth: depth + 1)
        }
    }

    let bytes: [UInt8]
    let references: [String: MarkdownLink]
    /// The start of every maximal backtick run, ascending, keyed by the run's width. A code span
    /// closes on the next run of its own width, so each lookup is one binary search.
    let backtickRuns: [Int: [Int]]
    /// Where each code span that opens at a backtick ends (one past its closing run). Code spans
    /// bind tighter than emphasis and brackets, so the closer searches jump over them.
    let codeSpanEnds: [Int: Int]
    /// For each index, the first `]` at or after it that closes past the bracket depth there.
    let bracketClosers: [Int32]
    /// For each index, the first `)` at or after it that closes past the paren depth there.
    let parenClosers: [Int32]
    var output: [MarkdownRun] = []
    private var buffer: [UInt8] = []
    /// Per marker and range end, a start at or past which no emphasis run can close, learned
    /// from a search that failed. Keeps a paragraph of unmatched openers linear.
    private var unclosableFrom: [Int: Int] = [:]
    /// The last scan for the byte ending an autolink: where it started and where it stopped.
    private var autolinkScan = (from: 0, stop: -1)

    init(bytes: [UInt8], references: [String: MarkdownLink]) {
        self.bytes = bytes
        self.references = references
        let runs = Self.backtickRuns(in: bytes)
        let spans = Self.codeSpans(in: bytes, runs: runs)
        self.backtickRuns = runs
        self.codeSpanEnds = spans
        self.bracketClosers = Self.closers(
            in: bytes, open: Ascii.leftBracket, close: Ascii.rightBracket, skipping: spans)
        self.parenClosers = Self.closers(
            in: bytes, open: Ascii.leftParen, close: Ascii.rightParen, skipping: [:])
    }

    private static func backtickRuns(in bytes: [UInt8]) -> [Int: [Int]] {
        var runs: [Int: [Int]] = [:]
        var cursor = 0
        while cursor < bytes.count {
            guard bytes[cursor] == Ascii.backtick else {
                cursor += 1
                continue
            }
            var stop = cursor
            while stop < bytes.count, bytes[stop] == Ascii.backtick { stop += 1 }
            runs[stop - cursor, default: []].append(cursor)
            cursor = stop
        }
        return runs
    }

    /// Pairs backtick runs by exact width, left to right, skipping escaped bytes outside spans.
    private static func codeSpans(in bytes: [UInt8], runs: [Int: [Int]]) -> [Int: Int] {
        var spans: [Int: Int] = [:]
        var cursor = 0
        while cursor < bytes.count {
            switch bytes[cursor] {
            case Ascii.backslash:
                cursor += 2
            case Ascii.backtick:
                var open = cursor
                while open < bytes.count, bytes[open] == Ascii.backtick { open += 1 }
                let width = open - cursor
                guard let close = firstRun(of: width, atOrAfter: open, in: runs) else {
                    cursor = open
                    continue
                }
                spans[cursor] = close + width
                cursor = close + width
            default:
                cursor += 1
            }
        }
        return spans
    }

    /// The start of the first backtick run of exactly `width` at or after `from`.
    private static func firstRun(of width: Int, atOrAfter from: Int, in runs: [Int: [Int]])
        -> Int?
    {
        guard let starts = runs[width] else { return nil }
        var low = 0
        var high = starts.count
        while low < high {
            let middle = (low + high) / 2
            if starts[middle] < from { low = middle + 1 } else { high = middle }
        }
        return low < starts.count ? starts[low] : nil
    }

    /// For every index `i`, the first `close` at or after `i` that takes the depth below the
    /// depth at `i`, or -1. Escaped bytes and the spans in `skipping` count as neither. Pairing
    /// once per block is what keeps a run of unmatched openers from rescanning to the end.
    private static func closers(
        in bytes: [UInt8], open: UInt8, close: UInt8, skipping spans: [Int: Int]
    ) -> [Int32] {
        guard bytes.contains(open) else { return [] }
        let depths = depths(in: bytes, open: open, close: close, skipping: spans)
        let lowest = depths.min() ?? 0
        let highest = depths.max() ?? 0
        var firstAt = [Int32](repeating: -1, count: Int(highest - lowest) + 1)
        var result = [Int32](repeating: -1, count: depths.count)
        for index in stride(from: depths.count - 1, through: 0, by: -1) {
            let level = Int(depths[index] - lowest)
            if level > 0, firstAt[level - 1] > 0 { result[index] = firstAt[level - 1] - 1 }
            firstAt[level] = Int32(index)
        }
        return result
    }

    /// The `open` minus `close` balance before each index, and once more after the last byte.
    private static func depths(
        in bytes: [UInt8], open: UInt8, close: UInt8, skipping spans: [Int: Int]
    ) -> [Int32] {
        var depths = [Int32](repeating: 0, count: bytes.count + 1)
        var level: Int32 = 0
        var cursor = 0
        while cursor < bytes.count {
            let skipTo =
                bytes[cursor] == Ascii.backslash
                ? min(cursor + 2, bytes.count) : spans[cursor] ?? cursor
            if skipTo > cursor {
                for index in cursor..<skipTo { depths[index] = level }
                cursor = skipTo
                continue
            }
            depths[cursor] = level
            if bytes[cursor] == open { level += 1 } else if bytes[cursor] == close { level -= 1 }
            cursor += 1
        }
        depths[bytes.count] = level
        return depths
    }

    /// The closer `table` records for `index`, when it falls before `end`.
    func closer(in table: [Int32], from index: Int, end: Int) -> Int? {
        guard index < table.count else { return nil }
        let found = Int(table[index])
        return found >= 0 && found < end ? found : nil
    }

    /// True when `index` falls before `end` and holds `byte`.
    func has(_ byte: UInt8, at index: Int, end: Int) -> Bool {
        index < end && bytes[index] == byte
    }

    /// Parses `start..<frame.end`, emitting every run with the frame's style and link.
    mutating func parse(from start: Int, in frame: Frame) {
        var cursor = start
        while cursor < frame.end {
            if let next = special(at: cursor, in: frame) {
                cursor = next
            } else {
                buffer.append(bytes[cursor])
                cursor += 1
            }
        }
        flush(frame)
    }

    /// Tries every construct that can start at `cursor`; `nil` means the byte is literal text.
    private mutating func special(at cursor: Int, in frame: Frame) -> Int? {
        switch bytes[cursor] {
        case Ascii.backslash:
            return escape(at: cursor, end: frame.end)
        case Ascii.backtick:
            return codeSpan(at: cursor, in: frame)
        case Ascii.asterisk, Ascii.underscore, Ascii.tilde:
            return emphasis(at: cursor, in: frame)
        case Ascii.leftBracket:
            return bracket(at: cursor, in: frame, image: false)
        case Ascii.exclamation:
            guard has(Ascii.leftBracket, at: cursor + 1, end: frame.end) else { return nil }
            return bracket(at: cursor + 1, in: frame, image: true)
        case Ascii.lessThan:
            return angle(at: cursor, in: frame)
        case Ascii.newline:
            return lineBreak(at: cursor, end: frame.end)
        default:
            return Self.urlStart[Int(bytes[cursor])] ? bareURL(at: cursor, in: frame) : nil
        }
    }

    private mutating func flush(_ frame: Frame) {
        guard !buffer.isEmpty else { return }
        output.append(
            MarkdownRun(
                text: String(decoding: buffer, as: UTF8.self), style: frame.style,
                link: frame.link))
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
    private mutating func codeSpan(at cursor: Int, in frame: Frame) -> Int? {
        var open = cursor
        while has(Ascii.backtick, at: open, end: frame.end) { open += 1 }
        let width = open - cursor
        if let close = Self.firstRun(of: width, atOrAfter: open, in: backtickRuns),
            close + width <= frame.end
        {
            emitCode(open..<close, in: frame)
            return close + width
        }
        buffer.append(contentsOf: bytes[cursor..<open])
        return open
    }

    private mutating func emitCode(_ range: Range<Int>, in frame: Frame) {
        flush(frame)
        var content = String(decoding: bytes[range], as: UTF8.self)
            .replacingOccurrences(of: "\n", with: " ")
        if content.count > 2, content.hasPrefix(" "), content.hasSuffix(" ") {
            content = String(content.dropFirst().dropLast())
        }
        guard !content.isEmpty else { return }
        output.append(
            MarkdownRun(text: content, style: frame.style.union(.code), link: frame.link))
    }

    private mutating func emphasis(at cursor: Int, in frame: Frame) -> Int? {
        guard frame.depth < MarkdownInlineParser.maximumDepth else { return nil }
        let end = frame.end
        let marker = bytes[cursor]
        var run = cursor
        while has(marker, at: run, end: end) { run += 1 }
        guard let (width, added) = Self.delimiter(marker: marker, runLength: run - cursor)
        else { return nil }
        guard canOpen(marker: marker, at: cursor, width: width, end: end) else { return nil }
        guard let close = closingRun(marker: marker, width: width, from: cursor + width, end: end)
        else { return nil }
        flush(frame)
        parse(
            from: cursor + width,
            in: frame.nested(end: close, style: frame.style.union(added), link: frame.link))
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
    ///
    /// A failed search records the start of the last run that could close: a later search
    /// starting there or past it sees no closer at all, so it fails without scanning.
    private mutating func closingRun(marker: UInt8, width: Int, from: Int, end: Int) -> Int? {
        let key = end << 8 | Int(marker)
        if let limit = unclosableFrom[key], from >= limit { return nil }
        var openers: [Int] = []
        var lastCloser = from
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
                lastCloser = cursor
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
        unclosableFrom[key] = min(unclosableFrom[key] ?? lastCloser, lastCloser)
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
    fileprivate mutating func bracket(at cursor: Int, in frame: Frame, image: Bool) -> Int? {
        guard frame.depth < MarkdownInlineParser.maximumDepth,
            let close = matchingBracket(from: cursor, end: frame.end)
        else { return nil }
        let inner = (cursor + 1)..<close
        if !image, inner.count > 1, bytes[inner.lowerBound] == Ascii.caret {
            emitFootnoteMarker(inner, in: frame)
            return close + 1
        }
        guard let target = destination(inner: inner, after: close + 1, end: frame.end) else {
            return nil
        }
        flush(frame)
        if image {
            emitImage(inner, target: frame.link ?? target.link, style: frame.style)
        } else {
            parse(
                from: inner.lowerBound,
                in: frame.nested(end: inner.upperBound, style: frame.style, link: target.link))
        }
        return target.end
    }

    private mutating func emitFootnoteMarker(_ inner: Range<Int>, in frame: Frame) {
        flush(frame)
        let label = String(
            decoding: bytes[(inner.lowerBound + 1)..<inner.upperBound], as: UTF8.self)
        output.append(
            MarkdownRun(
                text: label, style: frame.style.union(.footnoteMarker),
                link: MarkdownLink(destination: "#" + MarkdownDocument.footnoteAnchor(for: label))))
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
        if has(Ascii.leftParen, at: after, end: end),
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
        guard let titleEnd = closer(in: parenClosers, from: tail, end: end) else { return nil }
        let raw = String(decoding: bytes[tail..<titleEnd], as: UTF8.self)
            .trimmingCharacters(in: .whitespaces)
        let title = MarkdownInlineParser.unquoted(raw)
        return (MarkdownLink(destination: target.text, title: title), titleEnd + 1)
    }

    /// The destination itself: either `<anything but a newline>` or a run without whitespace.
    private func destinationText(from cursor: Int, end: Int) -> (text: String, end: Int)? {
        guard has(Ascii.lessThan, at: cursor, end: end) else {
            return bareDestination(from: cursor, end: end)
        }
        return angledDestination(from: cursor, end: end)
    }

    /// `<…>`: anything but a newline up to the first `>`, which must be there.
    private func angledDestination(from cursor: Int, end: Int) -> (text: String, end: Int)? {
        var stop = cursor + 1
        while stop < end, bytes[stop] != Ascii.greaterThan, bytes[stop] != Ascii.newline {
            stop += 1
        }
        guard has(Ascii.greaterThan, at: stop, end: end) else { return nil }
        return (String(decoding: bytes[(cursor + 1)..<stop], as: UTF8.self), stop + 1)
    }

    /// A run without whitespace whose parentheses balance; an unopened `)` ends it.
    private func bareDestination(from cursor: Int, end: Int) -> (text: String, end: Int) {
        var stop = cursor
        var depth = 0
        while stop < end, !Ascii.isSpace(bytes[stop]), bytes[stop] != Ascii.newline {
            if bytes[stop] == Ascii.leftParen {
                depth += 1
            } else if bytes[stop] == Ascii.rightParen {
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
        if has(Ascii.leftBracket, at: after, end: end),
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
        closer(in: bracketClosers, from: cursor + 1, end: end)
    }
}

// MARK: - Angle brackets, breaks, and bare URLs

extension InlineScanner {
    /// `<https://…>` and `<user@host>` become links; a recognised HTML tag is dropped so its
    /// markup does not show as prose; anything else stays literal.
    fileprivate mutating func angle(at cursor: Int, in frame: Frame) -> Int? {
        let stop = autolinkStop(from: cursor + 1)
        guard has(Ascii.greaterThan, at: stop, end: frame.end) else {
            return htmlTag(at: cursor, end: frame.end)
        }
        let content = String(decoding: bytes[(cursor + 1)..<stop], as: UTF8.self)
        guard frame.link == nil, Self.isAutolink(content) else {
            return htmlTag(at: cursor, end: frame.end)
        }
        flush(frame)
        let destination =
            content.contains("@") && !content.contains(":")
            ? "mailto:\(content)" : content
        output.append(
            MarkdownRun(
                text: content, style: frame.style, link: MarkdownLink(destination: destination)))
        return stop + 1
    }

    /// The first `>`, newline or space at or after `from`. Every `<` before that byte stops
    /// there too, so the last scan answers them all.
    private mutating func autolinkStop(from: Int) -> Int {
        if from >= autolinkScan.from, from <= autolinkScan.stop { return autolinkScan.stop }
        var stop = from
        while stop < bytes.count, bytes[stop] != Ascii.greaterThan, bytes[stop] != Ascii.newline,
            bytes[stop] != Ascii.space
        {
            stop += 1
        }
        autolinkScan = (from, stop)
        return stop
    }

    /// A CommonMark open or closing tag: the name starts with a letter, an open tag carries only
    /// well-formed attributes, and anything else returns nil so the `<` stays literal prose.
    private mutating func htmlTag(at cursor: Int, end: Int) -> Int? {
        var position = cursor + 1
        let closing = has(Ascii.slash, at: position, end: end)
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
        return has(Ascii.greaterThan, at: close, end: end) ? close : nil
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
                return has(Ascii.greaterThan, at: next + 1, end: end) ? next + 1 : nil
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
        guard has(Ascii.equals, at: equals, end: end) else { return nameEnd }
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
    fileprivate mutating func bareURL(at cursor: Int, in frame: Frame) -> Int? {
        let end = frame.end
        guard frame.link == nil, cursor == 0 || !Ascii.isLetter(bytes[cursor - 1]) else {
            return nil
        }
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
        flush(frame)
        let destination = visible.lowercased().hasPrefix("www.") ? "https://\(visible)" : visible
        output.append(
            MarkdownRun(
                text: visible, style: frame.style, link: MarkdownLink(destination: destination)))
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

    /// Indexed by byte: true for the first byte of a `urlPrefixes` entry, in either case.
    private static let urlStart: [Bool] = {
        var table = [Bool](repeating: false, count: 256)
        for first in urlPrefixes.compactMap(\.first) {
            table[Int(first)] = true
            table[Int(first & ~0x20)] = true
        }
        return table
    }()

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
            return isAutolinkScheme(Array(content[content.startIndex..<colon].utf8))
        }
        guard let at = content.firstIndex(of: "@") else { return false }
        return at != content.startIndex && content[at...].contains(".")
    }

    /// CommonMark's scheme rule: an ASCII letter, then 1-31 ASCII letters, digits, `+`, `.` or `-`.
    private static func isAutolinkScheme(_ scheme: [UInt8]) -> Bool {
        (2...32).contains(scheme.count) && Ascii.isSchemeShaped(scheme)
    }
}

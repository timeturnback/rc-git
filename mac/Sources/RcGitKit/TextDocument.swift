/// A text blob prepared for display. Building one costs a single pass over the content, so
/// construct it off the main actor and hand the finished value to the view.
public struct TextDocument: Sendable, Hashable {
    /// The content, decoded as UTF-8 with invalid sequences replaced by U+FFFD.
    public let text: String
    /// Number of lines, following the core's rule: a trailing newline does not add a line.
    public let lineCount: Int
    /// Size of the original bytes.
    public let byteCount: Int
    /// Whether any byte sequence had to be replaced during decoding. Comes from the core's
    /// one-time UTF-8 check; nothing here re-scans the bytes.
    public let isLossy: Bool
    /// UTF-16 offsets where lines start, for the line-number ruler. Same rule as the core's
    /// byte offsets: empty text has none, a trailing newline does not start a line.
    public let utf16LineStarts: [Int]
    /// UTF-16 offsets of the characters other than LF that TextKit would start a new line at:
    /// a CR not followed by LF, VT, FF, NEL, U+2028 and U+2029. Git does not count them as
    /// line breaks, so the view draws them as visible markers to keep its rows on these lines.
    public let utf16NonLFBreaks: [Int]

    public init(bytes: [UInt8], lineStarts: [UInt32], validUTF8: Bool) {
        let text = String(decoding: bytes, as: UTF8.self)
        self.text = text
        self.lineCount = lineStarts.count
        self.byteCount = bytes.count
        self.isLossy = !validUTF8
        (self.utf16LineStarts, self.utf16NonLFBreaks) = Self.utf16Breaks(of: text)
    }

    /// Builds a document from a blob when it is text; `nil` for binary or oversize blobs.
    public init?(content: BlobContent) {
        guard case let .text(data, lineStarts, validUTF8) = content else { return nil }
        self.init(bytes: data, lineStarts: lineStarts, validUTF8: validUTF8)
    }

    /// One walk over the UTF-16 view, so the ruler never scans the string itself.
    private static func utf16Breaks(of text: String) -> (lineStarts: [Int], nonLF: [Int]) {
        var starts = [0]
        var nonLF: [Int] = []
        var offset = 0
        var previousWasCR = false
        for unit in text.utf16 {
            if previousWasCR, unit != lineFeed { nonLF.append(offset - 1) }
            previousWasCR = unit == carriageReturn
            offset += 1
            if unit == lineFeed {
                starts.append(offset)
            } else if otherSeparators.contains(unit) {
                nonLF.append(offset - 1)
            }
        }
        if previousWasCR { nonLF.append(offset - 1) }
        if offset == 0 { return ([], []) }
        if starts.count > 1, starts[starts.count - 1] == offset { starts.removeLast() }
        return (starts, nonLF)
    }

    private static let lineFeed: UInt16 = 0x0A
    private static let carriageReturn: UInt16 = 0x0D
    /// VT, FF, NEL, LINE SEPARATOR, PARAGRAPH SEPARATOR: each always ends a TextKit line.
    private static let otherSeparators: Set<UInt16> = [0x0B, 0x0C, 0x85, 0x2028, 0x2029]
}

/// Which line number the gutter draws beside a row of text.
public enum LineNumbering {
    /// The 1-based line whose start is exactly `utf16Offset`, or `nil` when the row begins
    /// anywhere else, so a row TextKit broke off mid-line gets no number of its own.
    /// `lineStarts` is sorted ascending, as `TextDocument.utf16LineStarts` is.
    public static func label(at utf16Offset: Int, lineStarts: [Int]) -> Int? {
        var low = 0
        var high = lineStarts.count
        while low < high {
            let mid = (low + high) / 2
            if lineStarts[mid] < utf16Offset { low = mid + 1 } else { high = mid }
        }
        guard low < lineStarts.count, lineStarts[low] == utf16Offset else { return nil }
        return low + 1
    }
}

/// Human-readable byte counts, e.g. `1.2 MB`.
public enum ByteFormat {
    /// Picks the unit and the digit count from the value as it will print, so a value that
    /// rounds up to 1000 carries to the next unit and one that rounds up to 10 drops its decimal.
    public static func string(_ bytes: UInt64) -> String {
        let units = ["B", "KB", "MB", "GB"]
        var value = Double(bytes)
        var unit = 0
        while value.rounded() >= 1000, unit < units.count - 1 {
            value /= 1000
            unit += 1
        }
        if unit == 0 { return "\(bytes) B" }
        let digits = (value * 10).rounded() / 10 < 10 ? 1 : 0
        return String(format: "%.\(digits)f %@", value, units[unit])
    }

    /// A size and the limit it is over, worded for "is <size>, over the <limit> limit". Falls
    /// back to exact byte counts when the short strings would read the same.
    public static func overLimit(size: UInt64, limit: UInt64) -> (size: String, limit: String) {
        let short = (size: string(size), limit: string(limit))
        guard short.size == short.limit else { return short }
        return (size: "\(grouped(size)) bytes", limit: "\(grouped(limit))-byte")
    }

    /// Digits in groups of three separated by commas, independent of the user's locale.
    static func grouped(_ value: UInt64) -> String {
        var digits = Array(String(value))
        var index = digits.count - 3
        while index > 0 {
            digits.insert(",", at: index)
            index -= 3
        }
        return String(digits)
    }
}

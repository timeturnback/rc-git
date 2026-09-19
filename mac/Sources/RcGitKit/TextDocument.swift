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

    public init(bytes: [UInt8], lineStarts: [UInt32], validUTF8: Bool) {
        let text = String(decoding: bytes, as: UTF8.self)
        self.text = text
        self.lineCount = lineStarts.count
        self.byteCount = bytes.count
        self.isLossy = !validUTF8
        self.utf16LineStarts = Self.utf16LineStarts(of: text)
    }

    /// Builds a document from a blob when it is text; `nil` for binary or oversize blobs.
    public init?(content: BlobContent) {
        guard case let .text(data, lineStarts, validUTF8) = content else { return nil }
        self.init(bytes: data, lineStarts: lineStarts, validUTF8: validUTF8)
    }

    /// One walk over the UTF-16 view, so the ruler never scans the string itself.
    private static func utf16LineStarts(of text: String) -> [Int] {
        var starts = [0]
        var offset = 0
        let newline: UInt16 = 0x0A
        for unit in text.utf16 {
            offset += 1
            if unit == newline { starts.append(offset) }
        }
        if offset == 0 { return [] }
        if starts.count > 1, starts[starts.count - 1] == offset { starts.removeLast() }
        return starts
    }
}

/// Human-readable byte counts, e.g. `1.2 MB`.
public enum ByteFormat {
    public static func string(_ bytes: UInt64) -> String {
        let units = ["B", "KB", "MB", "GB"]
        var value = Double(bytes)
        var unit = 0
        while value >= 1000, unit < units.count - 1 {
            value /= 1000
            unit += 1
        }
        if unit == 0 { return "\(bytes) B" }
        let digits = value < 10 ? 1 : 0
        return String(format: "%.\(digits)f %@", value, units[unit])
    }
}

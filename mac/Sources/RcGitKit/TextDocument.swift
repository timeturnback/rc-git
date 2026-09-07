/// A text blob prepared for display.
public struct TextDocument: Sendable, Hashable {
    /// The content, decoded as UTF-8 with invalid sequences replaced by U+FFFD.
    public let text: String
    /// Number of lines, following the core's rule: a trailing newline does not add a line.
    public let lineCount: Int
    /// Size of the original bytes.
    public let byteCount: Int
    /// Whether any byte sequence had to be replaced during decoding.
    public let isLossy: Bool

    public init(bytes: [UInt8], lineStarts: [UInt32]) {
        let text = String(decoding: bytes, as: UTF8.self)
        self.text = text
        self.lineCount = lineStarts.count
        self.byteCount = bytes.count
        self.isLossy = text.utf8.count != bytes.count || !text.utf8.elementsEqual(bytes)
    }

    /// Builds a document from a blob when it is text; `nil` for binary or oversize blobs.
    public init?(content: BlobContent) {
        guard case let .text(data, lineStarts) = content else { return nil }
        self.init(bytes: data, lineStarts: lineStarts)
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

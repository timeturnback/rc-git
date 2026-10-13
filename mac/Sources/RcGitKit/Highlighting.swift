import CRcGit
import Foundation
import os

/// What a highlighted region means. Mirrors the Rust highlighter's classes.
public enum HighlightClass: Sendable, Hashable {
    case attribute
    case comment
    case constant
    case emphasis
    case function
    case heading
    case keyword
    case link
    case number
    case `operator`
    case property
    case punctuation
    case string
    case strong
    case type
    case variable

    init(c: RcGitHighlightClass) {
        switch c {
        case RcGitHighlightClass_Attribute: self = .attribute
        case RcGitHighlightClass_Comment: self = .comment
        case RcGitHighlightClass_Constant: self = .constant
        case RcGitHighlightClass_Emphasis: self = .emphasis
        case RcGitHighlightClass_Function: self = .function
        case RcGitHighlightClass_Heading: self = .heading
        case RcGitHighlightClass_Keyword: self = .keyword
        case RcGitHighlightClass_Link: self = .link
        case RcGitHighlightClass_Number: self = .number
        case RcGitHighlightClass_Operator: self = .operator
        case RcGitHighlightClass_Property: self = .property
        case RcGitHighlightClass_Punctuation: self = .punctuation
        case RcGitHighlightClass_String: self = .string
        case RcGitHighlightClass_Strong: self = .strong
        case RcGitHighlightClass_Type: self = .type
        default: self = .variable
        }
    }
}

/// One highlighted region: byte offsets into the UTF-8 content, half-open, non-overlapping,
/// ordered by start.
public struct HighlightSpan: Sendable, Hashable {
    public let start: UInt32
    public let end: UInt32
    public let `class`: HighlightClass

    public init(start: UInt32, end: UInt32, class highlightClass: HighlightClass) {
        self.start = start
        self.end = end
        self.class = highlightClass
    }
}

/// Syntax highlighting through the Rust core. Pure and repository-free: bytes in, spans out.
public enum Highlighter {
    /// Cap on what gets parsed: highlighting is decoration, and a parse of tens of megabytes
    /// would fight the editor for memory. Above this, files stay plain.
    public static let byteLimit = 2 * 1024 * 1024

    /// Highlights `data` as the language `name`'s extension implies. `nil` when the
    /// extension maps to no known language or the parse fails; the caller shows plain text.
    public static func highlight(name: String, data: [UInt8]) -> [HighlightSpan]? {
        guard data.count <= byteLimit else { return nil }
        var error: UnsafeMutablePointer<CChar>? = nil
        let list = data.withUnsafeBufferPointer { buffer in
            rcgit_highlight(name, buffer.baseAddress, buffer.count, nil, &error)
        }
        if let error { HighlightFailureLog.report(error) }
        guard let list else { return nil }
        defer { rcgit_highlight_list_free(list) }
        var spans: [HighlightSpan] = []
        spans.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.spans[index]
            spans.append(
                HighlightSpan(start: c.start, end: c.end, class: HighlightClass(c: c.kind)))
        }
        return spans
    }
}

/// Logs why the core could not highlight or link a file, once per distinct message. The
/// caller still shows plain text; the log is what makes a grammar or query that stopped
/// loading after a dependency bump visible, with its query row and column or ABI versions.
enum HighlightFailureLog {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "rcgit", category: "highlight")
    private static let logged = OSAllocatedUnfairLock(initialState: Set<String>())

    /// Takes ownership of a core error string: frees it, then logs it unless already logged.
    static func report(_ error: UnsafeMutablePointer<CChar>) {
        let message = String(cString: error)
        rcgit_string_free(error)
        guard isFirst(message) else { return }
        logger.error("\(message, privacy: .public)")
    }

    /// Whether `message` is seen here for the first time; records it when it is.
    static func isFirst(_ message: String) -> Bool {
        logged.withLock { $0.insert(message).inserted }
    }
}

/// Converts byte-offset spans into UTF-16 ranges for `NSAttributedString`. One pass over the
/// text, so a big file costs one walk however many spans it has.
public enum HighlightMapper {
    public static func utf16Ranges(
        for spans: [HighlightSpan], in text: String
    ) -> [(range: NSRange, class: HighlightClass)] {
        var result: [(range: NSRange, class: HighlightClass)] = []
        result.reserveCapacity(spans.count)
        var spanIndex = 0
        var utf8Position = 0
        var utf16Position = 0
        var openStart: Int?

        func advanceBoundaries() {
            while spanIndex < spans.count {
                let span = spans[spanIndex]
                if openStart == nil {
                    if Int(span.start) <= utf8Position {
                        openStart = utf16Position
                    } else {
                        return
                    }
                }
                if Int(span.end) <= utf8Position {
                    if let start = openStart, utf16Position > start {
                        result.append(
                            (NSRange(location: start, length: utf16Position - start), span.class))
                    }
                    openStart = nil
                    spanIndex += 1
                } else {
                    return
                }
            }
        }

        for scalar in text.unicodeScalars {
            advanceBoundaries()
            utf8Position += UTF8.width(scalar)
            utf16Position += UTF16.width(scalar)
        }
        advanceBoundaries()
        return result
    }
}

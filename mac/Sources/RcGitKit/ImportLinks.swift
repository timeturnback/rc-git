import CRcGit
import Foundation

/// One clickable region of a source file and the module it leads to: byte offsets into the
/// UTF-8 content, half-open. The specifier is a range of that same content, so a link holds
/// no string and costs sixteen bytes however long the path is.
public struct ImportLink: Sendable, Hashable {
    public let start: UInt32
    public let end: UInt32
    public let specifierStart: UInt32
    public let specifierEnd: UInt32

    public init(start: UInt32, end: UInt32, specifierStart: UInt32, specifierEnd: UInt32) {
        self.start = start
        self.end = end
        self.specifierStart = specifierStart
        self.specifierEnd = specifierEnd
    }
}

/// Import links through the Rust core. Pure and repository-free: text in, links out.
public enum ImportLinks {
    /// Finds the links of `text`, read as the language `name`'s extension implies, ordered by
    /// start and never overlapping. Empty when the language has no links or the file has
    /// none; `nil` when the extension maps to no known language, the text is over the
    /// highlighter's cap, or the parse fails. The caller shows no links either way.
    public static func find(name: String, text: String) -> [ImportLink]? {
        var text = text
        return text.withUTF8 { buffer -> [ImportLink]? in
            guard buffer.count <= Highlighter.byteLimit else { return nil }
            var error: UnsafeMutablePointer<CChar>? = nil
            let list = rcgit_import_links(name, buffer.baseAddress, buffer.count, &error)
            if let error { rcgit_string_free(error) }
            guard let list else { return nil }
            defer { rcgit_import_link_list_free(list) }
            var links: [ImportLink] = []
            links.reserveCapacity(list.pointee.len)
            for index in 0..<list.pointee.len {
                let c = list.pointee.links[index]
                links.append(
                    ImportLink(
                        start: c.start, end: c.end, specifierStart: c.specifier_start,
                        specifierEnd: c.specifier_end))
            }
            return links
        }
    }
}

/// The import links of one document, ready for hit-testing under the pointer. Text views
/// count in UTF-16 units and the core counts in bytes; the two agree for ASCII text, which
/// most source is, so that case keeps the links as they arrived and walks nothing.
public struct ImportLinkMap: Sendable {
    /// What the pointer is over: the clickable range in UTF-16 units and where it leads.
    public struct Hit: Sendable, Hashable {
        public let range: NSRange
        public let specifier: String
    }

    /// `start` and `end` in UTF-16 units; the specifier still in bytes of `text`.
    private let links: [ImportLink]
    private let text: String

    public init(links: [ImportLink], text: String) {
        self.text = text
        if links.isEmpty || text.utf16.count == text.utf8.count {
            self.links = links
        } else {
            self.links = Self.utf16Links(links, in: text)
        }
    }

    public var isEmpty: Bool { links.isEmpty }

    /// The link covering the UTF-16 offset, found by binary search. `nil` between links and
    /// for a specifier whose bytes do not cut the text at character boundaries.
    public func hit(at utf16Offset: Int) -> Hit? {
        var low = 0
        var high = links.count
        while low < high {
            let mid = (low + high) / 2
            if Int(links[mid].end) <= utf16Offset {
                low = mid + 1
            } else {
                high = mid
            }
        }
        guard low < links.count, Int(links[low].start) <= utf16Offset else { return nil }
        let link = links[low]
        let utf8 = text.utf8
        guard link.specifierStart <= link.specifierEnd,
            let from = utf8.index(
                utf8.startIndex, offsetBy: Int(link.specifierStart), limitedBy: utf8.endIndex),
            let to = utf8.index(
                utf8.startIndex, offsetBy: Int(link.specifierEnd), limitedBy: utf8.endIndex),
            let specifier = String(utf8[from..<to])
        else { return nil }
        let range = NSRange(location: Int(link.start), length: Int(link.end - link.start))
        return Hit(range: range, specifier: specifier)
    }

    /// One walk over the text, stopping at the last link, however many links there are.
    private static func utf16Links(_ links: [ImportLink], in text: String) -> [ImportLink] {
        var result: [ImportLink] = []
        result.reserveCapacity(links.count)
        var index = 0
        var utf8Position = 0
        var utf16Position = 0
        var openStart: Int?

        func advanceBoundaries() {
            while index < links.count {
                let link = links[index]
                if openStart == nil {
                    guard Int(link.start) <= utf8Position else { return }
                    openStart = utf16Position
                }
                guard Int(link.end) <= utf8Position else { return }
                if let start = openStart, utf16Position > start {
                    result.append(
                        ImportLink(
                            start: UInt32(start), end: UInt32(utf16Position),
                            specifierStart: link.specifierStart, specifierEnd: link.specifierEnd))
                }
                openStart = nil
                index += 1
            }
        }

        for scalar in text.unicodeScalars {
            advanceBoundaries()
            if index == links.count { break }
            utf8Position += UTF8.width(scalar)
            utf16Position += UTF16.width(scalar)
        }
        advanceBoundaries()
        return result
    }
}

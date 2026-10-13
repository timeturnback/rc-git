import Foundation

/// Inline appearance flags. Several can apply to one run, so they combine.
public struct MarkdownStyle: OptionSet, Sendable, Hashable {
    public let rawValue: UInt8

    public init(rawValue: UInt8) { self.rawValue = rawValue }

    public static let emphasis = MarkdownStyle(rawValue: 1 << 0)
    public static let strong = MarkdownStyle(rawValue: 1 << 1)
    public static let code = MarkdownStyle(rawValue: 1 << 2)
    public static let strikethrough = MarkdownStyle(rawValue: 1 << 3)
    /// A footnote reference marker, drawn raised and small.
    public static let footnoteMarker = MarkdownStyle(rawValue: 1 << 4)
}

/// Where an inline link points. `destination` is the raw markdown target, unresolved: it may
/// be a URL, a repository-relative path, or a bare fragment.
public struct MarkdownLink: Sendable, Hashable {
    public let destination: String
    public let title: String?

    public init(destination: String, title: String? = nil) {
        self.destination = destination
        self.title = title
    }
}

/// One stretch of inline text that shares a single appearance. A paragraph is a list of these,
/// in order, and concatenating their text reproduces the visible line.
public struct MarkdownRun: Sendable, Hashable {
    public let text: String
    public let style: MarkdownStyle
    public let link: MarkdownLink?
    /// True when the run stands in for an image: `text` is the alt text and `link` the source.
    public let isImage: Bool

    public init(
        text: String, style: MarkdownStyle = [], link: MarkdownLink? = nil, isImage: Bool = false
    ) {
        self.text = text
        self.style = style
        self.link = link
        self.isImage = isImage
    }
}

/// A GFM table: one alignment per column, a header row, and the body rows. Every row is padded
/// to `alignments.count` cells by the parser, so a renderer never has to check lengths.
public struct MarkdownTable: Sendable, Hashable {
    public enum Alignment: Sendable, Hashable {
        case natural
        case leading
        case center
        case trailing
    }

    public let alignments: [Alignment]
    public let header: [[MarkdownRun]]
    public let rows: [[[MarkdownRun]]]

    public init(alignments: [Alignment], header: [[MarkdownRun]], rows: [[[MarkdownRun]]]) {
        self.alignments = alignments
        self.header = header
        self.rows = rows
    }

    public var columnCount: Int { alignments.count }
}

/// What starts one item of a list.
public enum MarkdownListMarker: Sendable, Hashable {
    case bullet
    case ordered(Int)
    /// A GFM task item: `- [ ]` or `- [x]`.
    case task(done: Bool)
}

/// One block of a parsed document. Nesting is flat: `quoteDepth` and `listDepth` say how deep
/// the block sits, which is all a renderer needs to indent it, and lets the whole document be
/// one array instead of a tree to walk.
public struct MarkdownBlock: Sendable, Hashable {
    public enum Kind: Sendable, Hashable {
        case paragraph
        case heading(level: Int)
        /// A fenced or indented code block. `language` is the fence's info string, lowercased.
        case code(language: String?, text: String)
        case table(MarkdownTable)
        case thematicBreak
        /// The YAML block delimited by `---` at the very start of a file.
        case frontMatter(text: String)
        /// A raw HTML block, shown as-is rather than interpreted.
        case html(text: String)
        /// The body of `[^label]: …`, shown under the prose that references it.
        case footnote(label: String)
    }

    public let kind: Kind
    /// The inline content. Empty for code, tables, and thematic breaks.
    public let runs: [MarkdownRun]
    public let quoteDepth: Int
    /// 0 outside a list; 1 for a top-level item, 2 for one nested inside it.
    public let listDepth: Int
    /// Set only on the first block of a list item, so a renderer draws one bullet per item.
    public let marker: MarkdownListMarker?

    public init(
        kind: Kind,
        runs: [MarkdownRun] = [],
        quoteDepth: Int = 0,
        listDepth: Int = 0,
        marker: MarkdownListMarker? = nil
    ) {
        self.kind = kind
        self.runs = runs
        self.quoteDepth = quoteDepth
        self.listDepth = listDepth
        self.marker = marker
    }
}

/// A parsed markdown file, ready to render.
public struct MarkdownDocument: Sendable, Hashable {
    public let blocks: [MarkdownBlock]

    public init(blocks: [MarkdownBlock]) { self.blocks = blocks }

    /// Parses `source`. Never fails: anything it does not recognise stays literal text.
    public static func parse(_ source: String) -> MarkdownDocument {
        var parser = MarkdownBlockParser(source: source)
        return MarkdownDocument(blocks: parser.parse())
    }

    /// The fragment a heading answers to, by the rule GitHub uses: lowercased, spaces joined
    /// with hyphens, and everything but letters, digits, `-` and `_` dropped. It is what makes
    /// a table-of-contents link in a README land on its section.
    public static func anchor(for heading: String) -> String {
        var slug = ""
        for character in heading.lowercased() {
            if character.isLetter || character.isNumber || character == "-" || character == "_" {
                slug.append(character)
            } else if character == " " {
                slug.append("-")
            }
        }
        return slug
    }

    /// The fragment each heading in `blocks` answers to, one per `.heading` block in document
    /// order. A repeated slug takes the first free `-1`, `-2`, ... suffix, the way GitHub
    /// de-duplicates heading ids, so the second "Fixed" is `fixed-1`. Footnote anchors
    /// (`fn-<label>`) count as taken, so a heading never claims a footnote's id. A heading
    /// with no slug gets an empty string and claims nothing.
    public static func headingAnchors(for blocks: [MarkdownBlock]) -> [String] {
        var occurrences: [String: Int] = [:]
        for block in blocks {
            if case let .footnote(label) = block.kind { occurrences["fn-\(label)"] = 0 }
        }
        var anchors: [String] = []
        for block in blocks {
            guard case .heading = block.kind else { continue }
            let original = anchor(for: block.runs.map(\.text).joined())
            guard !original.isEmpty else {
                anchors.append("")
                continue
            }
            var slug = original
            while occurrences[slug] != nil {
                let count = (occurrences[original] ?? 0) + 1
                occurrences[original] = count
                slug = "\(original)-\(count)"
            }
            occurrences[slug] = 0
            anchors.append(slug)
        }
        return anchors
    }
}

import AppKit
import RcGitKit

/// Renders parsed markdown into a styled attributed string for the file pane's preview.
/// Pure — string in, string out — and safe to run off the main actor, which is where the file
/// pane builds it. Every link carries its raw destination as the `.link` attribute; the pane
/// resolves it through `MarkdownLinkTarget` when it is clicked.
enum MarkdownRenderer {
    /// A rendered document and the headings it can be scrolled to. `NSAttributedString` and
    /// the offsets are read-only once built, which is what lets the file pane build them off
    /// the main actor and hand them over.
    struct Rendered: @unchecked Sendable {
        let text: NSAttributedString
        /// Heading anchor to the UTF-16 offset where that heading starts.
        let anchors: [String: Int]
    }

    static func render(_ markdown: String) -> Rendered {
        var builder = MarkdownBuilder()
        builder.append(MarkdownDocument.parse(markdown))
        return builder.finish(fallback: markdown)
    }

    /// Fonts, colors, and indents the preview is drawn with. Computed rather than stored:
    /// `NSFont` is not `Sendable`, and it caches instances itself.
    enum Theme {
        static let bodySize: CGFloat = 13
        static let codeSize: CGFloat = 12
        static let listIndent: CGFloat = 22
        static let markerWidth: CGFloat = 20
        static let blockSpacing: CGFloat = 10
        /// Code blocks above this many bytes are shown plain; a preview is not an editor.
        static let highlightLimit = 128 * 1024

        static var body: NSFont { .systemFont(ofSize: bodySize) }
        static var code: NSFont { .monospacedSystemFont(ofSize: codeSize, weight: .regular) }

        static func heading(_ level: Int) -> NSFont {
            switch level {
            case 1: return .systemFont(ofSize: 22, weight: .bold)
            case 2: return .systemFont(ofSize: 18, weight: .bold)
            case 3: return .systemFont(ofSize: 15, weight: .semibold)
            case 4: return .systemFont(ofSize: 13, weight: .semibold)
            default: return .systemFont(ofSize: 12, weight: .semibold)
            }
        }

        /// The bar drawn down the left of a quoted block, one per nesting level.
        static func quoteBlock() -> NSTextBlock {
            let block = NSTextBlock()
            block.setWidth(3, type: .absoluteValueType, for: .border, edge: .minX)
            block.setBorderColor(.tertiaryLabelColor, for: .minX)
            block.setWidth(10, type: .absoluteValueType, for: .padding, edge: .minX)
            block.setWidth(4, type: .absoluteValueType, for: .padding, edge: .minY)
            block.setWidth(4, type: .absoluteValueType, for: .padding, edge: .maxY)
            return block
        }

        static func panel(background: NSColor) -> NSTextBlock {
            let block = NSTextBlock()
            block.backgroundColor = background
            block.setWidth(8, type: .absoluteValueType, for: .padding)
            return block
        }

        static var codeBackground: NSColor {
            NSColor.textColor.withAlphaComponent(0.05)
        }
    }
}

/// Accumulates the rendered string block by block.
private struct MarkdownBuilder {
    private let result = NSMutableAttributedString()
    private var anchors: [String: Int] = [:]

    mutating func append(_ document: MarkdownDocument) {
        for block in document.blocks { append(block) }
    }

    /// The finished string, or the plain source when nothing rendered — an empty pane would
    /// hide a file that does have bytes in it.
    func finish(fallback: String) -> MarkdownRenderer.Rendered {
        guard result.length > 0 else {
            let plain = NSAttributedString(
                string: fallback,
                attributes: [
                    .font: MarkdownRenderer.Theme.code, .foregroundColor: NSColor.textColor,
                ])
            return MarkdownRenderer.Rendered(text: plain, anchors: [:])
        }
        return MarkdownRenderer.Rendered(text: result, anchors: anchors)
    }

    private mutating func append(_ block: MarkdownBlock) {
        switch block.kind {
        case .paragraph:
            appendProse(block, font: MarkdownRenderer.Theme.body, color: .textColor)
        case let .heading(level):
            appendHeading(block, level: level)
        case let .code(language, text):
            appendCode(text, language: language, block: block)
        case let .table(table):
            appendTable(table, block: block)
        case .thematicBreak:
            appendRule(block)
        case let .frontMatter(text):
            appendCode(text, language: "yaml", block: block)
        case let .html(text):
            appendCode(text, language: nil, block: block)
        case let .footnote(label):
            appendFootnote(block, label: label)
        }
    }

    /// Remembers where a heading starts so a `#fragment` link can scroll to it. The first
    /// heading with a given anchor wins, which is what a browser does too.
    private mutating func record(anchor: String) {
        guard !anchor.isEmpty, anchors[anchor] == nil else { return }
        anchors[anchor] = result.length
    }

    // MARK: Blocks

    private mutating func appendProse(_ block: MarkdownBlock, font: NSFont, color: NSColor) {
        let style = paragraphStyle(for: block, hanging: block.marker != nil)
        appendMarker(block, style: style, font: font)
        appendRuns(block.runs, font: font, color: color, style: style)
        terminate(style: style, font: font)
    }

    private mutating func appendHeading(_ block: MarkdownBlock, level: Int) {
        record(anchor: MarkdownDocument.anchor(for: block.runs.map(\.text).joined()))
        let style = paragraphStyle(for: block, hanging: block.marker != nil)
        style.paragraphSpacingBefore = level <= 2 ? 14 : 8
        style.paragraphSpacing = 6
        let font = MarkdownRenderer.Theme.heading(level)
        appendMarker(block, style: style, font: font)
        appendRuns(
            block.runs, font: font, color: level >= 5 ? .secondaryLabelColor : .textColor,
            style: style)
        terminate(style: style, font: font)
    }

    private mutating func appendFootnote(_ block: MarkdownBlock, label: String) {
        record(anchor: "fn-\(label)")
        let style = paragraphStyle(for: block, hanging: true)
        let font = NSFont.systemFont(ofSize: MarkdownRenderer.Theme.bodySize - 1)
        result.append(
            NSAttributedString(
                string: "\(label). ",
                attributes: [
                    .font: NSFont.systemFont(ofSize: font.pointSize, weight: .semibold),
                    .foregroundColor: NSColor.secondaryLabelColor, .paragraphStyle: style,
                ]))
        appendRuns(block.runs, font: font, color: .secondaryLabelColor, style: style)
        terminate(style: style, font: font)
    }

    /// A rule is an empty line whose block carries a top border, so it spans the pane whatever
    /// the window width is.
    private mutating func appendRule(_ block: MarkdownBlock) {
        let rule = NSTextBlock()
        rule.setWidth(1, type: .absoluteValueType, for: .border, edge: .minY)
        rule.setBorderColor(.separatorColor, for: .minY)
        rule.setWidth(8, type: .absoluteValueType, for: .padding, edge: .minY)
        let style = paragraphStyle(for: block, hanging: false)
        style.textBlocks += [rule]
        terminate(style: style, font: MarkdownRenderer.Theme.body)
    }

    private mutating func appendCode(_ text: String, language: String?, block: MarkdownBlock) {
        let style = paragraphStyle(for: block, hanging: false)
        style.textBlocks += [
            MarkdownRenderer.Theme.panel(background: MarkdownRenderer.Theme.codeBackground)
        ]
        style.lineSpacing = 1
        let font = MarkdownRenderer.Theme.code
        result.append(
            Self.highlighted(text, language: language, style: style, font: font))
        terminate(style: style, font: font)
    }

    /// Colors a fenced block with the same highlighter source mode uses, so one language looks
    /// the same in both views. An unknown fence name simply stays plain.
    private static func highlighted(
        _ text: String, language: String?, style: NSParagraphStyle, font: NSFont
    ) -> NSAttributedString {
        let string = NSMutableAttributedString(
            string: text,
            attributes: [
                .font: font, .foregroundColor: NSColor.textColor, .paragraphStyle: style,
            ])
        guard let language, !text.isEmpty,
            text.utf8.count <= MarkdownRenderer.Theme.highlightLimit,
            let spans = Highlighter.highlight(name: language, data: Array(text.utf8))
        else { return string }
        for span in HighlightMapper.utf16Ranges(for: spans, in: text)
        where NSMaxRange(span.range) <= string.length {
            string.addAttribute(
                .foregroundColor, value: HighlightPalette.color(for: span.class),
                range: span.range)
        }
        return string
    }

    // MARK: Runs

    private mutating func appendRuns(
        _ runs: [MarkdownRun], font: NSFont, color: NSColor, style: NSParagraphStyle
    ) {
        for run in runs {
            let text = run.isImage ? "\u{1F5BC}\u{FE0E} \(run.text)" : run.text
            result.append(
                NSAttributedString(
                    string: text,
                    attributes: Self.attributes(
                        for: run, font: font, color: color, style: style)))
        }
    }

    private static func attributes(
        for run: MarkdownRun, font: NSFont, color: NSColor, style: NSParagraphStyle
    ) -> [NSAttributedString.Key: Any] {
        var font = run.style.contains(.code) ? monospaced(like: font) : font
        var attributes: [NSAttributedString.Key: Any] = [.paragraphStyle: style]
        if run.style.contains(.footnoteMarker) {
            font = .systemFont(ofSize: max(font.pointSize - 4, 8))
            attributes[.baselineOffset] = font.pointSize * 0.45
        }
        var traits: NSFontDescriptor.SymbolicTraits = []
        if run.style.contains(.emphasis) { traits.insert(.italic) }
        if run.style.contains(.strong) { traits.insert(.bold) }
        if !traits.isEmpty {
            let descriptor = font.fontDescriptor.withSymbolicTraits(traits)
            font = NSFont(descriptor: descriptor, size: font.pointSize) ?? font
        }
        attributes[.font] = font
        attributes[.foregroundColor] = color
        if run.style.contains(.strikethrough) {
            attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue
        }
        if run.style.contains(.code) {
            attributes[.backgroundColor] = MarkdownRenderer.Theme.codeBackground
        }
        if let link = run.link {
            attributes[.link] = link.destination
            attributes[.foregroundColor] = NSColor.linkColor
            attributes[.underlineStyle] = NSUnderlineStyle.single.rawValue
            if let title = link.title { attributes[.toolTip] = title }
        }
        return attributes
    }

    private static func monospaced(like font: NSFont) -> NSFont {
        .monospacedSystemFont(ofSize: max(font.pointSize - 1, 9), weight: .regular)
    }

    // MARK: Layout

    private func paragraphStyle(for block: MarkdownBlock, hanging: Bool) -> NSMutableParagraphStyle
    {
        let style = NSMutableParagraphStyle()
        let indent = CGFloat(block.listDepth) * MarkdownRenderer.Theme.listIndent
        style.firstLineHeadIndent = indent
        style.headIndent = indent + (block.listDepth > 0 ? MarkdownRenderer.Theme.markerWidth : 0)
        style.paragraphSpacing = MarkdownRenderer.Theme.blockSpacing
        style.lineSpacing = 2
        if !hanging, block.listDepth > 0 { style.firstLineHeadIndent = style.headIndent }
        if block.quoteDepth > 0 {
            style.textBlocks = (0..<block.quoteDepth).map { _ in
                MarkdownRenderer.Theme.quoteBlock()
            }
        }
        return style
    }

    private mutating func appendMarker(
        _ block: MarkdownBlock, style: NSParagraphStyle, font: NSFont
    ) {
        guard let marker = block.marker else { return }
        let text: String
        var color = NSColor.secondaryLabelColor
        switch marker {
        case .bullet:
            text = "\u{2022}  "
        case let .ordered(value):
            text = "\(value).  "
        case let .task(done):
            text = done ? "\u{2611}  " : "\u{2610}  "
            color = done ? .secondaryLabelColor : .labelColor
        }
        result.append(
            NSAttributedString(
                string: text,
                attributes: [
                    .font: font, .foregroundColor: color, .paragraphStyle: style,
                ]))
    }

    /// Ends a block with a newline that carries the block's own style, so the spacing after it
    /// is the block's and not the next one's.
    private mutating func terminate(style: NSParagraphStyle, font: NSFont) {
        result.append(
            NSAttributedString(string: "\n", attributes: [.font: font, .paragraphStyle: style]))
    }
}

// MARK: - Tables

extension MarkdownBuilder {
    /// Lays a GFM table out with `NSTextTable`, which is what gives the preview real cells:
    /// columns size themselves to their content, long cells wrap, and the alignment written in
    /// the delimiter row is the cell's own.
    fileprivate mutating func appendTable(_ table: MarkdownTable, block: MarkdownBlock) {
        guard table.columnCount > 0 else { return }
        let textTable = NSTextTable()
        textTable.numberOfColumns = table.columnCount
        textTable.layoutAlgorithm = .automaticLayoutAlgorithm
        textTable.collapsesBorders = true
        textTable.hidesEmptyCells = false
        var row = 0
        appendRow(table.header, in: table, of: textTable, row: &row, header: true, block: block)
        for cells in table.rows {
            appendRow(cells, in: table, of: textTable, row: &row, header: false, block: block)
        }
        // The paragraph that closes the table is invisible: it only exists to end the table's
        // block context, so it carries a 1-point font and the spacing the next block needs.
        let trailer = paragraphStyle(for: block, hanging: false)
        terminate(style: trailer, font: .systemFont(ofSize: 1))
    }

    private mutating func appendRow(
        _ cells: [[MarkdownRun]], in table: MarkdownTable, of textTable: NSTextTable,
        row: inout Int, header: Bool, block: MarkdownBlock
    ) {
        for column in 0..<table.columnCount {
            let cell = NSTextTableBlock(
                table: textTable, startingRow: row, rowSpan: 1, startingColumn: column,
                columnSpan: 1)
            cell.setBorderColor(.separatorColor)
            cell.setWidth(1, type: .absoluteValueType, for: .border)
            cell.setWidth(6, type: .absoluteValueType, for: .padding)
            if header { cell.backgroundColor = MarkdownRenderer.Theme.codeBackground }
            let style = paragraphStyle(for: block, hanging: false)
            style.paragraphSpacing = 0
            style.firstLineHeadIndent = 0
            style.headIndent = 0
            style.textBlocks += [cell]
            style.alignment = Self.alignment(of: table.alignments[column])
            let font =
                header
                ? NSFont.systemFont(ofSize: MarkdownRenderer.Theme.bodySize, weight: .semibold)
                : MarkdownRenderer.Theme.body
            appendRuns(cells[column], font: font, color: .textColor, style: style)
            terminate(style: style, font: font)
        }
        row += 1
    }

    private static func alignment(of alignment: MarkdownTable.Alignment) -> NSTextAlignment {
        switch alignment {
        case .natural: return .natural
        case .leading: return .left
        case .center: return .center
        case .trailing: return .right
        }
    }
}

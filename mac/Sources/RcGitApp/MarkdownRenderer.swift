import AppKit

/// Renders markdown source into a styled attributed string for the file pane's preview.
/// Foundation's markdown parser produces presentation intents; this maps them onto fonts,
/// indents, and spacing. Pure — string in, string out — and safe to run off the main actor,
/// which is where the file pane builds it.
enum MarkdownRenderer {
    static func render(_ markdown: String) -> NSAttributedString {
        var options = AttributedString.MarkdownParsingOptions()
        options.interpretedSyntax = .full
        options.failurePolicy = .returnPartiallyParsedIfPossible
        guard let parsed = try? AttributedString(markdown: markdown, options: options) else {
            return NSAttributedString(string: markdown, attributes: plainAttributes)
        }
        let result = NSMutableAttributedString()
        var previousBlock: [Int] = []
        var markedListItems: Set<Int> = []
        for run in parsed.runs {
            let intent = run.presentationIntent
            let blockPath = intent?.components.map(\.identity) ?? []
            let newBlock = blockPath != previousBlock
            if newBlock, result.length > 0 {
                result.append(NSAttributedString(string: "\n"))
            }
            previousBlock = blockPath
            if let intent, isThematicBreak(intent) {
                if newBlock {
                    result.append(
                        NSAttributedString(
                            string: "\u{2014}\u{2014}\u{2014}",
                            attributes: [
                                .font: bodyFont, .foregroundColor: NSColor.tertiaryLabelColor,
                            ]))
                }
                continue
            }
            var text = String(parsed[run.range].characters)
            if text.isEmpty { continue }
            if let marker = listMarker(for: intent, alreadyMarked: &markedListItems) {
                text = marker + text
            }
            result.append(NSAttributedString(string: text, attributes: attributes(for: run)))
        }
        if result.length == 0 {
            return NSAttributedString(string: markdown, attributes: plainAttributes)
        }
        return result
    }

    // Computed, not stored: NSFont is not Sendable, and NSFont itself caches instances.
    private static var bodyFont: NSFont { NSFont.systemFont(ofSize: 13) }
    private static var codeFont: NSFont {
        NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
    }

    private static var plainAttributes: [NSAttributedString.Key: Any] {
        [.font: codeFont, .foregroundColor: NSColor.textColor]
    }

    private static func isThematicBreak(_ intent: PresentationIntent) -> Bool {
        intent.components.contains { $0.kind == .thematicBreak }
    }

    /// The bullet or ordinal prefixed to the first run of each list item, once per item.
    private static func listMarker(
        for intent: PresentationIntent?, alreadyMarked: inout Set<Int>
    ) -> String? {
        guard let intent else { return nil }
        for component in intent.components {
            guard case let .listItem(ordinal) = component.kind else { continue }
            guard alreadyMarked.insert(component.identity).inserted else { return nil }
            let ordered = intent.components.contains { $0.kind == .orderedList }
            return ordered ? "\(ordinal). " : "\u{2022} "
        }
        return nil
    }

    private static func attributes(for run: AttributedString.Runs.Run)
        -> [NSAttributedString.Key: Any]
    {
        var font = bodyFont
        var color = NSColor.textColor
        let paragraph = NSMutableParagraphStyle()
        paragraph.paragraphSpacing = 8
        var indent: CGFloat = 0

        if let intent = run.presentationIntent {
            for component in intent.components {
                switch component.kind {
                case let .header(level):
                    font = headerFont(level: level)
                    paragraph.paragraphSpacingBefore = 6
                case .codeBlock:
                    font = codeFont
                    paragraph.paragraphSpacing = 0
                case .blockQuote:
                    color = .secondaryLabelColor
                    indent += 16
                case .orderedList, .unorderedList:
                    indent += 16
                case .paragraph, .listItem, .thematicBreak, .table, .tableHeaderRow,
                    .tableRow, .tableCell:
                    break
                @unknown default:
                    break
                }
            }
        }
        if let inline = run.inlinePresentationIntent {
            if inline.contains(.code) { font = codeFont }
            var traits: NSFontDescriptor.SymbolicTraits = []
            if inline.contains(.emphasized) { traits.insert(.italic) }
            if inline.contains(.stronglyEmphasized) { traits.insert(.bold) }
            if !traits.isEmpty {
                let descriptor = font.fontDescriptor.withSymbolicTraits(traits)
                font = NSFont(descriptor: descriptor, size: font.pointSize) ?? font
            }
        }
        paragraph.headIndent = indent
        paragraph.firstLineHeadIndent = indent
        var attributes: [NSAttributedString.Key: Any] = [
            .font: font,
            .foregroundColor: color,
            .paragraphStyle: paragraph,
        ]
        if let link = run.link {
            attributes[.link] = link
            attributes[.foregroundColor] = NSColor.linkColor
        }
        return attributes
    }

    private static func headerFont(level: Int) -> NSFont {
        let size: CGFloat
        switch level {
        case 1: size = 20
        case 2: size = 17
        case 3: size = 15
        default: size = 13
        }
        return NSFont.systemFont(ofSize: size, weight: .bold)
    }
}

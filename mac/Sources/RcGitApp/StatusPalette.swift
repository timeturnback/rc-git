import AppKit
import RcGitKit

/// The one place a working-tree status or a change kind becomes a color. The tree badge, the
/// Changes list and the History file list all read their status colors from here.
enum StatusPalette {
    static func color(for status: WorktreeStatus) -> NSColor {
        switch status {
        case .unchanged: return .labelColor
        case .modified: return .systemOrange
        case .untracked: return .systemGreen
        case .ignored: return .tertiaryLabelColor
        case .deleted: return .systemRed
        }
    }

    static func color(for kind: ChangeKind) -> NSColor {
        switch kind {
        case .added: return .systemGreen
        case .deleted: return .systemRed
        case .modified: return .systemOrange
        case .renamed: return .systemBlue
        }
    }
}

/// The label of one row in a changed-file list: a bold status letter in its color, then the
/// name. The truncation lives in the string's paragraph style — a label showing an attributed
/// string ignores the one set on the cell, and clips with no ellipsis.
enum ChangeRowLabel {
    /// A file row's label. A path truncates in the middle, never at the head: head truncation
    /// cuts from the start of the line, which is the status letter.
    static func make(letter: String, color: NSColor, name: String) -> NSAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = name.contains("/") ? .byTruncatingMiddle : .byTruncatingTail
        let text = NSMutableAttributedString(
            string: letter + "  ",
            attributes: [
                .font: NSFont.monospacedSystemFont(
                    ofSize: NSFont.smallSystemFontSize, weight: .bold),
                .foregroundColor: color,
                .paragraphStyle: paragraph,
            ])
        text.append(
            NSAttributedString(
                string: name,
                attributes: [
                    .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                    .foregroundColor: NSColor.labelColor,
                    .paragraphStyle: paragraph,
                ]))
        return text
    }

    /// A folder row's label: the name alone, in a medium secondary font.
    static func folder(name: String) -> NSAttributedString {
        NSAttributedString(
            string: name,
            attributes: [
                .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .medium),
                .foregroundColor: NSColor.secondaryLabelColor,
                .paragraphStyle: paragraphStyle(for: name),
            ])
    }

    /// A folder's whole path reads from its tail, a bare name from its head.
    private static func paragraphStyle(for name: String) -> NSParagraphStyle {
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = name.contains("/") ? .byTruncatingHead : .byTruncatingTail
        return paragraph
    }
}

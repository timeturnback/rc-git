import AppKit
import RcGitKit

/// Shared formatting for line diffs: the History pane's text view and the Changes pane's
/// row-based diff table both draw from here, so hunks look the same everywhere.
@MainActor
enum DiffRendering {
    /// Computed so the off-main diff build can read it; the system caches the font itself.
    nonisolated static var monoFont: NSFont {
        .monospacedSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
    }

    /// One `@@ -a,b +c,d @@` header line.
    static func header(
        oldStart: UInt32, oldLines: UInt32, newStart: UInt32, newLines: UInt32
    ) -> NSAttributedString {
        NSAttributedString(
            string: "@@ -\(oldStart),\(oldLines) +\(newStart),\(newLines) @@",
            attributes: [.font: monoFont, .foregroundColor: NSColor.tertiaryLabelColor])
    }

    /// One diff line with its sign and color.
    static func line(_ line: DiffLine) -> NSAttributedString {
        let (sign, color): (String, NSColor) =
            switch line.kind {
            case .context: (" ", .labelColor)
            case .added: ("+", .systemGreen)
            case .removed: ("-", .systemRed)
            }
        let result = NSMutableAttributedString(
            string: sign + line.text,
            attributes: [.font: monoFont, .foregroundColor: color])
        if let marker = line.eol.marker {
            result.append(
                NSAttributedString(
                    string: marker,
                    attributes: [.font: monoFont, .foregroundColor: NSColor.tertiaryLabelColor]))
        }
        return result
    }

    /// A built diff. The string is read-only once built, which is what lets the History pane
    /// build it off the main actor and hand it over.
    struct Built: @unchecked Sendable {
        let text: NSAttributedString
    }

    /// A whole diff as one attributed string, for text-view rendering. Built as one string
    /// coloured by range rather than one attributed string per line, and nonisolated so the
    /// History pane can build a lockfile-sized diff off the main actor.
    nonisolated static func attributed(hunks: [DiffHunk]) -> Built {
        var text = ""
        var length = 0
        var colored: [(range: NSRange, color: NSColor)] = []
        func append(_ piece: String, color: NSColor? = nil) {
            let pieceLength = piece.utf16.count
            if let color {
                colored.append((NSRange(location: length, length: pieceLength), color))
            }
            text += piece
            length += pieceLength
        }
        for (index, hunk) in hunks.enumerated() {
            if index > 0 { append("\n") }
            append(
                "@@ -\(hunk.oldStart),\(hunk.oldLines) +\(hunk.newStart),\(hunk.newLines) @@",
                color: .tertiaryLabelColor)
            append("\n")
            for diffLine in hunk.lines {
                switch diffLine.kind {
                case .context: append(" " + diffLine.text)
                case .added: append("+" + diffLine.text, color: .systemGreen)
                case .removed: append("-" + diffLine.text, color: .systemRed)
                }
                if let marker = diffLine.eol.marker {
                    append(marker, color: .tertiaryLabelColor)
                }
                append("\n")
            }
        }
        let result = NSMutableAttributedString(
            string: text, attributes: [.font: monoFont, .foregroundColor: NSColor.labelColor])
        result.beginEditing()
        for (range, color) in colored {
            result.addAttribute(.foregroundColor, value: color, range: range)
        }
        result.endEditing()
        return Built(text: result)
    }
}

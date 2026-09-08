import AppKit
import RcGitKit

/// Shared formatting for line diffs: the History pane's text view and the Changes pane's
/// row-based diff table both draw from here, so hunks look the same everywhere.
@MainActor
enum DiffRendering {
    static let monoFont = NSFont.monospacedSystemFont(
        ofSize: NSFont.smallSystemFontSize, weight: .regular)

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
        return NSAttributedString(
            string: sign + line.text,
            attributes: [.font: monoFont, .foregroundColor: color])
    }

    /// A whole diff as one attributed string, for text-view rendering.
    static func attributed(hunks: [DiffHunk]) -> NSAttributedString {
        let result = NSMutableAttributedString()
        for (index, hunk) in hunks.enumerated() {
            if index > 0 {
                result.append(NSAttributedString(string: "\n", attributes: [.font: monoFont]))
            }
            result.append(
                header(
                    oldStart: hunk.oldStart, oldLines: hunk.oldLines,
                    newStart: hunk.newStart, newLines: hunk.newLines))
            result.append(NSAttributedString(string: "\n", attributes: [.font: monoFont]))
            for diffLine in hunk.lines {
                result.append(line(diffLine))
                result.append(NSAttributedString(string: "\n", attributes: [.font: monoFont]))
            }
        }
        return result
    }
}

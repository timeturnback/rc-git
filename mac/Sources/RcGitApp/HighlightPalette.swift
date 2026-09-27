import AppKit
import RcGitKit

/// The one place a highlight class becomes a color. Source mode applies these as temporary
/// attributes per visible chunk; the markdown preview bakes them into its fenced code blocks.
enum HighlightPalette {
    static func color(for highlightClass: HighlightClass) -> NSColor {
        switch highlightClass {
        case .attribute: return .systemBrown
        case .comment: return .secondaryLabelColor
        case .constant: return .systemOrange
        case .emphasis: return .systemPurple
        case .function: return .systemTeal
        case .heading: return .systemBlue
        case .keyword: return .systemPurple
        case .link: return .linkColor
        case .number: return .systemOrange
        case .operator: return .labelColor
        case .property: return .systemIndigo
        case .punctuation: return .labelColor
        case .string: return .systemRed
        case .strong: return .systemPurple
        case .type: return .systemBlue
        case .variable: return .labelColor
        }
    }
}

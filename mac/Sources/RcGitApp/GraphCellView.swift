import AppKit
import RcGitKit

/// Draws one `GraphRow`: the commit dot plus the lane segments crossing the row. Lanes past
/// `maxDrawnLanes` clamp to the last drawn column, so a pathological repository widens
/// nothing.
final class GraphCellView: NSView {
    static let laneWidth: CGFloat = 10
    static let leadingPad: CGFloat = 4
    static let maxDrawnLanes = 12

    /// Fixed palette indexed by `colorIndex`; stable because lane colors are assigned
    /// monotonically.
    private static let palette: [NSColor] = [
        .systemBlue, .systemGreen, .systemOrange, .systemPurple,
        .systemTeal, .systemPink, .systemBrown, .systemIndigo,
    ]

    var row: GraphRow? {
        didSet {
            if row != oldValue { needsDisplay = true }
        }
    }

    override var isFlipped: Bool { true }

    /// The column width that fits `lanes` lanes, clamped to the drawing cap.
    static func width(lanes: Int) -> CGFloat {
        CGFloat(min(max(lanes, 1), maxDrawnLanes)) * laneWidth + leadingPad * 2
    }

    private func x(_ lane: Int) -> CGFloat {
        Self.leadingPad + CGFloat(min(lane, Self.maxDrawnLanes - 1)) * Self.laneWidth
            + Self.laneWidth / 2
    }

    private static func color(_ index: Int) -> NSColor {
        palette[index % palette.count]
    }

    override func draw(_ dirtyRect: NSRect) {
        guard let row else { return }
        let height = bounds.height
        let midY = height / 2
        for edge in row.edges {
            let path = NSBezierPath()
            path.lineWidth = 1.5
            let fromX = x(edge.from)
            let toX = x(edge.to)
            switch edge.phase {
            case .through:
                path.move(to: NSPoint(x: fromX, y: 0))
                if edge.from == edge.to {
                    path.line(to: NSPoint(x: toX, y: height))
                } else {
                    path.curve(
                        to: NSPoint(x: toX, y: height),
                        controlPoint1: NSPoint(x: fromX, y: midY),
                        controlPoint2: NSPoint(x: toX, y: midY))
                }
            case .into:
                path.move(to: NSPoint(x: fromX, y: 0))
                path.curve(
                    to: NSPoint(x: toX, y: midY),
                    controlPoint1: NSPoint(x: fromX, y: midY),
                    controlPoint2: NSPoint(x: toX, y: midY / 2))
            case .out:
                path.move(to: NSPoint(x: fromX, y: midY))
                path.curve(
                    to: NSPoint(x: toX, y: height),
                    controlPoint1: NSPoint(x: fromX, y: midY + midY / 2),
                    controlPoint2: NSPoint(x: toX, y: midY))
            }
            Self.color(edge.colorIndex).setStroke()
            path.stroke()
        }
        let radius: CGFloat = row.isMerge ? 4 : 3
        let dot = NSRect(
            x: x(row.commitLane) - radius, y: midY - radius,
            width: radius * 2, height: radius * 2)
        Self.color(row.colorIndex).setFill()
        NSBezierPath(ovalIn: dot).fill()
    }
}

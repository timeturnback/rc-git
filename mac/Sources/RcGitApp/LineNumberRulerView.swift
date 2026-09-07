import AppKit

/// Draws a line number beside every visible line fragment. Wrapping is off in the file pane,
/// so each fragment is one line; the number comes from a binary search over the UTF-16
/// offsets where lines start, computed once per document.
final class LineNumberRulerView: NSRulerView {
    private weak var textView: NSTextView?
    private var lineStarts: [Int] = [0]
    private let font = NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .regular)

    init(scrollView: NSScrollView, textView: NSTextView) {
        self.textView = textView
        super.init(scrollView: scrollView, orientation: .verticalRuler)
        clientView = textView
        ruleThickness = 40
        NotificationCenter.default.addObserver(
            self, selector: #selector(boundsChanged(_:)),
            name: NSView.boundsDidChangeNotification, object: scrollView.contentView)
    }

    @available(*, unavailable)
    required init(coder: NSCoder) { fatalError("not used") }

    /// Recomputes line starts for the current text and resizes the gutter to fit the count.
    func update() {
        guard let string = textView?.string as NSString? else { return }
        var starts = [0]
        var location = 0
        let length = string.length
        while location < length {
            let found = string.range(
                of: "\n", options: [], range: NSRange(location: location, length: length - location)
            )
            if found.location == NSNotFound { break }
            location = found.location + 1
            if location < length { starts.append(location) }
        }
        lineStarts = starts
        let digits = max(3, String(starts.count).count)
        let width = (String(repeating: "8", count: digits) as NSString).size(withAttributes: [
            .font: font
        ]).width
        ruleThickness = ceil(width) + 16
        needsDisplay = true
    }

    @objc private func boundsChanged(_ notification: Notification) {
        needsDisplay = true
    }

    override func drawHashMarksAndLabels(in rect: NSRect) {
        guard let textView, let layoutManager = textView.layoutManager,
            let container = textView.textContainer
        else { return }
        NSColor.controlBackgroundColor.setFill()
        bounds.fill()

        let visible = textView.visibleRect
        let glyphRange = layoutManager.glyphRange(forBoundingRect: visible, in: container)
        let attributes: [NSAttributedString.Key: Any] = [
            .font: font, .foregroundColor: NSColor.tertiaryLabelColor,
        ]
        var glyphIndex = glyphRange.location
        let end = NSMaxRange(glyphRange)
        while glyphIndex < end {
            var fragmentRange = NSRange()
            let fragment = layoutManager.lineFragmentRect(
                forGlyphAt: glyphIndex, effectiveRange: &fragmentRange)
            let charIndex = layoutManager.characterIndexForGlyph(at: glyphIndex)
            let line = lineNumber(forCharacterAt: charIndex)
            let label = String(line) as NSString
            let size = label.size(withAttributes: attributes)
            let y =
                fragment.minY + textView.textContainerInset.height - visible.minY
                + (fragment.height - size.height) / 2
            label.draw(
                at: NSPoint(x: ruleThickness - size.width - 8, y: y), withAttributes: attributes)
            glyphIndex = NSMaxRange(fragmentRange)
        }
    }

    private func lineNumber(forCharacterAt index: Int) -> Int {
        var low = 0
        var high = lineStarts.count - 1
        while low < high {
            let mid = (low + high + 1) / 2
            if lineStarts[mid] <= index { low = mid } else { high = mid - 1 }
        }
        return low + 1
    }
}

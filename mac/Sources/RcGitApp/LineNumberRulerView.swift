import AppKit
import RcGitKit

/// Draws a line number beside every visible line fragment that begins a line. Wrapping is off
/// in the file pane and non-LF separators are drawn as markers, so a fragment is normally a
/// whole line; a fragment that starts anywhere else gets no number. `LineNumbering` decides
/// from the UTF-16 offsets where lines start, which arrive precomputed with the document — this
/// view never scans the text itself.
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

    /// Installs the document's precomputed UTF-16 line starts and resizes the gutter to fit
    /// the count. Pass an empty array to reset for an empty or hidden document.
    func setLineStarts(_ starts: [Int]) {
        lineStarts = starts.isEmpty ? [0] : starts
        let digits = max(3, String(lineStarts.count).count)
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
            glyphIndex = NSMaxRange(fragmentRange)
            let charIndex = layoutManager.characterIndexForGlyph(at: fragmentRange.location)
            guard let line = LineNumbering.label(at: charIndex, lineStarts: lineStarts) else {
                continue
            }
            let label = String(line) as NSString
            let size = label.size(withAttributes: attributes)
            let y =
                fragment.minY + textView.textContainerInset.height - visible.minY
                + (fragment.height - size.height) / 2
            label.draw(
                at: NSPoint(x: ruleThickness - size.width - 8, y: y), withAttributes: attributes)
        }
    }
}

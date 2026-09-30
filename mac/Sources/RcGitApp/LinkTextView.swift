import AppKit

/// The file pane's text view, plus links the text storage knows nothing about: regions its
/// owner answers for by character index. A `.link` attribute would have to be written into
/// the storage, which invalidates layout for every range it touches; here the pointer shows
/// a link by an underline drawn as a temporary attribute, so following it changes no layout,
/// and a plain click follows one while a drag that starts on it still selects.
final class LinkTextView: NSTextView {
    /// The link covering a character index, if any. Asked on every pointer move over the
    /// text, so it must be cheap.
    var linkRange: ((Int) -> NSRange?)?
    /// A link was clicked; the argument is a character index inside it.
    var onFollowLink: ((Int) -> Void)?

    /// How far the pointer may travel between press and release and still count as a click.
    private static let clickSlop: CGFloat = 3
    private var hovered: NSRange?
    /// Whether this view put the pointing hand up and so owes the I-beam back.
    private var showsHand = false
    private var hoverArea: NSTrackingArea?

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let hoverArea { removeTrackingArea(hoverArea) }
        let area = NSTrackingArea(
            rect: .zero,
            options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
            owner: self, userInfo: nil)
        addTrackingArea(area)
        hoverArea = area
    }

    override func mouseMoved(with event: NSEvent) {
        super.mouseMoved(with: event)
        hover(link(under: event))
        // Set after `super`, so the cursor it picks for plain text does not win over a link.
        if hovered != nil {
            NSCursor.pointingHand.set()
            showsHand = true
        } else if showsHand {
            NSCursor.iBeam.set()
            showsHand = false
        }
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        hover(nil)
        // Outside the view the cursor belongs to whatever the pointer entered.
        showsHand = false
    }

    override func mouseDown(with event: NSEvent) {
        let modifiers: NSEvent.ModifierFlags = [.shift, .control, .option, .command]
        guard event.clickCount == 1, event.modifierFlags.isDisjoint(with: modifiers),
            let index = characterIndex(under: event), linkRange?(index) != nil,
            releasesInPlace(after: event)
        else {
            super.mouseDown(with: event)
            return
        }
        onFollowLink?(index)
    }

    /// Drops the hover underline. Call before the text changes or scrolls from under the
    /// pointer; the next move finds whatever is there then.
    func clearLinkHover() {
        hover(nil)
    }

    private func link(under event: NSEvent) -> NSRange? {
        guard let linkRange, let index = characterIndex(under: event) else { return nil }
        return linkRange(index)
    }

    private func hover(_ range: NSRange?) {
        guard range != hovered, let layoutManager else { return }
        let length = textStorage?.length ?? 0
        if let hovered, NSMaxRange(hovered) <= length {
            layoutManager.removeTemporaryAttribute(.underlineStyle, forCharacterRange: hovered)
        }
        if let range, NSMaxRange(range) <= length {
            layoutManager.addTemporaryAttribute(
                .underlineStyle, value: NSUnderlineStyle.single.rawValue, forCharacterRange: range)
        }
        hovered = range
    }

    /// The character whose glyph the pointer is on. `nil` in the margins and past a line's
    /// end, where the nearest glyph is not under the pointer.
    private func characterIndex(under event: NSEvent) -> Int? {
        guard let layoutManager, let textContainer, let textStorage, textStorage.length > 0
        else { return nil }
        let inView = convert(event.locationInWindow, from: nil)
        let origin = textContainerOrigin
        let point = NSPoint(x: inView.x - origin.x, y: inView.y - origin.y)
        let glyph = layoutManager.glyphIndex(for: point, in: textContainer)
        let box = layoutManager.boundingRect(
            forGlyphRange: NSRange(location: glyph, length: 1), in: textContainer)
        guard box.contains(point) else { return nil }
        return layoutManager.characterIndexForGlyph(at: glyph)
    }

    /// Waits for the button to come up. `false` as soon as the pointer travels past the slop,
    /// which makes the press the start of a selection instead.
    private func releasesInPlace(after press: NSEvent) -> Bool {
        guard let window else { return false }
        let start = press.locationInWindow
        while let next = window.nextEvent(matching: [.leftMouseUp, .leftMouseDragged]) {
            if next.type == .leftMouseUp { return true }
            let now = next.locationInWindow
            if abs(now.x - start.x) > Self.clickSlop || abs(now.y - start.y) > Self.clickSlop {
                return false
            }
        }
        return false
    }
}

import AppKit
import RcGitKit

/// The sidebar's stand-in for the ref list in Files and Changes: one rounded row naming what
/// the window browses, from a `BranchBanner`. Clickable, it opens the ref picker and carries a
/// chevron; read-only, it only names.
///
/// The height is fixed and fits two lines, so switching between a one-line and a two-line
/// banner never moves the section under it.
@MainActor
final class RefBannerView: NSView {
    var onClick: (() -> Void)?

    private static let height: CGFloat = 42
    private let icon = NSImageView()
    private let title = NSTextField(labelWithString: "")
    private let detail = NSTextField(labelWithString: "")
    private let chevron = NSImageView()
    /// Where the title sits: centered alone, or above the middle with the detail under it.
    /// One set is active at a time.
    private var oneLine: [NSLayoutConstraint] = []
    private var twoLines: [NSLayoutConstraint] = []
    private var trackingArea: NSTrackingArea?
    private var isClickable = false
    private var isHovered = false {
        didSet { needsDisplay = true }
    }
    private var isPressed = false {
        didSet { needsDisplay = true }
    }

    init() {
        super.init(frame: .zero)
        build()
        render(.nothing, clickable: false)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    func render(_ banner: BranchBanner, clickable: Bool) {
        isClickable = clickable
        if !clickable {
            isHovered = false
            isPressed = false
        }
        icon.image = Icons.symbol(for: banner.symbol)
        title.stringValue = banner.title
        title.textColor = banner.symbol == .nothing ? .secondaryLabelColor : .labelColor
        detail.stringValue = banner.detail ?? ""
        detail.isHidden = banner.detail == nil
        NSLayoutConstraint.deactivate(banner.detail == nil ? twoLines : oneLine)
        NSLayoutConstraint.activate(banner.detail == nil ? oneLine : twoLines)
        chevron.isHidden = !clickable
        setAccessibilityLabel(
            [banner.title, banner.detail].compactMap { $0 }.joined(separator: ", "))
        needsDisplay = true
    }

    private func build() {
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 13, weight: .regular)
        icon.contentTintColor = .secondaryLabelColor
        title.font = .systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
        title.lineBreakMode = .byTruncatingMiddle
        detail.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        detail.textColor = .secondaryLabelColor
        detail.lineBreakMode = .byTruncatingTail
        chevron.image = NSImage(
            systemSymbolName: "chevron.up.chevron.down", accessibilityDescription: nil)
        chevron.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 10, weight: .semibold)
        chevron.contentTintColor = .secondaryLabelColor
        for view in [icon, title, detail, chevron] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            view.setAccessibilityElement(false)
            addSubview(view)
        }
        // The sidebar's width is the tree's to decide; a long branch name truncates instead.
        for label in [title, detail] {
            label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        }
        let textTrailing = chevron.leadingAnchor
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: Self.height),
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 18),
            chevron.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            chevron.centerYAnchor.constraint(equalTo: centerYAnchor),
            title.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 6),
            title.trailingAnchor.constraint(lessThanOrEqualTo: textTrailing, constant: -6),
            detail.leadingAnchor.constraint(equalTo: title.leadingAnchor),
            detail.trailingAnchor.constraint(lessThanOrEqualTo: textTrailing, constant: -6),
            detail.topAnchor.constraint(equalTo: title.bottomAnchor),
        ])
        oneLine = [title.centerYAnchor.constraint(equalTo: centerYAnchor)]
        twoLines = [title.bottomAnchor.constraint(equalTo: centerYAnchor, constant: 1)]
        setAccessibilityElement(true)
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        let alpha: CGFloat
        if isPressed {
            alpha = 0.16
        } else if isHovered {
            alpha = 0.10
        } else {
            alpha = 0.05
        }
        NSColor.labelColor.withAlphaComponent(alpha).setFill()
        NSBezierPath(roundedRect: bounds, xRadius: 6, yRadius: 6).fill()
    }

    // MARK: Mouse

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingArea { removeTrackingArea(trackingArea) }
        let area = NSTrackingArea(
            rect: .zero, options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
            owner: self)
        addTrackingArea(area)
        trackingArea = area
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseEntered(with event: NSEvent) {
        isHovered = isClickable
    }

    override func mouseExited(with event: NSEvent) {
        isHovered = false
    }

    override func mouseDown(with event: NSEvent) {
        guard isClickable else { return super.mouseDown(with: event) }
        isPressed = true
    }

    override func mouseDragged(with event: NSEvent) {
        guard isClickable else { return super.mouseDragged(with: event) }
        isPressed = bounds.contains(convert(event.locationInWindow, from: nil))
    }

    override func mouseUp(with event: NSEvent) {
        guard isClickable else { return super.mouseUp(with: event) }
        let inside = bounds.contains(convert(event.locationInWindow, from: nil))
        isPressed = false
        if inside { onClick?() }
    }

    // MARK: Accessibility

    override func accessibilityRole() -> NSAccessibility.Role? {
        isClickable ? .button : .staticText
    }

    override func accessibilityPerformPress() -> Bool {
        guard isClickable else { return false }
        onClick?()
        return true
    }
}

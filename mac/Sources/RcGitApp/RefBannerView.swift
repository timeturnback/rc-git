import AppKit
import RcGitKit

/// The sidebar's stand-in for the ref list in Files and Changes: one rounded row naming what
/// the window browses, from a `BranchBanner`. Clickable, it opens the ref picker on mouse-down,
/// the way a pop-up button opens its menu, and carries a chevron; read-only, it only names.
///
/// The height is fixed and fits two lines, so switching between a one-line and a two-line
/// banner never moves the section under it.
@MainActor
final class RefBannerView: NSView {
    var onClick: (() -> Void)?

    private static let height: CGFloat = 42
    /// The inset of the icon and the chevron from the rounded edge.
    private static let padding: CGFloat = 8
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
        // The accent color, as the same symbol has in the ref list's rows.
        icon.contentTintColor = .controlAccentColor
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
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Self.padding),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 20),
            chevron.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Self.padding),
            chevron.centerYAnchor.constraint(equalTo: centerYAnchor),
            title.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: Style.rowGap),
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
        let fill: NSColor = isHovered ? .tertiarySystemFill : .quaternarySystemFill
        fill.setFill()
        NSBezierPath(roundedRect: bounds, xRadius: 8, yRadius: 8).fill()
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
        onClick?()
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

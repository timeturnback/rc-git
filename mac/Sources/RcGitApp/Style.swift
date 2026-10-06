import AppKit
import RcGitKit

/// Spacing and list styling the panes share, so the sidebar and the detail pane line up with
/// each other and every list in the sidebar reads as one panel.
@MainActor
enum Style {
    /// The horizontal margin inside the sidebar and in the bars across the top of the detail
    /// pane. A source list insets its selection by the same amount, so a field or banner above
    /// a list lines up with the highlighted row.
    static let margin: CGFloat = 10
    /// The space above and below the controls of a bar across the top of a pane.
    static let barPadding: CGFloat = 8
    /// The gap between a row's symbol and its label, and between the label and a badge.
    static let rowGap: CGFloat = 6

    /// Makes `table` a sidebar list: a source list drawn over the sidebar's material, whose
    /// rows — height, text, and symbol — follow the sidebar size set in System Settings unless
    /// `rowSize` fixes one.
    static func sidebarList(_ table: NSTableView, rowSize: NSTableView.RowSizeStyle = .default) {
        table.style = .sourceList
        table.rowSizeStyle = rowSize
    }

    /// The label size AppKit gives a source-list cell at `rowSize`: 11, 13, or 15 points.
    static func fontSize(for rowSize: NSTableView.RowSizeStyle) -> CGFloat {
        switch rowSize {
        case .small: NSFont.smallSystemFontSize
        case .large: 15
        default: NSFont.systemFontSize
        }
    }

    /// The width of a symbol column at `rowSize`, wide enough for the widest symbol so labels
    /// start at the same x on every row.
    static func iconWidth(for rowSize: NSTableView.RowSizeStyle) -> CGFloat {
        fontSize(for: rowSize) + 7
    }

    /// Makes `control` a change list's flat-or-folders switch, showing `grouping`. Read the
    /// pick back with `grouping(of:)`.
    static func groupingControl(_ control: NSSegmentedControl, showing grouping: ChangeGrouping) {
        control.segmentStyle = .texturedRounded
        control.controlSize = .small
        control.trackingMode = .selectOne
        control.segmentCount = ChangeGrouping.allCases.count
        for (index, each) in ChangeGrouping.allCases.enumerated() {
            let (symbol, help): (String, String) =
                switch each {
                case .flat: ("list.bullet", "List every path")
                case .folders: ("folder", "Group paths by folder")
                }
            control.setImage(
                NSImage(systemSymbolName: symbol, accessibilityDescription: help),
                forSegment: index)
            control.setToolTip(help, forSegment: index)
            control.setWidth(28, forSegment: index)
        }
        control.selectedSegment = ChangeGrouping.allCases.firstIndex(of: grouping) ?? 0
    }

    /// The grouping a control built by `groupingControl(_:showing:)` has picked.
    static func grouping(of control: NSSegmentedControl) -> ChangeGrouping {
        let all = ChangeGrouping.allCases
        return all.indices.contains(control.selectedSegment) ? all[control.selectedSegment] : .flat
    }
}

/// One sidebar row: a symbol, a label, and an optional status badge at the trailing edge.
///
/// The symbol is the cell's `imageView`, which is what the source list tints. The label is
/// deliberately not its `textField`: AppKit re-applies a plain system font to that outlet on
/// every layout pass, which drops the checked-out branch's weight. The cell sizes the label
/// itself whenever the table hands it a row size, so a table that shows these cells answers
/// type-select through its delegate.
@MainActor
final class SidebarCellView: NSTableCellView {
    /// The space between the badge and the cell's trailing edge.
    private static let badgeInset: CGFloat = 2

    let label = NSTextField(labelWithString: "")
    private let icon = NSImageView()
    private let badge = NSTextField(labelWithString: "")
    private var iconWidth: NSLayoutConstraint?

    /// Semibold rather than regular, for the checked-out branch.
    var isEmphasized = false {
        didSet { applyRowSize() }
    }

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        icon.imageScaling = .scaleProportionallyDown
        label.lineBreakMode = .byTruncatingMiddle
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        badge.alignment = .right
        badge.setContentHuggingPriority(.required, for: .horizontal)
        badge.setContentCompressionResistancePriority(.required, for: .horizontal)
        badge.isHidden = true
        for view in [icon, label, badge] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        imageView = icon
        let iconWidth = icon.widthAnchor.constraint(
            equalToConstant: Style.iconWidth(for: rowSizeStyle))
        self.iconWidth = iconWidth
        NSLayoutConstraint.activate([
            iconWidth,
            icon.leadingAnchor.constraint(equalTo: leadingAnchor),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: Style.rowGap),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.trailingAnchor.constraint(
                lessThanOrEqualTo: badge.leadingAnchor, constant: -Style.rowGap),
            badge.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Self.badgeInset),
            badge.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        applyRowSize()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    override var rowSizeStyle: NSTableView.RowSizeStyle {
        didSet { applyRowSize() }
    }

    /// Shows a status letter at the trailing edge, or hides it for `nil`.
    func setBadge(_ letter: String?, color: NSColor) {
        badge.stringValue = letter ?? ""
        badge.textColor = color
        badge.isHidden = letter == nil
    }

    /// The width a cell needs to show `label` and `badge` in full at `rowSize`, without
    /// building one: the symbol, the label, the badge, and the gaps between them.
    static func contentWidth(
        label: String, badge: String?, rowSize: NSTableView.RowSizeStyle
    ) -> CGFloat {
        // A label's alignment rect is its text; the padding its cell draws falls outside.
        func width(_ text: String, _ font: NSFont) -> CGFloat {
            (text as NSString).size(withAttributes: [.font: font]).width.rounded(.up)
        }
        let badgeWidth = badge.map { width($0, badgeFont(for: rowSize)) } ?? 0
        return Style.iconWidth(for: rowSize) + Style.rowGap
            + width(label, labelFont(for: rowSize)) + Style.rowGap + badgeWidth + badgeInset
    }

    private static func badgeFont(for rowSize: NSTableView.RowSizeStyle) -> NSFont {
        .systemFont(ofSize: Style.fontSize(for: rowSize) - 1, weight: .semibold)
    }

    private static func labelFont(
        for rowSize: NSTableView.RowSizeStyle, emphasized: Bool = false
    ) -> NSFont {
        .systemFont(ofSize: Style.fontSize(for: rowSize), weight: emphasized ? .semibold : .regular)
    }

    private func applyRowSize() {
        label.font = Self.labelFont(for: rowSizeStyle, emphasized: isEmphasized)
        badge.font = Self.badgeFont(for: rowSizeStyle)
        icon.symbolConfiguration = NSImage.SymbolConfiguration(
            pointSize: Style.fontSize(for: rowSizeStyle), weight: .regular)
        iconWidth?.constant = Style.iconWidth(for: rowSizeStyle)
    }
}

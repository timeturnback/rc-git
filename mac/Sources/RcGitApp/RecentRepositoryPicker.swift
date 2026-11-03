import AppKit
import RcGitKit

/// The Open Recent Repository panel (⌃R): the recents newest first, narrowed by fuzzy match
/// as you type. The arrow keys move the highlight, Return or a double-click opens the pick,
/// and Escape closes. What to list comes from `RecentRepositories.matching`; the app delegate
/// decides what opening does.
@MainActor
final class RecentRepositoryPicker: NSObject, NSTextFieldDelegate, NSTableViewDataSource,
    NSTableViewDelegate
{
    var onOpen: ((String) -> Void)?

    private let panel: NSPanel
    private let field = NSTextField()
    private let table = NSTableView()
    private let empty = NSTextField(labelWithString: "")
    private var recents: [String] = []
    private var results: [String] = []

    override init() {
        panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 320),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered, defer: false)
        super.init()
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.isMovableByWindowBackground = true
        panel.hidesOnDeactivate = true
        for kind in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            panel.standardWindowButton(kind)?.isHidden = true
        }

        field.placeholderString = "Open recent repository"
        field.font = NSFont.systemFont(ofSize: 16)
        field.focusRingType = .none
        field.isBezeled = false
        field.drawsBackground = false
        field.delegate = self

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("repository"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowSizeStyle = .medium
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(openClicked(_:))
        table.refusesFirstResponder = true

        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false

        let separator = NSBox()
        separator.boxType = .separator

        empty.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        empty.textColor = .secondaryLabelColor
        empty.isHidden = true

        let stack = NSStackView(views: [field, separator, empty, scroll])
        stack.orientation = .vertical
        stack.spacing = 8
        stack.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 8, right: 12)
        panel.contentView = stack
    }

    /// Shows the panel over `window`, or centered on screen when no window is open, listing
    /// `recents` (newest first) with the newest highlighted.
    func show(_ recents: [String], over window: NSWindow?) {
        if let frame = window?.frame {
            let size = panel.frame.size
            panel.setFrameOrigin(
                NSPoint(
                    x: frame.midX - size.width / 2,
                    y: frame.midY - size.height / 2 + frame.height * 0.15))
        } else {
            panel.center()
        }
        self.recents = recents
        field.stringValue = ""
        filter()
        panel.makeKeyAndOrderFront(nil)
        field.becomeFirstResponder()
    }

    func close() {
        panel.orderOut(nil)
    }

    private func filter() {
        results = RecentRepositories.matching(recents, query: field.stringValue)
        empty.stringValue = RecentRepositories.emptyNote(hasRecents: !recents.isEmpty)
        empty.isHidden = !results.isEmpty
        table.reloadData()
        if !results.isEmpty {
            table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
            table.scrollRowToVisible(0)
        }
    }

    private func openSelection() {
        let row = table.selectedRow >= 0 ? table.selectedRow : 0
        guard results.indices.contains(row) else { return }
        let path = results[row]
        close()
        onOpen?(path)
    }

    @objc private func openClicked(_ sender: Any?) {
        openSelection()
    }

    private func moveSelection(by delta: Int) {
        guard !results.isEmpty else { return }
        let next = min(max(table.selectedRow + delta, 0), results.count - 1)
        table.selectRowIndexes(IndexSet(integer: next), byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    // MARK: NSTextFieldDelegate

    func controlTextDidChange(_ notification: Notification) {
        filter()
    }

    func control(
        _ control: NSControl, textView: NSTextView, doCommandBy selector: Selector
    ) -> Bool {
        switch selector {
        case #selector(NSResponder.moveDown(_:)):
            moveSelection(by: 1)
            return true
        case #selector(NSResponder.moveUp(_:)):
            moveSelection(by: -1)
            return true
        case #selector(NSResponder.insertNewline(_:)):
            openSelection()
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            close()
            return true
        default:
            return false
        }
    }

    // MARK: NSTableViewDataSource

    func numberOfRows(in tableView: NSTableView) -> Int {
        results.count
    }

    // MARK: NSTableViewDelegate

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int)
        -> NSView?
    {
        guard results.indices.contains(row) else { return nil }
        let cell = reusableCell()
        let path = results[row]
        cell.imageView?.image = Icons.repository
        let truncation = NSMutableParagraphStyle()
        truncation.lineBreakMode = .byTruncatingMiddle
        let text = NSMutableAttributedString(
            string: (path as NSString).lastPathComponent,
            attributes: [
                .font: NSFont.systemFont(ofSize: NSFont.systemFontSize),
                .foregroundColor: NSColor.labelColor,
                .paragraphStyle: truncation,
            ])
        text.append(
            NSAttributedString(
                string: "  " + RecentRepositories.displayPath(path),
                attributes: [
                    .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                    .foregroundColor: NSColor.secondaryLabelColor,
                    .paragraphStyle: truncation,
                ]))
        cell.textField?.attributedStringValue = text
        return cell
    }

    private func reusableCell() -> NSTableCellView {
        let identifier = NSUserInterfaceItemIdentifier("recent")
        if let reused = table.makeView(withIdentifier: identifier, owner: self) as? NSTableCellView
        {
            return reused
        }
        let cell = NSTableCellView()
        cell.identifier = identifier
        let imageView = NSImageView()
        imageView.translatesAutoresizingMaskIntoConstraints = false
        let label = NSTextField(labelWithString: "")
        label.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(imageView)
        cell.addSubview(label)
        cell.imageView = imageView
        cell.textField = label
        NSLayoutConstraint.activate([
            imageView.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            imageView.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            imageView.widthAnchor.constraint(equalToConstant: 18),
            imageView.heightAnchor.constraint(equalToConstant: 18),
            label.leadingAnchor.constraint(equalTo: imageView.trailingAnchor, constant: 6),
            label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -6),
            label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }
}

import AppKit
import RcGitKit

/// The Open Quickly panel: type a few characters, fuzzy-match every path under the current
/// root, Enter opens the pick. The index builds in the background on first use; typing
/// before it finishes just waits on the same build.
@MainActor
final class GoToFileController: NSObject, NSTextFieldDelegate, NSTableViewDataSource,
    NSTableViewDelegate
{
    var provider: PathSearchSession.Provider? {
        get { session.provider }
        set { session.provider = newValue }
    }
    var onOpen: ((PathIndexEntry) -> Void)?

    private let panel: NSPanel
    private let field = NSTextField()
    private let table = NSTableView()
    private let footer = NSTextField(labelWithString: PathSearchResult.truncationNote)
    private let failure = NSTextField(wrappingLabelWithString: "")
    private let session = PathSearchSession()
    private var results: [PathIndexEntry] { session.results }

    override init() {
        panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 380),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered, defer: false)
        super.init()
        session.onUpdate = { [weak self] update in self?.render(update) }
        session.onOpen = { [weak self] entry in
            guard let self else { return }
            close()
            onOpen?(entry)
        }
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.isMovableByWindowBackground = true
        panel.hidesOnDeactivate = true
        for kind in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            panel.standardWindowButton(kind)?.isHidden = true
        }

        field.placeholderString = "Go to file"
        field.font = NSFont.systemFont(ofSize: 16)
        field.focusRingType = .none
        field.isBezeled = false
        field.drawsBackground = false
        field.delegate = self

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("path"))
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

        footer.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        footer.textColor = .secondaryLabelColor
        footer.isHidden = true

        failure.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        failure.textColor = .secondaryLabelColor
        failure.isHidden = true

        let stack = NSStackView(views: [field, failure, separator, scroll, footer])
        stack.orientation = .vertical
        stack.spacing = 8
        stack.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 8, right: 12)
        panel.contentView = stack
    }

    /// Shows the panel centered over `window` and starts an empty query, which lists the
    /// first files and warms the index.
    func show(over window: NSWindow?) {
        if let frame = window?.frame {
            let size = panel.frame.size
            panel.setFrameOrigin(
                NSPoint(
                    x: frame.midX - size.width / 2,
                    y: frame.midY - size.height / 2 + frame.height * 0.15))
        } else {
            panel.center()
        }
        field.stringValue = ""
        session.reset()
        footer.isHidden = true
        failure.isHidden = true
        table.reloadData()
        panel.makeKeyAndOrderFront(nil)
        field.becomeFirstResponder()
        session.search("")
    }

    func close() {
        session.cancelPendingOpen()
        panel.orderOut(nil)
    }

    private func render(_ update: PathSearchSession.Update) {
        switch update {
        case .results(let truncated):
            footer.isHidden = !truncated
            failure.isHidden = true
            table.reloadData()
            if !results.isEmpty {
                table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
                table.scrollRowToVisible(0)
            }
        case .failed(let error):
            footer.isHidden = true
            failure.stringValue = "Cannot list files: \(String(describing: error))"
            failure.isHidden = false
            table.reloadData()
        }
    }

    private func openSelection() {
        session.open(row: table.selectedRow >= 0 ? table.selectedRow : 0)
    }

    @objc private func openClicked(_ sender: Any?) {
        openSelection()
    }

    private func moveSelection(by delta: Int) {
        guard !results.isEmpty else { return }
        let current = table.selectedRow
        let next = min(max(current + delta, 0), results.count - 1)
        table.selectRowIndexes(IndexSet(integer: next), byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    // MARK: NSTextFieldDelegate

    func controlTextDidChange(_ notification: Notification) {
        session.search(field.stringValue)
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
        let entry = results[row]
        let identifier = NSUserInterfaceItemIdentifier("match")
        let cell: NSTableCellView
        if let reused = table.makeView(withIdentifier: identifier, owner: self) as? NSTableCellView
        {
            cell = reused
        } else {
            cell = NSTableCellView()
            cell.identifier = identifier
            let imageView = NSImageView()
            imageView.translatesAutoresizingMaskIntoConstraints = false
            let label = NSTextField(labelWithString: "")
            label.lineBreakMode = .byTruncatingMiddle
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
        }
        let treeEntry = TreeEntry(name: entry.name, oid: entry.oid, kind: entry.kind)
        cell.imageView?.image = Icons.image(for: treeEntry)
        let text = NSMutableAttributedString(
            string: entry.name,
            attributes: [
                .font: NSFont.systemFont(ofSize: NSFont.systemFontSize),
                .foregroundColor: NSColor.labelColor,
            ])
        text.append(
            NSAttributedString(
                string: "  " + entry.path,
                attributes: [
                    .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                    .foregroundColor: NSColor.secondaryLabelColor,
                ]))
        cell.textField?.attributedStringValue = text
        return cell
    }
}

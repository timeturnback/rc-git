import AppKit
import RcGitKit

/// The Find in Repository panel: type a literal string, press Return, and every file below the
/// current root is searched in the background, hits listing as they land, grouped by file.
/// Return again, or a double-click, opens the selected line at its match. The session keeps
/// the results while the panel is closed, so the next hit is one ⇧⌘F and an arrow key away,
/// until the browsed root changes.
@MainActor
final class FindInRepositoryController: NSObject, NSTextFieldDelegate, NSTableViewDataSource,
    NSTableViewDelegate
{
    var provider: ContentSearchSession.Provider? {
        get { session.provider }
        set { session.provider = newValue }
    }
    var onOpen: ((PathIndexEntry, ContentLineMatch?) -> Void)?

    private let panel: NSPanel
    private let field = NSTextField()
    private let table = NSTableView()
    private let status = NSTextField(labelWithString: "")
    private let session = ContentSearchSession()
    /// The root the results on screen were searched under; a different one empties the list.
    private var scope: String?

    override init() {
        panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 720, height: 460),
            styleMask: [.titled, .closable, .resizable, .fullSizeContentView],
            backing: .buffered, defer: false)
        super.init()
        session.onUpdate = { [weak self] in self?.render() }
        session.onOpen = { [weak self] entry, match in
            guard let self else { return }
            panel.orderOut(nil)
            onOpen?(entry, match)
        }
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.isMovableByWindowBackground = true
        panel.hidesOnDeactivate = true
        for kind in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            panel.standardWindowButton(kind)?.isHidden = true
        }

        field.placeholderString = "Find in repository — Return to search"
        field.font = NSFont.systemFont(ofSize: 16)
        field.focusRingType = .none
        field.isBezeled = false
        field.drawsBackground = false
        field.delegate = self

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("hit"))
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

        status.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        status.textColor = .secondaryLabelColor
        status.lineBreakMode = .byTruncatingTail
        status.isHidden = true

        let stack = NSStackView(views: [field, separator, scroll, status])
        stack.orientation = .vertical
        stack.spacing = 8
        stack.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 8, right: 12)
        panel.contentView = stack
    }

    /// Shows the panel centered over `window` with the last query selected. `scope` names the
    /// root being browsed; results searched under another one are dropped.
    func show(over window: NSWindow?, scope: String) {
        if scope != self.scope {
            self.scope = scope
            session.reset()
            render()
        }
        if let frame = window?.frame {
            let size = panel.frame.size
            panel.setFrameOrigin(
                NSPoint(
                    x: frame.midX - size.width / 2,
                    y: frame.midY - size.height / 2 + frame.height * 0.1))
        } else {
            panel.center()
        }
        panel.makeKeyAndOrderFront(nil)
        field.becomeFirstResponder()
        field.currentEditor()?.selectAll(nil)
    }

    /// Hides the panel; a search still running keeps filling the list.
    func close() {
        panel.orderOut(nil)
    }

    /// Hides the panel and forgets the results, as when the window closes.
    func discard() {
        session.reset()
        panel.orderOut(nil)
    }

    private func render() {
        let previous = table.selectedRow
        // Rows only grow between resets, and a reset renders its empty list, so an unchanged
        // count means a batch with no hits: only the status line moved.
        if session.rows.count != table.numberOfRows { table.reloadData() }
        let text: String?
        switch session.status {
        case .idle: text = nil
        case let .searching(progress): text = ContentSearch.statusText(progress, finished: false)
        case let .finished(progress): text = ContentSearch.statusText(progress, finished: true)
        case let .failed(message): text = "Cannot search: \(message)"
        }
        status.stringValue = text ?? ""
        status.isHidden = text == nil
        if previous >= 0, previous < session.rows.count {
            table.selectRowIndexes(IndexSet(integer: previous), byExtendingSelection: false)
        } else if session.rows.count > 1 {
            table.selectRowIndexes(IndexSet(integer: 1), byExtendingSelection: false)
        }
    }

    @objc private func openClicked(_ sender: Any?) {
        session.open(row: table.clickedRow >= 0 ? table.clickedRow : table.selectedRow)
    }

    private func moveSelection(by delta: Int) {
        guard !session.rows.isEmpty else { return }
        let next = min(max(table.selectedRow + delta, 0), session.rows.count - 1)
        table.selectRowIndexes(IndexSet(integer: next), byExtendingSelection: false)
        table.scrollRowToVisible(next)
    }

    // MARK: NSTextFieldDelegate

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
            let selected = table.selectedRow >= 0 ? table.selectedRow : nil
            session.submit(field.stringValue, selectedRow: selected)
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
        session.rows.count
    }

    // MARK: NSTableViewDelegate

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int)
        -> NSView?
    {
        guard session.rows.indices.contains(row) else { return nil }
        let cell = reusableCell()
        switch session.rows[row] {
        case let .file(file):
            let entry = session.hits[file].entry
            let treeEntry = TreeEntry(name: entry.name, oid: entry.oid, kind: entry.kind)
            cell.imageView?.image = Icons.image(for: treeEntry)
            cell.textField?.attributedStringValue = fileLabel(session.hits[file])
        case let .line(file, line):
            cell.imageView?.image = nil
            cell.textField?.attributedStringValue = lineLabel(session.hits[file].lines[line])
        }
        return cell
    }

    private func reusableCell() -> NSTableCellView {
        let identifier = NSUserInterfaceItemIdentifier("hit")
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

    /// The file's name, then its path and how many lines matched, dimmed.
    private func fileLabel(_ hit: ContentSearchHit) -> NSAttributedString {
        let truncation = NSMutableParagraphStyle()
        truncation.lineBreakMode = .byTruncatingMiddle
        let text = NSMutableAttributedString(
            string: hit.entry.name,
            attributes: [
                .font: NSFont.boldSystemFont(ofSize: NSFont.systemFontSize),
                .foregroundColor: NSColor.labelColor,
                .paragraphStyle: truncation,
            ])
        let count = hit.lines.count
        let lines = "\(count)\(hit.truncated ? "+" : "") line\(count == 1 ? "" : "s")"
        text.append(
            NSAttributedString(
                string: "  \(hit.entry.path) · \(lines)",
                attributes: [
                    .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                    .foregroundColor: NSColor.secondaryLabelColor,
                    .paragraphStyle: truncation,
                ]))
        return text
    }

    /// The 1-based line number, dimmed, then the excerpt with each match marked.
    private func lineLabel(_ match: ContentLineMatch) -> NSAttributedString {
        let truncation = NSMutableParagraphStyle()
        truncation.lineBreakMode = .byTruncatingTail
        let font = NSFont.monospacedSystemFont(
            ofSize: NSFont.smallSystemFontSize, weight: .regular)
        let text = NSMutableAttributedString(
            string: "\(match.line + 1)  ",
            attributes: [
                .font: NSFont.monospacedDigitSystemFont(
                    ofSize: NSFont.smallSystemFontSize, weight: .regular),
                .foregroundColor: NSColor.secondaryLabelColor,
                .paragraphStyle: truncation,
            ])
        let offset = text.length
        text.append(
            NSAttributedString(
                string: match.excerpt,
                attributes: [
                    .font: font,
                    .foregroundColor: NSColor.labelColor,
                    .paragraphStyle: truncation,
                ]))
        for highlight in match.highlights {
            let range = NSRange(location: offset + highlight.lowerBound, length: highlight.count)
            guard NSMaxRange(range) <= text.length else { continue }
            text.addAttributes(
                [
                    .backgroundColor: NSColor.findHighlightColor,
                    .foregroundColor: NSColor.black,
                ], range: range)
        }
        return text
    }
}

import AppKit
import RcGitKit

/// A floating panel listing which commit introduced each run of lines in the open file.
@MainActor
final class BlamePanelController: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    private let panel: NSPanel
    private let table = NSTableView()
    private var spans: [BlameSpan] = []

    private static let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .none
        return formatter
    }()

    override init() {
        panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 640, height: 400),
            styleMask: [.titled, .closable, .resizable, .utilityWindow],
            backing: .buffered, defer: false)
        super.init()
        panel.isFloatingPanel = false

        for (identifier, title, width) in [
            ("lines", "Lines", CGFloat(80)), ("hash", "Commit", 72), ("author", "Author", 130),
            ("date", "Date", 100), ("title", "Title", 220),
        ] {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(identifier))
            column.title = title
            column.width = width
            if identifier == "title" {
                column.resizingMask = .autoresizingMask
            } else {
                column.resizingMask = .userResizingMask
            }
            table.addTableColumn(column)
        }
        table.dataSource = self
        table.delegate = self
        table.rowSizeStyle = .small
        table.usesAlternatingRowBackgroundColors = true

        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        panel.contentView = scroll
    }

    /// Shows the ranges for `fileName`, replacing whatever the panel showed before.
    func show(_ spans: [BlameSpan], fileName: String, over window: NSWindow?) {
        self.spans = spans
        panel.title = "Blame — \(fileName)"
        table.reloadData()
        if let frame = window?.frame {
            let size = panel.frame.size
            panel.setFrameOrigin(
                NSPoint(x: frame.midX - size.width / 2, y: frame.midY - size.height / 2))
        }
        panel.makeKeyAndOrderFront(nil)
    }

    func close() {
        panel.orderOut(nil)
    }

    // MARK: NSTableViewDataSource

    func numberOfRows(in tableView: NSTableView) -> Int {
        spans.count
    }

    // MARK: NSTableViewDelegate

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int)
        -> NSView?
    {
        guard let tableColumn, spans.indices.contains(row) else { return nil }
        let span = spans[row]
        let identifier = tableColumn.identifier
        let cell: NSTableCellView
        if let reused = table.makeView(withIdentifier: identifier, owner: self) as? NSTableCellView
        {
            cell = reused
        } else {
            cell = NSTableCellView()
            cell.identifier = identifier
            let label = NSTextField(labelWithString: "")
            label.lineBreakMode = .byTruncatingTail
            label.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
            label.translatesAutoresizingMaskIntoConstraints = false
            cell.addSubview(label)
            cell.textField = label
            NSLayoutConstraint.activate([
                label.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
                label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -2),
                label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
        }
        switch identifier.rawValue {
        case "lines":
            let last = span.range.startLine + span.range.lineCount - 1
            cell.textField?.stringValue =
                span.range.lineCount == 1
                ? "\(span.range.startLine)" : "\(span.range.startLine)–\(last)"
            cell.textField?.font = NSFont.monospacedDigitSystemFont(
                ofSize: NSFont.smallSystemFontSize, weight: .regular)
        case "hash":
            cell.textField?.stringValue = span.commit.oid.short
            cell.textField?.font = NSFont.monospacedSystemFont(
                ofSize: NSFont.smallSystemFontSize, weight: .regular)
            cell.textField?.textColor = .secondaryLabelColor
        case "author":
            cell.textField?.stringValue = span.commit.authorName
        case "date":
            let date = Date(timeIntervalSince1970: TimeInterval(span.commit.authorTime))
            cell.textField?.stringValue = Self.dateFormatter.string(from: date)
        default:
            cell.textField?.stringValue = span.commit.title
            cell.toolTip = span.commit.title
        }
        return cell
    }
}

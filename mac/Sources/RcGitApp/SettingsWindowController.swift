import AppKit
import RcGitKit

/// The Settings window: the ssh hosts whose keys the user approved in the app, with Remove.
/// Removing a host is how a legitimately rotated key is recovered from — the next fetch from
/// that host offers the trust sheet again. `ApprovedHostsModel` owns the list and the removal.
@MainActor
final class SettingsWindowController: NSWindowController, NSTableViewDataSource,
    NSTableViewDelegate
{
    private let model = ApprovedHostsModel()
    private let table = NSTableView()
    private let removeButton = NSButton(title: "Remove", target: nil, action: nil)
    private let emptyLabel = NSTextField(labelWithString: "No host keys approved in RCGit.")
    private static let hostColumn = NSUserInterfaceItemIdentifier("host")
    private static let fingerprintColumn = NSUserInterfaceItemIdentifier("fingerprint")

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 320),
            styleMask: [.titled, .closable, .resizable],
            backing: .buffered, defer: false)
        window.tabbingMode = .disallowed
        window.title = "Settings"
        window.contentMinSize = NSSize(width: 420, height: 220)
        super.init(window: window)
        window.contentView = buildContentView()
        window.center()
        window.setFrameAutosaveName("rc-git.settings.v1")
        model.onChanged = { [weak self] in self?.render() }
        render()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used; the window is built in code")
    }

    /// Shows the window with the approvals as they stand now: a repository window may have
    /// approved a host since it was last open.
    func present() {
        model.reload()
        showWindow(nil)
        window?.makeKeyAndOrderFront(nil)
    }

    @objc private func removeSelected(_ sender: Any?) {
        let chosen = Array(table.selectedRowIndexes)
        table.deselectAll(nil)
        model.remove(at: chosen)
    }

    private func render() {
        table.reloadData()
        emptyLabel.isHidden = !model.rows.isEmpty
        removeButton.isEnabled = !table.selectedRowIndexes.isEmpty
    }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int {
        model.rows.count
    }

    func tableView(
        _ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int
    ) -> NSView? {
        guard model.rows.indices.contains(row), let tableColumn else { return nil }
        let approval = model.rows[row]
        let text =
            tableColumn.identifier == Self.hostColumn ? approval.host : approval.fingerprint
        let label = NSTextField(labelWithString: text)
        label.lineBreakMode = .byTruncatingMiddle
        label.toolTip = text
        if tableColumn.identifier == Self.fingerprintColumn {
            label.font = .monospacedSystemFont(
                ofSize: NSFont.smallSystemFontSize, weight: .regular)
            label.isSelectable = true
        }
        return label
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        removeButton.isEnabled = !table.selectedRowIndexes.isEmpty
    }

    // MARK: Views

    private func buildContentView() -> NSView {
        let heading = Style.sectionHeading(NSTextField(labelWithString: "Approved SSH Host Keys"))
        let note = NSTextField(
            wrappingLabelWithString:
                "Keys you trusted in RCGit, per host. Remove a host after its key was rotated: "
                + "the next fetch asks you to approve the new key. Keys in known_hosts are "
                + "managed with ssh, not here.")
        note.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        note.textColor = .secondaryLabelColor
        emptyLabel.textColor = .secondaryLabelColor

        removeButton.bezelStyle = .rounded
        removeButton.target = self
        removeButton.action = #selector(removeSelected(_:))

        let list = tableScrollView()
        let spacer = NSView()
        spacer.setContentHuggingPriority(.init(1), for: .horizontal)
        let footer = NSStackView(views: [emptyLabel, spacer, removeButton])
        footer.orientation = .horizontal
        footer.alignment = .centerY

        let content = NSStackView(views: [heading, note, list, footer])
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = Style.barPadding
        let inset = 2 * Style.margin
        content.edgeInsets = NSEdgeInsets(top: inset, left: inset, bottom: inset, right: inset)
        content.setHuggingPriority(.defaultLow, for: .horizontal)
        for view in [note, list, footer] {
            view.widthAnchor.constraint(equalTo: content.widthAnchor, constant: -2 * inset)
                .isActive = true
        }
        list.setContentHuggingPriority(.defaultLow, for: .vertical)
        return content
    }

    private func tableScrollView() -> NSScrollView {
        for (identifier, title, width) in [
            (Self.hostColumn, "Host", CGFloat(180)),
            (Self.fingerprintColumn, "Fingerprint", CGFloat(320)),
        ] {
            let column = NSTableColumn(identifier: identifier)
            column.title = title
            column.width = width
            table.addTableColumn(column)
        }
        table.allowsMultipleSelection = true
        table.usesAlternatingRowBackgroundColors = true
        table.dataSource = self
        table.delegate = self
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        return scroll
    }
}

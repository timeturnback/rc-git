import AppKit
import RcGitKit

/// The commit list of the selected ref, with a detail pane: the files one commit changed and
/// the line diff of the picked file. Commits load a page at a time when scrolled near the
/// end; the detail loads on selection, one tree diff and one blob diff at a time.
@MainActor
final class HistoryViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    var onNeedsMore: (() -> Void)?
    var onBrowseCommit: ((CommitSummary) -> Void)?
    var onSelectCommit: ((CommitSummary) -> Void)?
    var onSelectChange: ((TreeChange) -> Void)?
    var onJumpToCommit: ((Oid) -> Void)?
    var onDetailBack: (() -> Void)?

    /// How close to the bottom, in points, the scroll may get before the next page loads.
    private static let prefetchMargin: CGFloat = 600

    private let table = NSTableView()
    private let scroll = NSScrollView()
    private let placeholder = NSTextField(labelWithString: "No commits")
    private let changesTable = NSTableView()
    private let diffText = NSTextView()
    private let diffPlaceholder = NSTextField(labelWithString: "Select a commit")
    private var commits: [CommitSummary] = []
    private var complete = false
    private var changes: [TreeChange] = []
    private var isReloading = false

    private let detailHeader = NSStackView()
    private let detailTitle = NSTextField(wrappingLabelWithString: "")
    private let detailRefs = NSTextField(labelWithString: "")
    private let detailAuthor = NSTextField(labelWithString: "")
    private let detailCommitter = NSTextField(labelWithString: "")
    private let detailBody = NSTextField(wrappingLabelWithString: "")
    private let parentsRow = NSStackView()
    private let backButton = NSButton()
    private var parentOids: [Oid] = []

    private static let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter
    }()

    private static let monoFont = NSFont.monospacedSystemFont(
        ofSize: NSFont.smallSystemFontSize, weight: .regular)

    override func loadView() {
        for (identifier, title, width) in [
            ("hash", "Commit", CGFloat(72)), ("title", "Title", 380), ("author", "Author", 140),
            ("date", "Date", 150),
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
        table.rowSizeStyle = .default
        table.usesAlternatingRowBackgroundColors = true
        table.allowsColumnReordering = false
        table.target = self
        table.doubleAction = #selector(browseClicked(_:))
        table.menu = buildMenu()

        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(
            self, selector: #selector(boundsChanged(_:)),
            name: NSView.boundsDidChangeNotification, object: scroll.contentView)

        placeholder.textColor = .secondaryLabelColor
        placeholder.translatesAutoresizingMaskIntoConstraints = false

        let commitsPane = NSView()
        scroll.translatesAutoresizingMaskIntoConstraints = false
        commitsPane.addSubview(scroll)
        commitsPane.addSubview(placeholder)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: commitsPane.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: commitsPane.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: commitsPane.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: commitsPane.trailingAnchor),
            placeholder.centerXAnchor.constraint(equalTo: commitsPane.centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: commitsPane.centerYAnchor),
        ])

        let split = NSSplitView()
        split.isVertical = false
        split.dividerStyle = .thin
        split.autosaveName = "rc-git.history"
        split.addArrangedSubview(commitsPane)
        split.addArrangedSubview(buildDetailPane())
        split.setHoldingPriority(NSLayoutConstraint.Priority(rawValue: 251), forSubviewAt: 0)
        view = split
    }

    private func buildDetailPane() -> NSView {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("change"))
        column.resizingMask = .autoresizingMask
        changesTable.addTableColumn(column)
        changesTable.headerView = nil
        changesTable.rowSizeStyle = .small
        changesTable.dataSource = self
        changesTable.delegate = self

        let changesScroll = NSScrollView()
        changesScroll.documentView = changesTable
        changesScroll.hasVerticalScroller = true
        changesScroll.autohidesScrollers = true

        diffText.isEditable = false
        diffText.isRichText = false
        diffText.usesFindBar = true
        diffText.font = Self.monoFont
        diffText.isHorizontallyResizable = true
        diffText.isVerticallyResizable = true
        diffText.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        diffText.autoresizingMask = []
        diffText.textContainer?.widthTracksTextView = false
        diffText.textContainer?.size = NSSize(
            width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)

        let diffScroll = NSScrollView()
        diffScroll.documentView = diffText
        diffScroll.hasVerticalScroller = true
        diffScroll.hasHorizontalScroller = true
        diffScroll.autohidesScrollers = true

        diffPlaceholder.textColor = .secondaryLabelColor
        diffPlaceholder.translatesAutoresizingMaskIntoConstraints = false

        let detailSplit = NSSplitView()
        detailSplit.isVertical = true
        detailSplit.dividerStyle = .thin
        detailSplit.autosaveName = "rc-git.history-detail"
        changesScroll.setContentHuggingPriority(.defaultHigh, for: .horizontal)
        detailSplit.addArrangedSubview(changesScroll)
        detailSplit.addArrangedSubview(diffScroll)
        detailSplit.setHoldingPriority(NSLayoutConstraint.Priority(rawValue: 251), forSubviewAt: 0)

        buildDetailHeader()

        let stacked = NSStackView(views: [detailHeader, detailSplit])
        stacked.orientation = .vertical
        stacked.alignment = .leading
        stacked.spacing = 0
        stacked.detachesHiddenViews = true

        let container = NSView()
        stacked.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(stacked)
        container.addSubview(diffPlaceholder)
        NSLayoutConstraint.activate([
            stacked.topAnchor.constraint(equalTo: container.topAnchor),
            stacked.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            stacked.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            stacked.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            detailHeader.widthAnchor.constraint(equalTo: stacked.widthAnchor),
            detailSplit.widthAnchor.constraint(equalTo: stacked.widthAnchor),
            diffPlaceholder.centerXAnchor.constraint(equalTo: container.centerXAnchor),
            diffPlaceholder.centerYAnchor.constraint(equalTo: container.centerYAnchor),
            changesScroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 180),
            container.heightAnchor.constraint(greaterThanOrEqualToConstant: 120),
        ])
        return container
    }

    private func buildDetailHeader() {
        detailTitle.font = NSFont.systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
        detailTitle.maximumNumberOfLines = 3
        detailRefs.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .medium)
        detailRefs.textColor = .controlAccentColor
        for label in [detailAuthor, detailCommitter] {
            label.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
            label.textColor = .secondaryLabelColor
            label.lineBreakMode = .byTruncatingTail
        }
        detailBody.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        detailBody.maximumNumberOfLines = 12
        detailBody.isSelectable = true

        backButton.title = "‹ Back"
        backButton.bezelStyle = .accessoryBarAction
        backButton.controlSize = .small
        backButton.target = self
        backButton.action = #selector(detailBackClicked(_:))

        parentsRow.orientation = .horizontal
        parentsRow.spacing = 6
        parentsRow.alignment = .centerY

        detailHeader.orientation = .vertical
        detailHeader.alignment = .leading
        detailHeader.spacing = 3
        detailHeader.edgeInsets = NSEdgeInsets(top: 8, left: 10, bottom: 8, right: 10)
        detailHeader.detachesHiddenViews = true
        for view in [detailTitle, detailRefs, detailAuthor, detailCommitter, detailBody] {
            detailHeader.addArrangedSubview(view)
        }
        detailHeader.addArrangedSubview(parentsRow)
        detailHeader.isHidden = true
    }

    /// Renders the detail header: message, refs, identities, and parent navigation.
    func renderCommitDetail(_ state: CommitDetailState, canGoBack: Bool) {
        switch state {
        case .empty:
            detailHeader.isHidden = true
        case let .loading(summary):
            detailHeader.isHidden = false
            fillHeader(summary: summary, detail: nil, refs: [], canGoBack: canGoBack)
        case let .loaded(detail, refs):
            detailHeader.isHidden = false
            fillHeader(summary: detail.summary, detail: detail, refs: refs, canGoBack: canGoBack)
        case let .failed(summary, message):
            detailHeader.isHidden = false
            fillHeader(summary: summary, detail: nil, refs: [], canGoBack: canGoBack)
            detailAuthor.stringValue = message
        }
    }

    private func fillHeader(
        summary: CommitSummary, detail: CommitDetail?, refs: [RefEntry], canGoBack: Bool
    ) {
        detailTitle.stringValue = "\(summary.oid.short)  \(summary.title)"
        detailRefs.stringValue = refs.map(\.shortName).joined(separator: " · ")
        detailRefs.isHidden = refs.isEmpty
        detailAuthor.stringValue = Self.identityLine(
            role: "Author", name: summary.authorName, email: summary.authorEmail,
            time: summary.authorTime)
        if let detail, Self.committerDiffers(detail) {
            detailCommitter.stringValue = Self.identityLine(
                role: "Committer", name: detail.committerName, email: detail.committerEmail,
                time: detail.committerTime)
            detailCommitter.isHidden = false
        } else {
            detailCommitter.isHidden = true
        }
        detailBody.stringValue = detail?.body ?? ""
        detailBody.isHidden = detail?.body.isEmpty ?? true
        rebuildParentsRow(summary: summary, canGoBack: canGoBack)
    }

    private static func committerDiffers(_ detail: CommitDetail) -> Bool {
        detail.committerName != detail.summary.authorName
            || detail.committerEmail != detail.summary.authorEmail
            || detail.committerTime != detail.summary.authorTime
    }

    private static func identityLine(role: String, name: String, email: String, time: Int64)
        -> String
    {
        let date = dateFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(time)))
        return "\(role): \(name) <\(email)> · \(date)"
    }

    private func rebuildParentsRow(summary: CommitSummary, canGoBack: Bool) {
        for view in parentsRow.arrangedSubviews {
            parentsRow.removeArrangedSubview(view)
            view.removeFromSuperview()
        }
        parentOids = summary.parents
        if canGoBack {
            parentsRow.addArrangedSubview(backButton)
        }
        if !summary.parents.isEmpty {
            let label = NSTextField(
                labelWithString: summary.parents.count == 1 ? "Parent:" : "Parents:")
            label.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
            label.textColor = .secondaryLabelColor
            parentsRow.addArrangedSubview(label)
            for (index, parent) in summary.parents.enumerated() {
                let button = NSButton(
                    title: parent.short, target: self, action: #selector(parentClicked(_:)))
                button.bezelStyle = .accessoryBarAction
                button.controlSize = .small
                button.font = Self.monoFont
                button.tag = index
                parentsRow.addArrangedSubview(button)
            }
        }
        parentsRow.isHidden = parentsRow.arrangedSubviews.isEmpty
    }

    @objc private func parentClicked(_ sender: NSButton) {
        guard parentOids.indices.contains(sender.tag) else { return }
        onJumpToCommit?(parentOids[sender.tag])
    }

    @objc private func detailBackClicked(_ sender: Any?) {
        onDetailBack?()
    }

    /// Highlights the row of a commit reached by a parent link or back jump, when loaded.
    func selectCommitRow(_ oid: Oid) {
        guard let row = commits.firstIndex(where: { $0.oid == oid }), table.selectedRow != row
        else { return }
        isReloading = true
        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        table.scrollRowToVisible(row)
        isReloading = false
    }

    /// Replaces the rows. Called with the accumulated pages, so a reload keeps the scroll
    /// position: only rows below the previous end are new.
    func reload(_ commits: [CommitSummary], complete: Bool) {
        let selected = table.selectedRow
        self.commits = commits
        self.complete = complete
        placeholder.isHidden = !commits.isEmpty || !complete
        isReloading = true
        table.reloadData()
        if selected >= 0, selected < commits.count {
            table.selectRowIndexes(IndexSet(integer: selected), byExtendingSelection: false)
        }
        isReloading = false
        requestMoreIfNearEnd()
    }

    /// Renders the commit detail: the changed-file list and the picked file's diff.
    func renderDetail(changes: [TreeChange], state: FileDiffState) {
        self.changes = changes
        isReloading = true
        changesTable.reloadData()
        switch state {
        case .empty:
            showDiffPlaceholder(commits.isEmpty ? "No commits" : "Select a commit")
        case let .loading(change):
            selectChangeRow(change)
            showDiffPlaceholder("Loading \(change.path)…")
        case let .loaded(change, diff):
            selectChangeRow(change)
            renderDiff(change: change, diff: diff)
        case let .failed(_, message):
            showDiffPlaceholder(message)
        }
        isReloading = false
    }

    private func selectChangeRow(_ change: TreeChange) {
        if let row = changes.firstIndex(of: change) {
            changesTable.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        }
    }

    private func showDiffPlaceholder(_ message: String) {
        diffPlaceholder.stringValue = message
        diffPlaceholder.isHidden = false
        diffText.enclosingScrollView?.isHidden = changes.isEmpty
        diffText.textStorage?.setAttributedString(NSAttributedString(string: ""))
    }

    private func renderDiff(change: TreeChange, diff: BlobDiff) {
        diffPlaceholder.isHidden = true
        diffText.enclosingScrollView?.isHidden = false
        switch diff {
        case let .text(hunks):
            if hunks.isEmpty {
                showDiffPlaceholder("\(change.path): no line changes")
                return
            }
            diffText.textStorage?.setAttributedString(Self.attributed(hunks: hunks))
            diffText.scroll(.zero)
        case let .binary(oldSize, newSize):
            showDiffPlaceholder(
                "\(change.path) is binary (\(ByteFormat.string(oldSize)) → \(ByteFormat.string(newSize)))"
            )
        case let .tooLarge(size, limit):
            showDiffPlaceholder(
                "\(change.path) is \(ByteFormat.string(size)), over the \(ByteFormat.string(limit)) limit"
            )
        }
    }

    static func attributed(hunks: [DiffHunk]) -> NSAttributedString {
        let result = NSMutableAttributedString()
        for (index, hunk) in hunks.enumerated() {
            if index > 0 {
                result.append(NSAttributedString(string: "\n", attributes: [.font: monoFont]))
            }
            result.append(
                NSAttributedString(
                    string:
                        "@@ -\(hunk.oldStart),\(hunk.oldLines) +\(hunk.newStart),\(hunk.newLines) @@\n",
                    attributes: [.font: monoFont, .foregroundColor: NSColor.tertiaryLabelColor]))
            for line in hunk.lines {
                let (sign, color): (String, NSColor) =
                    switch line.kind {
                    case .context: (" ", .labelColor)
                    case .added: ("+", .systemGreen)
                    case .removed: ("-", .systemRed)
                    }
                result.append(
                    NSAttributedString(
                        string: sign + line.text + "\n",
                        attributes: [.font: monoFont, .foregroundColor: color]))
            }
        }
        return result
    }

    @objc private func boundsChanged(_ notification: Notification) {
        requestMoreIfNearEnd()
    }

    private func requestMoreIfNearEnd() {
        guard !complete, view.window != nil else { return }
        let visible = scroll.contentView.documentVisibleRect
        let height = table.frame.height
        if visible.maxY >= height - Self.prefetchMargin {
            onNeedsMore?()
        }
    }

    private func buildMenu() -> NSMenu {
        let menu = NSMenu()
        let browse = NSMenuItem(
            title: "Browse Files at This Commit", action: #selector(browseClicked(_:)),
            keyEquivalent: "")
        browse.target = self
        menu.addItem(browse)
        let copy = NSMenuItem(
            title: "Copy Commit Hash", action: #selector(copyHash(_:)), keyEquivalent: "")
        copy.target = self
        menu.addItem(copy)
        return menu
    }

    private var actedRow: Int {
        table.clickedRow >= 0 ? table.clickedRow : table.selectedRow
    }

    @objc private func browseClicked(_ sender: Any?) {
        guard commits.indices.contains(actedRow) else { return }
        onBrowseCommit?(commits[actedRow])
    }

    @objc private func copyHash(_ sender: Any?) {
        guard commits.indices.contains(actedRow) else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(commits[actedRow].oid.hex, forType: .string)
    }

    // MARK: NSTableViewDataSource

    func numberOfRows(in tableView: NSTableView) -> Int {
        tableView === table ? commits.count : changes.count
    }

    // MARK: NSTableViewDelegate

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !isReloading, let tableView = notification.object as? NSTableView else { return }
        if tableView === table {
            guard commits.indices.contains(table.selectedRow) else { return }
            onSelectCommit?(commits[table.selectedRow])
        } else {
            guard changes.indices.contains(changesTable.selectedRow) else { return }
            onSelectChange?(changes[changesTable.selectedRow])
        }
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int)
        -> NSView?
    {
        guard let tableColumn else { return nil }
        if tableView === changesTable {
            guard changes.indices.contains(row) else { return nil }
            let change = changes[row]
            let cell = reusableCell(in: changesTable, for: tableColumn.identifier)
            let label = "\(change.kind.letter)  \(change.path)"
            cell.textField?.stringValue = label
            cell.textField?.font = Self.monoFont
            cell.toolTip = change.sourcePath.map { "\(change.path) ← \($0)" } ?? change.path
            return cell
        }
        guard commits.indices.contains(row) else { return nil }
        let commit = commits[row]
        let cell = reusableCell(in: table, for: tableColumn.identifier)
        switch tableColumn.identifier.rawValue {
        case "hash":
            cell.textField?.stringValue = commit.oid.short
            cell.textField?.font = Self.monoFont
            cell.textField?.textColor = .secondaryLabelColor
        case "title":
            cell.textField?.stringValue = commit.title
            cell.toolTip = commit.title
        case "author":
            cell.textField?.stringValue = commit.authorName
            cell.toolTip = "\(commit.authorName) <\(commit.authorEmail)>"
        default:
            let date = Date(timeIntervalSince1970: TimeInterval(commit.authorTime))
            cell.textField?.stringValue = Self.dateFormatter.string(from: date)
        }
        return cell
    }

    private func reusableCell(
        in tableView: NSTableView, for identifier: NSUserInterfaceItemIdentifier
    ) -> NSTableCellView {
        if let cell = tableView.makeView(withIdentifier: identifier, owner: self)
            as? NSTableCellView
        {
            cell.toolTip = nil
            return cell
        }
        let cell = NSTableCellView()
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
        return cell
    }
}

import AppKit
import RcGitKit

/// A label the mouse passes through. The commit box's placeholder sits over the text view and
/// must not take the click that puts the caret there.
private final class PassthroughLabel: NSTextField {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

/// The Changes view, split across the window: the sidebar section holds every changed
/// working-tree path with a tick per file, the stash list, and the commit box, while the tab
/// itself holds one file's diff against `HEAD` with a tick per hunk. Renders state the window
/// controller pushes in and forwards every action back out; it never classifies or performs a
/// mutation itself.
@MainActor
final class ChangesViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate,
    NSTextViewDelegate
{
    var onSelectEntry: ((StatusEntry) -> Void)?
    var onToggleEntry: ((String, Bool) -> Void)?
    var onToggleHunk: ((String, HunkStamp, [HunkStamp]) -> Void)?
    var onCommit: ((String) -> Void)?
    var onStashPush: (() -> Void)?
    var onStashApply: ((Int) -> Void)?
    var onStashPop: ((Int) -> Void)?
    var onStashDrop: ((Int) -> Void)?
    var onDiscard: (([String]) -> Void)?
    var onDiscardHunk: ((String, HunkStamp) -> Void)?
    var onRefresh: (() -> Void)?
    var onFetch: (() -> Void)?
    var onPull: (() -> Void)?
    var onPush: (() -> Void)?
    /// The model's tick state, injected so rendering never duplicates it.
    var isTicked: ((String) -> Bool)?
    var isHunkTicked: ((String, HunkStamp) -> Bool)?

    private enum DiffRow {
        case header(stamp: HunkStamp, all: [HunkStamp])
        case line(DiffLine)
    }

    private let filesTable = NSTableView()
    private let filesScroll = NSScrollView()
    private let filesPlaceholder = NSTextField(labelWithString: "No changes")
    private let truncationLabel = NSTextField(labelWithString: "")
    private let stashTable = NSTableView()
    private let stashScroll = NSScrollView()
    private let stashGroup = NSStackView()
    private let applyButton = NSButton(title: "Apply", target: nil, action: nil)
    private let popButton = NSButton(title: "Pop", target: nil, action: nil)
    private let dropButton = NSButton(title: "Drop…", target: nil, action: nil)
    private let fetchButton = NSButton(title: "Fetch", target: nil, action: nil)
    private let pullButton = NSButton(title: "Pull", target: nil, action: nil)
    private let pushButton = NSButton(title: "Push", target: nil, action: nil)
    private let messageText = NSTextView()
    private let messageScroll = NSScrollView()
    private let messagePlaceholder = PassthroughLabel(labelWithString: "Commit message")
    private let summaryLabel = NSTextField(labelWithString: "")
    private let commitButton = NSButton(title: "Commit", target: nil, action: nil)
    private let stashButton = NSButton(title: "Stash", target: nil, action: nil)
    private let diffTable = NSTableView()
    private let diffScroll = NSScrollView()
    private let diffPlaceholder = NSTextField(labelWithString: "Select a file")

    private var entries: [StatusEntry] = []
    private var stashes: [StashEntry] = []
    private var diffRows: [DiffRow] = []
    private var diffPath: String?
    private var isReloading = false
    /// Re-made every time their row shows: the stack gives no width to a view it has not laid
    /// out, and it drops the one made here whenever the row is hidden again.
    private var truncationWidth: NSLayoutConstraint?
    private var stashWidth: NSLayoutConstraint?
    private weak var sectionStack: NSStackView?

    /// The gutter the sidebar section keeps on both sides.
    private static let sectionInset: CGFloat = 8

    // MARK: Layout

    /// The tab itself is the diff alone; the file list and the commit box live in the
    /// window sidebar, built by `makeSidebarSection()`.
    override func loadView() {
        view = buildDiffPane()
    }

    /// The change list, the stash list, and the commit box, for the window sidebar. Call
    /// once, before the sidebar lays out.
    func makeSidebarSection() -> NSView {
        buildFilesTable()
        buildStashTable()
        return buildSection()
    }

    private func buildFilesTable() {
        let tick = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("tick"))
        tick.width = 24
        tick.resizingMask = []
        filesTable.addTableColumn(tick)
        let file = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("file"))
        file.resizingMask = .autoresizingMask
        filesTable.addTableColumn(file)
        filesTable.headerView = nil
        filesTable.rowSizeStyle = .small
        filesTable.dataSource = self
        filesTable.delegate = self
        filesTable.allowsMultipleSelection = true
        filesTable.menu = filesMenu()

        filesScroll.documentView = filesTable
        filesScroll.hasVerticalScroller = true
        filesScroll.autohidesScrollers = true
    }

    private func buildStashTable() {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("stash"))
        column.resizingMask = .autoresizingMask
        stashTable.addTableColumn(column)
        stashTable.headerView = nil
        stashTable.rowSizeStyle = .small
        stashTable.dataSource = self
        stashTable.delegate = self
    }

    private func buildSection() -> NSView {
        for (button, action): (NSButton, Selector) in [
            (fetchButton, #selector(fetchClicked(_:))),
            (pullButton, #selector(pullClicked(_:))),
            (pushButton, #selector(pushClicked(_:))),
        ] {
            button.target = self
            button.action = action
            button.bezelStyle = .rounded
            button.controlSize = .small
            // Disabled until the window controller renders the first remote status.
            button.isEnabled = false
        }
        let syncRow = NSStackView(views: [fetchButton, pullButton, pushButton, NSView()])
        syncRow.orientation = .horizontal
        syncRow.spacing = 6

        truncationLabel.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        truncationLabel.textColor = .secondaryLabelColor
        truncationLabel.isHidden = true

        let stashLabel = NSTextField(labelWithString: "Stashes")
        stashLabel.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .semibold)
        stashLabel.textColor = .secondaryLabelColor
        stashScroll.documentView = stashTable
        stashScroll.hasVerticalScroller = true
        stashScroll.autohidesScrollers = true

        for (button, action): (NSButton, Selector) in [
            (applyButton, #selector(applyClicked(_:))),
            (popButton, #selector(popClicked(_:))),
            (dropButton, #selector(dropClicked(_:))),
        ] {
            button.target = self
            button.action = action
            button.bezelStyle = .rounded
            button.controlSize = .small
            button.isEnabled = false
        }
        let stashButtons = NSStackView(views: [applyButton, popButton, dropButton])
        stashButtons.orientation = .horizontal
        stashButtons.spacing = 6
        // One group, so a repository with no stashes hides the header, the list, and the
        // buttons together instead of showing an empty box under a heading.
        stashGroup.orientation = .vertical
        stashGroup.alignment = .width
        stashGroup.spacing = 6
        for row in [stashLabel, stashScroll, stashButtons] as [NSView] {
            stashGroup.addArrangedSubview(row)
        }

        messageText.isRichText = false
        messageText.font = NSFont.systemFont(ofSize: NSFont.systemFontSize)
        messageText.isVerticallyResizable = true
        messageText.autoresizingMask = [.width]
        messageText.textContainer?.widthTracksTextView = true
        messageText.delegate = self
        messageScroll.documentView = messageText
        messageScroll.hasVerticalScroller = true
        messageScroll.autohidesScrollers = true
        messageScroll.borderType = .bezelBorder
        // NSTextView has no placeholder, so one sits over the empty box.
        messagePlaceholder.font = NSFont.systemFont(ofSize: NSFont.systemFontSize)
        messagePlaceholder.textColor = .placeholderTextColor
        messagePlaceholder.translatesAutoresizingMaskIntoConstraints = false

        summaryLabel.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        summaryLabel.textColor = .secondaryLabelColor
        commitButton.target = self
        commitButton.action = #selector(commitClicked(_:))
        commitButton.bezelStyle = .rounded
        // Cmd-Return commits; a bare Return keeps typing newlines into the message.
        commitButton.keyEquivalent = "\r"
        commitButton.keyEquivalentModifierMask = [.command]
        stashButton.target = self
        stashButton.action = #selector(stashClicked(_:))
        stashButton.bezelStyle = .rounded
        let actionRow = NSStackView(views: [summaryLabel, NSView(), stashButton, commitButton])
        actionRow.orientation = .horizontal
        actionRow.spacing = 6

        let stack = NSStackView(views: [
            syncRow, filesScroll, truncationLabel, stashGroup, messageScroll, actionRow,
        ])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.distribution = .fill
        stack.spacing = 6
        stack.edgeInsets = NSEdgeInsets(
            top: 6, left: Self.sectionInset, bottom: Self.sectionInset, right: Self.sectionInset)
        stack.detachesHiddenViews = true
        sectionStack = stack
        filesScroll.setContentHuggingPriority(.init(rawValue: 1), for: .vertical)
        stashScroll.setContentHuggingPriority(.defaultHigh, for: .vertical)
        messageScroll.setContentHuggingPriority(.defaultHigh, for: .vertical)

        let pane = NSView()
        stack.translatesAutoresizingMaskIntoConstraints = false
        pane.addSubview(stack)
        pane.addSubview(filesPlaceholder)
        pane.addSubview(messagePlaceholder)
        filesPlaceholder.textColor = .secondaryLabelColor
        filesPlaceholder.translatesAutoresizingMaskIntoConstraints = false
        let inset = Self.sectionInset
        // The list keeps a usable height, but yields before the layout breaks: it shares
        // the sidebar with the ref list now.
        let filesFloor = filesScroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 150)
        filesFloor.priority = NSLayoutConstraint.Priority(rawValue: 700)
        NSLayoutConstraint.activate([
            syncRow.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -2 * inset),
            stack.topAnchor.constraint(equalTo: pane.topAnchor),
            stack.bottomAnchor.constraint(equalTo: pane.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: pane.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: pane.trailingAnchor),
            filesScroll.widthAnchor.constraint(
                equalTo: stack.widthAnchor, constant: -2 * inset),
            messageScroll.widthAnchor.constraint(
                equalTo: stack.widthAnchor, constant: -2 * inset),
            actionRow.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -2 * inset),
            filesFloor,
            stashScroll.heightAnchor.constraint(equalToConstant: 96),
            messageScroll.heightAnchor.constraint(equalToConstant: 64),
            filesPlaceholder.centerXAnchor.constraint(equalTo: filesScroll.centerXAnchor),
            filesPlaceholder.centerYAnchor.constraint(equalTo: filesScroll.centerYAnchor),
            // The text container's own inset, so the placeholder sits on the caret.
            messagePlaceholder.leadingAnchor.constraint(
                equalTo: messageScroll.leadingAnchor, constant: 5),
            messagePlaceholder.topAnchor.constraint(
                equalTo: messageScroll.topAnchor, constant: 2),
        ])
        show(stashGroup, visible: true, width: &stashWidth)
        return pane
    }

    private func buildDiffPane() -> NSView {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("diff"))
        column.resizingMask = .autoresizingMask
        diffTable.addTableColumn(column)
        diffTable.headerView = nil
        diffTable.rowHeight = 16
        diffTable.intercellSpacing = NSSize(width: 3, height: 0)
        diffTable.usesAlternatingRowBackgroundColors = false
        diffTable.selectionHighlightStyle = .none
        diffTable.dataSource = self
        diffTable.delegate = self
        diffTable.menu = diffMenu()

        diffScroll.documentView = diffTable
        diffScroll.hasVerticalScroller = true
        diffScroll.hasHorizontalScroller = true
        diffScroll.autohidesScrollers = true

        diffPlaceholder.textColor = .secondaryLabelColor
        diffPlaceholder.translatesAutoresizingMaskIntoConstraints = false

        let pane = NSView()
        diffScroll.translatesAutoresizingMaskIntoConstraints = false
        pane.addSubview(diffScroll)
        pane.addSubview(diffPlaceholder)
        NSLayoutConstraint.activate([
            diffScroll.topAnchor.constraint(equalTo: pane.topAnchor),
            diffScroll.bottomAnchor.constraint(equalTo: pane.bottomAnchor),
            diffScroll.leadingAnchor.constraint(equalTo: pane.leadingAnchor),
            diffScroll.trailingAnchor.constraint(equalTo: pane.trailingAnchor),
            diffScroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 150),
            diffPlaceholder.centerXAnchor.constraint(equalTo: pane.centerXAnchor),
            diffPlaceholder.centerYAnchor.constraint(equalTo: pane.centerYAnchor),
        ])
        return pane
    }

    private func filesMenu() -> NSMenu {
        let menu = NSMenu()
        menu.addItem(
            NSMenuItem(
                title: "Discard Changes…", action: #selector(discardClicked(_:)), keyEquivalent: ""
            ))
        menu.items.forEach { $0.target = self }
        return menu
    }

    private func diffMenu() -> NSMenu {
        let menu = NSMenu()
        menu.addItem(
            NSMenuItem(
                title: "Discard This Hunk…", action: #selector(discardHunkClicked(_:)),
                keyEquivalent: ""))
        menu.items.forEach { $0.target = self }
        return menu
    }

    // MARK: Rendering

    func renderStatus(_ state: ChangesState) {
        switch state {
        case .idle, .loading:
            return
        case let .loaded(report):
            entries = report.entries
            filesPlaceholder.isHidden = !entries.isEmpty
            filesPlaceholder.stringValue = "No changes"
            showTruncation(
                report.truncated
                    ? "Scan stopped after \(report.scanned) entries; the list is incomplete."
                    : nil)
        case let .failed(message):
            entries = []
            filesPlaceholder.isHidden = false
            filesPlaceholder.stringValue = message
            showTruncation(nil)
        }
        reloadPreservingSelection(filesTable)
        refreshSummary()
    }

    func renderDiff(_ state: WorktreeDiffState) {
        switch state {
        case .idle:
            diffPath = nil
            diffRows = []
            showDiffPlaceholder("Select a file")
        case let .loading(path):
            diffPath = path
            diffRows = []
            showDiffPlaceholder("Loading \(path)…")
        case let .failed(path, message):
            diffPath = path
            diffRows = []
            showDiffPlaceholder(message)
        case let .loaded(path, diff):
            diffPath = path
            switch diff {
            case let .text(hunks):
                let all = hunks.map(Self.stamp(of:))
                diffRows = []
                for hunk in hunks {
                    diffRows.append(.header(stamp: Self.stamp(of: hunk), all: all))
                    for line in hunk.lines {
                        diffRows.append(.line(line))
                    }
                }
                if diffRows.isEmpty {
                    showDiffPlaceholder("\(path): no line changes")
                } else {
                    diffPlaceholder.isHidden = true
                    diffScroll.isHidden = false
                }
            case let .binary(oldSize, newSize):
                diffRows = []
                showDiffPlaceholder(
                    "\(path) is binary (\(ByteFormat.string(oldSize)) → \(ByteFormat.string(newSize)))"
                )
            case let .tooLarge(size, limit):
                diffRows = []
                showDiffPlaceholder(
                    "\(path) is \(ByteFormat.string(size)), over the \(ByteFormat.string(limit)) limit"
                )
            }
        }
        diffTable.reloadData()
    }

    /// Re-renders the sync row from the last preflight: commit counts in the button
    /// titles, everything disabled while a workflow runs, pull/push disabled without
    /// an upstream.
    func renderSync(status: RemoteStatus?, busy: Bool) {
        pullButton.title = SyncTitles.pull(status)
        pushButton.title = SyncTitles.push(status)
        let hasUpstream = status?.hasUpstream != false
        fetchButton.isEnabled = !busy
        pullButton.isEnabled = !busy && hasUpstream
        pushButton.isEnabled = !busy && hasUpstream
    }

    func renderStashes(_ stashes: [StashEntry]) {
        self.stashes = stashes
        reloadPreservingSelection(stashTable)
        updateStashButtons()
        show(stashGroup, visible: !stashes.isEmpty, width: &stashWidth)
    }

    /// Re-renders tick checkboxes after the model's tick state moved.
    func refreshTicks() {
        reloadPreservingSelection(filesTable)
        diffTable.reloadData()
        refreshSummary()
    }

    var commitMessage: String {
        messageText.string
    }

    func clearMessage() {
        messageText.string = ""
        refreshMessagePlaceholder()
    }

    func textDidChange(_ notification: Notification) {
        refreshMessagePlaceholder()
    }

    private func refreshMessagePlaceholder() {
        messagePlaceholder.isHidden = !messageText.string.isEmpty
    }

    private func refreshSummary() {
        let ticked = entries.filter { isTicked?($0.path) ?? true }.count
        summaryLabel.stringValue = entries.isEmpty ? "" : "\(ticked) of \(entries.count) selected"
        commitButton.isEnabled = ticked > 0
        stashButton.isEnabled = ticked > 0
    }

    /// Shows the scan-truncation note, or hides it.
    private func showTruncation(_ message: String?) {
        if let message { truncationLabel.stringValue = message }
        show(truncationLabel, visible: message != nil, width: &truncationWidth)
    }

    /// Shows or hides a row of the sidebar section, carrying the row's width by hand. The
    /// stack only gives its own width to a view it has laid out, so a row that is hidden when
    /// the stack first lays out would never get one, and a row it detaches loses the one it
    /// has: the width is made again on every show.
    private func show(_ row: NSView, visible: Bool, width: inout NSLayoutConstraint?) {
        width?.isActive = false
        width = nil
        row.isHidden = !visible
        guard visible, let stack = sectionStack else { return }
        let constraint = row.widthAnchor.constraint(
            equalTo: stack.widthAnchor, constant: -2 * Self.sectionInset)
        constraint.isActive = true
        width = constraint
    }

    private func showDiffPlaceholder(_ message: String) {
        diffPlaceholder.stringValue = message
        diffPlaceholder.isHidden = false
        diffScroll.isHidden = true
    }

    private func reloadPreservingSelection(_ table: NSTableView) {
        isReloading = true
        let selected = table.selectedRowIndexes
        table.reloadData()
        table.selectRowIndexes(selected, byExtendingSelection: false)
        isReloading = false
    }

    private static func stamp(of hunk: DiffHunk) -> HunkStamp {
        HunkStamp(
            oldStart: hunk.oldStart, oldLines: hunk.oldLines,
            newStart: hunk.newStart, newLines: hunk.newLines)
    }

    // MARK: Actions

    @objc private func fetchClicked(_ sender: Any?) {
        onFetch?()
    }

    @objc private func pullClicked(_ sender: Any?) {
        onPull?()
    }

    @objc private func pushClicked(_ sender: Any?) {
        onPush?()
    }

    @objc private func commitClicked(_ sender: Any?) {
        onCommit?(messageText.string)
    }

    @objc private func stashClicked(_ sender: Any?) {
        onStashPush?()
    }

    @objc private func applyClicked(_ sender: Any?) {
        guard let index = selectedStashIndex else { return }
        onStashApply?(index)
    }

    @objc private func popClicked(_ sender: Any?) {
        guard let index = selectedStashIndex else { return }
        onStashPop?(index)
    }

    @objc private func dropClicked(_ sender: Any?) {
        guard let index = selectedStashIndex else { return }
        onStashDrop?(index)
    }

    @objc private func discardClicked(_ sender: Any?) {
        var rows = filesTable.selectedRowIndexes
        if filesTable.clickedRow >= 0, !rows.contains(filesTable.clickedRow) {
            rows = IndexSet(integer: filesTable.clickedRow)
        }
        let paths = rows.compactMap { entries.indices.contains($0) ? entries[$0].path : nil }
        guard !paths.isEmpty else { return }
        onDiscard?(paths)
    }

    @objc private func discardHunkClicked(_ sender: Any?) {
        let row = diffTable.clickedRow
        guard let diffPath, diffRows.indices.contains(row),
            case let .header(stamp, _) = diffRows[row]
        else { return }
        onDiscardHunk?(diffPath, stamp)
    }

    @objc private func fileTickToggled(_ sender: NSButton) {
        let row = filesTable.row(for: sender)
        guard entries.indices.contains(row) else { return }
        onToggleEntry?(entries[row].path, sender.state == .on)
    }

    @objc private func hunkTickToggled(_ sender: NSButton) {
        let row = diffTable.row(for: sender)
        guard let diffPath, diffRows.indices.contains(row),
            case let .header(stamp, all) = diffRows[row]
        else { return }
        onToggleHunk?(diffPath, stamp, all)
    }

    private var selectedStashIndex: Int? {
        let row = stashTable.selectedRow
        return stashes.indices.contains(row) ? stashes[row].index : nil
    }

    private func updateStashButtons() {
        let hasSelection = selectedStashIndex != nil
        applyButton.isEnabled = hasSelection
        popButton.isEnabled = hasSelection
        dropButton.isEnabled = hasSelection
    }

    // MARK: Tables

    func numberOfRows(in tableView: NSTableView) -> Int {
        if tableView === filesTable { return entries.count }
        if tableView === stashTable { return stashes.count }
        return diffRows.count
    }

    func tableView(
        _ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int
    ) -> NSView? {
        if tableView === filesTable {
            return fileCell(column: tableColumn, row: row)
        }
        if tableView === stashTable {
            guard stashes.indices.contains(row) else { return nil }
            let stash = stashes[row]
            return label("stash@{\(stash.index)}  \(stash.message)", tooltip: stash.oid.hex)
        }
        return diffCell(row: row)
    }

    private func fileCell(column: NSTableColumn?, row: Int) -> NSView? {
        guard entries.indices.contains(row) else { return nil }
        let entry = entries[row]
        if column?.identifier.rawValue == "tick" {
            let box = NSButton(
                checkboxWithTitle: "", target: self, action: #selector(fileTickToggled(_:)))
            box.state = (isTicked?(entry.path) ?? true) ? .on : .off
            box.controlSize = .small
            return box
        }
        let (letter, color): (String, NSColor) =
            switch entry.status {
            case .modified: ("M", .systemOrange)
            case .untracked: ("U", .systemGreen)
            case .deleted: ("D", .systemRed)
            case .unchanged, .ignored: (" ", .labelColor)
            }
        let text = NSMutableAttributedString(
            string: letter + "  ",
            attributes: [
                .font: NSFont.monospacedSystemFont(
                    ofSize: NSFont.smallSystemFontSize, weight: .bold),
                .foregroundColor: color,
            ])
        text.append(
            NSAttributedString(
                string: entry.path,
                attributes: [
                    .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                    .foregroundColor: NSColor.labelColor,
                ]))
        let field = NSTextField(labelWithAttributedString: text)
        field.lineBreakMode = .byTruncatingHead
        field.toolTip = entry.path
        return field
    }

    private func diffCell(row: Int) -> NSView? {
        guard diffRows.indices.contains(row) else { return nil }
        switch diffRows[row] {
        case let .header(stamp, _):
            let box = NSButton(
                checkboxWithTitle: "", target: self, action: #selector(hunkTickToggled(_:)))
            box.controlSize = .small
            box.state = (diffPath.flatMap { isHunkTicked?($0, stamp) } ?? true) ? .on : .off
            let header = NSTextField(
                labelWithAttributedString: DiffRendering.header(
                    oldStart: stamp.oldStart, oldLines: stamp.oldLines,
                    newStart: stamp.newStart, newLines: stamp.newLines))
            let stack = NSStackView(views: [box, header])
            stack.orientation = .horizontal
            stack.spacing = 4
            return stack
        case let .line(line):
            let field = NSTextField(labelWithAttributedString: DiffRendering.line(line))
            field.lineBreakMode = .byClipping
            return field
        }
    }

    private func label(_ text: String, tooltip: String? = nil) -> NSTextField {
        let field = NSTextField(labelWithString: text)
        field.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        field.lineBreakMode = .byTruncatingTail
        field.toolTip = tooltip
        return field
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !isReloading, let table = notification.object as? NSTableView else { return }
        if table === filesTable {
            let row = filesTable.selectedRow
            guard entries.indices.contains(row) else { return }
            onSelectEntry?(entries[row])
        } else if table === stashTable {
            updateStashButtons()
        }
    }
}

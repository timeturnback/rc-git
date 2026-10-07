import AppKit
import RcGitKit

/// A label the mouse passes through. The commit box's placeholder sits over the text view and
/// must not take the click that puts the caret there.
private final class PassthroughLabel: NSTextField {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

/// One Changes row: a tick box and the path label on one baseline. Both are centred on the
/// row by constraint — a checkbox and a label left to size themselves sit at different
/// heights.
private final class ChangeCellView: NSTableCellView {
    let tick = NSButton(checkboxWithTitle: "", target: nil, action: nil)
    let label = NSTextField(labelWithString: "")

    init(target: AnyObject, action: Selector) {
        super.init(frame: .zero)
        tick.target = target
        tick.action = action
        tick.controlSize = .small
        tick.translatesAutoresizingMaskIntoConstraints = false
        label.translatesAutoresizingMaskIntoConstraints = false
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        addSubview(tick)
        addSubview(label)
        textField = label
        NSLayoutConstraint.activate([
            tick.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            tick.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.leadingAnchor.constraint(equalTo: tick.trailingAnchor, constant: 4),
            label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -2),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }
}

/// The Changes view, split across the window: the sidebar section holds every changed
/// working-tree path with a tick per file, the stash list, and the commit box, while the tab
/// itself holds one file's diff against `HEAD` with a tick per hunk. Renders state the window
/// controller pushes in and forwards every action back out; it never classifies or performs a
/// mutation itself.
@MainActor
final class ChangesViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate,
    NSOutlineViewDataSource, NSOutlineViewDelegate, NSTextViewDelegate
{
    var onSelectEntry: ((StatusEntry) -> Void)?
    /// Ticks or unticks whole files: one path from a file row, every path beneath a folder row.
    var onTogglePaths: (([String], Bool) -> Void)?
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

    private let filesOutline = NSOutlineView()
    private let filesScroll = NSScrollView()
    private let filesPlaceholder = NSTextField(labelWithString: "No changes")
    private let groupingControl = NSSegmentedControl()
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

    private typealias StatusNode = ChangeNode<StatusEntry>

    private var entries: [StatusEntry] = []
    /// The change list's rows under the current grouping, rebuilt on every status render.
    private var fileNodes: [StatusNode] = []
    /// Folder rows are expanded unless the user closed them; reload rebuilds the nodes, so
    /// the state has to live by path rather than by row.
    private var collapsedFolders: Set<String> = []
    private let groupingStore = ChangeGroupingStore.worktree()
    private var stashes: [StashEntry] = []
    private var diffRows: [DiffRow] = []
    private var diffPath: String?
    private var isReloading = false
    /// Re-made every time their row shows: the stack gives no width to a view it has not laid
    /// out, and it drops the one made here whenever the row is hidden again.
    private var truncationWidth: NSLayoutConstraint?
    private var stashWidth: NSLayoutConstraint?
    private weak var sectionStack: NSStackView?

    /// The gutter the sidebar section keeps on both sides, the sidebar's own margin.
    private static let sectionInset = Style.margin

    // MARK: Layout

    /// The tab itself is the diff alone; the file list and the commit box live in the
    /// window sidebar, built by `makeSidebarSection()`.
    override func loadView() {
        view = buildDiffPane()
    }

    /// The change list, the stash list, and the commit box, for the window sidebar. Call
    /// once, before the sidebar lays out.
    func makeSidebarSection() -> NSView {
        buildFilesOutline()
        buildStashTable()
        return buildSection()
    }

    /// One outline serves both groupings: flat rows are roots with no children, folder rows
    /// nest under them.
    private func buildFilesOutline() {
        let file = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("file"))
        file.resizingMask = .autoresizingMask
        filesOutline.addTableColumn(file)
        filesOutline.outlineTableColumn = file
        filesOutline.headerView = nil
        // Dense rows: every one carries a tick box, and the list shares the sidebar with the
        // commit box.
        Style.sidebarList(filesOutline, rowSize: .small)
        filesOutline.indentationPerLevel = 12
        // The list has no horizontal scroller: the column stays the pane's width and the
        // labels truncate inside it.
        filesOutline.autoresizesOutlineColumn = false
        filesOutline.dataSource = self
        filesOutline.delegate = self
        filesOutline.allowsMultipleSelection = true
        filesOutline.menu = filesMenu()
        filesOutline.target = self
        filesOutline.doubleAction = #selector(toggleClickedRow(_:))

        filesScroll.documentView = filesOutline
        filesScroll.hasVerticalScroller = true
        filesScroll.autohidesScrollers = true
    }

    /// The change list's heading, matching the stash list's.
    private func changesLabel() -> NSTextField {
        let label = NSTextField(labelWithString: "Changes")
        label.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .semibold)
        label.textColor = .secondaryLabelColor
        return label
    }

    /// Flat list or folder tree, remembered between launches.
    private func buildGroupingControl() {
        Style.groupingControl(groupingControl, showing: groupingStore.grouping)
        groupingControl.target = self
        groupingControl.action = #selector(groupingChanged(_:))
    }

    private var grouping: ChangeGrouping { Style.grouping(of: groupingControl) }

    private func buildStashTable() {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("stash"))
        column.resizingMask = .autoresizingMask
        stashTable.addTableColumn(column)
        stashTable.headerView = nil
        Style.sidebarList(stashTable, rowSize: .small)
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

        // Its own row rather than the sync row's tail: pull and push carry commit counts in
        // their titles, and the sidebar is only 260 points wide.
        buildGroupingControl()
        let listRow = NSStackView(views: [changesLabel(), NSView(), groupingControl])
        listRow.orientation = .horizontal
        listRow.spacing = 6

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
            syncRow, listRow, filesScroll, truncationLabel, stashGroup, messageScroll, actionRow,
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
            listRow.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -2 * inset),
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
        reloadFiles()
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
                    "\(path) is binary "
                        + "(\(ByteFormat.string(oldSize)) → \(ByteFormat.string(newSize)))"
                )
            case let .tooLarge(size, limit):
                diffRows = []
                showDiffPlaceholder(
                    "\(path) is \(ByteFormat.string(size)), "
                        + "over the \(ByteFormat.string(limit)) limit"
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
        reloadFiles()
        diffTable.reloadData()
        refreshSummary()
    }

    /// Rebuilds the rows for the current grouping and puts back what the reload drops:
    /// the open folders and the selected path.
    private func reloadFiles() {
        isReloading = true
        let selected = selectedPaths
        fileNodes = ChangeTree.nodes(entries, grouping: grouping)
        filesOutline.reloadData()
        expandFolders()
        select(paths: selected)
        isReloading = false
    }

    private var selectedPaths: Set<String> {
        var paths: Set<String> = []
        for row in filesOutline.selectedRowIndexes {
            guard let node = filesOutline.item(atRow: row) as? StatusNode else { continue }
            paths.insert(node.path)
        }
        return paths
    }

    private func select(paths: Set<String>) {
        guard !paths.isEmpty else { return }
        var rows = IndexSet()
        for row in 0..<filesOutline.numberOfRows {
            guard let node = filesOutline.item(atRow: row) as? StatusNode,
                paths.contains(node.path)
            else { continue }
            rows.insert(row)
        }
        filesOutline.selectRowIndexes(rows, byExtendingSelection: false)
    }

    /// Folders open by default: a change list is short, and a closed tree hides the files
    /// the tick boxes are for. Only what the user closed stays closed.
    private func expandFolders() {
        var pending = fileNodes
        while let node = pending.popLast() {
            guard node.isFolder, !collapsedFolders.contains(node.path) else { continue }
            filesOutline.expandItem(node)
            pending.append(contentsOf: node.children)
        }
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
        var rows = filesOutline.selectedRowIndexes
        if filesOutline.clickedRow >= 0, !rows.contains(filesOutline.clickedRow) {
            rows = IndexSet(integer: filesOutline.clickedRow)
        }
        // A folder row discards everything under it, which is what its tick box covers too.
        var paths: [String] = []
        for row in rows {
            guard let node = filesOutline.item(atRow: row) as? StatusNode else { continue }
            paths.append(contentsOf: node.filePaths)
        }
        let unique = Array(Set(paths)).sorted()
        guard !unique.isEmpty else { return }
        onDiscard?(unique)
    }

    @objc private func groupingChanged(_ sender: Any?) {
        groupingStore.grouping = grouping
        collapsedFolders.removeAll()
        reloadFiles()
    }

    @objc private func toggleClickedRow(_ sender: Any?) {
        guard let node = filesOutline.item(atRow: filesOutline.clickedRow) as? StatusNode,
            node.isFolder
        else { return }
        if filesOutline.isItemExpanded(node) {
            filesOutline.collapseItem(node)
        } else {
            filesOutline.expandItem(node)
        }
    }

    @objc private func discardHunkClicked(_ sender: Any?) {
        let row = diffTable.clickedRow
        guard let diffPath, diffRows.indices.contains(row),
            case let .header(stamp, _) = diffRows[row]
        else { return }
        onDiscardHunk?(diffPath, stamp)
    }

    /// The box's own next state is ignored: a folder row with some files ticked has three
    /// states to cycle through and only one sensible answer — tick what is not ticked yet.
    @objc private func fileTickToggled(_ sender: NSButton) {
        guard let node = filesOutline.item(atRow: filesOutline.row(for: sender)) as? StatusNode
        else { return }
        onTogglePaths?(node.filePaths, tickState(of: node) != .on)
    }

    /// A folder row is ticked when every file under it is, mixed when only some are.
    private func tickState(of node: StatusNode) -> NSControl.StateValue {
        let ticked = node.filePaths.filter { isTicked?($0) ?? true }.count
        if ticked == 0 { return .off }
        return ticked == node.filePaths.count ? .on : .mixed
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
        tableView === stashTable ? stashes.count : diffRows.count
    }

    func tableView(
        _ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int
    ) -> NSView? {
        if tableView === stashTable {
            guard stashes.indices.contains(row) else { return nil }
            let stash = stashes[row]
            return label("stash@{\(stash.index)}  \(stash.message)", tooltip: stash.oid.hex)
        }
        return diffCell(row: row)
    }

    // MARK: Change list

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        guard let node = item as? StatusNode else { return fileNodes.count }
        return node.children.count
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        guard let node = item as? StatusNode else { return fileNodes[index] }
        return node.children[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        (item as? StatusNode)?.children.isEmpty == false
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any)
        -> NSView?
    {
        guard let node = item as? StatusNode else { return nil }
        let id = NSUserInterfaceItemIdentifier("change")
        let cell =
            outlineView.makeView(withIdentifier: id, owner: self) as? ChangeCellView
            ?? {
                let made = ChangeCellView(target: self, action: #selector(fileTickToggled(_:)))
                made.identifier = id
                return made
            }()
        cell.tick.allowsMixedState = node.isFolder
        cell.tick.state = tickState(of: node)
        cell.label.attributedStringValue = Self.label(for: node)
        cell.toolTip = node.path
        return cell
    }

    func outlineViewItemDidExpand(_ notification: Notification) {
        guard let node = notification.userInfo?["NSObject"] as? StatusNode else { return }
        collapsedFolders.remove(node.path)
    }

    func outlineViewItemDidCollapse(_ notification: Notification) {
        guard let node = notification.userInfo?["NSObject"] as? StatusNode else { return }
        collapsedFolders.insert(node.path)
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !isReloading,
            let node = filesOutline.item(atRow: filesOutline.selectedRow) as? StatusNode,
            let entry = node.entry
        else { return }
        onSelectEntry?(entry)
    }

    /// A change row's label: the status letter, then the path in `.flat` or the name in
    /// `.folders`.
    private static func label(for node: StatusNode) -> NSAttributedString {
        guard let entry = node.entry else { return ChangeRowLabel.folder(name: node.name) }
        return ChangeRowLabel.make(
            letter: entry.status.letter ?? " ", color: StatusPalette.color(for: entry.status),
            name: node.name)
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
        guard !isReloading, notification.object as? NSTableView === stashTable else { return }
        updateStashButtons()
    }
}

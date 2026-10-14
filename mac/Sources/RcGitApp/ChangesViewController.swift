import AppKit
import RcGitKit

/// A label the mouse passes through. The commit box's placeholder sits over the text view and
/// must not take the click that puts the caret there.
private final class PassthroughLabel: NSTextField {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

/// One Changes row, or one hunk header in the diff: a tick box and its label on one baseline.
/// Both are centred on the row by constraint — a checkbox and a label left to size themselves
/// sit at different heights.
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
    NSOutlineViewDataSource, NSOutlineViewDelegate, NSTextViewDelegate, NSMenuItemValidation
{
    var onSelectEntry: ((StatusEntry) -> Void)?
    /// Ticks or unticks whole files: one path from a file row, every path beneath a folder row.
    var onTogglePaths: (([String], Bool) -> Void)?
    var onToggleHunk: ((String, HunkStamp, [HunkStamp]) -> Void)?
    var onCommit: ((String) -> Void)?
    var onStashPush: (() -> Void)?
    var onStashApply: ((StashEntry) -> Void)?
    var onStashPop: ((StashEntry) -> Void)?
    var onStashDrop: ((StashEntry) -> Void)?
    var onDiscard: (([String]) -> Void)?
    var onDiscardHunk: ((String, HunkStamp) -> Void)?
    var onRefresh: (() -> Void)?
    var onFetch: (() -> Void)?
    var onPull: (() -> Void)?
    var onPush: (() -> Void)?
    /// The model's tick state, injected so rendering never duplicates it.
    var rowTick: ((ChangeNode<StatusEntry>) -> RowTick)?
    var tickSummary: (() -> TickSummary)?
    var isHunkTicked: ((String, HunkStamp) -> Bool)?

    private enum DiffRow {
        case header(stamp: HunkStamp, all: [HunkStamp])
        case line(DiffLine)
    }

    private let filesOutline = NSOutlineView()
    private let filesScroll = NSScrollView()
    private let filesPlaceholder = NSTextField(
        wrappingLabelWithString: ChangesState.idle.notice(showingRows: false).placeholder ?? "")
    private let groupingControl = NSSegmentedControl()
    private let truncationLabel = NSTextField(labelWithString: "")
    private let authorLabel = NSTextField(labelWithString: "")
    private let stashTable = NSTableView()
    private let stashScroll = NSScrollView()
    private let stashGroup = NSStackView()
    private let stashPlaceholder = NSTextField(labelWithString: "")
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
    private let diffPlaceholder = NSTextField(wrappingLabelWithString: "Select a file")

    private typealias StatusNode = ChangeNode<StatusEntry>

    private var entries: [StatusEntry] = []
    /// Whether `entries` came from the model's current report; while a scan has not landed,
    /// the rows left from an earlier one stay, but the summary dims and acts on nothing.
    private var listIsCurrent = false
    /// The change list's rows under the current grouping, rebuilt on every status render.
    private var fileNodes: [StatusNode] = []
    /// Folder rows are expanded unless the user closed them; reload rebuilds the nodes, so
    /// the state has to live by row key rather than by row.
    private var collapsedFolders: Set<String> = []
    private let groupingStore = ChangeGroupingStore.worktree()
    private var stashes: [StashEntry] = []
    private var diffRows: [DiffRow] = []
    /// The hunks `diffRows` shows, so a row's hunk is decided by `DiffHunk.owning`.
    private var diffHunks: [DiffHunk] = []
    private var diffPath: String?
    private var isReloading = false
    /// Re-made every time their row shows: the stack gives no width to a view it has not laid
    /// out, and it drops the one made here whenever the row is hidden again.
    private var truncationWidth: NSLayoutConstraint?
    private var stashWidth: NSLayoutConstraint?
    private weak var sectionStack: NSStackView?

    /// The gutter the sidebar section keeps on both sides, the sidebar's own margin.
    private static let sectionInset = Style.margin
    /// What a label adds around its text: 2 pt each side, measured against `fittingSize`.
    private static let labelInset: CGFloat = 4

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
        filesOutline.indentationPerLevel = Style.folderIndent
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
        configureSmallButtons([
            (fetchButton, #selector(fetchClicked(_:))),
            (pullButton, #selector(pullClicked(_:))),
            (pushButton, #selector(pushClicked(_:))),
        ])
        let syncRow = NSStackView(views: [fetchButton, pullButton, pushButton, NSView()])
        syncRow.orientation = .horizontal
        syncRow.spacing = 6

        // Its own row rather than the sync row's tail: pull and push carry commit counts in
        // their titles, and the sidebar is only 260 points wide.
        buildGroupingControl()
        let changesHeading = Style.sectionHeading(NSTextField(labelWithString: "Changes"))
        let listRow = NSStackView(views: [changesHeading, NSView(), groupingControl])
        listRow.orientation = .horizontal
        listRow.spacing = 6

        truncationLabel.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        truncationLabel.textColor = .secondaryLabelColor
        truncationLabel.isHidden = true

        buildStashGroup()
        let actionRow = buildCommitBox()

        let stack = NSStackView(views: [
            syncRow, listRow, filesScroll, truncationLabel, stashGroup, messageScroll, authorLabel,
            actionRow,
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
        filesPlaceholder.alignment = .center
        filesPlaceholder.translatesAutoresizingMaskIntoConstraints = false
        // The list keeps a usable height, but yields before the layout breaks: it shares
        // the sidebar with the ref list now.
        let filesFloor = filesScroll.heightAnchor.constraint(
            greaterThanOrEqualToConstant: LayoutPolicy.changesFilesFloor)
        filesFloor.priority = NSLayoutConstraint.Priority(rawValue: 700)
        let rows = [syncRow, listRow, filesScroll, messageScroll, authorLabel, actionRow]
        NSLayoutConstraint.activate(rows.map { fullWidth($0, in: stack) })
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: pane.topAnchor),
            stack.bottomAnchor.constraint(equalTo: pane.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: pane.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: pane.trailingAnchor),
            filesFloor,
            stashScroll.heightAnchor.constraint(
                equalToConstant: LayoutPolicy.changesStashHeight),
            messageScroll.heightAnchor.constraint(
                equalToConstant: LayoutPolicy.changesMessageHeight),
            filesPlaceholder.centerXAnchor.constraint(equalTo: filesScroll.centerXAnchor),
            filesPlaceholder.centerYAnchor.constraint(equalTo: filesScroll.centerYAnchor),
            filesPlaceholder.widthAnchor.constraint(
                lessThanOrEqualTo: filesScroll.widthAnchor, constant: -2 * Self.sectionInset),
            // The text container's own inset, so the placeholder sits on the caret.
            messagePlaceholder.leadingAnchor.constraint(
                equalTo: messageScroll.leadingAnchor, constant: 5),
            messagePlaceholder.topAnchor.constraint(
                equalTo: messageScroll.topAnchor, constant: 2),
        ])
        show(stashGroup, visible: true, width: &stashWidth)
        return pane
    }

    /// A row of the sidebar section as wide as the stack inside its insets. Not activated.
    private func fullWidth(_ row: NSView, in stack: NSStackView) -> NSLayoutConstraint {
        row.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -2 * Self.sectionInset)
    }

    /// Small rounded buttons sending `action` here, disabled until the first render enables
    /// them.
    private func configureSmallButtons(_ buttons: [(NSButton, Selector)]) {
        for (button, action) in buttons {
            button.target = self
            button.action = action
            button.bezelStyle = .rounded
            button.controlSize = .small
            button.isEnabled = false
        }
    }

    /// The stash list with its heading, empty-list note and buttons, as one group.
    private func buildStashGroup() {
        let stashLabel = Style.sectionHeading(NSTextField(labelWithString: "Stashes"))
        stashScroll.documentView = stashTable
        stashScroll.hasVerticalScroller = true
        stashScroll.autohidesScrollers = true
        // A subview of the scroll view, not the pane, so its constraints travel with the
        // stash group when the stack detaches it.
        stashPlaceholder.textColor = .secondaryLabelColor
        stashPlaceholder.lineBreakMode = .byTruncatingTail
        stashPlaceholder.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        stashPlaceholder.isHidden = true
        stashPlaceholder.translatesAutoresizingMaskIntoConstraints = false
        stashScroll.addSubview(stashPlaceholder)
        NSLayoutConstraint.activate([
            stashPlaceholder.centerXAnchor.constraint(equalTo: stashScroll.centerXAnchor),
            stashPlaceholder.centerYAnchor.constraint(equalTo: stashScroll.centerYAnchor),
            stashPlaceholder.leadingAnchor.constraint(
                greaterThanOrEqualTo: stashScroll.leadingAnchor, constant: Style.margin),
            stashPlaceholder.trailingAnchor.constraint(
                lessThanOrEqualTo: stashScroll.trailingAnchor, constant: -Style.margin),
        ])

        configureSmallButtons([
            (applyButton, #selector(applyClicked(_:))),
            (popButton, #selector(popClicked(_:))),
            (dropButton, #selector(dropClicked(_:))),
        ])
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
    }

    /// The commit message box and its labels; returns the row of summary and buttons.
    private func buildCommitBox() -> NSStackView {
        messageText.isRichText = false
        messageText.isAutomaticQuoteSubstitutionEnabled = false
        messageText.isAutomaticDashSubstitutionEnabled = false
        messageText.isAutomaticTextReplacementEnabled = false
        messageText.isAutomaticSpellingCorrectionEnabled = false
        messageText.allowsUndo = true
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
        authorLabel.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        authorLabel.textColor = .secondaryLabelColor
        authorLabel.lineBreakMode = .byTruncatingMiddle
        authorLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
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
        return actionRow
    }

    private func buildDiffPane() -> NSView {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("diff"))
        column.resizingMask = .autoresizingMask
        diffTable.addTableColumn(column)
        diffTable.headerView = nil
        diffTable.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
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
        diffPlaceholder.alignment = .center
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
            diffScroll.heightAnchor.constraint(
                greaterThanOrEqualToConstant: LayoutPolicy.changesDiffFloor),
            diffPlaceholder.centerXAnchor.constraint(equalTo: pane.centerXAnchor),
            diffPlaceholder.centerYAnchor.constraint(equalTo: pane.centerYAnchor),
            diffPlaceholder.widthAnchor.constraint(
                lessThanOrEqualTo: pane.widthAnchor, constant: -2 * Self.sectionInset),
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
            break
        case let .loaded(report):
            showTruncation(
                report.truncated
                    ? "Scan stopped after \(report.scanned) entries; the list is incomplete."
                    : nil)
        case .failed:
            showTruncation(nil)
        }
        let listed = state.listedEntries
        if let listed {
            entries = listed
        }
        let notice = state.notice(showingRows: !entries.isEmpty)
        filesPlaceholder.isHidden = notice.placeholder == nil
        filesPlaceholder.stringValue = notice.placeholder ?? ""
        if case let .failed(message) = state {
            filesPlaceholder.toolTip = message
        } else {
            filesPlaceholder.toolTip = nil
        }
        listIsCurrent = notice.isCurrent
        if listed != nil {
            reloadFiles()
        }
        refreshSummary()
    }

    func renderDiff(_ state: WorktreeDiffState) {
        diffHunks = []
        diffRows = []
        switch state {
        case .idle:
            diffPath = nil
            showDiffPlaceholder("Select a file")
        case let .loading(path):
            diffPath = path
            showDiffPlaceholder("Loading \(path)…")
        case let .failed(path, message):
            diffPath = path
            showDiffPlaceholder(message)
        case let .loaded(path, diff):
            diffPath = path
            if let placeholder = diff.placeholder(path: path) {
                showDiffPlaceholder(placeholder)
            } else if case let .text(hunks) = diff {
                diffHunks = hunks
                diffRows = Self.diffRows(for: hunks)
                diffPlaceholder.isHidden = true
                diffScroll.isHidden = false
            }
        }
        fitDiffColumn(to: diffHunks)
        diffTable.reloadData()
    }

    /// Each hunk's header row, carrying every hunk's stamp, followed by its lines.
    private static func diffRows(for hunks: [DiffHunk]) -> [DiffRow] {
        let all = hunks.map(\.stamp)
        return hunks.flatMap { hunk in
            [DiffRow.header(stamp: hunk.stamp, all: all)] + hunk.lines.map { DiffRow.line($0) }
        }
    }

    /// Widens the diff column to the longest line, so a line past the pane scrolls into view
    /// instead of clipping; the column still fills the pane when every line is shorter.
    private func fitDiffColumn(to hunks: [DiffHunk]) {
        guard let column = diffTable.tableColumns.first else { return }
        let widest =
            hunks.lazy.flatMap(\.lines)
            .map { DiffRendering.line($0).size().width }
            .max() ?? 0
        column.minWidth = ceil(widest) + Self.labelInset
        diffTable.sizeLastColumnToFit()
    }

    /// Re-renders the sync row: commit counts from the last preflight in the button titles,
    /// enabled state from `availability`.
    func renderSync(status: RemoteStatus?, availability: SyncAvailability) {
        pullButton.title = SyncTitles.pull(status)
        pushButton.title = SyncTitles.push(status)
        fetchButton.isEnabled = availability.canFetch
        pullButton.isEnabled = availability.canPull
        pushButton.isEnabled = availability.canPush
    }

    /// Shows the stash list, or, when the last read failed, its error in place of a list that
    /// is no longer current.
    func renderStashes(_ stashes: [StashEntry], state: StashListState) {
        let selected = selectedStash?.oid
        switch state {
        case .loaded:
            self.stashes = stashes
            stashPlaceholder.isHidden = true
        case let .failed(message):
            self.stashes = []
            stashPlaceholder.stringValue = message
            stashPlaceholder.toolTip = message
            stashPlaceholder.isHidden = false
        }
        isReloading = true
        stashTable.reloadData()
        // Re-select by commit, not row: a push from elsewhere shifts every entry down a row.
        let row = selected.flatMap { oid in self.stashes.firstIndex { $0.oid == oid } }
        stashTable.selectRowIndexes(
            row.map { IndexSet(integer: $0) } ?? [], byExtendingSelection: false)
        isReloading = false
        updateStashButtons()
        show(stashGroup, visible: !self.stashes.isEmpty || state != .loaded, width: &stashWidth)
    }

    /// Re-renders tick checkboxes after the model's tick state moved. The rows are unchanged,
    /// so only the visible ones redraw: a row scrolled into view later reads its tick state
    /// when its view is made, which keeps a click O(visible rows) rather than O(entries).
    func refreshTicks() {
        reloadVisibleRows(of: filesOutline)
        reloadVisibleHunkHeaders()
        refreshSummary()
    }

    /// Only a hunk's header row carries a tick, so its line rows are left as they are.
    private func reloadVisibleHunkHeaders() {
        let rows = diffTable.rows(in: diffTable.visibleRect)
        let headers = IndexSet(
            (rows.location..<NSMaxRange(rows)).filter { row in
                guard diffRows.indices.contains(row), case .header = diffRows[row] else {
                    return false
                }
                return true
            })
        guard !headers.isEmpty else { return }
        diffTable.reloadData(forRowIndexes: headers, columnIndexes: [0])
    }

    private func reloadVisibleRows(of table: NSTableView) {
        let rows = table.rows(in: table.visibleRect)
        guard rows.length > 0 else { return }
        table.reloadData(
            forRowIndexes: IndexSet(integersIn: rows.location..<NSMaxRange(rows)),
            columnIndexes: IndexSet(integersIn: 0..<table.numberOfColumns))
    }

    /// Rebuilds the rows for the current grouping and puts back what the reload drops:
    /// the open folders and the selected rows.
    private func reloadFiles() {
        isReloading = true
        let selected = selectedKeys
        fileNodes = ChangeTree.nodes(entries, grouping: grouping)
        filesOutline.reloadData()
        expandFolders(fileNodes)
        select(keys: selected)
        isReloading = false
    }

    private var selectedKeys: Set<String> {
        var keys: Set<String> = []
        for row in filesOutline.selectedRowIndexes {
            guard let node = filesOutline.item(atRow: row) as? StatusNode else { continue }
            keys.insert(node.key)
        }
        return keys
    }

    private func select(keys: Set<String>) {
        guard !keys.isEmpty else { return }
        var rows = IndexSet()
        for row in 0..<filesOutline.numberOfRows {
            guard let node = filesOutline.item(atRow: row) as? StatusNode,
                keys.contains(node.key)
            else { continue }
            rows.insert(row)
        }
        filesOutline.selectRowIndexes(rows, byExtendingSelection: false)
    }

    /// Folders open by default: a change list is short, and a closed tree hides the files
    /// the tick boxes are for. Only what the user closed stays closed.
    private func expandFolders(_ nodes: [StatusNode]) {
        filesOutline.beginUpdates()
        defer { filesOutline.endUpdates() }
        var pending = nodes
        while let node = pending.popLast() {
            guard node.isFolder, !collapsedFolders.contains(node.key) else { continue }
            filesOutline.expandItem(node)
            pending.append(contentsOf: node.children)
        }
    }

    var commitMessage: String {
        messageText.string
    }

    /// Shows who the next commit or stash is recorded as; `nil` while that is still being read.
    func showAuthor(_ author: CommitAuthor?) {
        authorLabel.stringValue = author?.label ?? ""
        if case let .unreadable(reason) = author {
            authorLabel.toolTip = reason
        } else {
            authorLabel.toolTip = authorLabel.stringValue
        }
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
        summaryLabel.textColor = listIsCurrent ? .secondaryLabelColor : .tertiaryLabelColor
        guard listIsCurrent else {
            commitButton.isEnabled = false
            stashButton.isEnabled = false
            return
        }
        let summary = tickSummary?() ?? TickSummary(text: "", hasSelection: false)
        summaryLabel.stringValue = summary.text
        commitButton.isEnabled = summary.hasSelection
        stashButton.isEnabled = summary.hasSelection
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
        let constraint = fullWidth(row, in: stack)
        constraint.isActive = true
        width = constraint
    }

    private func showDiffPlaceholder(_ message: String) {
        diffPlaceholder.stringValue = message
        diffPlaceholder.toolTip = message
        diffPlaceholder.isHidden = false
        diffScroll.isHidden = true
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
        guard let entry = selectedStash else { return }
        onStashApply?(entry)
    }

    @objc private func popClicked(_ sender: Any?) {
        guard let entry = selectedStash else { return }
        onStashPop?(entry)
    }

    @objc private func dropClicked(_ sender: Any?) {
        guard let entry = selectedStash else { return }
        onStashDrop?(entry)
    }

    @objc private func discardClicked(_ sender: Any?) {
        let selected = filesOutline.selectedRowIndexes.compactMap {
            filesOutline.item(atRow: $0) as? StatusNode
        }
        let clicked = filesOutline.item(atRow: filesOutline.clickedRow) as? StatusNode
        let paths = ChangeTree.discardPaths(selected: selected, clicked: clicked)
        guard !paths.isEmpty else { return }
        onDiscard?(paths)
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

    /// The hunk the diff table's context menu was opened on, from any of its rows.
    private var clickedHunk: DiffHunk? {
        DiffHunk.owning(row: diffTable.clickedRow, in: diffHunks)
    }

    @objc private func discardHunkClicked(_ sender: Any?) {
        guard let diffPath, let hunk = clickedHunk else { return }
        onDiscardHunk?(diffPath, hunk.stamp)
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(discardHunkClicked(_:)) {
            return clickedHunk != nil
        }
        return true
    }

    /// The box's own next state is ignored: a mixed row, a folder with some files ticked or a
    /// file with some hunks ticked, has three states to cycle through and only one sensible
    /// answer — tick what is not ticked yet.
    @objc private func fileTickToggled(_ sender: NSButton) {
        guard let node = filesOutline.item(atRow: filesOutline.row(for: sender)) as? StatusNode,
            let tick = rowTick?(node)
        else { return }
        onTogglePaths?(node.filePaths, tick.state != .on)
    }

    private static func buttonState(_ state: TickState) -> NSControl.StateValue {
        switch state {
        case .off: .off
        case .mixed: .mixed
        case .on: .on
        }
    }

    @objc private func hunkTickToggled(_ sender: NSButton) {
        let row = diffTable.row(for: sender)
        guard let diffPath, diffRows.indices.contains(row),
            case let .header(stamp, all) = diffRows[row]
        else { return }
        onToggleHunk?(diffPath, stamp, all)
    }

    /// The selected entry with the commit it showed, so an action names that exact stash.
    private var selectedStash: StashEntry? {
        let row = stashTable.selectedRow
        return stashes.indices.contains(row) ? stashes[row] : nil
    }

    private func updateStashButtons() {
        let hasSelection = selectedStash != nil
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
        let tick = rowTick?(node) ?? RowTick(state: .off, isEnabled: false)
        cell.tick.allowsMixedState = true
        cell.tick.state = Self.buttonState(tick.state)
        cell.tick.isEnabled = tick.isEnabled
        cell.label.attributedStringValue = Self.label(for: node)
        cell.toolTip = node.path
        return cell
    }

    /// Opening a folder re-opens the subfolders the user left open: AppKit closed them with
    /// it, and nodes rebuilt while it was closed have no expansion state of their own.
    func outlineViewItemDidExpand(_ notification: Notification) {
        guard let node = notification.userInfo?["NSObject"] as? StatusNode else { return }
        collapsedFolders.remove(node.key)
        guard !isReloading else { return }
        isReloading = true
        expandFolders(node.children)
        isReloading = false
    }

    /// Records only the folder the user closed. AppKit also posts a collapse for every open
    /// folder beneath it, after marking the parent closed; those are not the user's choice.
    func outlineViewItemDidCollapse(_ notification: Notification) {
        guard !isReloading,
            let node = notification.userInfo?["NSObject"] as? StatusNode
        else { return }
        if let parent = filesOutline.parent(forItem: node), !filesOutline.isItemExpanded(parent) {
            return
        }
        collapsedFolders.insert(node.key)
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

    /// Reuses row views through `makeView`: a worktree diff can run to 100k rows, and a view
    /// built per row made scrolling one about six times slower.
    private func diffCell(row: Int) -> NSView? {
        guard diffRows.indices.contains(row) else { return nil }
        switch diffRows[row] {
        case let .header(stamp, _):
            let id = NSUserInterfaceItemIdentifier("hunkHeader")
            let cell =
                diffTable.makeView(withIdentifier: id, owner: self) as? ChangeCellView
                ?? {
                    let made = ChangeCellView(
                        target: self, action: #selector(hunkTickToggled(_:)))
                    made.identifier = id
                    return made
                }()
            cell.tick.state =
                (diffPath.flatMap { isHunkTicked?($0, stamp) } ?? true) ? .on : .off
            cell.label.attributedStringValue = DiffRendering.header(
                oldStart: stamp.oldStart, oldLines: stamp.oldLines,
                newStart: stamp.newStart, newLines: stamp.newLines)
            return cell
        case let .line(line):
            let id = NSUserInterfaceItemIdentifier("diffLine")
            let field =
                diffTable.makeView(withIdentifier: id, owner: self) as? NSTextField
                ?? {
                    let made = NSTextField(labelWithString: "")
                    made.lineBreakMode = .byClipping
                    made.identifier = id
                    return made
                }()
            field.attributedStringValue = DiffRendering.line(line)
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

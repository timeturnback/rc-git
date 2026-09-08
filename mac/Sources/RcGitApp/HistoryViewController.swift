import AppKit
import RcGitKit

/// The commit list of the selected ref, with a detail pane: the files one commit changed and
/// the line diff of the picked file. Commits load a page at a time when scrolled near the
/// end; the detail loads on selection, one tree diff and one blob diff at a time.
@MainActor
final class HistoryViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate,
    NSSearchFieldDelegate
{
    var onNeedsMore: (() -> Void)?
    var onBrowseCommit: ((CommitSummary) -> Void)?
    var onSelectCommit: ((CommitSummary) -> Void)?
    var onSelectChange: ((TreeChange) -> Void)?
    var onJumpToCommit: ((Oid) -> Void)?
    var onDetailBack: (() -> Void)?
    var onClearScope: (() -> Void)?
    var onSearchFilterChanged: ((CommitFilter?) -> Void)?
    var onNeedsMoreSearch: (() -> Void)?
    var onJumpToRevision: ((String) -> Void)?

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
    private var graphRows: [GraphRow] = []
    private var isReloading = false

    private let scopeBar = NSStackView()
    private let scopeLabel = NSTextField(labelWithString: "")
    private var scopedPath: String?

    private let searchArea = NSStackView()
    private let searchField = NSSearchField()
    private let searchScopeControl = NSSegmentedControl(
        labels: ["Message", "Author"], trackingMode: .selectOne, target: nil, action: nil)
    private let searchProgress = NSTextField(labelWithString: "")
    private let datesToggle = NSButton()
    private let datesRow = NSStackView()
    private let sinceCheck = NSButton()
    private let sincePicker = NSDatePicker()
    private let untilCheck = NSButton()
    private let untilPicker = NSDatePicker()
    private var searchActive = false
    private var searchResults: [CommitSummary] = []
    private var searchComplete = false

    /// The rows the commit table shows: search matches while a search is active.
    private var displayed: [CommitSummary] { searchActive ? searchResults : commits }

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
        let graphColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("graph"))
        graphColumn.title = ""
        graphColumn.width = GraphCellView.width(lanes: 1)
        graphColumn.resizingMask = []
        graphColumn.isHidden = true
        table.addTableColumn(graphColumn)
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

        buildScopeBar()
        buildSearchArea()

        // A stack with detachesHiddenViews, so the hidden scope bar costs no height and
        // never fights a collapse constraint. The list keeps a minimum height so the split
        // can never crush it to nothing.
        let commitsStack = NSStackView(views: [searchArea, scopeBar, scroll])
        commitsStack.orientation = .vertical
        commitsStack.alignment = .leading
        commitsStack.distribution = .fill
        commitsStack.spacing = 0
        commitsStack.detachesHiddenViews = true
        scroll.setContentHuggingPriority(.init(rawValue: 1), for: .vertical)
        searchArea.setContentHuggingPriority(.defaultHigh, for: .vertical)
        scopeBar.setContentHuggingPriority(.defaultHigh, for: .vertical)

        let commitsPane = NSView()
        commitsStack.translatesAutoresizingMaskIntoConstraints = false
        commitsPane.addSubview(commitsStack)
        commitsPane.addSubview(placeholder)
        NSLayoutConstraint.activate([
            commitsStack.topAnchor.constraint(equalTo: commitsPane.topAnchor),
            commitsStack.bottomAnchor.constraint(equalTo: commitsPane.bottomAnchor),
            commitsStack.leadingAnchor.constraint(equalTo: commitsPane.leadingAnchor),
            commitsStack.trailingAnchor.constraint(equalTo: commitsPane.trailingAnchor),
            searchArea.widthAnchor.constraint(equalTo: commitsStack.widthAnchor),
            scopeBar.widthAnchor.constraint(equalTo: commitsStack.widthAnchor),
            scroll.widthAnchor.constraint(equalTo: commitsStack.widthAnchor),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 150),
            placeholder.centerXAnchor.constraint(equalTo: commitsPane.centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: commitsPane.centerYAnchor),
        ])

        let split = NSSplitView()
        split.isVertical = false
        split.dividerStyle = .thin
        // v2: the pre-fix build autosaved a crushed divider position under the old name.
        split.autosaveName = "rc-git.history.v2"
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
        detailSplit.autosaveName = "rc-git.history-detail.v2"
        changesScroll.setContentHuggingPriority(.defaultHigh, for: .horizontal)
        detailSplit.addArrangedSubview(changesScroll)
        detailSplit.addArrangedSubview(diffScroll)
        detailSplit.setHoldingPriority(NSLayoutConstraint.Priority(rawValue: 251), forSubviewAt: 0)

        buildDetailHeader()

        let stacked = NSStackView(views: [detailHeader, detailSplit])
        stacked.orientation = .vertical
        stacked.alignment = .leading
        stacked.distribution = .fill
        stacked.spacing = 0
        stacked.detachesHiddenViews = true
        detailHeader.setContentHuggingPriority(.defaultHigh, for: .vertical)
        detailSplit.setContentHuggingPriority(.init(rawValue: 1), for: .vertical)

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

    private func buildScopeBar() {
        scopeLabel.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .medium)
        scopeLabel.lineBreakMode = .byTruncatingMiddle
        let clear = NSButton(
            title: "✕", target: self, action: #selector(clearScopeClicked(_:)))
        clear.bezelStyle = .accessoryBarAction
        clear.controlSize = .small
        scopeBar.orientation = .horizontal
        scopeBar.alignment = .centerY
        scopeBar.spacing = 6
        scopeBar.edgeInsets = NSEdgeInsets(top: 4, left: 10, bottom: 4, right: 10)
        scopeBar.addArrangedSubview(scopeLabel)
        scopeBar.addArrangedSubview(clear)
        scopeBar.isHidden = true
    }

    /// Shows or hides the path chip above the commit list.
    func setScope(path: String?) {
        scopedPath = path
        scopeLabel.stringValue = path.map { "History of \($0)" } ?? ""
        scopeBar.isHidden = path == nil
        placeholder.stringValue = path == nil ? "No commits" : "No commits touch this path"
    }

    @objc private func clearScopeClicked(_ sender: Any?) {
        onClearScope?()
    }

    private func buildSearchArea() {
        searchField.placeholderString = "Search commits"
        searchField.delegate = self
        searchField.target = self
        searchField.action = #selector(searchSubmitted(_:))
        searchField.sendsWholeSearchString = false
        searchScopeControl.selectedSegment = 0
        searchScopeControl.target = self
        searchScopeControl.action = #selector(searchOptionsChanged(_:))
        searchProgress.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        searchProgress.textColor = .secondaryLabelColor

        datesToggle.title = "Dates"
        datesToggle.setButtonType(.pushOnPushOff)
        datesToggle.bezelStyle = .accessoryBarAction
        datesToggle.controlSize = .small
        datesToggle.target = self
        datesToggle.action = #selector(datesToggled(_:))

        let fieldRow = NSStackView(views: [
            searchField, searchScopeControl, datesToggle, searchProgress,
        ])
        fieldRow.orientation = .horizontal
        fieldRow.alignment = .centerY
        fieldRow.spacing = 6

        for (check, title) in [(sinceCheck, "Since"), (untilCheck, "Until")] {
            check.title = title
            check.setButtonType(.switch)
            check.controlSize = .small
            check.target = self
            check.action = #selector(searchOptionsChanged(_:))
        }
        for picker in [sincePicker, untilPicker] {
            picker.datePickerStyle = .textFieldAndStepper
            picker.datePickerElements = [.yearMonthDay]
            picker.controlSize = .small
            picker.target = self
            picker.action = #selector(searchOptionsChanged(_:))
        }
        sincePicker.dateValue = Date(timeIntervalSinceNow: -30 * 24 * 3600)
        untilPicker.dateValue = Date()
        datesRow.orientation = .horizontal
        datesRow.alignment = .centerY
        datesRow.spacing = 6
        for view in [sinceCheck, sincePicker, untilCheck, untilPicker] {
            datesRow.addArrangedSubview(view)
        }
        datesRow.isHidden = true

        searchArea.orientation = .vertical
        searchArea.alignment = .leading
        searchArea.spacing = 4
        searchArea.edgeInsets = NSEdgeInsets(top: 6, left: 10, bottom: 6, right: 10)
        searchArea.detachesHiddenViews = true
        searchArea.addArrangedSubview(fieldRow)
        searchArea.addArrangedSubview(datesRow)
        NSLayoutConstraint.activate([
            fieldRow.widthAnchor.constraint(equalTo: searchArea.widthAnchor, constant: -20),
            searchField.widthAnchor.constraint(greaterThanOrEqualToConstant: 160),
        ])
    }

    /// Moves keyboard focus into the search field.
    func focusSearch() {
        view.window?.makeFirstResponder(searchField)
    }

    /// Renders search state: swaps the table to the matches while a search is active.
    func renderSearch(
        active: Bool, results: [CommitSummary], scanned: Int, complete: Bool, searching: Bool
    ) {
        let wasActive = searchActive
        searchActive = active
        searchResults = results
        searchComplete = complete
        if active {
            searchProgress.stringValue =
                searching
                ? "Searched \(scanned) commits…"
                : "\(results.count) matches in \(scanned) commits"
            placeholder.stringValue = "No matching commits"
            placeholder.isHidden = !results.isEmpty || searching
        } else {
            searchProgress.stringValue = ""
            setScope(path: scopedPath)
            placeholder.isHidden = !commits.isEmpty
        }
        isReloading = true
        table.reloadData()
        isReloading = false
        if active != wasActive {
            table.scrollRowToVisible(0)
        }
        requestMoreIfNearEnd()
    }

    private func emitFilter() {
        let query = searchField.stringValue.trimmingCharacters(in: .whitespaces)
        var filter = CommitFilter()
        if !query.isEmpty {
            if searchScopeControl.selectedSegment == 1 {
                filter.author = query
            } else {
                filter.message = query
            }
        }
        if !datesRow.isHidden {
            if sinceCheck.state == .on {
                filter.after = Int64(sincePicker.dateValue.timeIntervalSince1970)
            }
            if untilCheck.state == .on {
                filter.before = Int64(untilPicker.dateValue.timeIntervalSince1970)
            }
        }
        onSearchFilterChanged?(filter.isEmpty ? nil : filter)
    }

    func controlTextDidChange(_ notification: Notification) {
        guard (notification.object as? NSSearchField) === searchField else { return }
        emitFilter()
    }

    @objc private func searchOptionsChanged(_ sender: Any?) {
        emitFilter()
    }

    @objc private func datesToggled(_ sender: Any?) {
        datesRow.isHidden = datesToggle.state != .on
        emitFilter()
    }

    @objc private func searchSubmitted(_ sender: Any?) {
        let query = searchField.stringValue.trimmingCharacters(in: .whitespaces)
        let isHex = query.count >= 4 && query.allSatisfy(\.isHexDigit)
        if isHex {
            onJumpToRevision?(query)
        } else {
            emitFilter()
        }
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
        guard let row = displayed.firstIndex(where: { $0.oid == oid }), table.selectedRow != row
        else { return }
        isReloading = true
        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        table.scrollRowToVisible(row)
        isReloading = false
    }

    /// Updates the graph column alongside `reload`. Empty rows hide the column: a filtered
    /// list has no meaningful topology.
    func renderGraph(_ rows: [GraphRow], maxLanes: Int) {
        graphRows = rows
        guard let column = table.tableColumns.first(where: { $0.identifier.rawValue == "graph" })
        else { return }
        column.isHidden = rows.isEmpty
        if !rows.isEmpty {
            column.width = GraphCellView.width(lanes: maxLanes)
        }
    }

    /// Replaces the rows. Called with the accumulated pages, so a reload keeps the scroll
    /// position: only rows below the previous end are new.
    func reload(_ commits: [CommitSummary], complete: Bool) {
        self.commits = commits
        self.complete = complete
        guard !searchActive else { return }
        let selected = table.selectedRow
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
            diffText.textStorage?.setAttributedString(DiffRendering.attributed(hunks: hunks))
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

    @objc private func boundsChanged(_ notification: Notification) {
        requestMoreIfNearEnd()
    }

    private func requestMoreIfNearEnd() {
        let done = searchActive ? searchComplete : complete
        guard !done, view.window != nil else { return }
        let visible = scroll.contentView.documentVisibleRect
        let height = table.frame.height
        if visible.maxY >= height - Self.prefetchMargin {
            if searchActive {
                onNeedsMoreSearch?()
            } else {
                onNeedsMore?()
            }
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
        guard displayed.indices.contains(actedRow) else { return }
        onBrowseCommit?(displayed[actedRow])
    }

    @objc private func copyHash(_ sender: Any?) {
        guard displayed.indices.contains(actedRow) else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(displayed[actedRow].oid.hex, forType: .string)
    }

    // MARK: NSTableViewDataSource

    func numberOfRows(in tableView: NSTableView) -> Int {
        tableView === table ? displayed.count : changes.count
    }

    // MARK: NSTableViewDelegate

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !isReloading, let tableView = notification.object as? NSTableView else { return }
        if tableView === table {
            guard displayed.indices.contains(table.selectedRow) else { return }
            onSelectCommit?(displayed[table.selectedRow])
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
        if tableColumn.identifier.rawValue == "graph" {
            guard !searchActive, graphRows.indices.contains(row) else { return nil }
            let id = NSUserInterfaceItemIdentifier("graphCell")
            let cell: GraphCellView
            if let reused = table.makeView(withIdentifier: id, owner: self) as? GraphCellView {
                cell = reused
            } else {
                cell = GraphCellView()
                cell.identifier = id
            }
            cell.row = graphRows[row]
            return cell
        }
        guard displayed.indices.contains(row) else { return nil }
        let commit = displayed[row]
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

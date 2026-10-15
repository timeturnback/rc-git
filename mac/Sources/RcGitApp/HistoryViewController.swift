import AppKit
import RcGitKit

/// The commit list of the selected ref. An open commit adds its detail under the list —
/// message, identities, and the files it changed, as a folder tree or a flat list — and the
/// picked file's line diff on the right. Commits load a page at a time when scrolled near the
/// end; the detail loads on selection, one tree diff and one blob diff at a time.
@MainActor
final class HistoryViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate,
    NSOutlineViewDataSource, NSOutlineViewDelegate, NSSearchFieldDelegate, NSSplitViewDelegate
{
    var onNeedsMore: (() -> Void)?
    var onRetryHistory: (() -> Void)?
    var onBrowseCommit: ((CommitSummary) -> Void)?
    var onSelectCommit: ((CommitSummary) -> Void)?
    var onSelectChange: ((TreeChange) -> Void)?
    var onJumpToCommit: ((Oid) -> Void)?
    var onDetailBack: (() -> Void)?
    var onCloseDetail: (() -> Void)?
    var onClearScope: (() -> Void)?
    var onSearchFilterChanged: ((CommitFilter?) -> Void)?
    var onNeedsMoreSearch: (() -> Void)?
    var onJumpToRevision: ((String) -> Void)?

    /// How close to the bottom, in points, the scroll may get before the next page loads.
    private static let prefetchMargin: CGFloat = 600
    /// How long typing in the search field must pause before the query is searched, so a
    /// burst of keystrokes starts one commit scan rather than one per key.
    private static let typingPause: Duration = .milliseconds(250)

    /// The height neither pane of the left column may drop below, from `LayoutPolicy`.
    private static let commitsPaneFloor = LayoutPolicy.historyCommitsFloor
    private static let detailPaneFloor = LayoutPolicy.historyDetailFloor

    private let table = NSTableView()
    private let scroll = NSScrollView()
    private let placeholder = NSTextField(labelWithString: "No commits")
    private typealias ChangeRow = ChangeNode<TreeChange>

    private let changesOutline = NSOutlineView()
    private let changesCount = NSTextField(labelWithString: "")
    private let groupingControl = NSSegmentedControl()
    private let groupingStore = ChangeGroupingStore.commit()
    private let diffText = NSTextView()
    private let diffPath = NSTextField(labelWithString: "")
    private let diffPlaceholder = NSTextField(wrappingLabelWithString: "Select a file")
    private var commits: [CommitSummary] = []
    private var complete = false
    /// Why the last history page failed; automatic paging stops while it is set.
    private var historyError: String?
    private var changes: [TreeChange] = []
    /// The changed files under the current grouping, rebuilt when the commit or the grouping
    /// changes.
    private var changeRows: [ChangeRow] = []
    /// The file whose diff the right pane shows, re-selected after a regrouping.
    private var shownChange: TreeChange?
    /// Bumped by every diff-pane change, so an off-main diff build that lost the race is dropped.
    private var diffGeneration = 0
    private var graphRows: [GraphRow] = []
    private var isReloading = false
    /// The query typing scheduled, sent once typing pauses; any immediate emit replaces it.
    private var pendingTypedFilter: Task<Void, Never>?

    private let scopeBar = NSStackView()
    private let scopeLabel = NSTextField(labelWithString: "")
    private var scopedPath: String?

    private let errorBar = NSStackView()
    private let errorLabel = NSTextField(labelWithString: "")

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
    /// The model's search filter; non-nil while a search is active.
    private var searchFilter: CommitFilter?
    private var searchActive: Bool { searchFilter != nil }
    private var searchResults: [CommitSummary] = []
    private var searchComplete = false
    private var searchFailed = false

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
    private let closeButton = NSButton()
    private var detailPane: NSView?
    private var diffPane: NSView?
    private var columnSplit: NSSplitView?
    private var outerSplit: NSSplitView?
    private var parentOids: [Oid] = []

    private static let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter
    }()

    override func loadView() {
        buildCommitTable()
        let commits = buildCommitsPane()
        let detail = buildDetailPane()
        detail.isHidden = true
        detailPane = detail
        let diff = buildDiffPane()
        diff.isHidden = true
        diffPane = diff
        view = buildSplits(commits: commits, detail: detail, diff: diff)
    }

    /// The commit table, a hidden graph column first, in its scroll view.
    private func buildCommitTable() {
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
        table.doubleAction = #selector(browseDoubleClicked(_:))
        table.menu = buildMenu()

        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(
            self, selector: #selector(boundsChanged(_:)),
            name: NSView.boundsDidChangeNotification, object: scroll.contentView)
    }

    /// The search area, scope bar and error bar over the commit table, with its placeholder.
    private func buildCommitsPane() -> NSView {
        placeholder.textColor = .secondaryLabelColor
        placeholder.translatesAutoresizingMaskIntoConstraints = false

        buildScopeBar()
        buildErrorBar()
        buildSearchArea()

        // A stack with detachesHiddenViews, so the hidden scope bar costs no height and
        // never fights a collapse constraint. Its width floor belongs to the outer split, which
        // places its panes through `layoutOuterPanes`.
        let commitsStack = NSStackView(views: [searchArea, scopeBar, errorBar, scroll])
        commitsStack.orientation = .vertical
        commitsStack.alignment = .leading
        commitsStack.distribution = .fill
        commitsStack.spacing = 0
        commitsStack.detachesHiddenViews = true
        scroll.setContentHuggingPriority(.init(rawValue: 1), for: .vertical)
        searchArea.setContentHuggingPriority(.defaultHigh, for: .vertical)
        scopeBar.setContentHuggingPriority(.defaultHigh, for: .vertical)
        errorBar.setContentHuggingPriority(.defaultHigh, for: .vertical)

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
            errorBar.widthAnchor.constraint(equalTo: commitsStack.widthAnchor),
            scroll.widthAnchor.constraint(equalTo: commitsStack.widthAnchor),
            placeholder.centerXAnchor.constraint(equalTo: commitsPane.centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: commitsPane.centerYAnchor),
        ])
        return commitsPane
    }

    /// The left column of the commit list over the commit's detail, beside the diff.
    private func buildSplits(commits: NSView, detail: NSView, diff: NSView) -> NSSplitView {
        // The commit list fills the tab until a commit opens. Then the left column holds the
        // list over the commit's detail, and the diff of the picked file takes the right.
        // Hidden arranged subviews collapse with their divider, so a closed commit costs no
        // room in either split.
        let column = NSSplitView()
        column.isVertical = false
        column.dividerStyle = .thin
        column.autosaveName = "rc-git.history-column.v1"
        column.delegate = self
        column.addArrangedSubview(commits)
        column.addArrangedSubview(detail)
        columnSplit = column
        NSLayoutConstraint.activate([
            // The floor belongs to the split, not to its panes: the split positions its panes
            // by frame, so a per-pane height constraint only fights the frame it is given and
            // loses silently.
            column.heightAnchor.constraint(
                greaterThanOrEqualToConstant: Self.commitsPaneFloor + Self.detailPaneFloor
                    + column.dividerThickness)
        ])

        let split = NSSplitView()
        split.isVertical = true
        split.dividerStyle = .thin
        // v5: the right pane used to be the whole commit drawer; it is now the diff alone.
        split.autosaveName = "rc-git.history.v5"
        // No width floor on the split: the diff is hidden until a commit opens, and a floor
        // for both panes would widen the bare list's minimum for nothing. `layoutOuterPanes`
        // keeps both panes off zero instead.
        split.delegate = self
        split.addArrangedSubview(column)
        split.addArrangedSubview(diff)
        outerSplit = split
        return split
    }

    /// The open commit's header over the files it changed, for the bottom of the left column.
    private func buildDetailPane() -> NSView {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("change"))
        column.resizingMask = .autoresizingMask
        changesOutline.addTableColumn(column)
        changesOutline.outlineTableColumn = column
        changesOutline.headerView = nil
        changesOutline.rowSizeStyle = .small
        changesOutline.indentationPerLevel = Style.folderIndent
        // No horizontal scroller: the column stays the pane's width and labels truncate.
        changesOutline.autoresizesOutlineColumn = false
        changesOutline.dataSource = self
        changesOutline.delegate = self
        changesOutline.target = self
        changesOutline.doubleAction = #selector(toggleClickedFolder(_:))

        let changesScroll = NSScrollView()
        changesScroll.documentView = changesOutline
        changesScroll.hasVerticalScroller = true
        changesScroll.autohidesScrollers = true

        buildDetailHeader()

        Style.sectionHeading(changesCount)
        Style.groupingControl(groupingControl, showing: groupingStore.grouping)
        groupingControl.target = self
        groupingControl.action = #selector(groupingChanged(_:))
        let filesBar = NSStackView(views: [changesCount, NSView(), groupingControl])
        filesBar.orientation = .horizontal
        filesBar.alignment = .centerY
        filesBar.spacing = Style.rowGap
        filesBar.edgeInsets = NSEdgeInsets(
            top: 0, left: Style.margin, bottom: Style.barPadding / 2, right: Style.margin)

        let stacked = NSStackView(views: [detailHeader, filesBar, changesScroll])
        stacked.orientation = .vertical
        stacked.alignment = .leading
        stacked.distribution = .fill
        stacked.spacing = 0
        stacked.detachesHiddenViews = true
        detailHeader.setContentHuggingPriority(.defaultHigh, for: .vertical)
        filesBar.setContentHuggingPriority(.defaultHigh, for: .vertical)
        changesScroll.setContentHuggingPriority(.init(rawValue: 1), for: .vertical)

        let container = NSView()
        stacked.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(stacked)
        NSLayoutConstraint.activate([
            stacked.topAnchor.constraint(equalTo: container.topAnchor),
            stacked.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            stacked.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            stacked.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            detailHeader.widthAnchor.constraint(equalTo: stacked.widthAnchor),
            filesBar.widthAnchor.constraint(equalTo: stacked.widthAnchor),
            changesScroll.widthAnchor.constraint(equalTo: stacked.widthAnchor),
        ])
        return container
    }

    /// The picked file's path over its line diff, for the right of the tab.
    private func buildDiffPane() -> NSView {
        diffText.isEditable = false
        diffText.isRichText = false
        diffText.usesFindBar = true
        diffText.font = DiffRendering.monoFont
        diffText.layoutManager?.allowsNonContiguousLayout = true
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

        diffPath.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .medium)
        diffPath.lineBreakMode = .byTruncatingHead
        diffPath.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        diffPlaceholder.alignment = .center
        diffPlaceholder.textColor = .secondaryLabelColor

        let container = NSView()
        for view in [diffPath, diffScroll, diffPlaceholder] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(view)
        }
        NSLayoutConstraint.activate([
            diffPath.topAnchor.constraint(equalTo: container.topAnchor, constant: Style.barPadding),
            diffPath.leadingAnchor.constraint(
                equalTo: container.leadingAnchor, constant: Style.margin),
            diffPath.trailingAnchor.constraint(
                equalTo: container.trailingAnchor, constant: -Style.margin),
            diffScroll.topAnchor.constraint(
                equalTo: diffPath.bottomAnchor, constant: Style.barPadding),
            diffScroll.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            diffScroll.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            diffScroll.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            diffPlaceholder.centerXAnchor.constraint(equalTo: diffScroll.centerXAnchor),
            diffPlaceholder.centerYAnchor.constraint(equalTo: diffScroll.centerYAnchor),
            diffPlaceholder.widthAnchor.constraint(
                lessThanOrEqualTo: diffScroll.widthAnchor, constant: -2 * Style.margin),
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

    private func buildErrorBar() {
        errorLabel.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        errorLabel.textColor = .secondaryLabelColor
        errorLabel.lineBreakMode = .byTruncatingTail
        errorLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let retry = NSButton(
            title: "Retry", target: self, action: #selector(retryHistoryClicked(_:)))
        retry.bezelStyle = .accessoryBarAction
        retry.controlSize = .small
        errorBar.orientation = .horizontal
        errorBar.alignment = .centerY
        errorBar.spacing = 6
        errorBar.edgeInsets = NSEdgeInsets(top: 4, left: 10, bottom: 4, right: 10)
        errorBar.addArrangedSubview(errorLabel)
        errorBar.addArrangedSubview(retry)
        errorBar.isHidden = true
    }

    /// Shows the failed history page in place of the list's end, or hides the bar. Hidden
    /// while a search shows its own results.
    private func renderHistoryError() {
        let message = searchActive ? nil : historyError
        errorLabel.stringValue = message.map { "Could not load history: \($0)" } ?? ""
        errorLabel.toolTip = message
        errorBar.isHidden = message == nil
    }

    @objc private func retryHistoryClicked(_ sender: Any?) {
        onRetryHistory?()
    }

    private func buildSearchArea() {
        searchField.placeholderString = "Search commits"
        searchField.delegate = self
        searchField.target = self
        searchField.action = #selector(searchSubmitted(_:))
        searchField.sendsWholeSearchString = true
        searchScopeControl.selectedSegment = 0
        searchScopeControl.target = self
        searchScopeControl.action = #selector(searchOptionsChanged(_:))
        searchProgress.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)
        searchProgress.textColor = .secondaryLabelColor

        // Every control in the bar is regular size, so the field, the scope, and the toggle
        // share one height — and the sidebar's ref filter beside them has it too.
        datesToggle.title = "Dates"
        datesToggle.setButtonType(.pushOnPushOff)
        datesToggle.bezelStyle = .accessoryBar
        datesToggle.target = self
        datesToggle.action = #selector(datesToggled(_:))

        let fieldRow = NSStackView(views: [
            searchField, searchScopeControl, datesToggle, searchProgress,
        ])
        fieldRow.orientation = .horizontal
        fieldRow.alignment = .centerY
        fieldRow.spacing = 6

        // The filter compares committer time, as `git log --since/--until` does, so the labels
        // name the date they filter on and the Date column shows it while a date bound is set.
        for (check, title) in [(sinceCheck, "Committed since"), (untilCheck, "Committed until")] {
            check.title = title
            check.toolTip =
                "Filters on the commit date. While a date bound is set, the Date column shows "
                + "the commit date too; otherwise it shows the author date."
            check.setButtonType(.switch)
            check.target = self
            check.action = #selector(searchOptionsChanged(_:))
        }
        for picker in [sincePicker, untilPicker] {
            picker.datePickerStyle = .textFieldAndStepper
            picker.datePickerElements = [.yearMonthDay]
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
        searchArea.spacing = 6
        searchArea.edgeInsets = NSEdgeInsets(
            top: Style.barPadding, left: Style.margin, bottom: Style.barPadding,
            right: Style.margin)
        searchArea.detachesHiddenViews = true
        searchArea.addArrangedSubview(fieldRow)
        searchArea.addArrangedSubview(datesRow)
        NSLayoutConstraint.activate([
            fieldRow.widthAnchor.constraint(
                equalTo: searchArea.widthAnchor, constant: -2 * Style.margin),
            searchField.widthAnchor.constraint(greaterThanOrEqualToConstant: 160),
        ])
    }

    /// Moves keyboard focus into the search field.
    func focusSearch() {
        view.window?.makeFirstResponder(searchField)
    }

    /// Renders search state: swaps the table to the matches while a search is active.
    /// A non-nil `error` means the search stopped on a failure: the count gives way to the
    /// message, and no further page is asked for.
    func renderSearch(
        filter: CommitFilter?, results: [CommitSummary], scanned: Int, complete: Bool,
        searching: Bool, error: String?
    ) {
        let wasActive = searchActive
        searchFilter = filter
        let active = searchActive
        searchResults = results
        searchComplete = complete
        searchFailed = error != nil
        if active {
            if let error {
                searchProgress.stringValue = "Search stopped: \(error)"
            } else {
                searchProgress.stringValue =
                    searching
                    ? "Searched \(scanned) commits…"
                    : "\(results.count) matches in \(scanned) commits"
            }
            searchProgress.toolTip = error
            placeholder.stringValue = "No matching commits"
            placeholder.isHidden = !results.isEmpty || searching || error != nil
        } else {
            searchProgress.stringValue = ""
            searchProgress.toolTip = nil
            clearDroppedSearchControls()
            setScope(path: scopedPath)
            placeholder.isHidden = !commits.isEmpty || !complete || historyError != nil
        }
        renderHistoryError()
        isReloading = true
        table.reloadData()
        isReloading = false
        if active != wasActive {
            table.scrollRowToVisible(0)
        }
        requestMoreIfNearEnd()
    }

    private var searchScope: CommitSearchScope {
        searchScopeControl.selectedSegment == 1 ? .author : .message
    }

    /// The model dropped the search (a ref pick, a moved tip, a path scope) while the
    /// controls still hold one: clear them, so the field never shows a query over the
    /// unfiltered list.
    private func clearDroppedSearchControls() {
        cancelPendingTypedFilter()
        guard !controlsFilter().isEmpty else { return }
        searchField.stringValue = ""
        datesToggle.state = .off
        datesRow.isHidden = true
    }

    private func emitFilter() {
        cancelPendingTypedFilter()
        let filter = controlsFilter()
        onSearchFilterChanged?(filter.isEmpty ? nil : filter)
    }

    /// The filter the search controls describe right now.
    private func controlsFilter() -> CommitFilter {
        let query = searchField.stringValue.trimmingCharacters(in: .whitespaces)
        var filter = CommitFilter()
        if !query.isEmpty {
            switch searchScope {
            case .author: filter.author = query
            case .message: filter.message = query
            }
        }
        if !datesRow.isHidden {
            let bounds = CommitFilter.dayBounds(
                since: sinceCheck.state == .on ? sincePicker.dateValue : nil,
                until: untilCheck.state == .on ? untilPicker.dateValue : nil,
                calendar: .current)
            filter.after = bounds.after
            filter.before = bounds.before
        }
        return filter
    }

    /// Emits the filter once typing pauses for `typingPause`; another keystroke restarts it.
    private func emitFilterAfterTypingPause() {
        cancelPendingTypedFilter()
        pendingTypedFilter = Task { [weak self] in
            do {
                try await Task.sleep(for: Self.typingPause)
            } catch {
                // Sleep throws only on cancellation: a newer keystroke or emit replaced this one.
                return
            }
            self?.emitFilter()
        }
    }

    private func cancelPendingTypedFilter() {
        pendingTypedFilter?.cancel()
        pendingTypedFilter = nil
    }

    func controlTextDidChange(_ notification: Notification) {
        guard (notification.object as? NSSearchField) === searchField else { return }
        emitFilterAfterTypingPause()
    }

    @objc private func searchOptionsChanged(_ sender: Any?) {
        emitFilter()
    }

    @objc private func datesToggled(_ sender: Any?) {
        datesRow.isHidden = datesToggle.state != .on
        if datesToggle.state == .on {
            untilPicker.dateValue = Date()
        }
        emitFilter()
    }

    @objc private func searchSubmitted(_ sender: Any?) {
        if let revision = CommitSearchQuery.revision(searchField.stringValue, scope: searchScope) {
            onJumpToRevision?(revision)
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
        // The body shares the column with the file list under it.
        detailBody.maximumNumberOfLines = 6
        detailBody.isSelectable = true

        backButton.title = "‹ Back"
        backButton.bezelStyle = .accessoryBarAction
        backButton.controlSize = .small
        backButton.target = self
        backButton.action = #selector(detailBackClicked(_:))

        closeButton.title = "✕"
        closeButton.bezelStyle = .accessoryBarAction
        closeButton.controlSize = .small
        closeButton.target = self
        closeButton.action = #selector(closeDetailClicked(_:))
        closeButton.toolTip = "Close commit"

        let titleRow = NSStackView(views: [detailTitle, closeButton])
        titleRow.orientation = .horizontal
        titleRow.alignment = .top
        titleRow.spacing = 6
        detailTitle.setContentHuggingPriority(.init(rawValue: 1), for: .horizontal)

        parentsRow.orientation = .horizontal
        parentsRow.spacing = 6
        parentsRow.alignment = .centerY

        detailHeader.orientation = .vertical
        detailHeader.alignment = .leading
        detailHeader.spacing = 3
        detailHeader.edgeInsets = NSEdgeInsets(top: 8, left: 10, bottom: 8, right: 10)
        detailHeader.detachesHiddenViews = true
        for view in [titleRow, detailRefs, detailAuthor, detailCommitter, detailBody] {
            detailHeader.addArrangedSubview(view)
        }
        detailHeader.addArrangedSubview(parentsRow)
        titleRow.widthAnchor.constraint(equalTo: detailHeader.widthAnchor, constant: -20)
            .isActive = true
        detailHeader.isHidden = true
    }

    /// Renders the detail header: message, refs, identities, and parent navigation. Any
    /// state but `.empty` opens the detail and the diff pane; `.empty` closes both.
    func renderCommitDetail(_ state: CommitDetailState, canGoBack: Bool) {
        setCommitPanesOpen(state != .empty)
        switch state {
        case .empty:
            detailHeader.isHidden = true
            changesCount.stringValue = ""
        case let .loading(summary):
            detailHeader.isHidden = false
            fillHeader(summary: summary, detail: nil, refs: [], canGoBack: canGoBack)
            changesCount.stringValue = "Loading changes…"
        case let .loaded(detail, refs):
            detailHeader.isHidden = false
            fillHeader(summary: detail.summary, detail: detail, refs: refs, canGoBack: canGoBack)
            changesCount.stringValue = Self.countLine(changes.count)
        case let .failed(summary, message):
            detailHeader.isHidden = false
            fillHeader(summary: summary, detail: nil, refs: [], canGoBack: canGoBack)
            // The body is the header's wrapping label; the error names its object at the end.
            detailBody.stringValue = message
            detailBody.toolTip = message
            detailBody.isHidden = false
            changesCount.stringValue = ""
        }
    }

    private static func countLine(_ count: Int) -> String {
        switch count {
        case 0: "No files changed"
        case 1: "1 file changed"
        default: "\(count) files changed"
        }
    }

    /// Shows or hides the detail under the list and the diff beside it, then lays the column
    /// out at once: a split does not ask its delegate when an arranged subview unhides.
    private func setCommitPanesOpen(_ open: Bool) {
        guard detailPane?.isHidden == open else { return }
        detailPane?.isHidden = !open
        diffPane?.isHidden = !open
        layoutOuterPanes()
        layoutColumnPanes()
        view.needsLayout = true
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
        detailBody.toolTip = nil
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
                button.font = DiffRendering.monoFont
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

    @objc private func closeDetailClicked(_ sender: Any?) {
        table.deselectAll(nil)
        onCloseDetail?()
    }

    /// Highlights the row of a commit reached by a parent link or back jump, when loaded. A
    /// commit with no row here (not a search match, not paged in yet) drops the highlight, so
    /// the previously clicked row can be picked again.
    func selectCommitRow(_ oid: Oid) {
        guard let row = displayed.firstIndex(where: { $0.oid == oid }) else {
            deselectCommitRow()
            return
        }
        guard table.selectedRow != row else { return }
        isReloading = true
        table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        table.scrollRowToVisible(row)
        isReloading = false
    }

    /// Drops the row highlight when the detail closed without a click here — a step back to
    /// before any commit was picked — so the same row can be picked again.
    func deselectCommitRow() {
        guard table.selectedRow >= 0 else { return }
        isReloading = true
        table.deselectAll(nil)
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
    /// A non-nil `error` means the last page failed: the error bar offers a retry and no
    /// further page is asked for until then.
    func reload(_ commits: [CommitSummary], complete: Bool, error: String?) {
        self.commits = commits
        self.complete = complete
        historyError = error
        guard !searchActive else { return }
        renderHistoryError()
        let selected = table.selectedRow
        placeholder.isHidden = !commits.isEmpty || !complete || error != nil
        isReloading = true
        table.reloadData()
        if selected >= 0, selected < commits.count {
            table.selectRowIndexes(IndexSet(integer: selected), byExtendingSelection: false)
        }
        isReloading = false
        requestMoreIfNearEnd()
    }

    /// Renders the commit detail: the changed-file list and the picked file's diff. `detail`
    /// words the placeholder while no file diff is shown.
    func renderDetail(changes: [TreeChange], state: FileDiffState, detail: CommitDetailState) {
        isReloading = true
        if changes != self.changes {
            self.changes = changes
            reloadChanges()
        }
        switch state {
        case .empty:
            shownChange = nil
            changesOutline.deselectAll(nil)
            showDiffPath(nil)
            showDiffPlaceholder(detail.emptyDiffPlaceholder(changeCount: changes.count))
        case let .loading(change):
            showChange(change)
            showDiffPlaceholder("Loading…")
        case let .loaded(change, diff):
            showChange(change)
            renderDiff(change: change, diff: diff)
        case let .submodule(change):
            showChange(change)
            showDiffPlaceholder(FileDiffState.submoduleSummary(change))
        case let .failed(change, message):
            showChange(change)
            showDiffPlaceholder(message)
        }
        isReloading = false
    }

    /// Rebuilds the file rows for the current grouping with every folder open.
    private func reloadChanges() {
        changeRows = ChangeTree.nodes(changes, grouping: Style.grouping(of: groupingControl))
        changesOutline.reloadData()
        changesOutline.expandItem(nil, expandChildren: true)
    }

    private func showChange(_ change: TreeChange) {
        shownChange = change
        selectChangeRow(change)
        showDiffPath(change)
    }

    private func selectChangeRow(_ change: TreeChange) {
        for row in 0..<changesOutline.numberOfRows {
            guard (changesOutline.item(atRow: row) as? ChangeRow)?.entry == change else {
                continue
            }
            changesOutline.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            changesOutline.scrollRowToVisible(row)
            return
        }
    }

    private func showDiffPath(_ change: TreeChange?) {
        guard let change else {
            diffPath.stringValue = ""
            diffPath.toolTip = nil
            return
        }
        diffPath.stringValue = change.sourcePath.map { "\($0) → \(change.path)" } ?? change.path
        diffPath.toolTip = diffPath.stringValue
    }

    private func showDiffPlaceholder(_ message: String) {
        diffGeneration += 1
        diffPlaceholder.stringValue = message
        diffPlaceholder.toolTip = message
        diffPlaceholder.isHidden = false
        diffText.textStorage?.setAttributedString(NSAttributedString(string: ""))
    }

    @objc private func groupingChanged(_ sender: Any?) {
        groupingStore.grouping = Style.grouping(of: groupingControl)
        isReloading = true
        reloadChanges()
        if let shownChange {
            selectChangeRow(shownChange)
        }
        isReloading = false
    }

    @objc private func toggleClickedFolder(_ sender: Any?) {
        guard let row = changesOutline.item(atRow: changesOutline.clickedRow) as? ChangeRow,
            row.isFolder
        else { return }
        if changesOutline.isItemExpanded(row) {
            changesOutline.collapseItem(row)
        } else {
            changesOutline.expandItem(row)
        }
    }

    /// Shows a loaded diff. The attributed string is built off the main actor, since a
    /// regenerated lockfile runs to hundreds of thousands of lines; a stale build is dropped by
    /// generation.
    private func renderDiff(change: TreeChange, diff: BlobDiff) {
        if let placeholder = diff.placeholder(path: change.path) {
            showDiffPlaceholder(placeholder)
            return
        }
        guard case let .text(hunks) = diff else { return }
        diffGeneration += 1
        let generation = diffGeneration
        Task { [weak self] in
            let built = await Task.detached(priority: .userInitiated) {
                DiffRendering.attributed(hunks: hunks)
            }.value
            guard let self, generation == self.diffGeneration else { return }
            self.diffPlaceholder.isHidden = true
            self.diffText.textStorage?.setAttributedString(built.text)
            self.diffText.scroll(.zero)
        }
    }

    /// Re-clamps both splits after every layout pass, which is where a divider position
    /// restored from the autosave first reaches the screen. Both layouts keep a legal position
    /// untouched, so this converges on the first pass.
    override func viewDidLayout() {
        super.viewDidLayout()
        layoutOuterPanes()
        layoutColumnPanes()
    }

    // MARK: NSSplitViewDelegate

    /// Lays the left column's two panes out. `NSSplitView` sizes arranged subviews by frame
    /// and asks the coordinate clamps below only while the user drags the divider — never from
    /// `adjustSubviews`, and never for the position the autosave restores. Owning the resize
    /// is what puts a zero-height pane out of reach; the clamps alone did not.
    func splitView(_ splitView: NSSplitView, resizeSubviewsWithOldSize oldSize: NSSize) {
        if splitView === columnSplit {
            layoutColumnPanes()
        } else if splitView === outerSplit {
            layoutOuterPanes()
        } else {
            splitView.adjustSubviews()
        }
    }

    /// Lays the outer split out: the left column alone while no commit is open, else the
    /// column and the diff side by side, each kept off zero by `LayoutPolicy.historyListWidth`.
    private func layoutOuterPanes() {
        guard let split = outerSplit, let column = columnSplit, let diff = diffPane,
            split.bounds.height > 0
        else { return }
        guard !diff.isHidden else {
            placeAcross(column, x: 0, width: split.bounds.width, in: split)
            return
        }
        let divider = split.dividerThickness
        let available = split.bounds.width - divider
        guard available > 0 else { return }
        let columnWidth = LayoutPolicy.historyListWidth(
            available: available, diffWidth: diff.frame.width)
        placeAcross(column, x: 0, width: columnWidth, in: split)
        placeAcross(diff, x: columnWidth + divider, width: available - columnWidth, in: split)
    }

    private func placeAcross(_ view: NSView, x: CGFloat, width: CGFloat, in split: NSSplitView) {
        let frame = NSRect(x: x, y: 0, width: width, height: split.bounds.height)
        guard view.frame != frame else { return }
        view.frame = frame
    }

    private func layoutColumnPanes() {
        guard let split = columnSplit, let commits = split.arrangedSubviews.first,
            let detail = detailPane, split.bounds.width > 0
        else { return }
        guard !detail.isHidden else {
            place(commits, y: 0, height: split.bounds.height, in: split)
            return
        }
        let divider = split.dividerThickness
        let available = split.bounds.height - divider
        guard available > 0 else { return }
        let commitsHeight = LayoutPolicy.historyCommitsHeight(
            available: available, detailHeight: detail.frame.height)
        let detailHeight = available - commitsHeight
        // A split with horizontal dividers is flipped, so the commit list starts at the top.
        let commitsY = split.isFlipped ? 0 : detailHeight + divider
        let detailY = split.isFlipped ? commitsHeight + divider : 0
        place(commits, y: commitsY, height: commitsHeight, in: split)
        place(detail, y: detailY, height: detailHeight, in: split)
    }

    private func place(_ view: NSView, y: CGFloat, height: CGFloat, in split: NSSplitView) {
        let frame = NSRect(x: 0, y: y, width: split.bounds.width, height: height)
        guard view.frame != frame else { return }
        view.frame = frame
    }

    /// Keeps a divider drag inside the two floors, so a drag cannot end where
    /// `layoutOuterPanes` or `layoutColumnPanes` would have to pull it back from. Drags are all
    /// these two methods govern: AppKit asks them nothing about a resize or about the position
    /// it restores.
    func splitView(
        _ splitView: NSSplitView, constrainMinCoordinate proposedMinimumPosition: CGFloat,
        ofSubviewAt dividerIndex: Int
    ) -> CGFloat {
        if splitView === outerSplit {
            return max(proposedMinimumPosition, LayoutPolicy.historyListMinWidth)
        }
        guard splitView === columnSplit else { return proposedMinimumPosition }
        return max(proposedMinimumPosition, Self.commitsPaneFloor)
    }

    func splitView(
        _ splitView: NSSplitView, constrainMaxCoordinate proposedMaximumPosition: CGFloat,
        ofSubviewAt dividerIndex: Int
    ) -> CGFloat {
        if splitView === outerSplit {
            let lastOpen =
                splitView.bounds.width - splitView.dividerThickness
                - LayoutPolicy.historyDiffMinWidth
            return min(proposedMaximumPosition, max(LayoutPolicy.historyListMinWidth, lastOpen))
        }
        guard splitView === columnSplit else { return proposedMaximumPosition }
        let lastOpen =
            splitView.bounds.height - splitView.dividerThickness - Self.detailPaneFloor
        return min(proposedMaximumPosition, max(Self.commitsPaneFloor, lastOpen))
    }

    @objc private func boundsChanged(_ notification: Notification) {
        requestMoreIfNearEnd()
    }

    private func requestMoreIfNearEnd() {
        let done =
            searchActive ? searchComplete || searchFailed : complete || historyError != nil
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

    @objc private func browseDoubleClicked(_ sender: Any?) {
        let row = table.clickedRow
        guard row >= 0, displayed.indices.contains(row) else { return }
        onBrowseCommit?(displayed[row])
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
        displayed.count
    }

    // MARK: NSTableViewDelegate

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !isReloading, notification.object as? NSTableView === table,
            displayed.indices.contains(table.selectedRow)
        else { return }
        onSelectCommit?(displayed[table.selectedRow])
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int)
        -> NSView?
    {
        guard let tableColumn else { return nil }
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
            cell.textField?.attributedStringValue = CellText.make(
                commit.oid.short, font: DiffRendering.monoFont, color: .secondaryLabelColor)
        case "title":
            cell.textField?.attributedStringValue = CellText.make(commit.title)
            cell.toolTip = commit.title
        case "author":
            cell.textField?.attributedStringValue = CellText.make(commit.authorName)
            cell.toolTip = "\(commit.authorName) <\(commit.authorEmail)>"
        default:
            let time = commit.listedTime(filter: searchFilter)
            let date = Date(timeIntervalSince1970: TimeInterval(time))
            cell.textField?.attributedStringValue = CellText.make(
                Self.dateFormatter.string(from: date))
        }
        return cell
    }

    // MARK: Changed files

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        guard let row = item as? ChangeRow else { return changeRows.count }
        return row.children.count
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        guard let row = item as? ChangeRow else { return changeRows[index] }
        return row.children[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        (item as? ChangeRow)?.children.isEmpty == false
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any)
        -> NSView?
    {
        guard let row = item as? ChangeRow, let tableColumn else { return nil }
        let cell = reusableCell(in: outlineView, for: tableColumn.identifier)
        cell.textField?.attributedStringValue = Self.label(for: row)
        cell.toolTip = row.entry?.sourcePath.map { "\($0) → \(row.path)" } ?? row.path
        return cell
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !isReloading,
            let row = changesOutline.item(atRow: changesOutline.selectedRow) as? ChangeRow,
            let change = row.entry
        else { return }
        onSelectChange?(change)
    }

    /// A changed file's label: the change letter in its color, then the whole path when flat
    /// or the name in a folder tree. Folder rows are the name alone.
    private static func label(for row: ChangeRow) -> NSAttributedString {
        guard let change = row.entry else { return ChangeRowLabel.folder(name: row.name) }
        return ChangeRowLabel.make(
            letter: change.kind.letter, color: StatusPalette.color(for: change.kind),
            name: row.name)
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

import AppKit
import RcGitKit

/// One browser window: a sidebar carrying the refs and the mode's navigation — the directory
/// tree, or the change list and commit box — beside the detail pane. Owns a `BrowserModel`
/// and applies `LayoutPolicy` as the window changes size.
@MainActor
final class MainWindowController: NSWindowController, NSWindowDelegate, NSToolbarDelegate,
    NSMenuItemValidation
{
    let model = BrowserModel()
    var onClose: (() -> Void)?
    private(set) var isEmpty = true

    /// Which pane fills the content area beside the sidebar.
    enum ContentMode: Int {
        case files = 0
        case history = 1
        case changes = 2
    }

    private let outerSplit = NSSplitViewController()
    private let contentTabs = NSTabViewController()
    private let sidebarController = SidebarViewController()
    private let treeController = TreeViewController()
    private let fileController = FileViewController()
    private let historyController = HistoryViewController()
    private let changesController = ChangesViewController()
    private var changesModel: ChangesModel?
    private var syncModel: SyncModel?
    private var syncButton: NSButton?
    private var syncSpinner: NSProgressIndicator?
    private var syncToolbarItem: NSToolbarItem?
    private let goToFileController = GoToFileController()
    private let blamePanel = BlamePanelController()
    private var refWatcher: RefWatcher?
    private var worktreeWatcher: WorktreeWatcher?
    /// Set when the working tree changed while another tab was frontmost; the switch to
    /// the Changes tab pays the rescan then.
    private var changesDirty = false
    private var modeControl: NSSegmentedControl?
    private var currentLayout: Layout?
    private var sidebarCollapsedByPolicy = false
    /// Set once the user drags the sidebar divider; the automatic fit stands back after that.
    private static let sidebarWidthPinnedKey = "rc-git.sidebar-width-pinned"
    private var repositoryName = ""
    private(set) var repositoryPath: String?
    private var hasWorkingTree = false
    /// The Changes view's list and commit box, hosted in the sidebar.
    private var changesSection: NSView?

    /// The ref list inside the sidebar; the sidebar stacks a mode's section under it.
    private var refsController: RefsViewController { sidebarController.refs }

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1120, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.minSize = NSSize(width: 440, height: 320)
        window.title = "rc-git"
        // Every repository lives in its own tab of one window group. A tab that is not
        // selected keeps its whole view hierarchy, model, and watchers, so switching
        // tabs reorders windows and reads nothing from any repository.
        window.tabbingMode = .preferred
        window.tabbingIdentifier = "rc-git.browser"
        window.toolbarStyle = .unified
        window.titlebarAppearsTransparent = false
        super.init(window: window)
        window.delegate = self
        // Views must exist before the frame restore below: restoring resizes the window,
        // which fires windowDidResize and runs applyLayout.
        buildSplitViews()
        buildToolbar()
        buildVersionBadge()
        wireModel()
        window.center()
        window.setFrameAutosaveName("rc-git.window")
        // A frame saved on a display that is now detached restores fully off-screen.
        if !NSScreen.screens.contains(where: { $0.visibleFrame.intersects(window.frame) }) {
            window.center()
        }
        applyLayout()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { nil }

    /// Attaches a repository and starts loading. The window title becomes the repository
    /// name, and a watcher starts re-reading refs whenever they change on disk.
    func show(_ repository: Repository, path: String) {
        isEmpty = false
        repositoryName = (path as NSString).lastPathComponent
        window?.title = repositoryName
        window?.representedURL = URL(fileURLWithPath: path)
        repositoryPath = path
        hasWorkingTree = repository.workingDirectory != nil
        showWindow(nil)
        refWatcher?.stop()
        let watcher = RefWatcher(gitDir: repository.gitDirectory) { [weak self] in
            Task { @MainActor [weak self] in
                await self?.model.refreshRefs()
                // A commit or stash from the terminal moves refs and the change list both.
                self?.worktreeDidChange()
                await self?.syncModel?.refreshStatus()
            }
        }
        watcher.start()
        refWatcher = watcher

        let changes = ChangesModel(repository: repository)
        changesModel = changes
        wireChanges(changes)
        let sync = SyncModel(
            repoPath: path, performer: GuardSyncPerformer(), store: KeychainCredentialStore())
        syncModel = sync
        wireSync(sync)
        Task { await sync.refreshStatus() }
        changesDirty = true
        worktreeWatcher?.stop()
        worktreeWatcher = nil
        if let workDir = repository.workingDirectory {
            let treeWatcher = WorktreeWatcher(
                workDir: workDir, gitDir: repository.gitDirectory
            ) { [weak self] in
                Task { @MainActor [weak self] in
                    self?.worktreeDidChange()
                }
            }
            treeWatcher.start()
            worktreeWatcher = treeWatcher
        }
        Task { await model.attach(repository) }
    }

    /// The working tree moved: rescan now when the Changes tab is frontmost, otherwise
    /// remember to rescan on the next switch to it.
    private func worktreeDidChange() {
        guard let changesModel else { return }
        if contentTabs.selectedTabViewItemIndex == ContentMode.changes.rawValue {
            Task { await changesModel.refresh() }
        } else {
            changesDirty = true
        }
    }

    /// Shows the file pane, the commit history, or the working-tree changes, and puts the
    /// section that belongs to the mode in the sidebar.
    func setMode(_ mode: ContentMode) {
        contentTabs.selectedTabViewItemIndex = mode.rawValue
        modeControl?.selectedSegment = mode.rawValue
        switch mode {
        case .files: sidebarController.show(section: treeController.view)
        case .history: sidebarController.show(section: nil)
        case .changes: sidebarController.show(section: changesSection)
        }
        if mode == .history, model.history.isEmpty {
            Task { await model.loadMoreHistory() }
        }
        if mode == .changes, let changesModel {
            let needsLoad = changesDirty || changesModel.statusState == .idle
            changesDirty = false
            if needsLoad {
                Task { await changesModel.refresh() }
            }
            // The sync row shows commit counts; re-read them on every switch to the tab.
            Task { await syncModel?.refreshStatus() }
        } else {
            // Nothing is watching the change list any more, and the watcher will mark the tab
            // dirty for the next switch back, so a scan still running is work for no one.
            changesModel?.cancelScans()
        }
    }

    @objc func showFiles(_ sender: Any?) {
        setMode(.files)
    }

    @objc func showHistory(_ sender: Any?) {
        setMode(.history)
    }

    @objc func showChanges(_ sender: Any?) {
        setMode(.changes)
    }

    @objc func goToFile(_ sender: Any?) {
        guard !isEmpty else { return }
        goToFileController.show(over: window)
    }

    @objc private func modeChanged(_ sender: NSSegmentedControl) {
        setMode(ContentMode(rawValue: sender.selectedSegment) ?? .files)
    }

    @objc func showBlame(_ sender: Any?) {
        guard model.openedFilePath != nil else { return }
        let name = model.openedFilePath ?? ""
        Task { [weak self] in
            guard let self else { return }
            do {
                let spans = try await model.blameOpenedFile()
                blamePanel.show(spans, fileName: name, over: window)
            } catch let error as RepositoryError {
                presentError(error.message)
            } catch {
                presentError(String(describing: error))
            }
        }
    }

    @objc func searchCommits(_ sender: Any?) {
        guard !isEmpty else { return }
        setMode(.history)
        historyController.focusSearch()
    }

    @objc func showFileHistory(_ sender: Any?) {
        guard let path = model.openedFilePath else { return }
        setMode(.history)
        Task { [weak self] in
            await self?.model.showPathHistory(path)
        }
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(showBlame(_:)) {
            return model.openedFilePath != nil
        }
        if menuItem.action == #selector(showFileHistory(_:)) {
            return model.openedFilePath != nil
        }
        if menuItem.action == #selector(searchCommits(_:)) {
            return !isEmpty
        }
        if menuItem.action == #selector(goToFile(_:)) {
            return !isEmpty
        }
        let syncSelectors = [
            #selector(fetchRemote(_:)), #selector(pullRemote(_:)),
            #selector(pushRemote(_:)), #selector(syncRepository(_:)),
        ]
        if menuItem.action == #selector(cancelSync(_:)) {
            return syncModel?.isCancellable ?? false
        }
        if let action = menuItem.action, syncSelectors.contains(action) {
            guard repositoryPath != nil, let syncModel, !syncModel.isBusy else { return false }
            let status = syncModel.remoteStatus
            if action == #selector(pullRemote(_:)) {
                menuItem.title = SyncTitles.pull(status)
            }
            if action == #selector(pushRemote(_:)) {
                menuItem.title = SyncTitles.push(status)
            }
            if action == #selector(fetchRemote(_:)) {
                return true
            }
            return status?.hasUpstream != false
        }
        return true
    }

    // MARK: Layout

    func windowDidResize(_ notification: Notification) {
        applyLayout()
    }

    func windowWillClose(_ notification: Notification) {
        NotificationCenter.default.removeObserver(
            self, name: NSSplitView.didResizeSubviewsNotification,
            object: outerSplit.splitView)
        refWatcher?.stop()
        refWatcher = nil
        worktreeWatcher?.stop()
        worktreeWatcher = nil
        changesModel?.cancelScans()
        syncModel = nil
        goToFileController.close()
        blamePanel.close()
        onClose?()
    }

    private func applyLayout() {
        guard let size = window?.contentView?.frame.size,
            let sidebar = outerSplit.splitViewItems.first
        else { return }
        let layout = LayoutPolicy.layout(for: size)
        guard layout != currentLayout else { return }
        let previous = currentLayout
        currentLayout = layout

        if !layout.showsSidebar, !sidebar.isCollapsed {
            sidebar.isCollapsed = true
            sidebarCollapsedByPolicy = true
        } else if layout.showsSidebar, previous?.showsSidebar == false, sidebarCollapsedByPolicy {
            sidebar.isCollapsed = false
            sidebarCollapsedByPolicy = false
        }

    }

    /// Sizes the sidebar to the tree rows it is showing so the detail pane keeps the rest of
    /// the window. A divider the user dragged is theirs from then on, so the fit stops, and
    /// only the tree measures its rows, so the fit runs only while the tree is showing.
    private func fitSidebarWidth(to contentWidth: CGFloat) {
        guard !UserDefaults.standard.bool(forKey: Self.sidebarWidthPinnedKey),
            contentTabs.selectedTabViewItemIndex == ContentMode.files.rawValue,
            // A width for a collapsed sidebar would re-open it behind the user.
            outerSplit.splitViewItems.first?.isCollapsed == false
        else { return }
        outerSplit.splitView.setPosition(
            LayoutPolicy.sidebarWidth(forContent: contentWidth), ofDividerAt: 0)
    }

    @objc private func sidebarDidResize(_ notification: Notification) {
        // Present only when a divider drag caused the resize, not when the window changed.
        guard notification.userInfo?["NSSplitViewDividerIndex"] != nil else { return }
        UserDefaults.standard.set(true, forKey: Self.sidebarWidthPinnedKey)
    }

    // MARK: Construction

    private func buildSplitViews() {
        // The sidebar carries the navigation for every mode: the refs, and under them the
        // tree or the change list. The content area beside it is the detail alone.
        sidebarController.add(section: treeController.view, from: treeController)
        let changes = changesController.makeSidebarSection()
        changesSection = changes
        sidebarController.add(section: changes)
        sidebarController.show(section: treeController.view)

        let sidebar = NSSplitViewItem(sidebarWithViewController: sidebarController)
        sidebar.minimumThickness = LayoutPolicy.sidebarMinimumWidth
        sidebar.maximumThickness = LayoutPolicy.sidebarMaximumWidth
        sidebar.canCollapse = true
        // Collapsing gives the width to the pane beside it; the window keeps the size the
        // user gave it.
        sidebar.collapseBehavior = .preferResizingSiblingsWithFixedSplitView

        contentTabs.tabStyle = .unspecified
        contentTabs.addTabViewItem(NSTabViewItem(viewController: fileController))
        contentTabs.addTabViewItem(NSTabViewItem(viewController: historyController))
        contentTabs.addTabViewItem(NSTabViewItem(viewController: changesController))

        let content = NSSplitViewItem(viewController: contentTabs)
        content.minimumThickness = 240
        outerSplit.addSplitViewItem(sidebar)
        outerSplit.addSplitViewItem(content)
        // .v2: the tree moved into the sidebar, so a saved width is a width for different
        // content and the content split it used to share is gone.
        outerSplit.splitView.autosaveName = "rc-git.main.v2"
        NotificationCenter.default.addObserver(
            self, selector: #selector(sidebarDidResize(_:)),
            name: NSSplitView.didResizeSubviewsNotification, object: outerSplit.splitView)
        let host = DropHostViewController(hosting: outerSplit)
        host.onDropDirectory = { path in
            (NSApp.delegate as? AppDelegate)?.open(path: path)
        }
        window?.contentViewController = host
    }

    private func buildToolbar() {
        let toolbar = NSToolbar(identifier: "rc-git.toolbar")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        toolbar.allowsUserCustomization = false
        window?.toolbar = toolbar
    }

    /// The version the bundle was stamped with, beside the window title. The stamp cannot
    /// change while the app runs, so the badge is built once and never updated. Frames are
    /// set by hand: this view lives in the titlebar, outside the content view's layout.
    private func buildVersionBadge() {
        guard let window else { return }
        let build = AppBuild.current
        let label = NSTextField(labelWithString: build.shortVersion)
        label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        label.textColor = .secondaryLabelColor
        label.toolTip = build.versionLine
        label.sizeToFit()
        let height: CGFloat = 28
        let inset: CGFloat = 6
        label.frame = NSRect(
            x: inset, y: ((height - label.frame.height) / 2).rounded(),
            width: label.frame.width, height: label.frame.height)
        label.autoresizingMask = [.minYMargin, .maxYMargin]
        let container = NSView(
            frame: NSRect(x: 0, y: 0, width: label.frame.width + inset * 2, height: height))
        container.addSubview(label)
        let accessory = NSTitlebarAccessoryViewController()
        accessory.view = container
        accessory.layoutAttribute = .leading
        window.addTitlebarAccessoryViewController(accessory)
    }

    private func wireModel() {
        model.onRefsChanged = { [weak self] in
            guard let self else { return }
            refsController.reload(
                model.refs, selected: model.selectedRef, head: model.head,
                showsWorkingTree: hasWorkingTree,
                workingTreeSelected: model.browsingWorkingTree)
            if model.browsingWorkingTree {
                window?.subtitle = "Working Tree"
            } else if let browsed = model.browsedCommit {
                window?.subtitle = "\(browsed.oid.short) — \(browsed.title)"
            } else {
                window?.subtitle = model.selectedRef?.shortName ?? ""
            }
        }
        refsController.onSelectWorkingTree = { [weak self] in
            guard let self else { return }
            setMode(.files)
            Task { await self.model.selectWorkingTree() }
        }
        refsController.onRefAction = { [weak self] action, ref in
            self?.performRefAction(action, on: ref)
        }
        model.onHistoryChanged = { [weak self] in
            guard let self else { return }
            if case let .path(path) = model.historyScope {
                historyController.setScope(path: path)
            } else {
                historyController.setScope(path: nil)
            }
            historyController.renderGraph(model.graphRows, maxLanes: model.graphMaxLaneCount)
            historyController.reload(model.history, complete: model.historyComplete)
        }
        model.onDiffChanged = { [weak self] in
            guard let self else { return }
            historyController.renderDetail(changes: model.commitChanges, state: model.fileDiff)
            historyController.renderCommitDetail(
                model.commitDetail, canGoBack: !model.detailBackStack.isEmpty)
            if let commit = model.diffCommit {
                historyController.selectCommitRow(commit.oid)
            }
        }
        historyController.onJumpToCommit = { [weak self] oid in
            guard let self else { return }
            Task { await self.model.jumpToCommit(oid) }
        }
        historyController.onDetailBack = { [weak self] in
            guard let self else { return }
            Task { await self.model.goBackInDetail() }
        }
        historyController.onCloseDetail = { [weak self] in
            self?.model.closeCommitDetail()
        }
        historyController.onClearScope = { [weak self] in
            self?.model.clearHistoryScope()
        }
        treeController.onShowHistory = { [weak self] path in
            guard let self else { return }
            setMode(.history)
            Task { await self.model.showPathHistory(path) }
        }
        model.onSearchChanged = { [weak self] in
            guard let self else { return }
            historyController.renderGraph(model.graphRows, maxLanes: model.graphMaxLaneCount)
            historyController.renderSearch(
                active: model.searchFilter != nil, results: model.searchResults,
                scanned: model.searchScanned, complete: model.searchComplete,
                searching: model.isSearching)
        }
        historyController.onSearchFilterChanged = { [weak self] filter in
            guard let self else { return }
            if let filter {
                Task { await self.model.search(filter) }
            } else {
                model.clearSearch()
            }
        }
        historyController.onNeedsMoreSearch = { [weak self] in
            guard let self else { return }
            Task { await self.model.loadMoreSearchResults() }
        }
        historyController.onJumpToRevision = { [weak self] spec in
            guard let self else { return }
            Task { await self.model.jumpToRevision(spec) }
        }
        historyController.onSelectCommit = { [weak self] commit in
            guard let self else { return }
            Task { await self.model.loadCommitChanges(commit) }
        }
        historyController.onSelectChange = { [weak self] change in
            guard let self else { return }
            Task { await self.model.loadFileDiff(change) }
        }
        historyController.onNeedsMore = { [weak self] in
            guard let self else { return }
            Task { await self.model.loadMoreHistory() }
        }
        historyController.onBrowseCommit = { [weak self] commit in
            guard let self else { return }
            setMode(.files)
            Task { await self.model.browseCommit(commit) }
        }
        goToFileController.provider = { [weak self] query in
            guard let self else { return [] }
            return try await model.searchPaths(query)
        }
        goToFileController.onOpen = { [weak self] entry in
            guard let self else { return }
            setMode(.files)
            let treeEntry = TreeEntry(name: entry.name, oid: entry.oid, kind: entry.kind)
            Task { await self.model.openFile(treeEntry, path: entry.path) }
        }
        model.onRootChanged = { [weak self] in
            guard let self else { return }
            treeController.reload(root: model.rootTree)
        }
        model.onFileChanged = { [weak self] in
            guard let self else { return }
            fileController.render(model.fileState)
        }
        model.onError = { [weak self] message in
            self?.presentError(message)
        }
        refsController.onSelect = { [weak self] ref in
            guard let self else { return }
            Task { await self.model.select(ref) }
        }
        treeController.childrenProvider = { [weak self] tree, path in
            guard let self else { return [] }
            if model.browsingWorkingTree {
                return try await model.worktreeRows(at: path)
            }
            return try await model.children(of: tree).map { BrowseRow(entry: $0) }
        }
        treeController.onOpenFile = { [weak self] row, path in
            guard let self else { return }
            if model.browsingWorkingTree, !row.missing {
                Task { await self.model.openWorktreeFile(name: row.entry.name, path: path) }
            } else {
                Task { await self.model.openFile(row.entry, path: path) }
            }
        }
        treeController.onError = { [weak self] message in
            self?.presentError(message)
        }
        treeController.onContentWidthChanged = { [weak self] width in
            self?.fitSidebarWidth(to: width)
        }
        fileController.onLoadAnyway = { [weak self] entry, limit in
            guard let self else { return }
            Task { await self.model.openFile(entry, limit: limit) }
        }
        fileController.highlightProvider = { [weak self] entry, document in
            guard let self else { return nil }
            return await model.highlights(for: entry, document: document)
        }
    }

    // MARK: Changes tab

    /// Connects a fresh `ChangesModel` to the Changes pane and routes its actions through
    /// the guard.
    private func wireChanges(_ changes: ChangesModel) {
        changes.onStatusChanged = { [weak self] in
            guard let self, let changesModel else { return }
            changesController.renderStatus(changesModel.statusState)
            changesController.refreshTicks()
        }
        changes.onDiffChanged = { [weak self] in
            guard let self, let changesModel else { return }
            changesController.renderDiff(changesModel.diffState)
        }
        changes.onStashChanged = { [weak self] in
            guard let self, let changesModel else { return }
            changesController.renderStashes(changesModel.stashes)
        }
        changes.onError = { [weak self] message in
            self?.presentError(message)
        }
        changesController.isTicked = { [weak self] path in
            self?.changesModel?.isTicked(path) ?? true
        }
        changesController.isHunkTicked = { [weak self] path, stamp in
            self?.changesModel?.isHunkTicked(path, stamp) ?? true
        }
        changesController.onSelectEntry = { [weak self] entry in
            guard let self, let changesModel else { return }
            Task { await changesModel.select(entry) }
        }
        changesController.onTogglePaths = { [weak self] paths, ticked in
            self?.changesModel?.setTicked(paths, ticked)
        }
        changesController.onToggleHunk = { [weak self] path, stamp, all in
            self?.changesModel?.toggleHunk(path, stamp, all: all)
        }
        changesController.onCommit = { [weak self] message in
            self?.performCommit(message)
        }
        changesController.onStashPush = { [weak self] in
            self?.performStashPush()
        }
        changesController.onStashApply = { [weak self] index in
            self?.performStashAction(
                verb: "Apply Stash", decision: RepositoryGuard.planStashApply()
            ) { path, input in
                try RepositoryGuard.stashApply(repoPath: path, index: index, input: input)
            }
        }
        changesController.onStashPop = { [weak self] index in
            self?.performStashAction(
                verb: "Pop Stash", decision: RepositoryGuard.planStashPop()
            ) { path, input in
                _ = try RepositoryGuard.stashPop(repoPath: path, index: index, input: input)
            }
        }
        changesController.onStashDrop = { [weak self] index in
            self?.performStashAction(
                verb: "Drop Stash", decision: RepositoryGuard.planStashDrop(index: index)
            ) { path, input in
                _ = try RepositoryGuard.stashDrop(repoPath: path, index: index, input: input)
            }
        }
        changesController.onDiscard = { [weak self] paths in
            self?.performStashAction(
                verb: "Discard Changes", decision: RepositoryGuard.planDiscard(files: paths.count)
            ) { path, input in
                try RepositoryGuard.discard(repoPath: path, paths: paths, input: input)
            }
        }
        changesController.onFetch = { [weak self] in
            self?.fetchRemote(nil)
        }
        changesController.onPull = { [weak self] in
            self?.pullRemote(nil)
        }
        changesController.onPush = { [weak self] in
            self?.pushRemote(nil)
        }
        changesController.onDiscardHunk = { [weak self] filePath, stamp in
            self?.performStashAction(
                verb: "Discard Hunk", decision: RepositoryGuard.planDiscard(files: 1)
            ) { path, input in
                try RepositoryGuard.discardHunks(
                    repoPath: path, path: filePath, stamps: [stamp], input: input)
            }
        }
    }

    private func performCommit(_ message: String, identity: CommitIdentity? = nil) {
        guard let path = repositoryPath, let changes = changesModel else { return }
        let selections = changes.selections
        guard !selections.isEmpty else { return }
        guard !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            presentError("Enter a commit message first.")
            return
        }
        guard let decision = RepositoryGuard.planCommit(files: selections.count) else { return }
        let expectedHead = changes.expectedHead
        let author = identity ?? Self.savedIdentity()
        followDecision(decision, verb: "Commit") { input in
            _ = try RepositoryGuard.commit(
                repoPath: path, message: message, author: author,
                expectedHead: expectedHead, selections: selections, input: input)
        } onSuccess: { [weak self] in
            self?.changesController.clearMessage()
            self?.refreshChanges()
        } onFailure: { [weak self] failure in
            guard let self else { return }
            if failure.contains("user.name and user.email") {
                promptForIdentity { [weak self] identity in
                    Self.saveIdentity(identity)
                    self?.performCommit(message, identity: identity)
                }
            } else {
                presentError(failure)
            }
        }
    }

    private func performStashPush() {
        guard let path = repositoryPath, let changes = changesModel else { return }
        let paths = changes.tickedPaths
        guard !paths.isEmpty,
            let decision = RepositoryGuard.planStashPush(files: paths.count)
        else { return }
        let message = changesController.commitMessage
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let author = Self.savedIdentity()
        followDecision(decision, verb: "Stash") { input in
            try RepositoryGuard.stashPush(
                repoPath: path, paths: paths, message: message.isEmpty ? nil : message,
                author: author, input: input)
        } onSuccess: { [weak self] in
            self?.changesController.clearMessage()
            self?.refreshChanges()
        } onFailure: { [weak self] failure in
            guard let self else { return }
            if failure.contains("user.name and user.email") {
                promptForIdentity { [weak self] identity in
                    Self.saveIdentity(identity)
                    self?.performStashPush()
                }
            } else {
                presentError(failure)
            }
        }
    }

    /// One guarded stash or discard action: plan, challenge, run, refresh.
    private func performStashAction(
        verb: String, decision: GuardDecision?,
        perform: @escaping @Sendable (String, String?) throws -> Void
    ) {
        guard let path = repositoryPath, let decision else { return }
        followDecision(
            decision, verb: verb,
            perform: { input in try perform(path, input) },
            onSuccess: { [weak self] in self?.refreshChanges() },
            onFailure: nil)
    }

    private func refreshChanges() {
        guard let changesModel else { return }
        Task { await changesModel.refresh() }
    }

    private static let identityNameKey = "rc-git.identity.name"
    private static let identityEmailKey = "rc-git.identity.email"

    private static func savedIdentity() -> CommitIdentity? {
        let defaults = UserDefaults.standard
        guard let name = defaults.string(forKey: identityNameKey),
            let email = defaults.string(forKey: identityEmailKey),
            !name.isEmpty, !email.isEmpty
        else { return nil }
        return CommitIdentity(name: name, email: email)
    }

    private static func saveIdentity(_ identity: CommitIdentity) {
        UserDefaults.standard.set(identity.name, forKey: identityNameKey)
        UserDefaults.standard.set(identity.email, forKey: identityEmailKey)
    }

    /// Asks for a commit identity once: the repository's own config sets none, and the
    /// isolated open never reads the global one. The answer persists in app defaults.
    private func promptForIdentity(completion: @escaping (CommitIdentity) -> Void) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = "Who is committing?"
        alert.informativeText =
            "This repository's own config sets no user.name/user.email. rc-git never reads "
            + "your global git config, so enter the identity to record; it is remembered."
        let nameField = NSTextField(frame: NSRect(x: 0, y: 30, width: 260, height: 24))
        nameField.placeholderString = "Name"
        let emailField = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        emailField.placeholderString = "email@example.com"
        let box = NSView(frame: NSRect(x: 0, y: 0, width: 260, height: 56))
        box.addSubview(nameField)
        box.addSubview(emailField)
        alert.accessoryView = box
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { response in
            let name = nameField.stringValue.trimmingCharacters(in: .whitespaces)
            let email = emailField.stringValue.trimmingCharacters(in: .whitespaces)
            guard response == .alertFirstButtonReturn, !name.isEmpty, !email.isEmpty else {
                return
            }
            completion(CommitIdentity(name: name, email: email))
        }
    }

    // MARK: Sync

    @objc func fetchRemote(_ sender: Any?) {
        Task { await syncModel?.fetch() }
    }

    @objc func pullRemote(_ sender: Any?) {
        Task { await syncModel?.pull() }
    }

    @objc func pushRemote(_ sender: Any?) {
        Task { await syncModel?.push() }
    }

    @objc func syncRepository(_ sender: Any?) {
        Task { await syncModel?.sync() }
    }

    /// Stops the running fetch, pull, or push. The flag reaches the transfer itself,
    /// so this is not a "stop waiting and pretend" — the step ends and the workflow
    /// unwinds without touching anything further.
    @objc func cancelSync(_ sender: Any?) {
        syncModel?.cancelRunning()
    }

    private func wireSync(_ sync: SyncModel) {
        sync.onStateChanged = { [weak self] in
            self?.renderSyncState()
        }
        sync.onCredentialsNeeded = { [weak self] host, kind, resume in
            self?.promptForCredentials(host: host, kind: kind, completion: resume)
        }
        sync.onHostKeyNeeded = { [weak self] host, hostKey, resume in
            self?.promptForHostKey(host: host, hostKey: hostKey, completion: resume)
        }
    }

    private func renderSyncState() {
        guard let sync = syncModel else { return }
        changesController.renderSync(status: sync.remoteStatus, busy: sync.isBusy)
        switch sync.state {
        case .idle:
            showSyncSpinner(false)
            syncToolbarItem?.toolTip = idleSyncTooltip()
        case let .running(step):
            showSyncSpinner(true)
            syncToolbarItem?.toolTip = "\(runningVerb(step)) — click to cancel"
        case let .cancelling(step):
            showSyncSpinner(true)
            syncToolbarItem?.toolTip = "Cancelling the \(runningVerb(step).lowercased())…"
        case let .done(summary):
            showSyncSpinner(false)
            syncToolbarItem?.toolTip = idleSyncTooltip()
            if summary.pulledFiles > 0 {
                worktreeDidChange()
            }
            sync.reset()
        case let .diverged(message):
            showSyncSpinner(false)
            presentDiverged(message)
            sync.reset()
        case let .failed(_, message):
            showSyncSpinner(false)
            presentError(message, title: "rc-git could not sync")
            sync.reset()
        }
    }

    /// What the running step is doing, for the tooltip.
    private func runningVerb(_ step: SyncStep) -> String {
        switch step {
        case .fetch: "Fetching"
        case .pull: "Pulling"
        case .push: "Pushing"
        }
    }

    /// The toolbar button stays on screen for the whole transfer and becomes the way
    /// to stop it; the spinner sits beside it rather than replacing it. Hiding the
    /// button while busy is what used to leave a stuck sync with nothing to click.
    private func showSyncSpinner(_ busy: Bool) {
        let cancellable = syncModel?.isCancellable ?? false
        syncButton?.image = NSImage(
            systemSymbolName: busy ? "stop.circle" : "arrow.triangle.2.circlepath",
            accessibilityDescription: busy ? "Cancel Sync" : "Sync")
        syncButton?.action = busy ? #selector(cancelSync(_:)) : #selector(syncRepository(_:))
        syncButton?.isEnabled = !busy || cancellable
        syncSpinner?.isHidden = !busy
        if busy {
            syncSpinner?.startAnimation(nil)
        } else {
            syncSpinner?.stopAnimation(nil)
        }
    }

    /// "Sync — 2 to push, 3 to pull" while idle, from the last preflight.
    private func idleSyncTooltip() -> String {
        guard let status = syncModel?.remoteStatus, status.hasUpstream else { return "Sync" }
        if status.ahead == 0 && status.behind == 0 {
            return "Sync — up to date"
        }
        var parts: [String] = []
        if status.ahead > 0 { parts.append("\(status.ahead) to push") }
        if status.behind > 0 { parts.append("\(status.behind) to pull") }
        return "Sync — " + parts.joined(separator: ", ")
    }

    private func presentDiverged(_ message: String) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = "Branch and upstream have diverged"
        alert.informativeText =
            message + "\n\nReconcile in git (merge or rebase), then sync again."
        alert.alertStyle = .informational
        alert.beginSheetModal(for: window)
    }

    private func promptForCredentials(
        host: String, kind: CredentialKind, completion: @escaping (Credentials?) -> Void
    ) {
        SyncPrompts.credentials(on: window, host: host, kind: kind, completion: completion)
    }

    private func promptForHostKey(
        host: String, hostKey: String, completion: @escaping (Bool) -> Void
    ) {
        SyncPrompts.hostKey(on: window, host: host, hostKey: hostKey, completion: completion)
    }

    // MARK: Guarded mutations

    /// Routes a context-menu request through the guard: names are asked for with a small
    /// sheet, destructive deletions run the guard's challenge, denials show its reason.
    private func performRefAction(_ action: RefsViewController.RefMenuAction, on ref: RefEntry) {
        guard let path = repositoryPath else { return }
        switch action {
        case .newBranch:
            promptForName(
                title: "New Branch",
                message: "Create a branch at “\(ref.shortName)” (\(ref.target.short))."
            ) { [weak self] name in
                self?.runGuarded {
                    try RepositoryGuard.createBranch(
                        repoPath: path, name: name, revspec: ref.target.hex)
                }
            }
        case .newTag:
            promptForName(
                title: "New Tag",
                message: "Create a tag at “\(ref.shortName)” (\(ref.target.short))."
            ) { [weak self] name in
                self?.runGuarded {
                    try RepositoryGuard.createTag(
                        repoPath: path, name: name, revspec: ref.target.hex)
                }
            }
        case .deleteBranch:
            let name = ref.shortName
            Task { [weak self] in
                let plan = await Task.detached {
                    Result { try RepositoryGuard.planBranchDelete(repoPath: path, name: name) }
                }.value
                guard let self else { return }
                switch plan {
                case let .success(decision):
                    followDecision(decision, verb: "Delete Branch") { input in
                        try RepositoryGuard.deleteBranch(
                            repoPath: path, name: name, input: input)
                    }
                case let .failure(error):
                    presentError(String(describing: error))
                }
            }
        case .deleteTag:
            guard let decision = RepositoryGuard.planTagDelete(name: ref.shortName) else {
                return
            }
            let name = ref.shortName
            followDecision(decision, verb: "Delete Tag") { input in
                try RepositoryGuard.deleteTag(repoPath: path, name: name, input: input)
            }
        }
    }

    /// Obeys a guard decision: run, challenge first, or show why not. Never offers a way
    /// around a challenge.
    private func followDecision(
        _ decision: GuardDecision, verb: String,
        perform: @escaping @Sendable (String?) throws -> String?
    ) {
        followDecision(
            decision, verb: verb, perform: { _ = try perform($0) },
            onSuccess: nil, onFailure: nil)
    }

    /// As above, with hooks: `onSuccess` refreshes what the action changed, `onFailure`
    /// replaces the default error alert (the commit path uses it to ask for an identity).
    private func followDecision(
        _ decision: GuardDecision, verb: String,
        perform: @escaping @Sendable (String?) throws -> Void,
        onSuccess: (() -> Void)?,
        onFailure: ((String) -> Void)?
    ) {
        switch decision.kind {
        case .allow:
            runGuarded({ try perform(nil) }, onSuccess: onSuccess, onFailure: onFailure)
        case .confirm:
            presentChallenge(decision, verb: verb) { [weak self] input in
                self?.runGuarded(
                    { try perform(input) }, onSuccess: onSuccess, onFailure: onFailure)
            }
        case .deny:
            guard let window else { return }
            let alert = NSAlert()
            alert.messageText = verb
            alert.informativeText = decision.reason ?? "The policy refuses this action."
            alert.alertStyle = .critical
            alert.beginSheetModal(for: window)
        }
    }

    private func presentChallenge(
        _ decision: GuardDecision, verb: String, completion: @escaping (String?) -> Void
    ) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = verb
        var message = decision.summary
        if decision.backupRef {
            message += "\nA backup ref under refs/rc-git/backup/ is written first."
        }
        var field: NSTextField?
        if let phrase = decision.typedPhrase {
            message += "\nType “\(phrase)” to confirm."
            let input = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 24))
            input.placeholderString = phrase
            alert.accessoryView = input
            field = input
        }
        alert.informativeText = message
        alert.alertStyle = decision.risk == .destructive ? .critical : .warning
        alert.addButton(withTitle: verb)
        alert.addButton(withTitle: "Cancel")
        if #available(macOS 11.0, *) {
            alert.buttons.first?.hasDestructiveAction = decision.risk == .destructive
        }
        alert.beginSheetModal(for: window) { response in
            guard response == .alertFirstButtonReturn else { return }
            completion(field?.stringValue)
        }
    }

    /// Asks for a ref name with a one-field sheet.
    private func promptForName(
        title: String, message: String, completion: @escaping (String) -> Void
    ) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 24))
        field.placeholderString = "name"
        alert.accessoryView = field
        alert.addButton(withTitle: "Create")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { response in
            let name = field.stringValue.trimmingCharacters(in: .whitespaces)
            guard response == .alertFirstButtonReturn, !name.isEmpty else { return }
            completion(name)
        }
    }

    /// Runs one guarded mutation off the main thread. By default success needs no handling —
    /// the watchers see the change and refresh — and failure shows the standard alert.
    private func runGuarded(
        _ work: @escaping @Sendable () throws -> Void,
        onSuccess: (() -> Void)? = nil,
        onFailure: ((String) -> Void)? = nil
    ) {
        Task { [weak self] in
            let outcome = await Task.detached { Result { try work() } }.value
            switch outcome {
            case .success:
                onSuccess?()
            case let .failure(error):
                let message = (error as? RepositoryError)?.message ?? String(describing: error)
                if let onFailure {
                    onFailure(message)
                } else {
                    self?.presentError(message)
                }
            }
        }
    }

    private func presentError(
        _ message: String, title: String = "rc-git could not read the repository"
    ) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.beginSheetModal(for: window)
    }

    // MARK: NSToolbarDelegate

    private static let viewModeItem = NSToolbarItem.Identifier("rc-git.viewMode")
    private static let syncItem = NSToolbarItem.Identifier("rc-git.sync")

    /// No `.sidebarTrackingSeparator`: it is vended by `NSSplitViewController` for the split
    /// it owns, and this window's split sits inside `DropHostViewController` rather than
    /// being the window's own content controller. Tracking a split it cannot measure, the
    /// separator sat off the divider while the sidebar was open and threw the items it
    /// carries across the titlebar when the sidebar collapsed.
    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.toggleSidebar, Self.viewModeItem, .flexibleSpace, Self.syncItem]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        toolbarDefaultItemIdentifiers(toolbar)
    }

    func toolbar(
        _ toolbar: NSToolbar, itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        switch itemIdentifier {
        case Self.viewModeItem:
            let control = NSSegmentedControl(
                labels: ["Files", "History", "Changes"], trackingMode: .selectOne, target: self,
                action: #selector(modeChanged(_:)))
            control.selectedSegment = ContentMode.files.rawValue
            modeControl = control
            let item = NSToolbarItem(itemIdentifier: itemIdentifier)
            item.view = control
            item.label = "View"
            return item
        case Self.syncItem:
            let button = NSButton(
                image: NSImage(
                    systemSymbolName: "arrow.triangle.2.circlepath",
                    accessibilityDescription: "Sync")!,
                target: self, action: #selector(syncRepository(_:)))
            button.bezelStyle = .texturedRounded
            let spinner = NSProgressIndicator()
            spinner.style = .spinning
            spinner.controlSize = .small
            spinner.isDisplayedWhenStopped = false
            spinner.isHidden = true
            let box = NSStackView(views: [button, spinner])
            box.orientation = .horizontal
            syncButton = button
            syncSpinner = spinner
            let item = NSToolbarItem(itemIdentifier: itemIdentifier)
            item.view = box
            item.label = "Sync"
            item.toolTip = "Sync"
            syncToolbarItem = item
            return item
        default:
            return nil
        }
    }
}

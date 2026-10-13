import AppKit
import RcGitKit

/// One browser window: a sidebar carrying the refs and the mode's navigation — the directory
/// tree, or the change list and commit box — beside the detail pane. Owns a `BrowserModel`
/// and applies `LayoutPolicy` as the window changes size.
@MainActor
final class MainWindowController: NSWindowController, NSWindowDelegate, NSToolbarDelegate,
    NSMenuItemValidation, NSPopoverDelegate
{
    let model = BrowserModel()
    var onClose: (() -> Void)?
    /// Fires with where this tab is after each move, for the app-wide back/forward list.
    var onStop: ((NavigationStop) -> Void)?
    /// Asks the app to step through that list: the toolbar arrows, a swipe, the commit
    /// detail's back arrow.
    var onNavigate: ((BackForwardList.Direction) -> Void)?
    /// Fires when this tab's window becomes the main one.
    var onBecomeMain: (() -> Void)?
    private(set) var isEmpty = true

    private let outerSplit = SidebarSplitViewController()
    private let contentTabs = NSTabViewController()
    private let sidebarController = SidebarViewController()
    private let treeController = TreeViewController()
    private let fileController = FileViewController()
    private let historyController = HistoryViewController()
    private let changesController = ChangesViewController()
    private var changesModel: ChangesModel?
    private var authorModel: CommitAuthorModel?
    private var syncModel: SyncModel?
    /// Set once the window closes: a sync step still unwinding must not raise a sheet on it.
    private var isClosed = false
    private var syncButton: NSButton?
    private var syncSpinner: NSProgressIndicator?
    private var syncToolbarItem: NSToolbarItem?
    private var backToDefaultButton: NSButton?
    private var backToDefaultToolbarItem: NSToolbarItem?
    private let goToFileController = GoToFileController()
    private let blamePanel = BlamePanelController()
    /// The ref picker the branch banner opens, and the popover showing it, while it is open.
    private var refPicker: RefPickerController?
    private var refPopover: NSPopover?
    /// When the event that closed the picker from outside happened. A click on the banner
    /// closes the open popover on its way in, and must not then open it again.
    private var refPickerCloseTimestamp: TimeInterval?
    private var refWatcher: RefWatcher?
    private var worktreeWatcher: WorktreeWatcher?
    private var modeControl: NSSegmentedControl?
    private var navigationControl: NSSegmentedControl?
    /// The back/forward list as the app delegate last rendered it here.
    private var backForward = BackForwardList()
    /// Set from the first scroll event of a swipe until its animation ends.
    private var trackingSwipe = false
    private var currentLayout: Layout?
    private var sidebarCollapse = SidebarCollapse()
    /// Set once the user drags the sidebar divider; the automatic fit stands back after that.
    private let sidebarWidthPin = SidebarWidthPin()
    /// The defaults name every tab saves its window frame under and the next window restores.
    private static let frameName = "rc-git.window"
    private var repositoryName = ""
    private(set) var repositoryPath: String?
    /// The checkout's root, `nil` for a bare repository.
    private var workingDirectory: String?
    private var hasWorkingTree: Bool { workingDirectory != nil }
    /// The Changes view's list and commit box, hosted in the sidebar.
    private var changesSection: NSView?

    /// The ref list inside the sidebar, which only History shows.
    private var refsController: RefsViewController { sidebarController.refs }

    private var mode: ContentMode {
        ContentMode(rawValue: contentTabs.selectedTabViewItemIndex) ?? .files
    }

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1120, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.minSize = NSSize(
            width: LayoutPolicy.windowMinimumWidth, height: LayoutPolicy.windowMinimumHeight)
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
        renderBanner()
        model.setShowsHiddenFiles(HiddenFilesPreference().shows)
        window.center()
        // The frame is saved by name rather than through `setFrameAutosaveName`: AppKit
        // grants an autosave name to one live window only, and every tab is its own window.
        // False only means no frame was saved yet, and the centered default stands.
        _ = window.setFrameUsingName(Self.frameName)
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
        workingDirectory = repository.workingDirectory
        renderBanner()
        showWindow(nil)
        refWatcher?.stop()
        let watcher = RefWatcher(
            gitDir: repository.gitDirectory, commonDir: repository.commonDirectory
        ) { [weak self] in
            Task { @MainActor [weak self] in
                await self?.model.refreshRefs()
                // A commit or stash from the terminal moves refs and the change list both.
                self?.changeListIsStale()
                await self?.syncModel?.refreshStatus()
            }
        }
        refWatcher = watcher
        startWatching(watcher.start)

        let changes = ChangesModel(
            repository: repository, showsHiddenFiles: model.showsHiddenFiles)
        changesModel = changes
        wireChanges(changes)
        let author = CommitAuthorModel(repoPath: path)
        authorModel = author
        author.onChange = { [weak self, weak author] in
            self?.changesController.showAuthor(author?.author)
        }
        changesController.showAuthor(nil)
        Task { await author.refresh() }
        let sync = SyncModel(
            repoPath: path, performer: GuardSyncPerformer(), store: AppDelegate.credentialStore)
        syncModel = sync
        wireSync(sync)
        Task { await sync.refreshStatus() }
        syncChangesVisibility()
        worktreeWatcher?.stop()
        worktreeWatcher = nil
        if let workDir = repository.workingDirectory {
            let treeWatcher = WorktreeWatcher(
                workDir: workDir, gitDir: repository.gitDirectory
            ) { [weak self] in
                Task { @MainActor [weak self] in
                    self?.changeListIsStale()
                }
            }
            worktreeWatcher = treeWatcher
            startWatching(treeWatcher.start)
        }
        Task { await model.attach(repository) }
    }

    private func startWatching(_ start: () throws -> Void) {
        do {
            try start()
        } catch {
            presentError(
                String(describing: error), title: "rc-git could not watch the repository")
        }
    }

    /// The change list no longer matches disk or the hidden-files setting: the model rescans
    /// now when the list is on screen, otherwise it remembers to rescan when it is shown.
    private func changeListIsStale() {
        guard let changesModel else { return }
        Task { await changesModel.markStale() }
    }

    /// Whether the change list can be seen: this tab is on Changes, the window is the
    /// selected tab of its tab group, and some of it is on screen.
    private var changesOnScreen: Bool {
        guard let window, contentTabs.selectedTabViewItemIndex == ContentMode.changes.rawValue
        else { return false }
        let frontTab = window.tabGroup.map { $0.selectedWindow === window } ?? true
        return frontTab && window.occlusionState.contains(.visible)
    }

    /// Tells the change list whether it is on screen, so a hidden one scans nothing and a
    /// shown one catches up on what it missed.
    private func syncChangesVisibility() {
        guard let changesModel else { return }
        let visible = changesOnScreen
        Task { await changesModel.setVisible(visible) }
    }

    /// Shows the file pane, the commit history, or the working-tree changes, and puts the
    /// section that belongs to the mode in the sidebar.
    func setMode(_ mode: ContentMode) {
        // The picker hangs off the banner, which History hides and Changes makes read-only.
        closeRefPicker()
        contentTabs.selectedTabViewItemIndex = mode.rawValue
        modeControl?.selectedSegment = mode.rawValue
        switch mode {
        case .files: sidebarController.show(section: treeController.view)
        case .history: sidebarController.show(section: nil)
        case .changes: sidebarController.show(section: changesSection)
        }
        renderBanner()
        if mode == .history, model.history.isEmpty {
            Task { await model.loadMoreHistory() }
        }
        syncChangesVisibility()
        if mode == .changes {
            // The sync row shows commit counts; re-read them on every switch to the tab.
            Task { await syncModel?.refreshStatus() }
            // The author line reads .git/config, which no watcher covers.
            refreshAuthor()
        }
    }

    /// A pane the user asked for by itself, which is a stop of its own. An action that
    /// changes the pane on the way somewhere else calls `setMode` and records where it ends.
    private func switchMode(_ mode: ContentMode) {
        setMode(mode)
        recordStop()
    }

    @objc func showFiles(_ sender: Any?) {
        switchMode(.files)
    }

    @objc func showHistory(_ sender: Any?) {
        switchMode(.history)
    }

    @objc func showChanges(_ sender: Any?) {
        switchMode(.changes)
    }

    // MARK: Back and forward

    /// Where this tab is, or `nil` while it browses nothing: no repository yet, or one that
    /// is still attaching. Such a stop would have nothing to return to.
    var currentStop: NavigationStop? {
        let location = model.location
        guard let repositoryPath, location.tree != .nothing else { return nil }
        return NavigationStop(repositoryPath: repositoryPath, mode: mode, browser: location)
    }

    private func recordStop() {
        guard let stop = currentStop else { return }
        onStop?(stop)
    }

    /// Shows a stop of the back/forward list: its pane, then what its browser was on.
    func restore(_ stop: NavigationStop) async {
        if mode != stop.mode {
            setMode(stop.mode)
        }
        await model.restore(stop.browser)
    }

    /// Shows what the back/forward list allows: the toolbar arrows, and the commit detail's
    /// own arrow, which is there while stepping back shows another commit.
    func renderNavigation(_ list: BackForwardList) {
        backForward = list
        navigationControl?.setEnabled(list.canGo(.back), forSegment: 0)
        navigationControl?.setEnabled(list.canGo(.forward), forSegment: 1)
        if model.diffCommit != nil {
            historyController.renderCommitDetail(
                model.commitDetail, canGoBack: detailCanGoBack)
        }
    }

    private var detailCanGoBack: Bool {
        repositoryPath.map { backForward.backReturnsToCommit(inRepository: $0) } ?? false
    }

    @objc private func navigationClicked(_ sender: NSSegmentedControl) {
        onNavigate?(sender.selectedSegment == 0 ? .back : .forward)
    }

    func windowDidBecomeMain(_ notification: Notification) {
        syncChangesVisibility()
        onBecomeMain?()
    }

    /// A tab moved behind another, a miniaturized window, or one covered by another app:
    /// the change list stops or resumes scanning with it.
    func windowDidChangeOcclusionState(_ notification: Notification) {
        syncChangesVisibility()
    }

    /// Lets a scroll view that is already at its edge hand a sideways scroll up the responder
    /// chain, where `scrollWheel` turns it into a step back or forward.
    override func wantsScrollEventsForSwipeTracking(on axis: NSEvent.GestureAxis) -> Bool {
        axis == .horizontal
    }

    /// A two-finger swipe, which arrives as scroll events when the system setting routes page
    /// swipes that way. The amount is clamped to the directions the list can go, so a swipe
    /// with nowhere to go rubber-bands and ends without a step.
    override func scrollWheel(with event: NSEvent) {
        let canGoBack = backForward.canGo(.back)
        let canGoForward = backForward.canGo(.forward)
        guard !event.phase.isEmpty, abs(event.scrollingDeltaX) > abs(event.scrollingDeltaY),
            NSEvent.isSwipeTrackingFromScrollEventsEnabled, !trackingSwipe,
            canGoBack || canGoForward
        else {
            super.scrollWheel(with: event)
            return
        }
        trackingSwipe = true
        event.trackSwipeEvent(
            options: .lockDirection, dampenAmountThresholdMin: canGoForward ? -1 : 0,
            max: canGoBack ? 1 : 0
        ) { [weak self] amount, phase, isComplete, _ in
            if phase == .ended, let direction = BackForwardList.Direction(swipeAmount: amount) {
                self?.onNavigate?(direction)
            }
            if isComplete {
                self?.trackingSwipe = false
            }
        }
    }

    /// A three-finger swipe, for the system setting that sends page swipes as their own event.
    override func swipe(with event: NSEvent) {
        guard let direction = BackForwardList.Direction(swipeAmount: event.deltaX) else {
            super.swipe(with: event)
            return
        }
        onNavigate?(direction)
    }

    /// Opens the ref picker, which stands in for the ref list outside History; History has the
    /// list in the sidebar already, so its filter takes the keyboard instead. Changes has none:
    /// its banner names the checked-out branch, which picking a ref would not change.
    @objc func chooseRef(_ sender: Any?) {
        guard !isEmpty, mode != .changes, refPicker == nil else { return }
        if mode == .history {
            refsController.focusFilter()
            return
        }
        if let event = NSApp.currentEvent, event.timestamp == refPickerCloseTimestamp {
            return
        }
        let picker = RefPickerController()
        picker.onClose = { [weak self] in self?.closeRefPicker() }
        picker.refs.onSelect = { [weak self] ref in
            self?.closeRefPicker()
            self?.browse(ref)
        }
        picker.refs.onSelectWorkingTree = { [weak self] in
            self?.closeRefPicker()
            self?.browseWorkingTree()
        }
        // The action asks with a sheet of its own, over the window the popover left.
        picker.refs.onRefAction = { [weak self] action, ref in
            self?.closeRefPicker()
            self?.performRefAction(action, on: ref)
        }
        let popover = NSPopover()
        popover.behavior = .transient
        popover.contentViewController = picker
        popover.delegate = self
        refPicker = picker
        refPopover = popover
        reloadRefs(in: picker.refs)
        let banner = sidebarController.banner
        if banner.window != nil, !banner.isHiddenOrHasHiddenAncestor, !banner.visibleRect.isEmpty {
            popover.show(relativeTo: banner.bounds, of: banner, preferredEdge: .minY)
        } else {
            // A collapsed sidebar takes the banner with it; hang the picker off the top of
            // the detail pane, where the sidebar would open.
            // Popover edges are in the view's own coordinates, so "below" flips with it.
            let anchor = contentTabs.view
            let top = anchor.isFlipped ? anchor.bounds.minY : anchor.bounds.maxY - 1
            let corner = NSRect(x: anchor.bounds.minX + Style.margin, y: top, width: 1, height: 1)
            popover.show(
                relativeTo: corner, of: anchor, preferredEdge: anchor.isFlipped ? .maxY : .minY)
        }
    }

    private func closeRefPicker() {
        guard let popover = refPopover else { return }
        refPicker = nil
        refPopover = nil
        popover.close()
    }

    // MARK: NSPopoverDelegate

    /// Only a close the window controller did not ask for reaches here with the picker still
    /// set: a click outside it, which may be the click on the banner that opened it.
    func popoverWillClose(_ notification: Notification) {
        guard let popover = notification.object as? NSPopover, popover === refPopover else {
            return
        }
        refPickerCloseTimestamp = NSApp.currentEvent?.timestamp
    }

    func popoverDidClose(_ notification: Notification) {
        guard let popover = notification.object as? NSPopover, popover === refPopover else {
            return
        }
        refPicker = nil
        refPopover = nil
    }

    private func browse(_ ref: RefEntry) {
        Task { await model.select(ref) }
    }

    /// The working tree is files on disk, so it opens in Files whichever mode picked it.
    private func browseWorkingTree() {
        setMode(.files)
        Task {
            await model.selectWorkingTree()
            recordStop()
        }
    }

    @objc func goToFile(_ sender: Any?) {
        guard !isEmpty else { return }
        model.forgetWorktreePaths()
        goToFileController.show(over: window)
    }

    /// Applies the app-wide hidden-files setting: the tree re-filters the rows it already
    /// read, path search filters its next query, and the change list rescans.
    func setShowsHiddenFiles(_ shows: Bool) {
        model.setShowsHiddenFiles(shows)
        treeController.refilter()
        changesModel?.showsHiddenFiles = shows
        changeListIsStale()
    }

    @objc private func modeChanged(_ sender: NSSegmentedControl) {
        switchMode(ContentMode(rawValue: sender.selectedSegment) ?? .files)
    }

    @objc func showBlame(_ sender: Any?) {
        guard model.canBlameOpenedFile else { return }
        Task { [model] in await model.requestBlame() }
    }

    @objc func searchCommits(_ sender: Any?) {
        guard !isEmpty else { return }
        switchMode(.history)
        historyController.focusSearch()
    }

    @objc func showFileHistory(_ sender: Any?) {
        guard let path = model.openedFilePath else { return }
        showHistory(ofPath: path, pathIsExact: model.openedPathIsExact)
    }

    /// Scopes History to one path and shows it. The scope is reported as soon as it is set;
    /// the record after the page covers a path that was already the scope. An inexact path
    /// is refused by the model and leaves the pane where it is.
    private func showHistory(ofPath path: String, pathIsExact: Bool) {
        guard pathIsExact else {
            Task { await model.showPathHistory(path, pathIsExact: false) }
            return
        }
        setMode(.history)
        Task { [weak self] in
            await self?.model.showPathHistory(path)
            self?.recordStop()
        }
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(showBlame(_:)) {
            return model.canBlameOpenedFile
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
        if menuItem.action == #selector(editCommitIdentity(_:)) {
            return authorModel != nil
        }
        if menuItem.action == #selector(chooseRef(_:)) {
            return !isEmpty && mode != .changes && refPicker == nil
        }
        let syncSelectors = [
            #selector(fetchRemote(_:)), #selector(pullRemote(_:)),
            #selector(pushRemote(_:)), #selector(syncRepository(_:)),
        ]
        if menuItem.action == #selector(cancelSync(_:)) {
            return syncModel?.isCancellable ?? false
        }
        if menuItem.action == #selector(backToDefaultBranch(_:)) {
            menuItem.title = SyncTitles.backToDefault(syncModel?.defaultBranch)
            guard repositoryPath != nil, let syncModel, !syncModel.isBusy else { return false }
            // No upstream check: the switch is local, and the branch switched to is the one
            // whose upstream decides whether the fetch can run.
            return hasWorkingTree
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
            if action == #selector(pushRemote(_:)) {
                return SyncTitles.canPush(status: status, busy: syncModel.isBusy)
            }
            return SyncTitles.canPull(status: status, busy: syncModel.isBusy)
        }
        return true
    }

    // MARK: Layout

    func windowDidResize(_ notification: Notification) {
        applyLayout()
        saveFrame()
    }

    func windowDidMove(_ notification: Notification) {
        saveFrame()
    }

    /// Saves the frame of the tab in front, so whichever tab survives a close or moves to
    /// its own window keeps the frame the next launch restores. Full screen is not a frame.
    private func saveFrame() {
        guard let window, window.isMainWindow, !window.styleMask.contains(.fullScreen) else {
            return
        }
        window.saveFrame(usingName: Self.frameName)
    }

    func windowWillClose(_ notification: Notification) {
        saveFrame()
        NotificationCenter.default.removeObserver(
            self, name: NSSplitView.didResizeSubviewsNotification,
            object: outerSplit.splitView)
        refWatcher?.stop()
        refWatcher = nil
        worktreeWatcher?.stop()
        worktreeWatcher = nil
        changesModel?.cancelScans()
        isClosed = true
        syncModel?.cancelRunning()
        syncModel = nil
        goToFileController.close()
        model.cancelBlame()
        blamePanel.close()
        closeRefPicker()
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

        if let collapsed = sidebarCollapse.apply(
            layout, previous: previous, isCollapsed: sidebar.isCollapsed)
        {
            sidebar.isCollapsed = collapsed
        }
    }

    /// Sizes the sidebar to the tree rows it is showing so the detail pane keeps the rest of
    /// the window. A divider the user dragged is theirs from then on, so the fit stops, and
    /// only the tree measures its rows, so the fit runs only while the tree is showing.
    private func fitSidebarWidth(to contentWidth: CGFloat) {
        guard !sidebarWidthPin.isPinned,
            contentTabs.selectedTabViewItemIndex == ContentMode.files.rawValue,
            // A width for a collapsed sidebar would re-open it behind the user.
            outerSplit.splitViewItems.first?.isCollapsed == false
        else { return }
        outerSplit.splitView.setPosition(
            LayoutPolicy.sidebarWidth(forContent: contentWidth), ofDividerAt: 0)
    }

    @objc private func sidebarDidResize(_ notification: Notification) {
        guard SidebarResize.isUserDrag(notification.userInfo) else { return }
        sidebarWidthPin.pin()
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
        content.minimumThickness = LayoutPolicy.contentMinimumWidth
        outerSplit.addSplitViewItem(sidebar)
        outerSplit.onToggleSidebar = { [weak self] in self?.sidebarCollapse.userToggled() }
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
            reloadRefs(in: refsController)
            if let refPicker { reloadRefs(in: refPicker.refs) }
            renderBanner()
            window?.subtitle = browsingBanner().summary
        }
        refsController.onSelectWorkingTree = { [weak self] in
            self?.browseWorkingTree()
        }
        sidebarController.banner.onClick = { [weak self] in
            self?.chooseRef(nil)
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
            historyController.reload(
                model.history, complete: model.historyComplete, error: model.historyError)
        }
        model.onDiffChanged = { [weak self] in
            guard let self else { return }
            historyController.renderDetail(
                changes: model.commitChanges, state: model.fileDiff, detail: model.commitDetail)
            historyController.renderCommitDetail(model.commitDetail, canGoBack: detailCanGoBack)
            if let commit = model.diffCommit {
                historyController.selectCommitRow(commit.oid)
            } else {
                historyController.deselectCommitRow()
            }
        }
        model.onLocationChanged = { [weak self] in
            self?.recordStop()
        }
        historyController.onJumpToCommit = { [weak self] oid in
            guard let self else { return }
            Task { await self.model.jumpToCommit(oid) }
        }
        historyController.onDetailBack = { [weak self] in
            self?.onNavigate?(.back)
        }
        historyController.onCloseDetail = { [weak self] in
            self?.model.closeCommitDetail()
        }
        historyController.onClearScope = { [weak self] in
            self?.model.clearHistoryScope()
        }
        treeController.onShowHistory = { [weak self] path, pathIsExact in
            self?.showHistory(ofPath: path, pathIsExact: pathIsExact)
        }
        treeController.onCopyPath = { [weak self] path, pathIsExact in
            guard let text = self?.model.copyablePath(path, pathIsExact: pathIsExact) else {
                return
            }
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
        }
        model.onSearchChanged = { [weak self] in
            guard let self else { return }
            historyController.renderGraph(model.graphRows, maxLanes: model.graphMaxLaneCount)
            historyController.renderSearch(
                active: model.searchFilter != nil, results: model.searchResults,
                scanned: model.searchScanned, complete: model.searchComplete,
                searching: model.isSearching, error: model.searchError)
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
        historyController.onRetryHistory = { [weak self] in
            guard let self else { return }
            Task { await self.model.retryHistory() }
        }
        historyController.onBrowseCommit = { [weak self] commit in
            guard let self else { return }
            setMode(.files)
            Task {
                await self.model.browseCommit(commit)
                self.recordStop()
            }
        }
        goToFileController.provider = { [weak self] query in
            guard let self else { return PathSearchResult(entries: [], truncated: false) }
            return try await model.searchPaths(query)
        }
        goToFileController.onOpen = { [weak self] entry in
            guard let self else { return }
            setMode(.files)
            Task {
                await self.model.openSearchResult(entry)
                self.recordStop()
            }
        }
        model.onRootChanged = { [weak self] in
            guard let self else { return }
            treeController.reload(root: model.rootTree)
        }
        model.onFileChanged = { [weak self] in
            guard let self else { return }
            fileController.repositoryPath = model.openedFilePath
            fileController.render(model.fileState)
            treeController.showOpenFile(
                model.openedFilePath, pathIsExact: model.openedPathIsExact)
        }
        model.onError = { [weak self] message in
            self?.presentError(message)
        }
        model.onBlameChanged = { [weak self] in
            guard let self else { return }
            switch model.blame {
            case .empty:
                blamePanel.close()
            case let .loading(_, title):
                blamePanel.show([], title: title, over: window)
            case let .loaded(_, spans, title):
                blamePanel.show(spans, title: title, over: window)
            case let .failed(message):
                blamePanel.close()
                presentError(message)
            }
        }
        model.onInputError = { [weak self] title, message in
            self?.presentError(message, title: title)
        }
        refsController.onSelect = { [weak self] ref in
            self?.browse(ref)
        }
        treeController.childrenProvider = { [weak self] tree, path in
            guard let self else { return [] }
            if model.browsingWorkingTree {
                return try await model.worktreeRows(at: path)
            }
            return try await model.children(of: tree).map { BrowseRow(entry: $0) }
        }
        treeController.onOpenFile = { [weak self] row, path, pathIsExact in
            guard let self else { return }
            Task { await self.model.openTreeRow(row, path: path, pathIsExact: pathIsExact) }
        }
        treeController.isRowVisible = { [weak self] row in
            self?.model.isVisible(row) ?? true
        }
        treeController.onError = { [weak self] message in
            self?.presentError(message)
        }
        treeController.onContentWidthChanged = { [weak self] width in
            self?.fitSidebarWidth(to: width)
        }
        fileController.onLoadAnyway = { [weak self] limit in
            guard let self else { return }
            Task { await self.model.loadAnyway(limit: limit) }
        }
        fileController.highlightProvider = { [weak self] entry, document in
            guard let self else { return nil }
            return await model.highlights(for: entry, document: document)
        }
        fileController.importLinkProvider = { [weak self] entry, document in
            guard let self else { return nil }
            return await model.importLinks(for: entry, document: document)
        }
        fileController.onOpenImport = { [weak self] specifier, source in
            guard let self else { return }
            Task {
                // Most misses are packages from a registry; a sheet for each would be noise.
                guard await self.model.openImport(specifier, from: source) else {
                    NSSound.beep()
                    return
                }
            }
        }
        fileController.onOpenRepositoryPath = { [weak self] path in
            guard let self else { return }
            setMode(.files)
            Task {
                switch await self.model.openPath(path) {
                case .opened:
                    self.recordStop()
                case .missing:
                    self.presentError(
                        "\(path) is not in this tree.", title: "rc-git could not open the link")
                case let .failed(message):
                    self.presentError(message)
                }
            }
        }
    }

    private func reloadRefs(in refs: RefsViewController) {
        refs.reload(
            model.refs, selected: model.selectedRef, head: model.head,
            showsWorkingTree: hasWorkingTree, workingTreeSelected: model.browsingWorkingTree)
    }

    private func browsingBanner() -> BranchBanner {
        BranchBanner.browsing(
            selectedRef: model.selectedRef, browsedCommit: model.browsedCommit,
            browsingWorkingTree: model.browsingWorkingTree, head: model.head
        ).markingRefsStale(model.refsStale)
    }

    /// Names what Files browses and opens the picker on a click. Changes names the branch
    /// `HEAD` is on and stays read-only: its list always compares the working tree with
    /// `HEAD`. History shows the ref list itself, so its banner is off screen.
    private func renderBanner() {
        let banner = sidebarController.banner
        if isEmpty {
            banner.render(BranchBanner(symbol: .nothing, title: "No repository"), clickable: false)
            banner.toolTip = nil
        } else if mode == .changes {
            banner.render(
                BranchBanner.checkedOut(head: model.head).markingRefsStale(model.refsStale),
                clickable: false)
            banner.toolTip = "The checked-out branch. Changes compare the working tree with it."
        } else {
            banner.render(browsingBanner(), clickable: true)
            banner.toolTip = "Choose the branch, tag, or working tree to browse (⌘B)"
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
            changesController.renderStashes(
                changesModel.stashes, state: changesModel.stashState)
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
        changesController.onStashApply = { [weak self] entry in
            self?.performStashAction(
                verb: "Apply Stash", decision: RepositoryGuard.planStashApply()
            ) { path, input in
                try RepositoryGuard.stashApply(repoPath: path, entry: entry, input: input)
            }
        }
        changesController.onStashPop = { [weak self] entry in
            self?.performStashAction(
                verb: "Pop Stash", decision: RepositoryGuard.planStashPop(entry)
            ) { path, input in
                _ = try RepositoryGuard.stashPop(repoPath: path, entry: entry, input: input)
            }
        }
        changesController.onStashDrop = { [weak self] entry in
            self?.performStashAction(
                verb: "Drop Stash", decision: RepositoryGuard.planStashDrop(entry)
            ) { path, input in
                _ = try RepositoryGuard.stashDrop(repoPath: path, entry: entry, input: input)
            }
        }
        changesController.isActionable = { [weak self] path in
            self?.changesModel?.isActionable(path) ?? true
        }
        changesController.onDiscard = { [weak self] paths in
            if let refusal = self?.changesModel?.discardRefusal(paths) {
                self?.presentError(
                    refusal, title: RepositoryGuard.failureTitle(verb: "Discard Changes"))
                return
            }
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
            if let refusal = self?.changesModel?.discardRefusal([filePath]) {
                self?.presentError(
                    refusal, title: RepositoryGuard.failureTitle(verb: "Discard Hunk"))
                return
            }
            self?.performStashAction(
                verb: "Discard Hunk", decision: RepositoryGuard.planDiscard(files: 1)
            ) { path, input in
                try RepositoryGuard.discardHunks(
                    repoPath: path, path: filePath, stamps: [stamp], input: input)
            }
        }
    }

    /// Commits the ticked selection. With no identity anywhere — the repository's config sets
    /// none and none is saved for it — it asks first, and saves the answer for this repository
    /// only once the commit it was asked for succeeds.
    private func performCommit(_ message: String, prompted: CommitIdentity? = nil) {
        guard let path = repositoryPath, let changes = changesModel, let authorModel else {
            return
        }
        let selections = changes.selections
        guard !selections.isEmpty else { return }
        guard !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            presentError(
                "Enter a commit message first.", title: "No commit message",
                style: .informational)
            return
        }
        guard let decision = RepositoryGuard.planCommit(files: selections.count) else { return }
        if prompted == nil, authorModel.author == .unset {
            askForIdentity { [weak self] identity in
                self?.performCommit(message, prompted: identity)
            }
            return
        }
        let expectedHead = changes.expectedHead
        let fallback = prompted ?? authorModel.saved
        followDecision(decision, verb: "Commit") { input in
            _ = try RepositoryGuard.commit(
                repoPath: path, message: message, fallback: fallback,
                expectedHead: expectedHead, selections: selections, input: input)
        } onSuccess: { [weak self] in
            self?.changesController.clearMessage()
            self?.refreshChanges()
            self?.remember(prompted)
        } onFailure: { [weak self] error in
            self?.refreshAuthor()
            if prompted == nil, error is IdentityRequiredError {
                self?.askForIdentity { identity in
                    self?.performCommit(message, prompted: identity)
                }
            } else {
                self?.presentError(
                    Self.failureMessage(error), title: RepositoryGuard.failureTitle(verb: "Commit"))
            }
        }
    }

    private func performStashPush(prompted: CommitIdentity? = nil) {
        guard let path = repositoryPath, let changes = changesModel, let authorModel else {
            return
        }
        let paths = changes.tickedPaths
        guard !paths.isEmpty,
            let decision = RepositoryGuard.planStashPush(files: paths.count)
        else { return }
        if prompted == nil, authorModel.author == .unset {
            askForIdentity { [weak self] identity in
                self?.performStashPush(prompted: identity)
            }
            return
        }
        let message = changesController.commitMessage
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let fallback = prompted ?? authorModel.saved
        followDecision(decision, verb: "Stash") { input in
            try RepositoryGuard.stashPush(
                repoPath: path, paths: paths, message: message.isEmpty ? nil : message,
                fallback: fallback, input: input)
        } onSuccess: { [weak self] in
            self?.changesController.clearMessage()
            self?.refreshChanges()
            self?.remember(prompted)
        } onFailure: { [weak self] error in
            self?.refreshAuthor()
            if prompted == nil, error is IdentityRequiredError {
                self?.askForIdentity { identity in
                    self?.performStashPush(prompted: identity)
                }
            } else {
                self?.presentError(
                    Self.failureMessage(error), title: RepositoryGuard.failureTitle(verb: "Stash"))
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

    private func refreshAuthor() {
        guard let authorModel else { return }
        Task { await authorModel.refresh() }
    }

    /// Saves an identity the user typed for a commit or stash that has now succeeded.
    private func remember(_ prompted: CommitIdentity?) {
        guard let prompted, let authorModel else { return }
        Task { await authorModel.save(prompted) }
    }

    /// Asks for an identity before the first commit or stash in a repository that has none.
    private func askForIdentity(completion: @escaping (CommitIdentity) -> Void) {
        guard let window else { return }
        IdentitySheet.present(
            on: window, title: "Who is committing?",
            message: "This repository’s own config sets no user.name/user.email, and rc-git "
                + "never reads your global git config. Enter the identity to record; it is "
                + "saved for this repository only, and only once the commit succeeds.",
            initial: nil, allowsClear: false
        ) { outcome in
            if case let .save(identity) = outcome { completion(identity) }
        }
    }

    /// Repository > Commit Identity…: shows, edits, or clears the identity saved for this
    /// repository. It is a fallback: the repository's own config wins whenever it sets one.
    @objc func editCommitIdentity(_ sender: Any?) {
        guard let window, let authorModel else { return }
        var message = "Saved for this repository only, and used only when its own config sets "
        message += "no user.name/user.email."
        if case let .configured(identity) = authorModel.author {
            message += "\nIts config sets \(identity.display) now, so commits use that."
        }
        IdentitySheet.present(
            on: window, title: "Commit Identity", message: message,
            initial: authorModel.saved, allowsClear: authorModel.saved != nil
        ) { outcome in
            Task {
                switch outcome {
                case let .save(identity): await authorModel.save(identity)
                case .clear: await authorModel.clear()
                }
            }
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

    /// Switch to the mainline branch and bring it up to date, in one go.
    @objc func backToDefaultBranch(_ sender: Any?) {
        Task { await syncModel?.backToDefaultBranch() }
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
        sync.onCredentialsNeeded = { [weak self] host, kind, key, resume in
            guard let self, !isClosed else { return resume(.unavailable) }
            promptForCredentials(host: host, kind: kind, key: key, completion: resume)
        }
        sync.onHostKeyNeeded = { [weak self] host, hostKey, resume in
            guard let self, !isClosed else { return resume(.unavailable) }
            promptForHostKey(host: host, hostKey: hostKey, completion: resume)
        }
    }

    private func renderSyncState() {
        guard let sync = syncModel else { return }
        changesController.renderSync(status: sync.remoteStatus, busy: sync.isBusy)
        // A bare repository has no working tree to switch, exactly as the menu item decides.
        backToDefaultButton?.isEnabled = !sync.isBusy && hasWorkingTree
        backToDefaultToolbarItem?.toolTip = backToDefaultTooltip()
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
                changeListIsStale()
            }
            if let notice = summary.notice {
                presentError(notice, title: "rc-git synced, with a problem")
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
        // A switch that landed is followed however the workflow ended: a fetch that fails
        // after it still leaves HEAD and the working tree on the new branch.
        if let branch = sync.takeSwitchedBranch() {
            follow(branch: branch)
        }
    }

    /// Points the browser at the branch that was just checked out. The ref watcher reloads
    /// the listing on its own, but it keeps whatever ref was selected — and after a switch
    /// the user wants the branch they switched to.
    private func follow(branch: String) {
        Task { [weak self] in
            guard let self else { return }
            await model.refreshRefs()
            guard
                let entry = model.refs.refs.first(where: { $0.fullName == "refs/heads/\(branch)" })
            else { return }
            await model.select(entry)
        }
    }

    /// What the running step is doing, for the tooltip.
    private func runningVerb(_ step: SyncStep) -> String {
        switch step {
        case .checkout: "Switching branch"
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

    /// "Back to master — switch, fetch, and fast-forward", named once the mainline is known.
    private func backToDefaultTooltip() -> String {
        let title = SyncTitles.backToDefault(syncModel?.defaultBranch)
        return "\(title) — switch, fetch, and fast-forward"
    }

    /// The idle tooltip, from the last preflight or the reason it failed.
    private func idleSyncTooltip() -> String {
        SyncTitles.idleSyncTooltip(
            status: syncModel?.remoteStatus, error: syncModel?.statusError)
    }

    /// rc-git only fast-forwards, so a diverged branch is reconciled in git. The sheet names
    /// the command and hands it over, on the clipboard or with Terminal opened in the
    /// checkout. A bare repository has no checkout to pull into, so it gets the message alone.
    private func presentDiverged(_ message: String) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = "Branch and upstream have diverged"
        alert.alertStyle = .informational
        guard let workDir = workingDirectory else {
            alert.informativeText =
                message + "\n\nReconcile in git (merge or rebase), then sync again."
            alert.beginSheetModal(for: window)
            return
        }
        let command = ReconcileCommand.rebase(workDir: workDir)
        alert.informativeText =
            message + "\n\nrc-git only fast-forwards. Rebase in a terminal, then sync again:\n\n"
            + command
        alert.addButton(withTitle: "Copy Command")
        alert.addButton(withTitle: "Open in Terminal").toolTip =
            "Opens Terminal in the checkout with the command on the clipboard"
        alert.addButton(withTitle: "Close").keyEquivalent = "\u{1b}"
        alert.beginSheetModal(for: window) { [weak self] response in
            switch response {
            case .alertFirstButtonReturn:
                Self.copyToPasteboard(command)
            case .alertSecondButtonReturn:
                Self.copyToPasteboard(command)
                self?.openTerminal(at: workDir)
            default:
                break
            }
        }
    }

    private static func copyToPasteboard(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    /// Opens a Terminal window whose shell starts in `workDir`.
    private func openTerminal(at workDir: String) {
        guard
            let terminal = NSWorkspace.shared.urlForApplication(
                withBundleIdentifier: "com.apple.Terminal")
        else {
            presentError("Terminal is not installed.", title: "rc-git could not open Terminal")
            return
        }
        let folder = URL(fileURLWithPath: workDir, isDirectory: true)
        Task { [weak self] in
            do {
                _ = try await NSWorkspace.shared.open(
                    [folder], withApplicationAt: terminal,
                    configuration: NSWorkspace.OpenConfiguration())
            } catch {
                self?.presentError(
                    "Cannot open \(workDir) in Terminal: \(error.localizedDescription)",
                    title: "rc-git could not open Terminal")
            }
        }
    }

    private func promptForCredentials(
        host: String, kind: CredentialKind, key: String?,
        completion: @escaping (CredentialReply) -> Void
    ) {
        SyncPrompts.credentials(
            on: window, host: host, kind: kind, key: key, completion: completion)
    }

    private func promptForHostKey(
        host: String, hostKey: String, completion: @escaping (HostKeyReply) -> Void
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
                self?.runGuarded(verb: "Create Branch") {
                    try RepositoryGuard.createBranch(
                        repoPath: path, name: name, revspec: ref.target.hex)
                }
            }
        case .newTag:
            promptForName(
                title: "New Tag",
                message: "Create a tag at “\(ref.shortName)” (\(ref.target.short))."
            ) { [weak self] name in
                self?.runGuarded(verb: "Create Tag") {
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
                    presentError(
                        String(describing: error),
                        title: RepositoryGuard.failureTitle(verb: "Delete Branch"))
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
    /// replaces the default error alert.
    private func followDecision(
        _ decision: GuardDecision, verb: String,
        perform: @escaping @Sendable (String?) throws -> Void,
        onSuccess: (() -> Void)?,
        onFailure: ((any Error) -> Void)?
    ) {
        switch decision.kind {
        case .allow:
            runGuarded(
                verb: verb, { try perform(nil) }, onSuccess: onSuccess, onFailure: onFailure)
        case .confirm:
            presentChallenge(decision, verb: verb) { [weak self] input in
                self?.runGuarded(
                    verb: verb, { try perform(input) }, onSuccess: onSuccess,
                    onFailure: onFailure)
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

    /// Asks for a ref name with a one-field sheet. Create stays disabled until a name is
    /// entered; a sheet that still closes without one says so instead of dropping the action.
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
        let watcher = FieldWatcher([field]) {
            let name = RepositoryGuard.enteredRefName(field.stringValue)
            alert.buttons.first?.isEnabled = name != nil
        }
        alert.beginSheetModal(for: window) { [weak self] response in
            withExtendedLifetime(watcher) {}
            guard response == .alertFirstButtonReturn else { return }
            guard let name = RepositoryGuard.enteredRefName(field.stringValue) else {
                self?.presentError("No name was entered, so nothing was created.", title: title)
                return
            }
            completion(name)
        }
    }

    /// Runs one guarded mutation off the main thread. By default success needs no handling —
    /// the watchers see the change and refresh — and failure shows an alert titled by `verb`.
    private func runGuarded(
        verb: String,
        _ work: @escaping @Sendable () throws -> Void,
        onSuccess: (() -> Void)? = nil,
        onFailure: ((any Error) -> Void)? = nil
    ) {
        Task { [weak self] in
            let outcome = await Task.detached { Result { try work() } }.value
            switch outcome {
            case .success:
                onSuccess?()
            case let .failure(error):
                if let onFailure {
                    onFailure(error)
                } else {
                    self?.presentError(
                        Self.failureMessage(error),
                        title: RepositoryGuard.failureTitle(verb: verb))
                }
            }
        }
    }

    /// The text to show for a failed guarded action.
    private static func failureMessage(_ error: any Error) -> String {
        (error as? RepositoryError)?.message ?? String(describing: error)
    }

    private func presentError(
        _ message: String, title: String = RepositoryGuard.readFailureTitle,
        style: NSAlert.Style = .warning
    ) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = style
        alert.beginSheetModal(for: window)
    }

    // MARK: NSToolbarDelegate

    private static let navigationItem = NSToolbarItem.Identifier("rc-git.navigation")
    private static let viewModeItem = NSToolbarItem.Identifier("rc-git.viewMode")
    private static let backToDefaultItem = NSToolbarItem.Identifier("rc-git.backToDefault")
    private static let syncItem = NSToolbarItem.Identifier("rc-git.sync")

    /// No `.sidebarTrackingSeparator`: it is vended by `NSSplitViewController` for the split
    /// it owns, and this window's split sits inside `DropHostViewController` rather than
    /// being the window's own content controller. Tracking a split it cannot measure, the
    /// separator sat off the divider while the sidebar was open and threw the items it
    /// carries across the titlebar when the sidebar collapsed.
    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [
            .toggleSidebar, Self.navigationItem, Self.viewModeItem, .flexibleSpace,
            Self.backToDefaultItem, Self.syncItem,
        ]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        toolbarDefaultItemIdentifiers(toolbar)
    }

    func toolbar(
        _ toolbar: NSToolbar, itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        switch itemIdentifier {
        case Self.navigationItem:
            let control = NSSegmentedControl(
                images: [
                    NSImage(systemSymbolName: "chevron.left", accessibilityDescription: "Back")!,
                    NSImage(
                        systemSymbolName: "chevron.right", accessibilityDescription: "Forward")!,
                ],
                trackingMode: .momentary, target: self, action: #selector(navigationClicked(_:)))
            control.setToolTip("Back (⌘[)", forSegment: 0)
            control.setToolTip("Forward (⌘])", forSegment: 1)
            navigationControl = control
            renderNavigation(backForward)
            let item = NSToolbarItem(itemIdentifier: itemIdentifier)
            item.view = control
            item.label = "Back/Forward"
            return item
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
        case Self.backToDefaultItem:
            let button = NSButton(
                image: NSImage(
                    systemSymbolName: "arrow.uturn.backward",
                    accessibilityDescription: "Back to the default branch")!,
                target: self, action: #selector(backToDefaultBranch(_:)))
            button.bezelStyle = .texturedRounded
            backToDefaultButton = button
            let item = NSToolbarItem(itemIdentifier: itemIdentifier)
            item.view = button
            item.label = "Back to Default"
            item.toolTip = backToDefaultTooltip()
            backToDefaultToolbarItem = item
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

/// The window's outer split. Toggle Sidebar, from the menu or the toolbar, reaches the split
/// controller through the responder chain, so this is where a manual open or close is seen.
private final class SidebarSplitViewController: NSSplitViewController {
    var onToggleSidebar: (() -> Void)?

    override func toggleSidebar(_ sender: Any?) {
        onToggleSidebar?()
        super.toggleSidebar(sender)
    }
}

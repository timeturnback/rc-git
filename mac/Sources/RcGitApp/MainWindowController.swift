import AppKit
import RcGitKit

/// One browser window: refs sidebar, directory tree, file pane. Owns a `BrowserModel` and
/// applies `LayoutPolicy` as the window changes size.
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
    }

    private let outerSplit = NSSplitViewController()
    private let contentSplit = NSSplitViewController()
    private let contentTabs = NSTabViewController()
    private let refsController = RefsViewController()
    private let treeController = TreeViewController()
    private let fileController = FileViewController()
    private let historyController = HistoryViewController()
    private let goToFileController = GoToFileController()
    private let blamePanel = BlamePanelController()
    private var refWatcher: RefWatcher?
    private var modeControl: NSSegmentedControl?
    private var currentLayout: Layout?
    private var sidebarCollapsedByPolicy = false
    private var repositoryName = ""
    private var repositoryPath: String?
    private var hasWorkingTree = false

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1120, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.minSize = NSSize(width: 440, height: 320)
        window.title = "rc-git"
        window.toolbarStyle = .unified
        window.titlebarAppearsTransparent = false
        super.init(window: window)
        window.delegate = self
        // Views must exist before the frame restore below: restoring resizes the window,
        // which fires windowDidResize and runs applyLayout.
        buildSplitViews()
        buildToolbar()
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
            }
        }
        watcher.start()
        refWatcher = watcher
        Task { await model.attach(repository) }
    }

    /// Shows the tree and file panes or the commit history.
    func setMode(_ mode: ContentMode) {
        contentTabs.selectedTabViewItemIndex = mode.rawValue
        modeControl?.selectedSegment = mode.rawValue
        if mode == .history, model.history.isEmpty {
            Task { await model.loadMoreHistory() }
        }
    }

    @objc func showFiles(_ sender: Any?) {
        setMode(.files)
    }

    @objc func showHistory(_ sender: Any?) {
        setMode(.history)
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

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(showBlame(_:)) {
            return model.openedFilePath != nil
        }
        if menuItem.action == #selector(goToFile(_:)) {
            return !isEmpty
        }
        return true
    }

    // MARK: Layout

    func windowDidResize(_ notification: Notification) {
        applyLayout()
    }

    func windowWillClose(_ notification: Notification) {
        refWatcher?.stop()
        refWatcher = nil
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

        let vertical = layout.contentAxis == .horizontal
        if contentSplit.splitView.isVertical != vertical {
            contentSplit.splitView.isVertical = vertical
            contentSplit.splitView.adjustSubviews()
        }
    }

    // MARK: Construction

    private func buildSplitViews() {
        let sidebar = NSSplitViewItem(sidebarWithViewController: refsController)
        sidebar.minimumThickness = 180
        sidebar.maximumThickness = 380
        sidebar.canCollapse = true
        sidebar.collapseBehavior = .preferResizingSplitViewWithFixedSiblings

        let tree = NSSplitViewItem(contentListWithViewController: treeController)
        tree.minimumThickness = 200
        tree.canCollapse = false
        tree.holdingPriority = NSLayoutConstraint.Priority(rawValue: 251)

        let file = NSSplitViewItem(viewController: fileController)
        file.minimumThickness = 240
        file.canCollapse = false
        file.holdingPriority = NSLayoutConstraint.Priority(rawValue: 249)

        contentSplit.addSplitViewItem(tree)
        contentSplit.addSplitViewItem(file)
        contentSplit.splitView.isVertical = true
        contentSplit.splitView.dividerStyle = .thin
        contentSplit.splitView.autosaveName = "rc-git.content"

        contentTabs.tabStyle = .unspecified
        contentTabs.addTabViewItem(NSTabViewItem(viewController: contentSplit))
        contentTabs.addTabViewItem(NSTabViewItem(viewController: historyController))

        outerSplit.addSplitViewItem(sidebar)
        outerSplit.addSplitViewItem(NSSplitViewItem(viewController: contentTabs))
        outerSplit.splitView.autosaveName = "rc-git.main"
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
            historyController.reload(model.history, complete: model.historyComplete)
        }
        model.onDiffChanged = { [weak self] in
            guard let self else { return }
            historyController.renderDetail(changes: model.commitChanges, state: model.fileDiff)
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
        fileController.onLoadAnyway = { [weak self] entry, limit in
            guard let self else { return }
            Task { await self.model.openFile(entry, limit: limit) }
        }
        fileController.highlightProvider = { [weak self] entry, document in
            guard let self else { return nil }
            return await model.highlights(for: entry, document: document)
        }
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
        switch decision.kind {
        case .allow:
            runGuarded { _ = try perform(nil) }
        case .confirm:
            presentChallenge(decision, verb: verb) { [weak self] input in
                self?.runGuarded { _ = try perform(input) }
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

    /// Runs one guarded mutation off the main thread and reports its failure. Success needs
    /// no handling: the ref watcher sees the change and refreshes the sidebar.
    private func runGuarded(_ work: @escaping @Sendable () throws -> Void) {
        Task { [weak self] in
            let outcome = await Task.detached { Result { try work() } }.value
            if case let .failure(error) = outcome {
                self?.presentError(String(describing: error))
            }
        }
    }

    private func presentError(_ message: String) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = "rc-git could not read the repository"
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.beginSheetModal(for: window)
    }

    // MARK: NSToolbarDelegate

    private static let viewModeItem = NSToolbarItem.Identifier("rc-git.viewMode")

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.toggleSidebar, .sidebarTrackingSeparator, Self.viewModeItem, .flexibleSpace]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        toolbarDefaultItemIdentifiers(toolbar)
    }

    func toolbar(
        _ toolbar: NSToolbar, itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        guard itemIdentifier == Self.viewModeItem else { return nil }
        let control = NSSegmentedControl(
            labels: ["Files", "History"], trackingMode: .selectOne, target: self,
            action: #selector(modeChanged(_:)))
        control.selectedSegment = ContentMode.files.rawValue
        modeControl = control
        let item = NSToolbarItem(itemIdentifier: itemIdentifier)
        item.view = control
        item.label = "View"
        return item
    }
}

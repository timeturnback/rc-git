import AppKit
import RcGitKit
import os

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    /// The one credential store every window and the updater share, so a secret is read from
    /// the keychain once per launch rather than once per network step.
    nonisolated static let credentialStore: CredentialStoring = CachingCredentialStore(
        wrapping: KeychainCredentialStore())

    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "rcgit", category: "tabs")

    private var controllers: [MainWindowController] = []
    private let recents = RecentRepositories()
    private let hiddenFiles = HiddenFilesPreference()
    private let openTabs = OpenTabs()
    private var closedTabs = ClosedTabs()
    private let recentMenu = NSMenu(title: "Open Recent")
    private let build = AppBuild.current
    private var updateController: UpdateWindowController?
    /// Set while the tabs are handed to a relaunching build; see `handOffTabs` and
    /// `reclaimTabs`.
    private var tabsHandedOff = false
    /// The stops the user went through, across every tab, and which tab's moves count. In
    /// memory only: a stop names a tab, and closing the tab drops its stops.
    private var navigator = Navigator()
    /// Set while ⇧⌘W closes a window group and once the app is quitting: tabs closed then
    /// stay in the saved list. A tab closed on its own leaves it.
    private var keepingClosedTabs = false
    /// False until `applicationDidFinishLaunching` returns. A document that arrives before
    /// then (Finder, the Dock, `open -a`) is what launched the app.
    private var hasFinishedLaunching = false
    /// The repository a document opened before launch finished; `restoreTabs` keeps its tab
    /// selected over the saved selection.
    private var launchDocument: String?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = MainMenu.build(
            recentMenu: recentMenu, appName: build.name, updateMode: build.updateMode)
        rebuildRecentMenu()
        watchNavigationButtons()
        NSApp.activate(ignoringOtherApps: true)
        // AppKit has already handed every argv path to `application(_:openFile:)`, skipping
        // `-Key value` pairs, so argv is not read here; `restoreTabs` keeps that tab selected.
        restoreTabs()
        hasFinishedLaunching = true
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        keepingClosedTabs = true
        return .terminateNow
    }

    func applicationWillTerminate(_ notification: Notification) {
        saveTabs()
    }

    func application(_ sender: NSApplication, openFile filename: String) -> Bool {
        let controller = open(path: filename)
        if !hasFinishedLaunching, let path = controller?.repositoryPath {
            launchDocument = path
        }
        return true
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool)
        -> Bool
    {
        if !flag { newWindow().showWindow(nil) }
        return true
    }

    @objc func openDocument(_ sender: Any?) {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Choose a git repository: its working directory or its .git directory."
        panel.prompt = "Open"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        open(path: url.path)
    }

    @objc func openRecent(_ sender: NSMenuItem) {
        guard let path = sender.representedObject as? String else { return }
        open(path: path)
    }

    @objc func clearRecents(_ sender: Any?) {
        recents.clear()
        rebuildRecentMenu()
    }

    /// Opens the update window, which rebuilds this bundle from the checkout it was built
    /// from and relaunches into it: from upstream for the release app, from the working
    /// tree as it stands for the dev bundle. An unstamped build has no menu item.
    @objc func showUpdate(_ sender: Any?) {
        updateWindow()
    }

    /// Opens the update window and starts the rebuild without waiting for its button: the
    /// dev bundle's edit-run loop. The window still shows the log, and a failure stops there.
    @objc func rebuildNow(_ sender: Any?) {
        updateWindow()?.start()
    }

    /// Nil for an unstamped build, which has no menu item to reach this.
    @discardableResult private func updateWindow() -> UpdateWindowController? {
        guard
            let controller = updateController
                ?? UpdateWindowController(
                    build: build, handOffState: { [weak self] in self?.handOffTabs() },
                    reclaimState: { [weak self] in self?.reclaimTabs() })
        else { return nil }
        updateController = controller
        controller.showWindow(nil)
        controller.window?.makeKeyAndOrderFront(nil)
        return controller
    }

    /// Shows or hides the entries `HiddenFiles` names in every tab, and remembers the choice
    /// for the next launch.
    @objc func toggleHiddenFiles(_ sender: Any?) {
        let shows = hiddenFiles.toggle()
        for controller in controllers {
            controller.setShowsHiddenFiles(shows)
        }
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(toggleHiddenFiles(_:)) {
            menuItem.state = hiddenFiles.shows ? .on : .off
        }
        if menuItem.action == #selector(clearRecents(_:)) {
            return !recents.paths.isEmpty
        }
        if menuItem.action == #selector(reopenClosedTab(_:)) {
            return closedTabs.canReopen(skipping: openPaths)
        }
        if menuItem.action == #selector(goBack(_:)) {
            return navigator.canGo(.back)
        }
        if menuItem.action == #selector(goForward(_:)) {
            return navigator.canGo(.forward)
        }
        return true
    }

    /// Adds an empty tab; also gives the tab bar its "+" button (AppKit shows it when
    /// something in the responder chain implements this selector).
    @objc func newWindowForTab(_ sender: Any?) {
        newWindow().showWindow(nil)
    }

    /// Closes every tab of the front window group and keeps their repositories in the saved
    /// tab list; ⌘W closes only the tab on screen and drops it — see `saveTabs`.
    @objc func closeWindowGroup(_ sender: Any?) {
        guard let window = NSApp.keyWindow ?? NSApp.mainWindow else { return }
        keepingClosedTabs = true
        defer { keepingClosedTabs = false }
        for tab in window.tabGroup?.windows ?? [window] { tab.performClose(nil) }
    }

    /// Reopens the repository of the most recently closed tab, as a browser's ⇧⌘T does.
    @objc func reopenClosedTab(_ sender: Any?) {
        guard let path = closedTabs.pop(skipping: openPaths) else { return }
        open(path: path)
    }

    private var openPaths: Set<String> {
        Set(controllers.compactMap(\.repositoryPath))
    }

    /// Opens a repository in an empty tab, or a new one when every tab is in use. A
    /// repository that is already open gets its existing tab selected instead. Returns the
    /// tab, or nil after alerting that the repository did not open.
    @discardableResult
    func open(path: String) -> MainWindowController? {
        do {
            let tab = try openTab(path)
            guard tab.isNew, let path = tab.controller.repositoryPath else {
                tab.controller.showWindow(nil)
                return tab.controller
            }
            Self.logger.notice("opened \(path, privacy: .public)")
            recents.note(path)
            rebuildRecentMenu()
            saveTabs()
            return tab.controller
        } catch {
            let message = "open: could not open \(path): \(error)"
            Self.logger.error("\(message, privacy: .public)")
            let alert = NSAlert()
            alert.messageText = "Could not open repository"
            alert.informativeText = String(describing: error)
            alert.alertStyle = .warning
            alert.runModal()
            return nil
        }
    }

    /// The one opener every arrival goes through — a menu, Finder, argv, a restore. Dedupes
    /// on `Repository.tabKey`: a repository that is already open, under any spelling of its
    /// working directory or its `.git`, returns its tab, not a second.
    ///
    /// The path is canonical (`CanonicalPath.resolve`), so tabs, recents and the sync sheet's
    /// `cd` hold neither a relative path nor a `/private`-stripped one. The same path as an
    /// open tab skips opening the repository again. The tab shows and saves
    /// `Repository.tabPath`: the working directory even when `path` named its `.git`.
    ///
    /// - Throws: The error `Repository(path:)` throws when `path` does not open.
    private func openTab(_ path: String) throws -> (controller: MainWindowController, isNew: Bool) {
        let path = CanonicalPath.resolve(path)
        if let existing = controllers.first(where: { $0.repositoryPath == path }) {
            return (existing, false)
        }
        let repository = try Repository(path: path)
        let key = repository.tabKey
        if let existing = controllers.first(where: { $0.repositoryKey == key }) {
            return (existing, false)
        }
        let controller = controllers.first(where: { $0.isEmpty }) ?? newWindow()
        controller.show(repository, path: repository.tabPath(openedAs: path), key: key)
        return (controller, true)
    }

    @discardableResult
    private func newWindow() -> MainWindowController {
        let controller = MainWindowController()
        if let window = controller.window,
            let host = controllers.first(where: { $0.window === NSApp.mainWindow })?.window
                ?? controllers.last?.window
        {
            host.addTabbedWindow(window, ordered: .above)
        }
        controllers.append(controller)
        let tab = ObjectIdentifier(controller)
        controller.onClose = { [weak self, weak controller] in
            guard let self else { return }
            let path = controller?.repositoryPath
            if let path { closedTabs.push(path) }
            controllers.removeAll { $0 === controller }
            saveTabs(dropping: keepingClosedTabs ? nil : path)
            navigator.tabClosed(tab, repositoryPath: path)
            renderNavigation()
        }
        controller.onStop = { [weak self] stop in
            guard let self, navigator.report(stop, from: tab) else { return }
            renderNavigation()
        }
        controller.onBecomeMain = { [weak self, weak controller] in
            guard let self, let controller,
                navigator.tabCameToFront(tab, at: controller.currentStop)
            else { return }
            renderNavigation()
        }
        controller.onNavigate = { [weak self] direction in
            self?.go(direction)
        }
        controller.renderNavigation(navigator.list)
        return controller
    }

    // MARK: Back and forward

    @objc func goBack(_ sender: Any?) {
        go(.back)
    }

    @objc func goForward(_ sender: Any?) {
        go(.forward)
    }

    /// Steps to the stop before or after the current one: brings its tab to the front and
    /// has it show the stop.
    private func go(_ direction: BackForwardList.Direction) {
        guard let stop = navigator.go(direction) else { return }
        renderNavigation()
        guard
            let controller = controllers.first(where: {
                $0.repositoryPath == stop.repositoryPath
            })
        else { return }
        navigator.beginRestore(in: ObjectIdentifier(controller))
        controller.showWindow(nil)
        Task {
            await controller.restore(stop)
            navigator.endRestore()
        }
    }

    private func renderNavigation() {
        for controller in controllers {
            controller.renderNavigation(navigator.list)
        }
    }

    /// The fourth and fifth buttons of a mouse step back and forward, as they do in a
    /// browser. No view takes those buttons, so one monitor covers every browser window.
    private func watchNavigationButtons() {
        NSEvent.addLocalMonitorForEvents(matching: .otherMouseDown) { [weak self] event in
            guard let self,
                let direction = BackForwardList.Direction(mouseButton: event.buttonNumber),
                controllers.contains(where: { $0.window === event.window })
            else { return event }
            go(direction)
            return nil
        }
    }

    // MARK: Tab persistence

    /// Reopens the tabs of the previous run and selects the tab that was selected then — or
    /// the launch document's tab, when opening one launched the app. Falls back to one empty
    /// tab.
    ///
    /// Restore goes through `openTab`, so a repository a launch document already opened is
    /// not opened twice. The saved list becomes the tabs now on screen, launch document
    /// included, followed by the repositories whose volume is not mounted yet; one that is
    /// gone from a reachable folder is dropped, so it is not retried on every launch (see
    /// `OpenTabs.restoredPaths`). Each repository that failed to open is logged once and
    /// named in a banner on the front window (`RestoreNotice`).
    private func restoreTabs() {
        let saved = openTabs.paths
        let savedSelection = openTabs.selectedIndex
        var failed: [String] = []
        for path in saved {
            do {
                _ = try openTab(path)
            } catch {
                failed.append(path)
                let message = "restore tab: could not open \(path): \(error)"
                Self.logger.error("\(message, privacy: .public)")
            }
        }
        let live = tabState().paths
        Self.logger.notice(
            "restore tabs: \(saved.count) saved, \(live.count) open, \(failed.count) failed")
        let kept = OpenTabs.restoredPaths(opened: live, failed: failed)
        for path in failed where !kept.contains(path) {
            Self.logger.notice("restore tab: dropped \(path, privacy: .public) from the saved tabs")
        }
        let notice = RestoreNotice.make(failed: failed, kept: kept)
        guard !live.isEmpty else {
            openTabs.save(paths: kept, selectedIndex: 0)
            let front = newWindow()
            front.showWindow(nil)
            if let notice { front.showRestoreNotice(notice) }
            return
        }
        let selected = OpenTabs.restoredSelection(
            saved: saved, selectedIndex: savedSelection, opened: live,
            launchDocument: launchDocument)
        let front = controllers.first { $0.repositoryPath == live[selected] }
        front?.showWindow(nil)
        if let notice { front?.showRestoreNotice(notice) }
        openTabs.save(paths: kept, selectedIndex: selected)
    }

    /// Records the open repositories in on-screen tab order plus the selected tab. Runs on
    /// every open and close and once more at quit, which also captures drag reordering, until
    /// the tabs are handed off: a self-update relaunch goes through `handOffTabs`, and every
    /// call after it returns without writing.
    ///
    /// `dropping` names the repository of a tab closed with ⌘W, the one close that removes it
    /// from the list. A whole window closed with ⇧⌘W and quitting with ⌘Q leave the saved
    /// repositories in the list, so the next launch reopens them.
    func saveTabs(dropping: String? = nil) {
        // This process outlives the launch of its replacement. A write after the handover
        // would land on top of whatever the new process has already recorded.
        guard !tabsHandedOff else { return }
        let state = tabState()
        openTabs.record(
            live: state.paths, selectedIndex: state.selectedIndex, dropping: dropping)
    }

    /// Hands the tabs to the build that is about to replace this one, flushed so the new
    /// process reads them, and stops recording after that.
    func handOffTabs() {
        let state = tabState()
        openTabs.handOff(live: state.paths, selectedIndex: state.selectedIndex)
        tabsHandedOff = true
    }

    /// Undoes `handOffTabs` after a relaunch that failed: this process keeps running, so it
    /// records its tabs again, starting with the ones on screen now.
    func reclaimTabs() {
        tabsHandedOff = false
        saveTabs()
    }

    private func tabState() -> (paths: [String], selectedIndex: Int) {
        var ordered: [MainWindowController] = []
        if let group = controllers.first?.window?.tabGroup {
            ordered = group.windows.compactMap { window in
                controllers.first { $0.window === window }
            }
        }
        for controller in controllers where !ordered.contains(where: { $0 === controller }) {
            ordered.append(controller)
        }
        let withRepository = ordered.filter { $0.repositoryPath != nil }
        let selectedWindow = controllers.first?.window?.tabGroup?.selectedWindow
        return (
            withRepository.compactMap(\.repositoryPath),
            withRepository.firstIndex { $0.window === selectedWindow } ?? 0
        )
    }

    private func rebuildRecentMenu() {
        recentMenu.removeAllItems()
        for path in recents.paths {
            let item = NSMenuItem(
                title: (path as NSString).abbreviatingWithTildeInPath,
                action: #selector(openRecent(_:)), keyEquivalent: "")
            item.representedObject = path
            item.target = self
            recentMenu.addItem(item)
        }
        if !recents.paths.isEmpty { recentMenu.addItem(.separator()) }
        let clear = NSMenuItem(
            title: "Clear Menu", action: #selector(clearRecents(_:)), keyEquivalent: "")
        clear.target = self
        recentMenu.addItem(clear)
    }
}

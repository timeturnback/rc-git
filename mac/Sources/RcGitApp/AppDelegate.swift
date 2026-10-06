import AppKit
import RcGitKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    private var controllers: [MainWindowController] = []
    private let recents = RecentRepositories()
    private let openTabs = OpenTabs()
    private var closedTabs = ClosedTabs()
    private let recentMenu = NSMenu(title: "Open Recent")
    private let build = AppBuild.current
    private var updateController: UpdateWindowController?
    /// Set once the tabs were handed to a relaunching build; see `handOffTabs`.
    private var tabsHandedOff = false
    /// The stops the user went through, across every tab. In memory only: a stop names a
    /// tab, and closing the tab drops its stops.
    private var backForward = BackForwardList()
    /// The tab whose moves are the user's: the one in the main window.
    private weak var frontController: MainWindowController?
    /// How many stops are being restored. What a tab reports meanwhile is the restore itself.
    private var restoresInFlight = 0

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = MainMenu.build(
            recentMenu: recentMenu, appName: build.name, updateMode: build.updateMode)
        rebuildRecentMenu()
        watchNavigationButtons()
        NSApp.activate(ignoringOtherApps: true)
        let paths = CommandLine.arguments.dropFirst().filter { !$0.hasPrefix("-") }
        if paths.isEmpty {
            restoreTabs()
        } else {
            for path in paths { open(path: path) }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        saveTabs()
    }

    func application(_ sender: NSApplication, openFile filename: String) -> Bool {
        open(path: filename)
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
        let controller =
            updateController
            ?? UpdateWindowController(
                build: build, handOffState: { [weak self] in self?.handOffTabs() })
        updateController = controller
        controller.showWindow(nil)
        controller.window?.makeKeyAndOrderFront(nil)
    }

    /// Shows or hides the entries `HiddenFiles` names in every tab, and remembers the choice
    /// for the next launch.
    @objc func toggleHiddenFiles(_ sender: Any?) {
        let shows = !UserDefaults.standard.bool(forKey: HiddenFiles.preferenceKey)
        UserDefaults.standard.set(shows, forKey: HiddenFiles.preferenceKey)
        for controller in controllers {
            controller.setShowsHiddenFiles(shows)
        }
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(toggleHiddenFiles(_:)) {
            menuItem.state =
                UserDefaults.standard.bool(forKey: HiddenFiles.preferenceKey) ? .on : .off
        }
        if menuItem.action == #selector(clearRecents(_:)) {
            return !recents.paths.isEmpty
        }
        if menuItem.action == #selector(reopenClosedTab(_:)) {
            return closedTabs.canReopen(skipping: openPaths)
        }
        if menuItem.action == #selector(goBack(_:)) {
            return backForward.canGo(.back)
        }
        if menuItem.action == #selector(goForward(_:)) {
            return backForward.canGo(.forward)
        }
        return true
    }

    /// Adds an empty tab; also gives the tab bar its "+" button (AppKit shows it when
    /// something in the responder chain implements this selector).
    @objc func newWindowForTab(_ sender: Any?) {
        newWindow().showWindow(nil)
    }

    /// Closes every tab of the front window group; ⌘W closes only the tab on screen. Neither
    /// drops a repository from the saved tab list — see `saveTabs`.
    @objc func closeWindowGroup(_ sender: Any?) {
        guard let window = NSApp.keyWindow ?? NSApp.mainWindow else { return }
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
    /// repository that is already open gets its existing tab selected instead.
    func open(path: String) {
        let path = (path as NSString).standardizingPath
        if let existing = controllers.first(where: { $0.repositoryPath == path }) {
            existing.showWindow(nil)
            return
        }
        do {
            let repository = try Repository(path: path)
            recents.note(path)
            rebuildRecentMenu()
            let controller = controllers.first(where: { $0.isEmpty }) ?? newWindow()
            controller.show(repository, path: path)
            saveTabs()
        } catch {
            let alert = NSAlert()
            alert.messageText = "Could not open repository"
            alert.informativeText = String(describing: error)
            alert.alertStyle = .warning
            alert.runModal()
        }
    }

    @discardableResult
    private func newWindow() -> MainWindowController {
        let controller = MainWindowController()
        if let window = controller.window,
            let host = NSApp.mainWindow ?? controllers.last?.window
        {
            host.addTabbedWindow(window, ordered: .above)
        }
        controllers.append(controller)
        controller.onClose = { [weak self, weak controller] in
            if let path = controller?.repositoryPath { self?.closedTabs.push(path) }
            self?.controllers.removeAll { $0 === controller }
            self?.saveTabs()
            self?.forgetStops(of: controller)
        }
        controller.onStop = { [weak self, weak controller] stop in
            self?.record(stop, from: controller)
        }
        controller.onBecomeMain = { [weak self, weak controller] in
            self?.cameToFront(controller)
        }
        controller.onNavigate = { [weak self] direction in
            self?.go(direction)
        }
        controller.renderNavigation(backForward)
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
        guard let stop = backForward.go(direction) else { return }
        renderNavigation()
        guard
            let controller = controllers.first(where: {
                $0.repositoryPath == stop.repositoryPath
            })
        else { return }
        frontController = controller
        controller.showWindow(nil)
        restoresInFlight += 1
        Task {
            await controller.restore(stop)
            restoresInFlight -= 1
        }
    }

    /// Records where the front tab is. A background tab's moves are not the user going
    /// anywhere — a repository attaching at launch, a ref refresh — and neither is a stop
    /// being restored.
    private func record(_ stop: NavigationStop, from controller: MainWindowController?) {
        guard restoresInFlight == 0, let controller, controller === frontController else {
            return
        }
        backForward.visit(stop)
        renderNavigation()
    }

    /// A switch to another tab is a move to where that tab is. The same window becoming
    /// main again, when the app is reactivated, is not.
    private func cameToFront(_ controller: MainWindowController?) {
        guard let controller, controller !== frontController else { return }
        frontController = controller
        if let stop = controller.currentStop {
            record(stop, from: controller)
        }
    }

    private func forgetStops(of controller: MainWindowController?) {
        guard let path = controller?.repositoryPath else { return }
        backForward.removeStops(inRepository: path)
        renderNavigation()
    }

    private func renderNavigation() {
        for controller in controllers {
            controller.renderNavigation(backForward)
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

    /// Reopens the tabs of the previous run, silently skipping repositories that no longer
    /// open, and selects the tab that was selected then. Falls back to one empty tab.
    ///
    /// This is the only place the list shrinks: a path that no longer opens is dropped here,
    /// so it is not retried on every launch. Closing a tab never drops one — see `saveTabs`.
    private func restoreTabs() {
        var opened: [MainWindowController] = []
        for path in openTabs.paths {
            guard let repository = try? Repository(path: path) else { continue }
            let controller = newWindow()
            controller.show(repository, path: path)
            opened.append(controller)
        }
        guard !opened.isEmpty else {
            openTabs.clear()
            newWindow().showWindow(nil)
            return
        }
        let selected = OpenTabs.clampedSelection(openTabs.selectedIndex, count: opened.count)
        opened[selected].showWindow(nil)
        openTabs.save(paths: opened.compactMap(\.repositoryPath), selectedIndex: selected)
    }

    /// Records the open repositories in on-screen tab order plus the selected tab. Runs on
    /// every open and close, once more at quit, and once before a self-update relaunches,
    /// which also captures drag reordering.
    ///
    /// Adding only: a tab closed with ⌘W, a whole window closed with ⇧⌘W, and quitting with
    /// ⌘Q all leave the saved repositories in the list, so the next launch reopens them.
    func saveTabs() {
        // This process outlives the launch of its replacement. A write after the handover
        // would land on top of whatever the new process has already recorded.
        guard !tabsHandedOff else { return }
        let state = tabState()
        openTabs.record(live: state.paths, selectedIndex: state.selectedIndex)
    }

    /// Hands the tabs to the build that is about to replace this one, flushed so the new
    /// process reads them, and stops recording after that.
    func handOffTabs() {
        let state = tabState()
        openTabs.handOff(live: state.paths, selectedIndex: state.selectedIndex)
        tabsHandedOff = true
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
        clear.isEnabled = !recents.paths.isEmpty
        recentMenu.addItem(clear)
    }
}

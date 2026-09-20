import AppKit
import RcGitKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var controllers: [MainWindowController] = []
    private let recents = RecentRepositories()
    private let openTabs = OpenTabs()
    private let recentMenu = NSMenu(title: "Open Recent")
    private let build = AppBuild.current
    private var updateController: UpdateWindowController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = MainMenu.build(
            recentMenu: recentMenu, appName: build.name, canUpdate: build.canSelfUpdate)
        rebuildRecentMenu()
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

    /// Opens the update window, which rebuilds the release bundle from the checkout it was
    /// built from and relaunches into it. Only the release bundle has the menu item.
    @objc func showUpdate(_ sender: Any?) {
        let controller =
            updateController
            ?? UpdateWindowController(build: build, saveState: { [weak self] in self?.saveTabs() })
        updateController = controller
        controller.showWindow(nil)
        controller.window?.makeKeyAndOrderFront(nil)
    }

    /// Adds an empty tab; also gives the tab bar its "+" button (AppKit shows it when
    /// something in the responder chain implements this selector).
    @objc func newWindowForTab(_ sender: Any?) {
        newWindow().showWindow(nil)
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
            self?.controllers.removeAll { $0 === controller }
            self?.saveTabs()
        }
        return controller
    }

    // MARK: Tab persistence

    /// Reopens the tabs of the previous run, silently skipping repositories that no longer
    /// open, and selects the tab that was selected then. Falls back to one empty tab.
    private func restoreTabs() {
        var opened: [MainWindowController] = []
        for path in openTabs.paths {
            guard let repository = try? Repository(path: path) else { continue }
            let controller = newWindow()
            controller.show(repository, path: path)
            opened.append(controller)
        }
        guard !opened.isEmpty else {
            newWindow().showWindow(nil)
            return
        }
        opened[OpenTabs.clampedSelection(openTabs.selectedIndex, count: opened.count)]
            .showWindow(nil)
        saveTabs()
    }

    /// Records the open repositories in on-screen tab order plus the selected tab. Runs on
    /// every open and close, once more at quit, and once before a self-update relaunches,
    /// which also captures drag reordering.
    func saveTabs() {
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
        openTabs.save(
            paths: withRepository.compactMap(\.repositoryPath),
            selectedIndex: withRepository.firstIndex { $0.window === selectedWindow } ?? 0)
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

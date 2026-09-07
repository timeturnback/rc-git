import AppKit
import RcGitKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var controllers: [MainWindowController] = []
    private let recents = RecentRepositories()
    private let recentMenu = NSMenu(title: "Open Recent")

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = MainMenu.build(recentMenu: recentMenu)
        rebuildRecentMenu()
        NSApp.activate(ignoringOtherApps: true)
        let paths = CommandLine.arguments.dropFirst().filter { !$0.hasPrefix("-") }
        if paths.isEmpty {
            newWindow().showWindow(nil)
        } else {
            for path in paths { open(path: path) }
        }
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

    /// Opens a repository in an empty window, or a new one when every window is in use.
    func open(path: String) {
        let path = (path as NSString).standardizingPath
        do {
            let repository = try Repository(path: path)
            recents.note(path)
            rebuildRecentMenu()
            let controller = controllers.first(where: { $0.isEmpty }) ?? newWindow()
            controller.show(repository, path: path)
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
        if let last = controllers.last?.window, let window = controller.window {
            window.cascadeTopLeft(from: NSPoint(x: last.frame.minX, y: last.frame.maxY))
        }
        controllers.append(controller)
        controller.onClose = { [weak self, weak controller] in
            self?.controllers.removeAll { $0 === controller }
        }
        return controller
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

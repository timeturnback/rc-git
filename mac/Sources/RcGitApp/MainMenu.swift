import AppKit
import RcGitKit

/// The main menu, built in code so the app needs no nib. Actions use the standard AppKit
/// selectors and travel the responder chain; only the open and recent items point at the
/// app delegate.
@MainActor
enum MainMenu {
    /// `updateMode` adds the item that rebuilds the app: the release bundle updates from
    /// upstream, the dev bundle rebuilds the working tree. An unstamped build has neither,
    /// because it does not know which checkout to build from.
    static func build(recentMenu: NSMenu, appName: String, updateMode: UpdateMode?) -> NSMenu {
        let main = NSMenu()
        main.addItem(submenu(appMenu(appName: appName, updateMode: updateMode)))
        main.addItem(submenu(fileMenu(recentMenu: recentMenu)))
        main.addItem(submenu(editMenu()))
        main.addItem(submenu(viewMenu()))
        main.addItem(submenu(repositoryMenu()))
        main.addItem(submenu(windowMenu()))
        return main
    }

    private static func submenu(_ menu: NSMenu) -> NSMenuItem {
        let item = NSMenuItem()
        item.submenu = menu
        return item
    }

    private static func item(
        _ title: String, _ action: Selector?, _ key: String,
        _ modifiers: NSEvent.ModifierFlags = .command
    ) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        item.keyEquivalentModifierMask = modifiers
        return item
    }

    private static func appMenu(appName: String, updateMode: UpdateMode?) -> NSMenu {
        let menu = NSMenu(title: appName)
        menu.addItem(
            item(
                "About \(appName)", #selector(NSApplication.orderFrontStandardAboutPanel(_:)),
                ""))
        if let updateMode {
            menu.addItem(.separator())
            // The dev bundle earns a key equivalent: rebuilding is the edit-run loop, not
            // an occasional errand. Cmd-R is free; the Repository menu's Sync is Cmd-Shift-R.
            switch updateMode {
            case .update:
                menu.addItem(item("Update \(appName)…", #selector(AppDelegate.showUpdate(_:)), ""))
            case .rebuild:
                menu.addItem(
                    item("Rebuild and Relaunch…", #selector(AppDelegate.showUpdate(_:)), "r"))
            }
        }
        menu.addItem(.separator())
        menu.addItem(item("Hide \(appName)", #selector(NSApplication.hide(_:)), "h"))
        menu.addItem(
            item(
                "Hide Others", #selector(NSApplication.hideOtherApplications(_:)), "h",
                [.command, .option]))
        menu.addItem(item("Show All", #selector(NSApplication.unhideAllApplications(_:)), ""))
        menu.addItem(.separator())
        menu.addItem(item("Quit \(appName)", #selector(NSApplication.terminate(_:)), "q"))
        return menu
    }

    private static func fileMenu(recentMenu: NSMenu) -> NSMenu {
        let menu = NSMenu(title: "File")
        menu.addItem(item("New Tab", #selector(AppDelegate.newWindowForTab(_:)), "t"))
        menu.addItem(item("Open Repository…", #selector(AppDelegate.openDocument(_:)), "o"))
        let recent = NSMenuItem(title: "Open Recent", action: nil, keyEquivalent: "")
        recent.submenu = recentMenu
        menu.addItem(recent)
        menu.addItem(.separator())
        menu.addItem(
            item(
                "Go to File…", #selector(MainWindowController.goToFile(_:)), "o",
                [.command, .shift]))
        menu.addItem(item("Go to Branch…", #selector(MainWindowController.chooseRef(_:)), "b"))
        menu.addItem(.separator())
        menu.addItem(item("Close Tab", #selector(NSWindow.performClose(_:)), "w"))
        menu.addItem(
            item(
                "Close Window", #selector(AppDelegate.closeWindowGroup(_:)), "w",
                [.command, .shift]))
        return menu
    }

    private static func editMenu() -> NSMenu {
        let menu = NSMenu(title: "Edit")
        menu.addItem(item("Copy", #selector(NSText.copy(_:)), "c"))
        menu.addItem(item("Select All", #selector(NSText.selectAll(_:)), "a"))
        menu.addItem(.separator())
        let find = NSMenu(title: "Find")
        for (title, key, tag) in [
            ("Find…", "f", 1), ("Find Next", "g", 2), ("Find Previous", "G", 3),
            ("Use Selection for Find", "e", 7),
        ] {
            let entry = item(title, #selector(NSTextView.performFindPanelAction(_:)), key)
            entry.tag = tag
            find.addItem(entry)
        }
        let findItem = NSMenuItem(title: "Find", action: nil, keyEquivalent: "")
        findItem.submenu = find
        menu.addItem(findItem)
        return menu
    }

    private static func viewMenu() -> NSMenu {
        let menu = NSMenu(title: "View")
        menu.addItem(item("Files", #selector(MainWindowController.showFiles(_:)), "1"))
        menu.addItem(item("History", #selector(MainWindowController.showHistory(_:)), "2"))
        menu.addItem(item("Changes", #selector(MainWindowController.showChanges(_:)), "3"))
        menu.addItem(.separator())
        // Finder's and Safari's shortcuts. The tab switch in the Window menu adds Shift.
        menu.addItem(item("Back", #selector(AppDelegate.goBack(_:)), "["))
        menu.addItem(item("Forward", #selector(AppDelegate.goForward(_:)), "]"))
        menu.addItem(.separator())
        // Finder's shortcut for the same toggle.
        menu.addItem(
            item(
                "Show Hidden Files", #selector(AppDelegate.toggleHiddenFiles(_:)), ".",
                [.command, .shift]))
        menu.addItem(.separator())
        menu.addItem(
            item(
                "Show Blame", #selector(MainWindowController.showBlame(_:)), "b",
                [.command, .option]))
        menu.addItem(
            item(
                "History of This File", #selector(MainWindowController.showFileHistory(_:)), "h",
                [.command, .option]))
        menu.addItem(
            item(
                "Search Commits", #selector(MainWindowController.searchCommits(_:)), "F",
                [.command, .shift]))
        menu.addItem(.separator())
        menu.addItem(
            item(
                "Toggle Sidebar", #selector(NSSplitViewController.toggleSidebar(_:)), "s",
                [.command, .control]))
        menu.addItem(
            item(
                "Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), "f",
                [.command, .control]))
        return menu
    }

    private static func repositoryMenu() -> NSMenu {
        let menu = NSMenu(title: "Repository")
        menu.addItem(
            item(
                "Fetch", #selector(MainWindowController.fetchRemote(_:)), "f",
                [.command, .option]))
        menu.addItem(
            item(
                "Pull", #selector(MainWindowController.pullRemote(_:)), "u",
                [.command, .shift]))
        menu.addItem(
            item(
                "Push", #selector(MainWindowController.pushRemote(_:)), "p",
                [.command, .shift]))
        menu.addItem(.separator())
        menu.addItem(
            item(
                "Sync", #selector(MainWindowController.syncRepository(_:)), "r",
                [.command, .shift]))
        // The title is rewritten with the branch name in `validateMenuItem`.
        menu.addItem(
            item(
                SyncTitles.backToDefault(nil),
                #selector(MainWindowController.backToDefaultBranch(_:)), "m",
                [.command, .shift]))
        menu.addItem(
            item(
                "Cancel Sync", #selector(MainWindowController.cancelSync(_:)), ".",
                [.command]))
        return menu
    }

    private static func windowMenu() -> NSMenu {
        let menu = NSMenu(title: "Window")
        menu.addItem(item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"))
        menu.addItem(item("Zoom", #selector(NSWindow.performZoom(_:)), ""))
        menu.addItem(.separator())
        menu.addItem(
            item(
                "Show Previous Tab", #selector(NSWindow.selectPreviousTab(_:)), "[",
                [.command, .shift]))
        menu.addItem(
            item(
                "Show Next Tab", #selector(NSWindow.selectNextTab(_:)), "]",
                [.command, .shift]))
        menu.addItem(
            item(
                "Move Tab to New Window", #selector(NSWindow.moveTabToNewWindow(_:)), ""))
        menu.addItem(item("Merge All Windows", #selector(NSWindow.mergeAllWindows(_:)), ""))
        menu.addItem(.separator())
        menu.addItem(item("Bring All to Front", #selector(NSApplication.arrangeInFront(_:)), ""))
        NSApp.windowsMenu = menu
        return menu
    }
}

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

    private static let keys = MenuShortcuts()

    private static func item(_ title: String, _ action: Selector?) -> NSMenuItem {
        NSMenuItem(title: title, action: action, keyEquivalent: "")
    }

    /// An item whose key equivalent comes from `MenuShortcuts`; `title` overrides the row's
    /// placeholder for the items named after the app or the branch.
    private static func item(
        _ shortcut: MenuShortcut, _ action: Selector?, title: String? = nil
    ) -> NSMenuItem {
        let item = NSMenuItem(
            title: title ?? shortcut.title, action: action, keyEquivalent: shortcut.key)
        item.keyEquivalentModifierMask = modifierFlags(shortcut.modifiers)
        return item
    }

    private static func modifierFlags(_ modifiers: MenuModifiers) -> NSEvent.ModifierFlags {
        var flags: NSEvent.ModifierFlags = []
        if modifiers.contains(.command) { flags.insert(.command) }
        if modifiers.contains(.option) { flags.insert(.option) }
        if modifiers.contains(.shift) { flags.insert(.shift) }
        if modifiers.contains(.control) { flags.insert(.control) }
        return flags
    }

    private static func appMenu(appName: String, updateMode: UpdateMode?) -> NSMenu {
        let menu = NSMenu(title: appName)
        menu.addItem(
            item(
                "About \(appName)", #selector(NSApplication.orderFrontStandardAboutPanel(_:))))
        if let updateMode {
            menu.addItem(.separator())
            // The dev bundle earns key equivalents: rebuilding is the edit-run loop, not
            // an occasional errand. Cmd-R opens the window, Cmd-Option-R starts the rebuild
            // without asking; the Repository menu's Sync is Cmd-Shift-R.
            switch updateMode {
            case .update:
                menu.addItem(item("Update \(appName)…", #selector(AppDelegate.showUpdate(_:))))
            case .rebuild:
                menu.addItem(item(keys.rebuild, #selector(AppDelegate.showUpdate(_:))))
                menu.addItem(item(keys.rebuildNow, #selector(AppDelegate.rebuildNow(_:))))
            }
        }
        menu.addItem(.separator())
        menu.addItem(
            item(keys.hideApp, #selector(NSApplication.hide(_:)), title: "Hide \(appName)"))
        menu.addItem(item(keys.hideOthers, #selector(NSApplication.hideOtherApplications(_:))))
        menu.addItem(item("Show All", #selector(NSApplication.unhideAllApplications(_:))))
        menu.addItem(.separator())
        menu.addItem(
            item(keys.quit, #selector(NSApplication.terminate(_:)), title: "Quit \(appName)"))
        return menu
    }

    private static func fileMenu(recentMenu: NSMenu) -> NSMenu {
        let menu = NSMenu(title: "File")
        menu.addItem(item(keys.newTab, #selector(AppDelegate.newWindowForTab(_:))))
        menu.addItem(item(keys.reopenClosedTab, #selector(AppDelegate.reopenClosedTab(_:))))
        menu.addItem(item(keys.openRepository, #selector(AppDelegate.openDocument(_:))))
        let recent = NSMenuItem(title: "Open Recent", action: nil, keyEquivalent: "")
        recent.submenu = recentMenu
        menu.addItem(recent)
        menu.addItem(.separator())
        menu.addItem(item(keys.goToFile, #selector(MainWindowController.goToFile(_:))))
        menu.addItem(item(keys.goToBranch, #selector(MainWindowController.chooseRef(_:))))
        menu.addItem(.separator())
        menu.addItem(item(keys.closeTab, #selector(NSWindow.performClose(_:))))
        menu.addItem(item(keys.closeWindow, #selector(AppDelegate.closeWindowGroup(_:))))
        return menu
    }

    private static func editMenu() -> NSMenu {
        let menu = NSMenu(title: "Edit")
        // `UndoManager.undo` resolves to `undo` without the colon, which no responder handles.
        menu.addItem(item(keys.undo, Selector(("undo:"))))
        menu.addItem(item(keys.redo, Selector(("redo:"))))
        menu.addItem(.separator())
        menu.addItem(item(keys.cut, #selector(NSText.cut(_:))))
        menu.addItem(item(keys.copy, #selector(NSText.copy(_:))))
        menu.addItem(item(keys.paste, #selector(NSText.paste(_:))))
        menu.addItem(
            item(keys.pasteAndMatchStyle, #selector(NSTextView.pasteAsPlainText(_:))))
        menu.addItem(item("Delete", #selector(NSText.delete(_:))))
        menu.addItem(item(keys.selectAll, #selector(NSText.selectAll(_:))))
        menu.addItem(.separator())
        let find = NSMenu(title: "Find")
        for (shortcut, tag) in [
            (keys.find, 1), (keys.findNext, 2), (keys.findPrevious, 3),
            (keys.useSelectionForFind, 7),
        ] {
            let entry = item(shortcut, #selector(NSTextView.performFindPanelAction(_:)))
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
        menu.addItem(item(keys.files, #selector(MainWindowController.showFiles(_:))))
        menu.addItem(item(keys.history, #selector(MainWindowController.showHistory(_:))))
        menu.addItem(item(keys.changes, #selector(MainWindowController.showChanges(_:))))
        menu.addItem(.separator())
        // Finder's and Safari's shortcuts. The tab switch in the Window menu adds Shift.
        menu.addItem(item(keys.back, #selector(AppDelegate.goBack(_:))))
        menu.addItem(item(keys.forward, #selector(AppDelegate.goForward(_:))))
        menu.addItem(.separator())
        // Finder's shortcut for the same toggle.
        menu.addItem(item(keys.showHiddenFiles, #selector(AppDelegate.toggleHiddenFiles(_:))))
        menu.addItem(.separator())
        menu.addItem(item(keys.showBlame, #selector(MainWindowController.showBlame(_:))))
        menu.addItem(
            item(keys.fileHistory, #selector(MainWindowController.showFileHistory(_:))))
        menu.addItem(
            item(keys.searchCommits, #selector(MainWindowController.searchCommits(_:))))
        menu.addItem(.separator())
        menu.addItem(
            item(keys.toggleSidebar, #selector(NSSplitViewController.toggleSidebar(_:))))
        menu.addItem(item(keys.enterFullScreen, #selector(NSWindow.toggleFullScreen(_:))))
        return menu
    }

    private static func repositoryMenu() -> NSMenu {
        let menu = NSMenu(title: "Repository")
        menu.addItem(item(keys.fetch, #selector(MainWindowController.fetchRemote(_:))))
        menu.addItem(item(keys.pull, #selector(MainWindowController.pullRemote(_:))))
        menu.addItem(item(keys.push, #selector(MainWindowController.pushRemote(_:))))
        menu.addItem(.separator())
        menu.addItem(item(keys.sync, #selector(MainWindowController.syncRepository(_:))))
        // The title is rewritten with the branch name in `validateMenuItem`.
        menu.addItem(
            item(
                keys.backToDefault, #selector(MainWindowController.backToDefaultBranch(_:)),
                title: SyncTitles.backToDefault(nil)))
        menu.addItem(item(keys.cancelSync, #selector(MainWindowController.cancelSync(_:))))
        menu.addItem(.separator())
        menu.addItem(
            item("Commit Identity…", #selector(MainWindowController.editCommitIdentity(_:))))
        return menu
    }

    private static func windowMenu() -> NSMenu {
        let menu = NSMenu(title: "Window")
        menu.addItem(item(keys.minimize, #selector(NSWindow.performMiniaturize(_:))))
        menu.addItem(item("Zoom", #selector(NSWindow.performZoom(_:))))
        menu.addItem(.separator())
        menu.addItem(item(keys.previousTab, #selector(NSWindow.selectPreviousTab(_:))))
        menu.addItem(item(keys.nextTab, #selector(NSWindow.selectNextTab(_:))))
        menu.addItem(item("Move Tab to New Window", #selector(NSWindow.moveTabToNewWindow(_:))))
        menu.addItem(item("Merge All Windows", #selector(NSWindow.mergeAllWindows(_:))))
        menu.addItem(.separator())
        menu.addItem(item("Bring All to Front", #selector(NSApplication.arrangeInFront(_:))))
        NSApp.windowsMenu = menu
        return menu
    }
}

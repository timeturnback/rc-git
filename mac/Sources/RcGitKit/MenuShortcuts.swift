/// The modifier keys a menu key equivalent can carry, independent of AppKit.
public struct MenuModifiers: OptionSet, Hashable, Sendable {
    public let rawValue: Int

    public init(rawValue: Int) { self.rawValue = rawValue }

    public static let command = MenuModifiers(rawValue: 1 << 0)
    public static let option = MenuModifiers(rawValue: 1 << 1)
    public static let shift = MenuModifiers(rawValue: 1 << 2)
    public static let control = MenuModifiers(rawValue: 1 << 3)
}

/// The keystroke a key equivalent fires on. An uppercase letter is its lowercase letter with
/// Shift, which is how AppKit matches it, so "G" with Command and "g" with Command-Shift are
/// the same chord.
public struct MenuChord: Hashable, Sendable {
    public let key: String
    public let modifiers: MenuModifiers

    public init(key: String, modifiers: MenuModifiers) {
        let lower = key.lowercased()
        self.key = lower
        self.modifiers = lower == key ? modifiers : modifiers.union(.shift)
    }
}

/// One menu item's title and key equivalent.
public struct MenuShortcut: Hashable, Sendable {
    public let title: String
    public let key: String
    public let modifiers: MenuModifiers

    public init(_ title: String, _ key: String, _ modifiers: MenuModifiers = .command) {
        self.title = title
        self.key = key
        self.modifiers = modifiers
    }

    public var chord: MenuChord { MenuChord(key: key, modifiers: modifiers) }
}

/// Every key equivalent in the main menu. `MainMenu` takes each item's key from here, so a
/// test can prove no two items share a chord: the menu fires the first match in menu order,
/// and the later item would never fire from the keyboard. Every row is a stored property so
/// the test can enumerate them all by reflection; an item whose title carries the app name
/// keeps a placeholder title here.
public struct MenuShortcuts: Sendable {
    public init() {}

    public let settings = MenuShortcut("Settings…", ",")
    public let hideApp = MenuShortcut("Hide", "h")
    public let hideOthers = MenuShortcut("Hide Others", "h", [.command, .option])
    public let quit = MenuShortcut("Quit", "q")
    public let rebuild = MenuShortcut("Rebuild and Relaunch…", "r")
    public let rebuildNow = MenuShortcut("Rebuild and Relaunch Now", "r", [.command, .option])

    public let newTab = MenuShortcut("New Tab", "t")
    public let reopenClosedTab = MenuShortcut("Reopen Closed Tab", "t", [.command, .shift])
    public let openRepository = MenuShortcut("Open Repository…", "o")
    public let goToFile = MenuShortcut("Go to File…", "o", [.command, .shift])
    public let goToBranch = MenuShortcut("Go to Branch…", "b")
    public let closeTab = MenuShortcut("Close Tab", "w")
    public let closeWindow = MenuShortcut("Close Window", "w", [.command, .shift])

    public let undo = MenuShortcut("Undo", "z")
    public let redo = MenuShortcut("Redo", "z", [.command, .shift])
    public let cut = MenuShortcut("Cut", "x")
    public let copy = MenuShortcut("Copy", "c")
    public let paste = MenuShortcut("Paste", "v")
    public let pasteAndMatchStyle = MenuShortcut(
        "Paste and Match Style", "v", [.command, .option, .shift])
    public let selectAll = MenuShortcut("Select All", "a")
    public let find = MenuShortcut("Find…", "f")
    public let findNext = MenuShortcut("Find Next", "g")
    public let findPrevious = MenuShortcut("Find Previous", "G")
    public let useSelectionForFind = MenuShortcut("Use Selection for Find", "e")

    public let files = MenuShortcut("Files", "1")
    public let history = MenuShortcut("History", "2")
    public let changes = MenuShortcut("Changes", "3")
    public let back = MenuShortcut("Back", "[")
    public let forward = MenuShortcut("Forward", "]")
    public let showHiddenFiles = MenuShortcut("Show Hidden Files", ".", [.command, .shift])
    public let showBlame = MenuShortcut("Show Blame", "b", [.command, .option])
    public let fileHistory = MenuShortcut("History of This File", "y", [.command, .option])
    public let searchCommits = MenuShortcut("Search Commits", "F", [.command, .shift])
    public let toggleSidebar = MenuShortcut("Toggle Sidebar", "s", [.command, .control])
    public let enterFullScreen = MenuShortcut("Enter Full Screen", "f", [.command, .control])

    public let fetch = MenuShortcut("Fetch", "f", [.command, .option])
    public let pull = MenuShortcut("Pull", "u", [.command, .shift])
    public let push = MenuShortcut("Push", "p", [.command, .shift])
    public let sync = MenuShortcut("Sync", "r", [.command, .shift])
    public let backToDefault = MenuShortcut("Back to Default Branch", "m", [.command, .shift])
    public let cancelSync = MenuShortcut("Cancel Sync", ".")

    public let minimize = MenuShortcut("Minimize", "m")
    public let previousTab = MenuShortcut("Show Previous Tab", "[", [.command, .shift])
    public let nextTab = MenuShortcut("Show Next Tab", "]", [.command, .shift])
}

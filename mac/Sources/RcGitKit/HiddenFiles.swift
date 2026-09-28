/// Entries the file lists hide until the user asks to see hidden files: version-control
/// metadata and operating-system clutter, matched by name at any depth. The names follow VS
/// Code's `files.exclude` defaults plus the metadata macOS scatters on disks. Gitignored
/// files are deliberately absent: `.env` or `node_modules` stay visible, dimmed as ignored.
public enum HiddenFiles {
    /// The `UserDefaults` key holding whether hidden files are shown, app-wide.
    public static let preferenceKey = "rc-git.showHiddenFiles"

    /// Names hidden wherever they appear.
    public static let names: Set<String> = [
        ".git", ".svn", ".hg", "CVS", ".DS_Store", "Thumbs.db",
        ".Spotlight-V100", ".Trashes", ".fseventsd",
    ]

    /// AppleDouble files: the `._name` resource-fork shadows macOS writes on foreign volumes.
    static let appleDoublePrefix = "._"

    /// Whether one file or folder name is hidden.
    public static func isHidden(name: some StringProtocol) -> Bool {
        names.contains(String(name)) || name.hasPrefix(appleDoublePrefix)
    }

    /// Whether any component of a `/`-separated path is hidden, so everything inside a hidden
    /// folder is hidden with it.
    public static func isHidden(path: String) -> Bool {
        path.split(separator: "/").contains { isHidden(name: $0) }
    }
}

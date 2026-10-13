/// What the file tree says while it has no root to show, decided from what the model knows
/// about the repository, so a window that has one open never sends the user to open one.
public enum TreePlaceholder {
    /// No repository is attached to the window.
    public static let noRepository = "Open a repository (⌘O)"
    /// HEAD is on an unborn branch (`git init`, no commits) and no ref gave a tree either.
    public static let noCommits = "No commits yet"
    /// A repository is open but nothing in it is browsed: its refs could not be read, or
    /// HEAD has a commit while no ref names one.
    public static let nothingToBrowse = "Nothing to browse"

    /// The placeholder for a tree with no root, given whether a repository is attached and
    /// the HEAD last read from it (`nil` until a listing loads).
    public static func text(attached: Bool, head: Head?) -> String {
        guard attached else { return noRepository }
        if let head, head.target == nil { return noCommits }
        return nothingToBrowse
    }
}

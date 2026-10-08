import CRcGit

/// How a file on disk relates to the same path in the compared tree.
public enum WorktreeStatus: Sendable, Hashable {
    case unchanged
    case modified
    case untracked
    case ignored
    case deleted

    init(c: RcGitWorktreeStatus) {
        switch c {
        case RcGitWorktreeStatus_Unchanged: self = .unchanged
        case RcGitWorktreeStatus_Modified: self = .modified
        case RcGitWorktreeStatus_Untracked: self = .untracked
        case RcGitWorktreeStatus_Deleted: self = .deleted
        default: self = .ignored
        }
    }

    /// One-letter label for a row badge; `nil` when the path is unchanged.
    public var letter: String? {
        switch self {
        case .unchanged: return nil
        case .modified: return "M"
        case .untracked: return "U"
        case .ignored: return "I"
        case .deleted: return "D"
        }
    }
}

/// One changed path from a full working-tree scan.
public struct StatusEntry: Sendable, Hashable {
    /// Repository-relative path with `/` separators.
    public let path: String
    /// The kind on disk; the tree's kind for a deleted path.
    public let kind: EntryKind
    /// Always `.modified`, `.untracked`, or `.deleted`.
    public let status: WorktreeStatus
    /// The blob at the same path in the compared tree; `nil` for untracked files.
    public let oid: Oid?
    /// Size on disk in bytes; 0 for deleted files.
    public let size: UInt64
    /// Whether `path` is the stored path byte for byte; see `TreeEntry.hasExactName`.
    public let hasExactPath: Bool

    public init(
        path: String, kind: EntryKind, status: WorktreeStatus, oid: Oid?, size: UInt64,
        hasExactPath: Bool = true
    ) {
        self.path = path
        self.kind = kind
        self.status = status
        self.oid = oid
        self.size = size
        self.hasExactPath = hasExactPath
    }
}

/// Every changed path in the working tree, with the walk's own cost accounting.
public struct StatusReport: Sendable, Hashable {
    /// Changed paths, sorted by path.
    public let entries: [StatusEntry]
    /// Entries visited before the walk finished or hit its cap.
    public let scanned: Int
    /// Whether the walk stopped at the cap before covering the whole tree.
    public let truncated: Bool

    public init(entries: [StatusEntry], scanned: Int, truncated: Bool) {
        self.entries = entries
        self.scanned = scanned
        self.truncated = truncated
    }
}

/// One file on disk from a full working-tree listing.
public struct WorktreeFile: Sendable, Hashable {
    /// Repository-relative path with `/` separators.
    public let path: String
    /// What the file is on disk: a blob, an executable blob, or a symlink.
    public let kind: EntryKind
    /// Whether `path` is the stored path byte for byte; see `TreeEntry.hasExactName`.
    public let hasExactPath: Bool

    public init(path: String, kind: EntryKind, hasExactPath: Bool = true) {
        self.path = path
        self.kind = kind
        self.hasExactPath = hasExactPath
    }
}

/// Every file on disk, tracked, untracked, and ignored alike, with the walk's cost accounting.
public struct WorktreeFileList: Sendable, Hashable {
    /// Files sorted by path.
    public let files: [WorktreeFile]
    /// Entries visited before the walk finished or hit its cap.
    public let scanned: Int
    /// Whether the walk stopped at the cap before covering the whole tree.
    public let truncated: Bool

    public init(files: [WorktreeFile], scanned: Int, truncated: Bool) {
        self.files = files
        self.scanned = scanned
        self.truncated = truncated
    }
}

/// One stashed change set, as `git stash list` shows it.
public struct StashEntry: Sendable, Hashable {
    /// Position in the stash, so 0 is `stash@{0}`, the newest.
    public let index: Int
    /// The stash commit.
    public let oid: Oid
    /// The reflog message, e.g. `WIP on main: 1a2b3c4 Subject`.
    public let message: String

    public init(index: Int, oid: Oid, message: String) {
        self.index = index
        self.oid = oid
        self.message = message
    }
}

/// One entry of a working-tree directory listing.
public struct WorktreeEntry: Sendable, Hashable {
    /// File name on disk.
    public let name: String
    /// What the entry is on disk.
    public let kind: EntryKind
    /// How it relates to the compared tree.
    public let status: WorktreeStatus
    /// The blob at the same path in the compared tree, when there is one.
    public let oid: Oid?
    /// Size in bytes; 0 for directories.
    public let size: UInt64
    /// Whether `name` is the stored name byte for byte; see `TreeEntry.hasExactName`.
    public let hasExactName: Bool

    public init(
        name: String, kind: EntryKind, status: WorktreeStatus, oid: Oid?, size: UInt64,
        hasExactName: Bool = true
    ) {
        self.name = name
        self.kind = kind
        self.status = status
        self.oid = oid
        self.size = size
        self.hasExactName = hasExactName
    }
}

/// A working-tree directory: entries on disk plus tree entries missing from disk.
public struct WorktreeListing: Sendable, Hashable {
    public let entries: [WorktreeEntry]
    /// Tree entries with no counterpart on disk: deleted files.
    public let missing: [TreeEntry]

    public init(entries: [WorktreeEntry], missing: [TreeEntry] = []) {
        self.entries = entries
        self.missing = missing
    }
}

/// One row of the tree pane: a tree or working-tree entry with its optional status overlay.
public struct BrowseRow: Sendable, Hashable {
    public let entry: TreeEntry
    /// The working-tree status; `nil` when browsing a commit.
    public let status: WorktreeStatus?
    /// True for a file the compared tree has but disk lost: shown as deleted.
    public let missing: Bool

    public init(entry: TreeEntry, status: WorktreeStatus? = nil, missing: Bool = false) {
        self.entry = entry
        self.status = status
        self.missing = missing
    }
}

import CRcGit

/// How a file on disk relates to the same path in the compared tree.
public enum WorktreeStatus: Sendable, Hashable {
    case unchanged
    case modified
    case untracked
    case ignored

    init(c: RcGitWorktreeStatus) {
        switch c {
        case RcGitWorktreeStatus_Unchanged: self = .unchanged
        case RcGitWorktreeStatus_Modified: self = .modified
        case RcGitWorktreeStatus_Untracked: self = .untracked
        default: self = .ignored
        }
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

    public init(name: String, kind: EntryKind, status: WorktreeStatus, oid: Oid?, size: UInt64) {
        self.name = name
        self.kind = kind
        self.status = status
        self.oid = oid
        self.size = size
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

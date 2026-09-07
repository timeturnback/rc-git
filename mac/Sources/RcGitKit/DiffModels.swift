import CRcGit

/// What happened to one path between two trees.
public enum ChangeKind: Sendable, Hashable {
    case added
    case deleted
    case modified
    case renamed
    case copied

    init(c: RcGitChangeKind) {
        switch c {
        case RcGitChangeKind_Added: self = .added
        case RcGitChangeKind_Deleted: self = .deleted
        case RcGitChangeKind_Modified: self = .modified
        case RcGitChangeKind_Renamed: self = .renamed
        default: self = .copied
        }
    }

    /// One-letter label, as `git status` prints it.
    public var letter: String {
        switch self {
        case .added: return "A"
        case .deleted: return "D"
        case .modified: return "M"
        case .renamed: return "R"
        case .copied: return "C"
        }
    }
}

/// One changed file between two trees.
public struct TreeChange: Sendable, Hashable, Identifiable {
    public let kind: ChangeKind
    /// Path in the new tree, or in the old tree for a deletion.
    public let path: String
    /// Where the content came from, for renames and copies.
    public let sourcePath: String?
    /// The blob before the change; `nil` for additions.
    public let old: Oid?
    /// The blob after the change; `nil` for deletions.
    public let new: Oid?
    /// What the entry is after the change, or was before it for deletions.
    public let entry: EntryKind

    public var id: String { path }

    public init(
        kind: ChangeKind, path: String, sourcePath: String?, old: Oid?, new: Oid?,
        entry: EntryKind
    ) {
        self.kind = kind
        self.path = path
        self.sourcePath = sourcePath
        self.old = old
        self.new = new
        self.entry = entry
    }
}

/// The role of one line in a diff hunk.
public enum DiffLineKind: Sendable, Hashable {
    case context
    case added
    case removed

    init(c: RcGitDiffLineKind) {
        switch c {
        case RcGitDiffLineKind_Context: self = .context
        case RcGitDiffLineKind_Added: self = .added
        default: self = .removed
        }
    }
}

/// A line of a diff hunk, without its line terminator.
public struct DiffLine: Sendable, Hashable {
    public let kind: DiffLineKind
    public let text: String

    public init(kind: DiffLineKind, text: String) {
        self.kind = kind
        self.text = text
    }
}

/// A run of changed lines with its surrounding context.
public struct DiffHunk: Sendable, Hashable {
    /// 1-based first line of the hunk in the old blob.
    public let oldStart: UInt32
    public let oldLines: UInt32
    /// 1-based first line of the hunk in the new blob.
    public let newStart: UInt32
    public let newLines: UInt32
    public let lines: [DiffLine]

    public init(
        oldStart: UInt32, oldLines: UInt32, newStart: UInt32, newLines: UInt32, lines: [DiffLine]
    ) {
        self.oldStart = oldStart
        self.oldLines = oldLines
        self.newStart = newStart
        self.newLines = newLines
        self.lines = lines
    }
}

/// What a blob diff produced.
public enum BlobDiff: Sendable, Hashable {
    /// Both sides are text; empty `hunks` means identical content.
    case text(hunks: [DiffHunk])
    /// One side is binary.
    case binary(oldSize: UInt64, newSize: UInt64)
    /// One side is over the limit.
    case tooLarge(size: UInt64, limit: UInt64)
}

/// What the history detail's diff pane shows.
public enum FileDiffState: Sendable {
    case empty
    case loading(TreeChange)
    case loaded(TreeChange, BlobDiff)
    case failed(TreeChange, String)
}

/// One blame range together with the commit that owns it.
public struct BlameSpan: Sendable, Hashable {
    public let range: BlameRange
    public let commit: CommitSummary

    public init(range: BlameRange, commit: CommitSummary) {
        self.range = range
        self.commit = commit
    }
}

/// A run of consecutive lines introduced by one commit.
public struct BlameRange: Sendable, Hashable {
    public let commit: Oid
    /// 1-based first line of the run in the blamed file.
    public let startLine: UInt32
    public let lineCount: UInt32

    public init(commit: Oid, startLine: UInt32, lineCount: UInt32) {
        self.commit = commit
        self.startLine = startLine
        self.lineCount = lineCount
    }
}

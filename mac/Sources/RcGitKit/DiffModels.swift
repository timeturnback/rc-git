import CRcGit

/// What happened to one path between two trees.
public enum ChangeKind: Sendable, Hashable {
    case added
    case deleted
    case modified
    case renamed

    init(c: RcGitChangeKind) {
        switch c {
        case RcGitChangeKind_Added: self = .added
        case RcGitChangeKind_Deleted: self = .deleted
        case RcGitChangeKind_Modified: self = .modified
        default: self = .renamed
        }
    }

    /// One-letter label, as `git status` prints it.
    public var letter: String {
        switch self {
        case .added: return "A"
        case .deleted: return "D"
        case .modified: return "M"
        case .renamed: return "R"
        }
    }
}

/// One changed file between two trees.
public struct TreeChange: Sendable, Hashable {
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
    /// What the entry was before the change; `nil` for additions. It differs from `entry` when
    /// the path changed kind, such as a file replaced by a submodule.
    public let oldEntry: EntryKind?

    /// `oldEntry` defaults to `entry` whenever there is an old side.
    public init(
        kind: ChangeKind, path: String, sourcePath: String?, old: Oid?, new: Oid?,
        entry: EntryKind, oldEntry: EntryKind? = nil
    ) {
        self.kind = kind
        self.path = path
        self.sourcePath = sourcePath
        self.old = old
        self.new = new
        self.entry = entry
        self.oldEntry = old == nil ? nil : oldEntry ?? entry
    }

    /// The blob before the change: `nil` for an addition, and for a gitlink, whose id names a
    /// commit in another repository.
    public var oldBlob: Oid? { oldEntry == .submodule ? nil : old }

    /// The blob after the change: `nil` for a deletion, and for a gitlink.
    public var newBlob: Oid? { entry == .submodule ? nil : new }
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

/// The terminator a diff line had before it was stripped.
public enum LineEnd: Sendable, Hashable {
    case lf
    case crlf
    /// The last line of a blob that does not end in a newline.
    case none

    init(c: RcGitLineEnd) {
        switch c {
        case RcGitLineEnd_CrLf: self = .crlf
        case RcGitLineEnd_None: self = .none
        default: self = .lf
        }
    }

    /// The note drawn after a line whose terminator is not a plain `\n`, so a change in line
    /// ending alone does not render as two identical lines. `nil` for `\n`.
    public var marker: String? {
        switch self {
        case .lf: nil
        case .crlf: " \u{240D}CRLF"
        case .none: " \u{2205} no newline at end of file"
        }
    }
}

/// A line of a diff hunk, without its line terminator.
public struct DiffLine: Sendable, Hashable {
    public let kind: DiffLineKind
    public let text: String
    /// How the line ended, so a change in terminator alone stays visible.
    public let eol: LineEnd

    public init(kind: DiffLineKind, text: String, eol: LineEnd = .lf) {
        self.kind = kind
        self.text = text
        self.eol = eol
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

extension DiffHunk {
    /// The hunk's header ranges, which is how the guard and `ChangesModel` name a hunk.
    public var stamp: HunkStamp {
        HunkStamp(oldStart: oldStart, oldLines: oldLines, newStart: newStart, newLines: newLines)
    }

    /// The hunk that owns `row` of a diff pane that shows each hunk as its header row followed
    /// by one row per line, so an action on any row of a hunk acts on that hunk. Nil for a row
    /// outside the pane.
    public static func owning(row: Int, in hunks: [DiffHunk]) -> DiffHunk? {
        guard row >= 0 else { return nil }
        var end = 0
        for hunk in hunks {
            end += 1 + hunk.lines.count
            if row < end { return hunk }
        }
        return nil
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

extension BlobDiff {
    /// The line a diff pane shows in place of hunks, or nil when there are hunks to show.
    public func placeholder(path: String) -> String? {
        switch self {
        case let .text(hunks):
            return hunks.isEmpty ? "\(path): no line changes" : nil
        case let .binary(oldSize, newSize):
            return "\(path) is binary "
                + "(\(ByteFormat.string(oldSize)) → \(ByteFormat.string(newSize)))"
        case let .tooLarge(size, limit):
            return Self.tooLargePlaceholder(path: path, size: size, limit: limit)
        }
    }

    /// The line a pane shows for a file over its size limit.
    public static func tooLargePlaceholder(path: String, size: UInt64, limit: UInt64) -> String {
        let text = ByteFormat.overLimit(size: size, limit: limit)
        return "\(path) is \(text.size), over the \(text.limit) limit"
    }
}

/// What the history detail's diff pane shows.
public enum FileDiffState: Sendable {
    case empty
    case loading(TreeChange)
    case loaded(TreeChange, BlobDiff)
    /// A change with a gitlink on every side: its ids name commits in another repository, so
    /// there is no line diff.
    case submodule(TreeChange)
    case failed(TreeChange, String)

    /// The line the diff pane shows for a submodule change, as git words it.
    public static func submoduleSummary(_ change: TreeChange) -> String {
        let old = change.old?.hex ?? "none"
        let new = change.new?.hex ?? "none"
        return "Subproject commit \(old) → \(new)"
    }
}

/// One blame range together with the commit that owns it.
public struct BlameSpan: Sendable, Hashable {
    public let range: BlameRange
    public let commit: CommitSummary
    /// For a boundary range, the floor the walk stopped at, in seconds since the Unix epoch:
    /// the lines are older than it, and `commit` is only where the walk gave up. `nil` when
    /// the range was traced to the commit that introduced it.
    public let olderThan: Int64?

    public init(range: BlameRange, commit: CommitSummary, olderThan: Int64? = nil) {
        self.range = range
        self.commit = commit
        self.olderThan = olderThan
    }
}

/// The blame the model last started or finished for the open file, for the blame panel to
/// present.
public enum BlameState: Sendable, Hashable {
    /// No blame is showing: none was asked for, or the one running was dropped or cancelled.
    case empty
    /// A walk of the file at `path` is running; `title` says so while the panel waits.
    case loading(path: String, title: String)
    /// Every range of the file at `path`, paired with its commit, and the panel's title.
    case loaded(path: String, spans: [BlameSpan], title: String)
    case failed(String)
}

/// What a blame produced.
public enum Blame: Sendable, Hashable {
    /// Every line attributed, as consecutive ranges ordered by line number.
    case ranges([BlameRange])
    /// The file at the tip is larger than the requested limit; no history was walked.
    case tooLarge(size: UInt64, limit: UInt64)
}

/// A run of consecutive lines introduced by one commit.
public struct BlameRange: Sendable, Hashable {
    public let commit: Oid
    /// 1-based first line of the run in the blamed file.
    public let startLine: UInt32
    public let lineCount: UInt32
    /// The walk stopped at the `since` floor before finding where these lines came from:
    /// `commit` is the first commit older than the floor, and the lines are at least that old.
    public let boundary: Bool

    public init(commit: Oid, startLine: UInt32, lineCount: UInt32, boundary: Bool = false) {
        self.commit = commit
        self.startLine = startLine
        self.lineCount = lineCount
        self.boundary = boundary
    }
}

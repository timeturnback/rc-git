import CRcGit

/// Namespace of a reference.
public enum RefKind: Sendable, Hashable {
    case localBranch
    case remoteBranch
    case tag
    case other

    init(c: RcGitRefKind) {
        switch c {
        case RcGitRefKind_LocalBranch: self = .localBranch
        case RcGitRefKind_RemoteBranch: self = .remoteBranch
        case RcGitRefKind_Tag: self = .tag
        default: self = .other
        }
    }
}

/// A reference and the object it finally points to.
public struct RefEntry: Sendable, Hashable, Identifiable {
    /// Full name, e.g. `refs/heads/main`.
    public let fullName: String
    /// Name without the namespace, e.g. `main`, `origin/main`, `v1.0`.
    public let shortName: String
    public let kind: RefKind
    /// The object after following symbolic refs and peeling annotated tags.
    public let target: Oid

    public var id: String { fullName }

    public init(fullName: String, shortName: String, kind: RefKind, target: Oid) {
        self.fullName = fullName
        self.shortName = shortName
        self.kind = kind
        self.target = target
    }
}

/// A reference whose target could not be read.
public struct BrokenRef: Sendable, Hashable {
    public let fullName: String
    public let error: String

    public init(fullName: String, error: String) {
        self.fullName = fullName
        self.error = error
    }
}

/// Every reference in a repository.
public struct RefListing: Sendable, Hashable {
    public var refs: [RefEntry]
    public var broken: [BrokenRef]

    public init(refs: [RefEntry], broken: [BrokenRef] = []) {
        self.refs = refs
        self.broken = broken
    }
}

/// Where `HEAD` points.
public struct Head: Sendable, Hashable {
    /// Full branch name when on a branch, `nil` when detached.
    public let refName: String?
    /// `nil` on an unborn branch.
    public let target: Oid?

    public init(refName: String?, target: Oid?) {
        self.refName = refName
        self.target = target
    }
}

/// What a tree entry points at.
public enum EntryKind: Sendable, Hashable {
    case tree
    case blob
    case executableBlob
    case symlink
    case submodule

    init(c: RcGitEntryKind) {
        switch c {
        case RcGitEntryKind_Tree: self = .tree
        case RcGitEntryKind_Blob: self = .blob
        case RcGitEntryKind_ExecutableBlob: self = .executableBlob
        case RcGitEntryKind_Symlink: self = .symlink
        default: self = .submodule
        }
    }

    /// Whether the entry can be expanded in a tree view.
    public var isDirectory: Bool { self == .tree }
}

/// One child of a tree.
public struct TreeEntry: Sendable, Hashable, Identifiable {
    /// Name for display. Git allows non-UTF-8 names; those arrive with U+FFFD replacements.
    public let name: String
    public let oid: Oid
    public let kind: EntryKind

    public var id: String { name }

    public init(name: String, oid: Oid, kind: EntryKind) {
        self.name = name
        self.oid = oid
        self.kind = kind
    }
}

/// What a blob read produced.
public enum BlobContent: Sendable, Hashable {
    /// Raw bytes, not validated as UTF-8, and byte offsets where lines begin.
    case text(data: [UInt8], lineStarts: [UInt32])
    /// Contains a NUL byte near the start.
    case binary(size: UInt64)
    /// Larger than the requested limit; nothing was decoded.
    case tooLarge(size: UInt64, limit: UInt64)
}

/// The parts of a commit a list shows.
public struct CommitSummary: Sendable, Hashable, Identifiable {
    public let oid: Oid
    public let parents: [Oid]
    public let authorName: String
    public let authorEmail: String
    /// Seconds since the Unix epoch.
    public let authorTime: Int64
    /// Seconds east of UTC.
    public let authorOffset: Int32
    public let title: String

    public var id: Oid { oid }

    public init(
        oid: Oid, parents: [Oid], authorName: String, authorEmail: String,
        authorTime: Int64, authorOffset: Int32, title: String
    ) {
        self.oid = oid
        self.parents = parents
        self.authorName = authorName
        self.authorEmail = authorEmail
        self.authorTime = authorTime
        self.authorOffset = authorOffset
        self.title = title
    }
}

/// What a commit search matches. Every set field must match.
public struct CommitFilter: Sendable, Hashable {
    /// Case-insensitive (ASCII) substring of the full message.
    public var message: String?
    /// Case-insensitive (ASCII) substring of the author name or email.
    public var author: String?
    /// Only commits at or after this time, seconds since the Unix epoch. Also stops the walk.
    public var after: Int64?
    /// Only commits at or before this time, seconds since the Unix epoch.
    public var before: Int64?

    public init(
        message: String? = nil, author: String? = nil, after: Int64? = nil, before: Int64? = nil
    ) {
        self.message = message
        self.author = author
        self.after = after
        self.before = before
    }

    /// True when no field is set, which would match every commit.
    public var isEmpty: Bool {
        message == nil && author == nil && after == nil && before == nil
    }
}

/// One page of a filtered history walk.
public struct CommitPage: Sendable, Hashable {
    /// The commits that matched, newest first.
    public let commits: [CommitSummary]
    /// How many commits the call examined past its scan skip.
    public let scanned: Int
    /// True when the walk reached the end of history within this call.
    public let complete: Bool

    public init(commits: [CommitSummary], scanned: Int, complete: Bool) {
        self.commits = commits
        self.scanned = scanned
        self.complete = complete
    }
}

/// Everything a commit detail view shows beyond the summary.
public struct CommitDetail: Sendable, Hashable {
    public let summary: CommitSummary
    /// Message body after the title. Empty when the message is only a title.
    public let body: String
    public let committerName: String
    public let committerEmail: String
    /// Seconds since the Unix epoch.
    public let committerTime: Int64
    /// Seconds east of UTC.
    public let committerOffset: Int32
    /// The root tree of the commit.
    public let tree: Oid

    public init(
        summary: CommitSummary, body: String, committerName: String, committerEmail: String,
        committerTime: Int64, committerOffset: Int32, tree: Oid
    ) {
        self.summary = summary
        self.body = body
        self.committerName = committerName
        self.committerEmail = committerEmail
        self.committerTime = committerTime
        self.committerOffset = committerOffset
        self.tree = tree
    }
}

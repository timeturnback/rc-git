import CRcGit
import Foundation

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
    /// Whether the stored name is valid UTF-8. When it is not, both names carry U+FFFD
    /// replacements and sending one back would name a different ref, so the ref menu offers
    /// no delete or create.
    public let hasExactName: Bool

    public var id: String { fullName }

    public init(
        fullName: String, shortName: String, kind: RefKind, target: Oid, hasExactName: Bool = true
    ) {
        self.fullName = fullName
        self.shortName = shortName
        self.kind = kind
        self.target = target
        self.hasExactName = hasExactName
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

/// Every reference in a repository, plus the ref list's display order. The order is sorted
/// once, when the listing is built or its refs change, so filtering never sorts; a listing
/// built inside `Task.detached` is sorted off the main actor.
public struct RefListing: Sendable, Hashable {
    public var refs: [RefEntry] {
        didSet { sortedSections = Self.naturalSections(of: refs) }
    }
    public var broken: [BrokenRef] {
        didSet { sortedBroken = Self.naturalOrder(of: broken) }
    }
    /// Every section of the ref list, unfiltered: section order, natural order within each.
    private(set) var sortedSections: [RefSection]
    /// `broken` in natural order of full name.
    private(set) var sortedBroken: [BrokenRef]

    public init(refs: [RefEntry], broken: [BrokenRef] = []) {
        self.refs = refs
        self.broken = broken
        sortedSections = Self.naturalSections(of: refs)
        sortedBroken = Self.naturalOrder(of: broken)
    }

    /// Equal when the refs are: the sorted copies follow from them.
    public static func == (lhs: RefListing, rhs: RefListing) -> Bool {
        lhs.refs == rhs.refs && lhs.broken == rhs.broken
    }

    public func hash(into hasher: inout Hasher) {
        hasher.combine(refs)
        hasher.combine(broken)
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
    /// Whether `name` is the stored name byte for byte. A path built from an inexact name
    /// can name a different object, so nothing that reads by path accepts one.
    public let hasExactName: Bool

    public var id: String { name }

    public init(name: String, oid: Oid, kind: EntryKind, hasExactName: Bool = true) {
        self.name = name
        self.oid = oid
        self.kind = kind
        self.hasExactName = hasExactName
    }
}

/// Why an action is refused on a path that crossed the FFI with U+FFFD replacements: the
/// core reads path arguments as UTF-8, so the repaired spelling would act on whatever has
/// that name, possibly another object.
public enum InexactName {
    /// The refusal for `action` (a verb phrase, such as "blame") on `path`.
    public static func pathRefusal(_ action: String, _ path: String) -> String {
        "cannot \(action) '\(path)': the path is not valid UTF-8"
    }
}

/// What a blob read produced.
public enum BlobContent: Sendable, Hashable {
    /// Raw bytes, byte offsets where lines begin, and whether the bytes are valid UTF-8
    /// (checked once in the core, so decoding needs no second scan).
    case text(data: [UInt8], lineStarts: [UInt32], validUTF8: Bool)
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
    /// Only commits whose committer time is at or after this time, seconds since the Unix
    /// epoch, as `git log --since` compares. Also stops the walk at the first commit committed
    /// before it, so clock skew can hide newer ancestors. Committer time can differ from the
    /// author time a `CommitSummary` carries.
    public var after: Int64?
    /// Only commits whose committer time is at or before this time, seconds since the Unix
    /// epoch, as `git log --until` compares.
    public var before: Int64?
    /// Only commits that changed the entry at this path, as path history judges it.
    public var path: String?

    public init(
        message: String? = nil, author: String? = nil, after: Int64? = nil,
        before: Int64? = nil, path: String? = nil
    ) {
        self.message = message
        self.author = author
        self.after = after
        self.before = before
        self.path = path
    }

    /// True when no field is set, which would match every commit.
    public var isEmpty: Bool {
        message == nil && author == nil && after == nil && before == nil && path == nil
    }

    /// The `after` and `before` instants for a Since/Until day pick: the first second of the
    /// `since` day and the last second of the `until` day in `calendar`, whatever time of day
    /// the picked dates carry. The day end is the next day's start less one second, so a day a
    /// DST change makes 23 or 25 hours long is covered whole.
    public static func dayBounds(
        since: Date?, until: Date?, calendar: Calendar
    ) -> (after: Int64?, before: Int64?) {
        let after = since.map { Int64(calendar.startOfDay(for: $0).timeIntervalSince1970) }
        let before = until.flatMap { day -> Int64? in
            let start = calendar.startOfDay(for: day)
            // Adding one day to a day start only fails for dates outside the calendar's range,
            // which a date picker cannot produce.
            guard let next = calendar.date(byAdding: .day, value: 1, to: start) else { return nil }
            return Int64(next.timeIntervalSince1970) - 1
        }
        return (after, before)
    }
}

/// Which commit field the History search box matches.
public enum CommitSearchScope: Sendable, Hashable {
    case message
    case author
}

/// Decides whether a submitted History search names a revision to jump to.
public enum CommitSearchQuery {
    /// How many hex digits a submitted query may hold and still count as an abbreviated hash.
    public static let revisionLengths = 4...40

    /// The revision to try for a submitted `query`, or nil when it is only search text: 4 to 40
    /// ASCII hex digits once trimmed, and never in the author scope. `Character.isHexDigit`
    /// would also accept fullwidth digits, so the check runs over the UTF-8 bytes.
    public static func revision(_ query: String, scope: CommitSearchScope) -> String? {
        guard scope == .message else { return nil }
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        let bytes = trimmed.utf8
        guard revisionLengths.contains(bytes.count), bytes.allSatisfy(isASCIIHexDigit) else {
            return nil
        }
        return trimmed
    }

    private static func isASCIIHexDigit(_ byte: UInt8) -> Bool {
        switch byte {
        case UInt8(ascii: "0")...UInt8(ascii: "9"), UInt8(ascii: "a")...UInt8(ascii: "f"),
            UInt8(ascii: "A")...UInt8(ascii: "F"):
            true
        default:
            false
        }
    }
}

/// One page of a filtered history walk.
public struct CommitPage: Sendable, Hashable {
    /// The commits that matched, newest first.
    public let commits: [CommitSummary]
    /// How many commits the page examined, matched or not.
    public let scanned: Int
    /// True when the walk reached the end of history within this page.
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

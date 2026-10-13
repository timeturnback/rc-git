/// One file in the flattened tree below a root. `path` is repository-relative with `/`
/// separators, exactly as the tree stores it.
public struct PathIndexEntry: Sendable, Hashable {
    public let path: String
    /// The blob id; empty for a file listed from disk, which has no id until it is hashed.
    public let oid: Oid
    public let kind: EntryKind
    /// Whether the path is under `HiddenFiles`, so search can leave it out.
    public let hidden: Bool
    /// Whether every component of `path` is the stored name byte for byte; see
    /// `TreeEntry.hasExactName`.
    public let hasExactPath: Bool
    /// `path` in precomposed (NFC) form, which search matches against, so a name stored
    /// decomposed on disk matches the precomposed letters a keyboard types. Equal to `path`
    /// when the path is ASCII.
    public let searchKey: String

    public init(path: String, oid: Oid, kind: EntryKind, hidden: Bool, hasExactPath: Bool = true) {
        self.path = path
        self.oid = oid
        self.kind = kind
        self.hidden = hidden
        self.hasExactPath = hasExactPath
        self.searchKey =
            FuzzyMatch.isASCII(path) ? path : path.precomposedStringWithCanonicalMapping
    }

    /// The file name after the last `/`.
    public var name: String {
        path.lastIndex(of: "/").map { String(path[path.index(after: $0)...]) } ?? path
    }
}

/// Every file below one root tree, in depth-first display order. Keyed by the root tree id,
/// so the index for a ref stays valid until the ref moves to a different tree. The working
/// tree's index is built from disk instead and keyed by the `HEAD` tree it was walked against.
public struct PathIndex: Sendable {
    public let root: Oid
    public let entries: [PathIndexEntry]
    /// True when the walk stopped at `PathIndex.maxEntries`; matches may be missing.
    public let truncated: Bool

    /// Upper bound on indexed files, so one pathological repository cannot exhaust memory.
    public static let maxEntries = 200_000

    public init(root: Oid, entries: [PathIndexEntry], truncated: Bool) {
        self.root = root
        self.entries = entries
        self.truncated = truncated
    }

    /// Flattens the tree below `root`, one tree object per directory, depth-first, stopping at
    /// `limit` files. Each directory lists its subdirectories first, each in full and in the
    /// order `children` returns them, then its own files. This is the one operation that walks a
    /// whole tree, so it must run in the background and checks for task cancellation between
    /// directory reads.
    public static func build(
        root: Oid, limit: Int = maxEntries, children: (Oid) throws -> [TreeEntry]
    ) throws -> PathIndex {
        var entries: [PathIndexEntry] = []
        var pending: [PendingWalk] = [.tree(root, prefix: "", hidden: false, exact: true)]
        while let next = pending.popLast() {
            switch next {
            case .files(let files):
                for file in files {
                    if entries.count >= limit {
                        return PathIndex(root: root, entries: entries, truncated: true)
                    }
                    entries.append(file)
                }
            case .tree(let tree, let prefix, let hidden, let exact):
                try Task.checkCancellation()
                let listed = try children(tree)
                pending += readDirectory(listed, prefix: prefix, hidden: hidden, exact: exact)
            }
        }
        return PathIndex(root: root, entries: entries, truncated: false)
    }

    /// One step of `build`'s walk: a directory still to read, or files ready to emit.
    private enum PendingWalk {
        case tree(Oid, prefix: String, hidden: Bool, exact: Bool)
        case files([PathIndexEntry])
    }

    /// The stack pushes for one directory: its files as one chunk, then its subdirectories in
    /// reverse, so the first subdirectory pops next and the files pop after every subdirectory.
    private static func readDirectory(
        _ children: [TreeEntry], prefix: String, hidden parentHidden: Bool,
        exact parentExact: Bool
    ) -> [PendingWalk] {
        var files: [PathIndexEntry] = []
        var trees: [PendingWalk] = []
        for entry in children {
            let path = prefix.isEmpty ? entry.name : prefix + "/" + entry.name
            let hidden = parentHidden || HiddenFiles.isHidden(name: entry.name)
            let exact = parentExact && entry.hasExactName
            switch entry.kind {
            case .tree:
                trees.append(.tree(entry.oid, prefix: path, hidden: hidden, exact: exact))
            case .blob, .executableBlob, .symlink:
                files.append(
                    PathIndexEntry(
                        path: path, oid: entry.oid, kind: entry.kind, hidden: hidden,
                        hasExactPath: exact))
            case .submodule:
                continue
            }
        }
        return [.files(files)] + trees.reversed()
    }

    /// Every file a working-tree walk found, compared against the tree `root`. Disk files
    /// carry no blob id, so their entries hold `Oid.unhashed`.
    public init(root: Oid, worktreeFiles list: WorktreeFileList) {
        self.init(
            root: root,
            entries: list.files.map {
                PathIndexEntry(
                    path: $0.path, oid: .unhashed, kind: $0.kind,
                    hidden: HiddenFiles.isHidden(path: $0.path), hasExactPath: $0.hasExactPath)
            },
            truncated: list.truncated)
    }

    /// The best `limit` matches for `query`, highest score first, ties by shorter then
    /// lexicographic path so the order is stable. An empty query returns the first entries.
    /// Entries under `HiddenFiles` are left out unless `includeHidden`. The query is matched in
    /// precomposed (NFC) form against each entry's `searchKey`.
    public func search(_ query: String, limit: Int = 100, includeHidden: Bool = true)
        -> [PathIndexEntry]
    {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
            .precomposedStringWithCanonicalMapping
        if trimmed.isEmpty {
            return Array(entries.lazy.filter { includeHidden || !$0.hidden }.prefix(limit))
        }
        var scored: [(score: Int, entry: PathIndexEntry)] = []
        for entry in entries where includeHidden || !entry.hidden {
            if let score = FuzzyMatch.score(entry.searchKey, query: trimmed) {
                scored.append((score, entry))
            }
        }
        scored.sort {
            if $0.score != $1.score { return $0.score > $1.score }
            if $0.entry.path.count != $1.entry.path.count {
                return $0.entry.path.count < $1.entry.path.count
            }
            return $0.entry.path < $1.entry.path
        }
        return scored.prefix(limit).map(\.entry)
    }
}

/// What one path search found, and whether the index it searched stopped short.
public struct PathSearchResult: Sendable, Equatable {
    public let entries: [PathIndexEntry]
    /// True when the searched index stopped at `PathIndex.maxEntries`, so a file that exists
    /// may be missing from `entries`.
    public let truncated: Bool

    /// The note to show while `truncated`. It says entries, not files: the working-tree walk
    /// counts directories too, in disk order, so the cut is not "the first N files".
    public static let truncationNote =
        "Stopped indexing at 200,000 entries; some files may not be found"

    public init(entries: [PathIndexEntry], truncated: Bool) {
        self.entries = entries
        self.truncated = truncated
    }
}

/// Case-insensitive subsequence matching for jump-to-file. Greedy left-to-right with bonuses
/// for runs and word starts; no backtracking, so scoring 200 000 paths stays cheap.
public enum FuzzyMatch {
    /// Points for a match directly after the previous one.
    static let consecutiveBonus = 8
    /// Points for a match at a word start: after `/`, `.`, `_`, `-`, or at the beginning.
    /// Applied only when the match does not continue a run, so a run scores as one word.
    static let boundaryBonus = 10
    /// Points lost for every gap between two matched characters, however long.
    static let gapPenalty = 3
    /// Extra points when the whole match sits inside the file name, not the directories.
    static let fileNameBonus = 20

    /// Score of matching `query` as a subsequence of `candidate`, or `nil` when it is not
    /// one. Comparison is ASCII-case-insensitive, matching how people type paths. An ASCII
    /// query compares bytes, since an ASCII byte never occurs inside a multi-byte character;
    /// any other query compares whole Unicode scalars, so one typed letter never matches bytes
    /// taken from different letters. Both strings are expected in the same normalization form.
    public static func score(_ candidate: String, query: String) -> Int? {
        if isASCII(query) {
            return score(path: Array(candidate.utf8), needle: Array(query.utf8))
        }
        return score(
            path: candidate.unicodeScalars.map(\.value), needle: query.unicodeScalars.map(\.value))
    }

    /// Whether every byte of `text` is ASCII.
    static func isASCII(_ text: String) -> Bool {
        text.utf8.allSatisfy { $0 < 0x80 }
    }

    /// The scoring loop over code units: UTF-8 bytes or Unicode scalar values.
    private static func score<Unit: FixedWidthInteger>(path: [Unit], needle: [Unit]) -> Int? {
        if needle.isEmpty { return 0 }
        let lastSlash = path.lastIndex(of: Unit(UInt8(ascii: "/")))
        let nameStart = lastSlash.map { $0 + 1 } ?? 0
        var score = 0
        var needleIndex = 0
        var previousMatch = -2
        var firstMatch = -1
        for index in 0..<path.count where lower(path[index]) == lower(needle[needleIndex]) {
            score += 1
            if index == previousMatch + 1 {
                score += consecutiveBonus
            } else {
                if previousMatch >= 0 { score -= gapPenalty }
                if index == 0 || isBoundary(path[index - 1]) { score += boundaryBonus }
            }
            if firstMatch < 0 { firstMatch = index }
            previousMatch = index
            needleIndex += 1
            if needleIndex == needle.count { break }
        }
        guard needleIndex == needle.count else { return nil }
        if firstMatch >= nameStart { score += fileNameBonus }
        score -= (path.count - nameStart) / 8
        return score
    }

    private static func lower<Unit: FixedWidthInteger>(_ unit: Unit) -> Unit {
        unit >= Unit(UInt8(ascii: "A")) && unit <= Unit(UInt8(ascii: "Z")) ? unit + 32 : unit
    }

    private static func isBoundary<Unit: FixedWidthInteger>(_ unit: Unit) -> Bool {
        unit == Unit(UInt8(ascii: "/")) || unit == Unit(UInt8(ascii: "."))
            || unit == Unit(UInt8(ascii: "_")) || unit == Unit(UInt8(ascii: "-"))
    }
}

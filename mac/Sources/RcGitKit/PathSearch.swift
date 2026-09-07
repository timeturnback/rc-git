/// One file in the flattened tree below a root. `path` is repository-relative with `/`
/// separators, exactly as the tree stores it.
public struct PathIndexEntry: Sendable, Hashable {
    public let path: String
    public let oid: Oid
    public let kind: EntryKind

    public init(path: String, oid: Oid, kind: EntryKind) {
        self.path = path
        self.oid = oid
        self.kind = kind
    }

    /// The file name after the last `/`.
    public var name: String {
        path.lastIndex(of: "/").map { String(path[path.index(after: $0)...]) } ?? path
    }
}

/// Every file below one root tree, in depth-first display order. Keyed by the root tree id,
/// so the index for a ref stays valid until the ref moves to a different tree.
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

    /// Flattens the tree below `root`, one tree object per directory, depth-first. This is
    /// the one operation that walks a whole tree, so it must run in the background and checks
    /// for task cancellation between directory reads.
    public static func build(
        root: Oid, children: (Oid) throws -> [TreeEntry]
    ) throws -> PathIndex {
        var entries: [PathIndexEntry] = []
        var pending: [(tree: Oid, prefix: String)] = [(root, "")]
        while let (tree, prefix) = pending.popLast() {
            try Task.checkCancellation()
            for entry in try children(tree) {
                let path = prefix.isEmpty ? entry.name : prefix + "/" + entry.name
                switch entry.kind {
                case .tree:
                    pending.append((entry.oid, path))
                case .blob, .executableBlob, .symlink:
                    entries.append(PathIndexEntry(path: path, oid: entry.oid, kind: entry.kind))
                    if entries.count >= maxEntries {
                        return PathIndex(root: root, entries: entries, truncated: true)
                    }
                case .submodule:
                    continue
                }
            }
        }
        return PathIndex(root: root, entries: entries, truncated: false)
    }

    /// The best `limit` matches for `query`, highest score first, ties by shorter then
    /// lexicographic path so the order is stable. An empty query returns the first entries.
    public func search(_ query: String, limit: Int = 100) -> [PathIndexEntry] {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty { return Array(entries.prefix(limit)) }
        var scored: [(score: Int, entry: PathIndexEntry)] = []
        for entry in entries {
            if let score = FuzzyMatch.score(entry.path, query: trimmed) {
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
    /// one. Comparison is ASCII-case-insensitive, matching how people type paths.
    public static func score(_ candidate: String, query: String) -> Int? {
        let path = Array(candidate.utf8)
        let needle = Array(query.utf8)
        if needle.isEmpty { return 0 }
        let lastSlash = path.lastIndex(of: UInt8(ascii: "/"))
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

    private static func lower(_ byte: UInt8) -> UInt8 {
        byte >= UInt8(ascii: "A") && byte <= UInt8(ascii: "Z") ? byte + 32 : byte
    }

    private static func isBoundary(_ byte: UInt8) -> Bool {
        byte == UInt8(ascii: "/") || byte == UInt8(ascii: ".") || byte == UInt8(ascii: "_")
            || byte == UInt8(ascii: "-")
    }
}

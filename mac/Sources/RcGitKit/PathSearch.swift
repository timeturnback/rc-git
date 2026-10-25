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
        self.searchKey = FuzzyMatch.searchKey(path)
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
    /// Entries `search` scores between two cancellation checks.
    static let cancellationStride = 4_096

    public init(root: Oid, entries: [PathIndexEntry], truncated: Bool) {
        self.root = root
        self.entries = entries
        self.truncated = truncated
    }

    /// Flattens the tree below `root`, one tree object per directory, depth-first, stopping at
    /// `limit` files. Each directory lists its subdirectories first, each in full and in the
    /// order `children` returns them, then its own files. This is the one operation that walks a
    /// whole tree, so it must run in the background and checks for task cancellation between
    /// directory reads. `children` is awaited once per directory, so a blocking read can hop
    /// off the cooperative pool while the walk stays cancellable.
    public static func build(
        root: Oid, limit: Int = maxEntries, children: (Oid) async throws -> [TreeEntry]
    ) async throws -> PathIndex {
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
                let listed = try await children(tree)
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

    /// The best `limit` matches for `query`, highest score first, ties by shorter (in UTF-8
    /// bytes, which is O(1); `String.count` walks graphemes) then lexicographic path so the
    /// order is stable. An empty query returns the first entries.
    /// Entries under `HiddenFiles` are left out unless `includeHidden`. The query is matched in
    /// precomposed (NFC) form against each entry's `searchKey`. A scan over a large index
    /// checks for task cancellation every `cancellationStride` entries, so a superseded query
    /// stops within milliseconds.
    ///
    /// - Throws: `CancellationError` when the calling task is cancelled during the scan.
    public func search(_ query: String, limit: Int = 100, includeHidden: Bool = true) throws
        -> [PathIndexEntry]
    {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
            .precomposedStringWithCanonicalMapping
        if trimmed.isEmpty {
            return Array(entries.lazy.filter { includeHidden || !$0.hidden }.prefix(limit))
        }
        var scored = try matches(trimmed, includeHidden: includeHidden)
        scored.sort { Self.ranksBefore($0, $1) }
        return scored.prefix(limit).map(\.entry)
    }

    /// Every entry `query` matches, with its score, in index order.
    ///
    /// - Throws: `CancellationError` when the calling task is cancelled during the scan.
    private func matches(_ query: String, includeHidden: Bool) throws
        -> [(score: Int, entry: PathIndexEntry)]
    {
        var scored: [(score: Int, entry: PathIndexEntry)] = []
        for (position, entry) in entries.enumerated() {
            if position % Self.cancellationStride == 0 { try Task.checkCancellation() }
            guard includeHidden || !entry.hidden else { continue }
            if let score = FuzzyMatch.score(entry.searchKey, query: query) {
                scored.append((score, entry))
            }
        }
        return scored
    }

    /// Search order: higher score first, then the shorter path, then the path itself.
    private static func ranksBefore(
        _ left: (score: Int, entry: PathIndexEntry), _ right: (score: Int, entry: PathIndexEntry)
    ) -> Bool {
        if left.score != right.score { return left.score > right.score }
        let leftLength = left.entry.path.utf8.count
        let rightLength = right.entry.path.utf8.count
        if leftLength != rightLength { return leftLength < rightLength }
        return left.entry.path < right.entry.path
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
    /// When the leftmost match starts in a directory, the scan runs once more from the file
    /// name, so `Model/Model.swift` still earns the file-name bonus for `model`.
    public static func score(_ candidate: String, query: String) -> Int? {
        if isASCII(query) {
            return score(path: Array(candidate.utf8), needle: Array(query.utf8))
        }
        return score(
            path: candidate.unicodeScalars.map(\.value), needle: query.unicodeScalars.map(\.value))
    }

    /// `text` in the form `score` compares: as is when ASCII, else precomposed (NFC), so a
    /// typed query and a stored path meet in one normalization.
    public static func searchKey(_ text: String) -> String {
        isASCII(text) ? text : text.precomposedStringWithCanonicalMapping
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
        guard let leftmost = scan(path: path, needle: needle, from: 0) else { return nil }
        var score = leftmost.score
        if leftmost.firstMatch >= nameStart {
            score += fileNameBonus
        } else if let inName = scan(path: path, needle: needle, from: nameStart) {
            score = max(score, inName.score + fileNameBonus)
        }
        return score - (path.count - nameStart) / 8
    }

    /// One greedy left-to-right match of `needle` in `path` starting at `from`: its run and
    /// boundary score and where it began, or `nil` when `needle` is not a subsequence there.
    private static func scan<Unit: FixedWidthInteger>(path: [Unit], needle: [Unit], from: Int)
        -> (score: Int, firstMatch: Int)?
    {
        var score = 0
        var needleIndex = 0
        var previousMatch = -2
        var firstMatch = -1
        for index in from..<path.count where lower(path[index]) == lower(needle[needleIndex]) {
            score += points(at: index, after: previousMatch, in: path)
            if firstMatch < 0 { firstMatch = index }
            previousMatch = index
            needleIndex += 1
            if needleIndex == needle.count { break }
        }
        guard needleIndex == needle.count else { return nil }
        return (score, firstMatch)
    }

    /// What a match at `index` earns given the previous match: the run bonus when it follows
    /// directly, else the gap penalty after an earlier match and the word-start bonus.
    private static func points<Unit: FixedWidthInteger>(
        at index: Int, after previousMatch: Int, in path: [Unit]
    ) -> Int {
        if index == previousMatch + 1 { return 1 + consecutiveBonus }
        var points = 1
        if previousMatch >= 0 { points -= gapPenalty }
        if index == 0 || isBoundary(path[index - 1]) { points += boundaryBonus }
        return points
    }

    private static func lower<Unit: FixedWidthInteger>(_ unit: Unit) -> Unit {
        unit >= Unit(UInt8(ascii: "A")) && unit <= Unit(UInt8(ascii: "Z")) ? unit + 32 : unit
    }

    private static func isBoundary<Unit: FixedWidthInteger>(_ unit: Unit) -> Bool {
        unit == Unit(UInt8(ascii: "/")) || unit == Unit(UInt8(ascii: "."))
            || unit == Unit(UInt8(ascii: "_")) || unit == Unit(UInt8(ascii: "-"))
    }
}

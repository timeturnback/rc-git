import Foundation

/// Bounds for one content-search call into the core. They match the core's defaults.
public struct ContentSearchLimits: Sendable, Hashable {
    /// Largest file read; a bigger one is skipped after a header read or a `stat`.
    public let fileLimit: UInt64
    /// Bytes one call reads before it returns; the file that crosses it is finished.
    public let batchBytes: UInt64

    public init(fileLimit: UInt64 = 1024 * 1024, batchBytes: UInt64 = 8 * 1024 * 1024) {
        self.fileLimit = fileLimit
        self.batchBytes = batchBytes
    }
}

/// One line that holds the query, ready to show.
public struct ContentLineMatch: Sendable, Hashable {
    /// Zero-based line number.
    public let line: Int
    /// Byte offset of the first match from the start of the line.
    public let column: Int
    /// Length of one match in bytes: the query's UTF-8 length.
    public let matchLength: Int
    /// The line, or a window of a long one, without indentation or line ending; decoded
    /// lossily.
    public let excerpt: String
    /// UTF-16 ranges of the matches in `excerpt`. Empty when the excerpt was not valid
    /// UTF-8: a replacement character shifts every later offset, so the bytes no longer map.
    public let highlights: [Range<Int>]

    /// A match from the core: `excerptBytes` and the byte `ranges` inside them.
    public init(
        line: Int, column: Int, matchLength: Int, excerptBytes: [UInt8], ranges: [Range<Int>]
    ) {
        self.line = line
        self.column = column
        self.matchLength = matchLength
        self.excerpt = String(decoding: excerptBytes, as: UTF8.self)
        self.highlights = Self.utf16Ranges(of: ranges, in: excerptBytes)
    }

    /// `ranges`, byte offsets into `bytes`, as UTF-16 offsets into its decoded text. Every
    /// byte that is not a continuation byte starts one UTF-16 unit, and a 4-byte leader two.
    static func utf16Ranges(of ranges: [Range<Int>], in bytes: [UInt8]) -> [Range<Int>] {
        guard !ranges.isEmpty, String(validating: bytes, as: UTF8.self) != nil else { return [] }
        var units = [Int](repeating: 0, count: bytes.count + 1)
        var offset = 0
        for (index, byte) in bytes.enumerated() {
            units[index] = offset
            if byte & 0xC0 != 0x80 { offset += byte >= 0xF0 ? 2 : 1 }
        }
        units[bytes.count] = offset
        var mapped: [Range<Int>] = []
        for range in ranges where range.lowerBound >= 0 && range.upperBound <= bytes.count {
            mapped.append(units[range.lowerBound]..<units[range.upperBound])
        }
        return mapped
    }
}

/// What searching one file found.
public enum FileSearchResult: Sendable, Hashable {
    /// Text that was searched; `lines` is empty when nothing matched.
    case searched(lines: [ContentLineMatch], truncated: Bool)
    /// A NUL byte near the start; not searched.
    case binary
    /// Larger than the limit; not read.
    case tooLarge(size: UInt64, limit: UInt64)
    /// A working-tree file that could not be read, such as one deleted since it was listed.
    case unreadable(String)
}

/// One file with matching lines.
public struct ContentSearchHit: Sendable, Hashable {
    public let entry: PathIndexEntry
    /// Matching lines in file order.
    public let lines: [ContentLineMatch]
    /// Whether the file had more matching lines than `lines` holds.
    public let truncated: Bool

    public init(entry: PathIndexEntry, lines: [ContentLineMatch], truncated: Bool) {
        self.entry = entry
        self.lines = lines
        self.truncated = truncated
    }
}

/// How far a content search got and what it could not read.
public struct ContentSearchProgress: Sendable, Hashable {
    /// Files the search will read.
    public var totalFiles: Int
    /// Files read so far, matching or not.
    public var searchedFiles = 0
    /// Files with at least one matching line.
    public var matchedFiles = 0
    /// Matching lines found.
    public var matchedLines = 0
    /// Files skipped as binary or over the size limit.
    public var skippedFiles = 0
    /// Working-tree files that could not be read.
    public var unreadableFiles = 0
    /// Whether the search stopped at `ContentSearch.maxMatchedLines`.
    public var stoppedAtLimit = false
    /// Whether the file list stopped at `PathIndex.maxEntries`, so files may be missing.
    public var indexTruncated: Bool

    public init(totalFiles: Int, indexTruncated: Bool) {
        self.totalFiles = totalFiles
        self.indexTruncated = indexTruncated
    }

    /// Counts one file's result, returning its hit when it matched.
    mutating func count(_ result: FileSearchResult, for entry: PathIndexEntry)
        -> ContentSearchHit?
    {
        searchedFiles += 1
        switch result {
        case let .searched(lines, truncated):
            guard !lines.isEmpty else { return nil }
            matchedFiles += 1
            matchedLines += lines.count
            return ContentSearchHit(entry: entry, lines: lines, truncated: truncated)
        case .binary, .tooLarge:
            skippedFiles += 1
        case .unreadable:
            unreadableFiles += 1
        }
        return nil
    }
}

/// Searches file contents a batch at a time over a file list the caller already has: the path
/// index, so nothing new walks the tree. Each batch is one core call bounded by
/// `ContentSearchLimits.batchBytes`, run off the cooperative pool; the search checks for
/// cancellation between batches and stops at `maxMatchedLines`.
public enum ContentSearch {
    /// Matching lines after which a search stops, so a one-letter query cannot fill memory.
    public static let maxMatchedLines = 2_000
    /// Files asked for in one core call; the core may stop sooner at its byte budget.
    static let filesPerCall = 1_024

    /// The files a search reads: regular files, and hidden ones only when `includeHidden`.
    /// Symlinks hold a path, not text, and are left out.
    public static func searchable(_ entries: [PathIndexEntry], includeHidden: Bool)
        -> [PathIndexEntry]
    {
        entries.filter { entry in
            (entry.kind == .blob || entry.kind == .executableBlob)
                && (includeHidden || !entry.hidden)
        }
    }

    /// Searches `files` in order, `filesPerCall` keys per call to `search`, and hands each
    /// batch's hits to `report` in file order. Files sharing a key (one blob at two paths) are
    /// read once.
    ///
    /// - Throws: `CancellationError` when the task is cancelled between batches; the error
    ///   `search` throws; a `RepositoryError` when a call reads nothing, which would never end.
    @MainActor
    static func run<Key: Hashable & Sendable>(
        files: [PathIndexEntry], indexTruncated: Bool,
        key: (PathIndexEntry) -> Key,
        search: @escaping @Sendable ([Key]) throws -> [FileSearchResult],
        report: ([ContentSearchHit], ContentSearchProgress) -> Void
    ) async throws -> ContentSearchProgress {
        var progress = ContentSearchProgress(
            totalFiles: files.count, indexTruncated: indexTruncated)
        var keys: [Key] = []
        var seen = Set<Key>()
        for file in files where seen.insert(key(file)).inserted {
            keys.append(key(file))
        }
        var found: [Key: FileSearchResult] = [:]
        var nextKey = 0
        var nextFile = 0
        while nextKey < keys.count, !progress.stoppedAtLimit {
            try Task.checkCancellation()
            let asked = Array(keys[nextKey..<min(nextKey + filesPerCall, keys.count)])
            let results = try await BlockingWork.run { Result { try search(asked) } }.get()
            try Task.checkCancellation()
            guard !results.isEmpty else {
                throw RepositoryError("content search read none of \(asked.count) files")
            }
            for (asked, result) in zip(asked, results) { found[asked] = result }
            nextKey += results.count
            var hits: [ContentSearchHit] = []
            while nextFile < files.count, let result = found[key(files[nextFile])] {
                if let hit = progress.count(result, for: files[nextFile]) { hits.append(hit) }
                nextFile += 1
                if progress.matchedLines >= maxMatchedLines {
                    progress.stoppedAtLimit = true
                    break
                }
            }
            report(hits, progress)
        }
        return progress
    }

    /// The panel's status line for `progress`: the count while searching, the totals and what
    /// was left out once done.
    public static func statusText(_ progress: ContentSearchProgress, finished: Bool) -> String {
        guard finished else {
            guard progress.totalFiles > 0 else { return "Searching…" }
            return "Searching \(grouped(progress.searchedFiles)) of "
                + "\(grouped(progress.totalFiles)) files…"
        }
        var parts: [String] = []
        if progress.matchedLines == 0 {
            parts.append("No matches")
        } else {
            let lines = plural(progress.matchedLines, "line")
            parts.append("\(lines) in \(plural(progress.matchedFiles, "file"))")
        }
        if progress.stoppedAtLimit {
            parts.append("stopped at \(grouped(maxMatchedLines)) lines")
        }
        if progress.indexTruncated {
            parts.append("only the first \(grouped(PathIndex.maxEntries)) entries searched")
        }
        if progress.skippedFiles > 0 {
            parts.append("\(plural(progress.skippedFiles, "file")) binary or over 1 MB skipped")
        }
        if progress.unreadableFiles > 0 {
            parts.append("\(plural(progress.unreadableFiles, "file")) unreadable")
        }
        return parts.joined(separator: " · ")
    }

    private static func plural(_ count: Int, _ noun: String) -> String {
        "\(grouped(count)) \(noun)\(count == 1 ? "" : "s")"
    }

    private static func grouped(_ count: Int) -> String {
        ByteFormat.grouped(UInt64(max(count, 0)))
    }
}

/// One Find in Repository panel session: runs a query, collects its hits as batches land, and
/// keeps only the newest query's results. Return searches when the field holds a query not yet
/// searched, and opens the selected row otherwise, so a content search never runs per
/// keystroke.
@MainActor
public final class ContentSearchSession {
    public typealias Report =
        @MainActor @Sendable ([ContentSearchHit], ContentSearchProgress) -> Void
    public typealias Provider = (String, @escaping Report) async throws -> ContentSearchProgress

    /// What the panel shows below the field.
    public enum Status: Equatable {
        case idle
        case searching(ContentSearchProgress)
        case finished(ContentSearchProgress)
        case failed(String)
    }

    /// One row of the result list: a file, or one of its matching lines.
    public enum Row: Hashable {
        case file(Int)
        case line(file: Int, line: Int)
    }

    public var provider: Provider?
    /// Fires on the main actor whenever `hits`, `rows` or `status` changed.
    public var onUpdate: (() -> Void)?
    /// Opens a file at a match; `nil` match opens it at the top.
    public var onOpen: ((PathIndexEntry, ContentLineMatch?) -> Void)?

    public private(set) var hits: [ContentSearchHit] = []
    public private(set) var rows: [Row] = []
    public private(set) var status: Status = .idle
    /// The query whose results are on screen or loading; `nil` before the first search.
    public private(set) var searchedQuery: String?
    private var generation = 0
    private var running: Task<Void, Never>?

    public init() {}

    /// Stops any search and empties the list.
    public func reset() {
        running?.cancel()
        running = nil
        generation += 1
        hits = []
        rows = []
        status = .idle
        searchedQuery = nil
    }

    /// Return in the field: searches `query` when it is not the query on screen, else opens
    /// `selectedRow`, or the first row when none is selected.
    ///
    /// - Returns: The task applying the search, or nil when nothing was searched.
    @discardableResult
    public func submit(_ query: String, selectedRow: Int?) -> Task<Void, Never>? {
        if query == searchedQuery {
            open(row: selectedRow ?? 0)
            return nil
        }
        return search(query)
    }

    /// Starts `query`, cancelling any search still running. An empty query clears the list.
    ///
    /// - Returns: The task applying the search, or nil when nothing was searched.
    @discardableResult
    public func search(_ query: String) -> Task<Void, Never>? {
        reset()
        guard let provider, !query.isEmpty else {
            onUpdate?()
            return nil
        }
        let generation = generation
        searchedQuery = query
        status = .searching(ContentSearchProgress(totalFiles: 0, indexTruncated: false))
        onUpdate?()
        let task = Task { @MainActor in
            do {
                let progress = try await provider(query) { batch, progress in
                    guard generation == self.generation else { return }
                    self.append(batch)
                    self.status = .searching(progress)
                    self.onUpdate?()
                }
                guard generation == self.generation else { return }
                self.status = .finished(progress)
            } catch is CancellationError {
                return
            } catch {
                guard generation == self.generation else { return }
                self.status = .failed(RepositoryError(wrapping: error).message)
            }
            self.running = nil
            self.onUpdate?()
        }
        running = task
        return task
    }

    /// Opens `row`: a line at its match, a file at its first match. A row outside the list
    /// opens nothing.
    public func open(row: Int) {
        guard rows.indices.contains(row) else { return }
        switch rows[row] {
        case let .file(file):
            onOpen?(hits[file].entry, hits[file].lines.first)
        case let .line(file, line):
            onOpen?(hits[file].entry, hits[file].lines[line])
        }
    }

    private func append(_ batch: [ContentSearchHit]) {
        for hit in batch {
            let file = hits.count
            hits.append(hit)
            rows.append(.file(file))
            for line in hit.lines.indices {
                rows.append(.line(file: file, line: line))
            }
        }
    }
}

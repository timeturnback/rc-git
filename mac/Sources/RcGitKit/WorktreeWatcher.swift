import CoreServices
import Foundation

/// Decides which filesystem events under the working directory mean the change list may be
/// stale. Everything inside `.git` is dropped except the common dir's `info/exclude`, whose
/// patterns decide which untracked files the status walk reports: the ref watcher owns the rest
/// of that side, and object or index churn never changes what the working tree looks like. In a
/// linked worktree that file lies outside the working directory, under the shared common dir,
/// and still counts. Events strictly
/// below a directory the last scan pruned as ignored (`StatusReport.ignoredDirs`) are dropped
/// too: the walk never opens it, so a build writing into `target/` cannot change the list. The
/// directory's own path stays relevant, since a file replacing it may not be ignored. Pure, so
/// the filter is testable without FSEvents.
public enum WorktreeWatchFilter {
    /// Whether an event at `path` can change the outcome of `worktreeStatus`. `commonDir` is
    /// the git directory every worktree shares (`gitDir` itself outside a linked worktree);
    /// its `info/exclude` is the one the status walk reads. `ignoredRoots` holds
    /// repository-relative directories the last status walk pruned unopened.
    public static func isRelevant(
        _ path: String, workDir: String, gitDir: String, commonDir: String,
        ignoredRoots: Set<String> = []
    ) -> Bool {
        if path == excludeFile(commonDir: commonDir) { return true }
        let root = normalized(workDir)
        guard path == root || path.hasPrefix(root + "/") else { return false }
        let git = normalized(gitDir)
        if path == git || path.hasPrefix(git + "/") { return false }
        return !isBelowIgnoredRoot(String(path.dropFirst(root.count + 1)), ignoredRoots)
    }

    /// Whether some proper ancestor of the repository-relative `relative` is an ignored root.
    /// Checks each ancestor in the set, so the cost follows the path's depth, not the set.
    private static func isBelowIgnoredRoot(_ relative: String, _ roots: Set<String>) -> Bool {
        guard !roots.isEmpty else { return false }
        var index = relative.startIndex
        while let slash = relative[index...].firstIndex(of: "/") {
            if roots.contains(String(relative[..<slash])) { return true }
            index = relative.index(after: slash)
        }
        return false
    }

    /// The repository's own exclude file, shared by every worktree.
    static func excludeFile(commonDir: String) -> String {
        normalized(commonDir) + "/info/exclude"
    }

    private static func normalized(_ dir: String) -> String {
        dir.hasSuffix("/") ? String(dir.dropLast()) : dir
    }
}

/// Watches a repository's working directory with FSEvents and reports when the change list
/// may be stale. Events are filtered through `WorktreeWatchFilter`. FSEvents delivers a burst
/// such as one `git checkout` as several callbacks: one at once (`NoDefer`, so a single editor
/// save refreshes immediately), then the rest after the latency window, possibly in more than
/// one chunk. Consumers must coalesce, as `ChangesModel.loadStatus` does.
///
/// The stream is rooted at the working directory and, when the common dir lies outside it (a
/// linked worktree), also at the common dir's `info` folder, so an exclude edit there is seen.
///
/// FSEvents reports canonical realpaths, so every directory is resolved with `realpath(3)`
/// once at init; resolving only the working directory would let every `.git` write past the
/// exclusion.
///
/// `onChange` fires on a private queue; hop to the main actor before touching models.
public final class WorktreeWatcher: @unchecked Sendable {
    private let workDir: String
    private let gitDir: String
    private let commonDir: String
    private let latency: TimeInterval
    private let onChange: @Sendable () -> Void
    private let queue = DispatchQueue(label: "rc-git.worktree-watcher")
    private var stream: FSEventStreamRef?
    private let ignoredLock = NSLock()
    private var ignoredRoots: Set<String> = []

    public init(
        workDir: String, gitDir: String, commonDir: String, latency: TimeInterval = 0.5,
        onChange: @escaping @Sendable () -> Void
    ) {
        self.workDir = Self.canonical(workDir)
        self.gitDir = Self.canonical(gitDir)
        self.commonDir = Self.canonical(commonDir)
        self.latency = latency
        self.onChange = onChange
    }

    /// The realpath of `path`, or `path` itself when it cannot be resolved (it does not exist).
    static func canonical(_ path: String) -> String {
        guard let resolved = realpath(path, nil) else { return path }
        defer { free(resolved) }
        return String(cString: resolved)
    }

    deinit {
        stop()
    }

    /// Replaces the directories whose contents no longer count as a change, with the
    /// `StatusReport.ignoredDirs` of the latest scan. Safe to call from any thread.
    public func setIgnoredDirs(_ dirs: [String]) {
        let roots = Set(dirs)
        ignoredLock.withLock { ignoredRoots = roots }
    }

    /// The `.git` subdirectories FSEvents should never report: object, ref and reflog churn
    /// from a fetch or gc. Not the whole `.git`, whose `info/exclude` stays relevant, and none
    /// when `.git` lies outside the watched root (a linked worktree), where they would not apply.
    var excludedPaths: [String] {
        guard gitDir.hasPrefix(workDir + "/") else { return [] }
        return ["objects", "refs", "logs"].map { gitDir + "/" + $0 }
    }

    /// The directories the stream is rooted at: the working directory, plus the common dir's
    /// `info` folder when it lies outside it.
    var watchedPaths: [String] {
        let info = commonDir + "/info"
        guard !info.hasPrefix(workDir + "/") else { return [workDir] }
        return [workDir, info]
    }

    /// Starts delivering events. Later calls do nothing while a stream is running; after a
    /// failure, `start()` may be called again.
    ///
    /// - Throws: `WatcherStartError` naming the FSEvents call that failed and the watched path.
    public func start() throws {
        try queue.sync {
            guard stream == nil else { return }
            var context = FSEventStreamContext(
                version: 0,
                info: Unmanaged.passUnretained(self).toOpaque(),
                retain: nil, release: nil, copyDescription: nil
            )
            let flags = UInt32(
                kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagUseCFTypes
                    | kFSEventStreamCreateFlagNoDefer)
            guard
                let created = FSEventStreamCreate(
                    kCFAllocatorDefault, worktreeEventCallback, &context,
                    watchedPaths as CFArray, FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
                    latency, flags)
            else { throw WatcherStartError(call: "FSEventStreamCreate", path: workDir) }
            // A refused exclusion list only costs filtered callbacks: `WorktreeWatchFilter`
            // drops every `.git` path the stream would have excluded.
            _ = FSEventStreamSetExclusionPaths(created, excludedPaths as CFArray)
            FSEventStreamSetDispatchQueue(created, queue)
            guard FSEventStreamStart(created) else {
                FSEventStreamInvalidate(created)
                FSEventStreamRelease(created)
                throw WatcherStartError(call: "FSEventStreamStart", path: workDir)
            }
            stream = created
        }
    }

    /// Stops delivering events and releases the stream.
    public func stop() {
        let stopped: FSEventStreamRef? = queue.sync {
            defer { stream = nil }
            return stream
        }
        guard let stopped else { return }
        FSEventStreamStop(stopped)
        FSEventStreamInvalidate(stopped)
        FSEventStreamRelease(stopped)
    }

    fileprivate func handle(paths: [String]) {
        let ignored = ignoredLock.withLock { ignoredRoots }
        if paths.contains(where: {
            WorktreeWatchFilter.isRelevant(
                $0, workDir: workDir, gitDir: gitDir, commonDir: commonDir,
                ignoredRoots: ignored)
        }) {
            onChange()
        }
    }
}

private func worktreeEventCallback(
    _ stream: ConstFSEventStreamRef,
    _ info: UnsafeMutableRawPointer?,
    _ count: Int,
    _ eventPaths: UnsafeMutableRawPointer,
    _ flags: UnsafePointer<FSEventStreamEventFlags>,
    _ ids: UnsafePointer<FSEventStreamEventId>
) {
    guard let info else { return }
    let watcher = Unmanaged<WorktreeWatcher>.fromOpaque(info).takeUnretainedValue()
    guard let paths = unsafeBitCast(eventPaths, to: CFArray.self) as? [String] else { return }
    watcher.handle(paths: paths)
}

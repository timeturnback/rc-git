import CoreServices
import Foundation

/// Decides which filesystem events inside `.git` mean the refs or their upstreams may have
/// changed.
///
/// The watcher observes the whole `.git` directory, because `HEAD`, `packed-refs` and `config`
/// (remotes and upstreams) live at its top level while branches and tags live anywhere under
/// `refs/`. Everything else in the directory (objects, index, logs) changes far more often and
/// never affects the ref listing, so those events are filtered out here. Pure, so the filter
/// is testable without FSEvents.
public enum RefWatchFilter {
    /// Whether an event at `path` can change the outcome of `refs()`, `head()`, or the
    /// upstream status.
    public static func isRelevant(_ path: String, gitDir: String) -> Bool {
        let root = gitDir.hasSuffix("/") ? String(gitDir.dropLast()) : gitDir
        guard path == root || path.hasPrefix(root + "/") else { return false }
        var relative = String(path.dropFirst(root.count))
        if relative.hasPrefix("/") { relative.removeFirst() }
        while relative.hasSuffix("/") { relative.removeLast() }
        if relative.hasSuffix(".lock") { return false }
        // The directory itself: `packed-refs` rewrites and ref packs land as events here.
        if relative.isEmpty { return true }
        // `config` names the remotes and upstreams the sync status reads: `git remote add`
        // and `git branch -u` change nothing else.
        return relative == "HEAD" || relative == "packed-refs" || relative == "config"
            || relative == "refs" || relative.hasPrefix("refs/")
    }
}

/// A watcher's FSEvents stream could not be created or started, so no change will be reported.
public struct WatcherStartError: Error, CustomStringConvertible {
    /// The FSEvents call that failed.
    public let call: String
    /// The directory the watcher was asked to observe.
    public let path: String

    public var description: String {
        "\(call) failed for \(path): changes on disk will not refresh this window"
    }
}

/// Watches a repository's `.git` directory with FSEvents and reports when its refs may have
/// changed. Events are coalesced by FSEvents' own latency window and filtered through
/// `RefWatchFilter`, so one `git commit` or `git fetch` produces one callback, not a storm.
///
/// FSEvents reports canonical realpaths (`/private/tmp/...`, symlinks resolved), so the watched
/// directory is resolved with `realpath(3)` once at init and that spelling is both watched and
/// filtered against.
///
/// `onChange` fires on a private queue; hop to the main actor before touching models.
public final class RefWatcher: @unchecked Sendable {
    private let gitDir: String
    private let latency: TimeInterval
    private let onChange: @Sendable () -> Void
    private let queue = DispatchQueue(label: "rc-git.ref-watcher")
    private var stream: FSEventStreamRef?

    public init(
        gitDir: String, latency: TimeInterval = 0.2, onChange: @escaping @Sendable () -> Void
    ) {
        self.gitDir = Self.canonical(gitDir)
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
                    kCFAllocatorDefault, eventCallback, &context,
                    [gitDir] as CFArray, FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
                    latency, flags)
            else { throw WatcherStartError(call: "FSEventStreamCreate", path: gitDir) }
            FSEventStreamSetDispatchQueue(created, queue)
            guard FSEventStreamStart(created) else {
                FSEventStreamInvalidate(created)
                FSEventStreamRelease(created)
                throw WatcherStartError(call: "FSEventStreamStart", path: gitDir)
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
        if paths.contains(where: { RefWatchFilter.isRelevant($0, gitDir: gitDir) }) {
            onChange()
        }
    }
}

private func eventCallback(
    _ stream: ConstFSEventStreamRef,
    _ info: UnsafeMutableRawPointer?,
    _ count: Int,
    _ eventPaths: UnsafeMutableRawPointer,
    _ flags: UnsafePointer<FSEventStreamEventFlags>,
    _ ids: UnsafePointer<FSEventStreamEventId>
) {
    guard let info else { return }
    let watcher = Unmanaged<RefWatcher>.fromOpaque(info).takeUnretainedValue()
    guard let paths = unsafeBitCast(eventPaths, to: CFArray.self) as? [String] else { return }
    watcher.handle(paths: paths)
}

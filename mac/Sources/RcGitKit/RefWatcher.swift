import CoreServices
import Foundation

/// Decides which filesystem events inside `.git` mean the refs may have changed.
///
/// The watcher observes the whole `.git` directory, because `HEAD` and `packed-refs` live at
/// its top level while branches and tags live anywhere under `refs/`. Everything else in the
/// directory (objects, index, logs) changes far more often and never affects the ref listing,
/// so those events are filtered out here. Pure, so the filter is testable without FSEvents.
public enum RefWatchFilter {
    /// Whether an event at `path` can change the outcome of `refs()` or `head()`.
    public static func isRelevant(_ path: String, gitDir: String) -> Bool {
        let root = gitDir.hasSuffix("/") ? String(gitDir.dropLast()) : gitDir
        guard path.hasPrefix(root) else { return false }
        var relative = String(path.dropFirst(root.count))
        if relative.hasPrefix("/") { relative.removeFirst() }
        while relative.hasSuffix("/") { relative.removeLast() }
        if relative.hasSuffix(".lock") { return false }
        // The directory itself: `packed-refs` rewrites and ref packs land as events here.
        if relative.isEmpty { return true }
        return relative == "HEAD" || relative == "packed-refs" || relative == "refs"
            || relative.hasPrefix("refs/")
    }
}

/// Watches a repository's `.git` directory with FSEvents and reports when its refs may have
/// changed. Events are coalesced by FSEvents' own latency window and filtered through
/// `RefWatchFilter`, so one `git commit` or `git fetch` produces one callback, not a storm.
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
        self.gitDir = gitDir
        self.latency = latency
        self.onChange = onChange
    }

    deinit {
        stop()
    }

    /// Starts delivering events. Safe to call once; later calls do nothing.
    public func start() {
        queue.sync {
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
            else { return }
            FSEventStreamSetDispatchQueue(created, queue)
            FSEventStreamStart(created)
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
